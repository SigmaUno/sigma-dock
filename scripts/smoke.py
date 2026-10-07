#!/usr/bin/env python3
"""Exercise real daemon RPC, PTYs, worktrees and recovery in a temporary repository."""
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'debug'

def run(*args, cwd=None):
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True).stdout

def wait_for(predicate, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError('timed out')

with tempfile.TemporaryDirectory(prefix='sigma-smoke-', dir='/tmp') as temp:
    temp = Path(temp)
    repo, state = temp / 'repo', temp / 'state'
    repo.mkdir()
    run('git', 'init', '-b', 'main', str(repo))
    run('git', '-C', str(repo), 'config', 'user.email', 'test@localhost')
    run('git', '-C', str(repo), 'config', 'user.name', 'Test')
    run('git', '-C', str(repo), 'config', 'commit.gpgsign', 'false')
    (repo / 'README').write_text('test\n')
    run('git', '-C', str(repo), 'add', '.')
    run('git', '-C', str(repo), 'commit', '-m', 'initial')
    sock = str(state / 'daemon.sock')
    env = dict(os.environ, SHELL='/bin/sh', SIGMA_DOCK_SOCKET=sock)
    log = open(temp / 'daemon.log', 'w')
    daemon = None
    workers = []

    def rpc(method, params=None, error=False):
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(20)
            stream.connect(sock)
            stream.sendall(json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params or {}}).encode() + b'\n')
            reply = json.loads(stream.makefile('rb').readline())
        if error:
            assert 'error' in reply, reply
            return reply['error']
        assert 'error' not in reply, reply
        return reply['result']

    def start():
        process = subprocess.Popen([str(BIN / 'sigmadockd'), '--state-dir', str(state), '--max-workers', '5'], env=env, stdout=log, stderr=log)
        def ready():
            if process.poll() is not None:
                raise AssertionError((temp / 'daemon.log').read_text())
            try:
                return rpc('ping')['version'] == 2
            except (OSError, AssertionError):
                return False
        wait_for(ready)
        return process

    def exited(worker):
        return rpc('get_worker_status', {'worker_id': worker['id']})['worker']['facts']['session'] == 'exited'

    try:
        daemon = start()
        project = rpc('add_project', {'path': str(repo)})
        assert rpc('add_project', {'path': str(repo)})['id'] == project['id']
        # Invalid creation must not reserve a worktree or break subsequent requests.
        rpc('spawn_worker', {'project_id': project['id'], 'title': 'invalid', 'agent': 'unknown'}, error=True)
        for title in ['one', 'two', 'three', 'four', 'five']:
            worker = rpc('spawn_worker', {'project_id': project['id'], 'title': title, 'agent': 'shell'})
            workers.append(worker)
        assert len({w['port'] for w in workers}) == 5
        assert len({w['worktree'] for w in workers}) == 5
        assert all(Path(w['worktree']).exists() for w in workers)
        rpc('spawn_worker', {'project_id': project['id'], 'title': 'overflow', 'agent': 'shell'}, error=True)
        assert [worker['berth'] for worker in workers] == [1, 2, 3, 4, 5]
        status = rpc('get_worker_status', {'worker_id': workers[0]['id']})
        assert status['berth'] == 1 and status['status'] == status['column']
        capacity = rpc('capacity')
        assert capacity['in_use'] == 5 and capacity['queued'] == 0
        assert capacity['per_project'][project['id']]['in_use'] == 5
        rpc('set_max_workers', {'max_workers': 0}, error=True)
        rpc('set_max_workers', {'max_workers': 256}, error=True)
        rpc('set_max_workers', {'max_workers': 2})
        assert rpc('get_worker_status', {'worker_id': workers[4]['id']})['berth'] == 5
        rpc('spawn_worker', {'project_id': project['id'], 'title': 'over lowered limit', 'agent': 'shell'}, error=True)
        rpc('set_max_workers', {'max_workers': 5})
        # A second project lets us verify a global FIFO rather than one queue per repo.
        other_repo = temp / 'other-repo'
        run('git', 'clone', str(repo), str(other_repo))
        other_project = rpc('add_project', {'path': str(other_repo)})
        # Large queues stay within the transport frame limit and CLI listing fetches all pages.
        paged_ids = [rpc('spawn_worker', {'project_id': project['id'], 'title': f'page {index}', 'agent': 'shell', 'queue': True})['id'] for index in range(101)]
        page = rpc('list_queue')
        assert len(page) == 100 and all('prompt' not in task and 'forge' not in task for task in page)
        assert rpc('list_queue', {'offset': 100})[0]['position'] == 101
        assert len(json.loads(run(str(BIN / 'sdk'), '--socket', sock, 'queue'))) == 101
        rpc('list_queue', {'limit': 101}, error=True)
        for task_id in paged_ids:
            rpc('cancel_queued', {'id': task_id})
        first = rpc('spawn_worker', {'project_id': project['id'], 'title': 'queued first', 'agent': 'shell', 'queue': True})
        second = json.loads(run(str(BIN / 'sdk'), '--socket', sock, 'spawn', other_project['id'], '--title', 'queued second', '--agent', 'shell', '--queue'))
        assert first['queued'] and first['position'] == 1 and second['position'] == 2
        assert [task['id'] for task in rpc('list_queue')] == [first['id'], second['id']]
        assert not (state / 'worktrees' / first['id']).exists()
        assert not (state / 'worktrees' / second['id']).exists()
        capacity = rpc('capacity')
        assert capacity['queued'] == 2 and capacity['per_project'][other_project['id']]['queued'] == 1
        rpc('remove_project', {'project_id': other_project['id']}, error=True)
        rpc('cancel_queued', {'id': second['id'], 'project_id': project['id']}, error=True)
        # Queued tasks resolve main when they start, rather than freezing the old commit.
        (repo / 'README').write_text('advanced while waiting\n')
        run('git', '-C', str(repo), 'commit', '-am', 'advance main')
        fresh_head = run('git', '-C', str(repo), 'rev-parse', 'HEAD').strip()
        released = workers[4]
        rpc('stop_worker', {'worker_id': released['id']})
        wait_for(lambda: any(worker['id'] == first['id'] for worker in rpc('list_workers')))
        first_worker = rpc('get_worker_status', {'worker_id': first['id']})['worker']
        assert first_worker['berth'] == 5
        assert run('git', '-C', first_worker['worktree'], 'rev-parse', 'HEAD').strip() == fresh_head
        assert [task['id'] for task in rpc('list_queue')] == [second['id']]
        assert rpc('get_worker_status', {'worker_id': workers[2]['id']})['berth'] == 3
        rpc('stop_worker', {'worker_id': first_worker['id']})
        wait_for(lambda: any(worker['id'] == second['id'] for worker in rpc('list_workers')))
        second_worker = rpc('get_worker_status', {'worker_id': second['id']})['worker']
        assert second_worker['berth'] == 5 and not rpc('list_queue')
        # A free remembered slot is preferred on resume; occupied slots fall back.
        rpc('stop_worker', {'worker_id': second_worker['id']})
        wait_for(lambda: exited(second_worker))
        rpc('resume_worker', {'worker_id': second_worker['id']})
        assert rpc('get_worker_status', {'worker_id': second_worker['id']})['berth'] == 5
        rpc('stop_worker', {'worker_id': second_worker['id']})
        wait_for(lambda: exited(second_worker))
        for queued_worker in [first_worker, second_worker]:
            rpc('archive_worker', {'worker_id': queued_worker['id'], 'cleanup': True})
        rpc('remove_project', {'project_id': other_project['id']})
        assert not any(value['id'] == other_project['id'] for value in rpc('list_projects'))
        assert other_repo.exists()
        assert rpc('add_project', {'path': str(other_repo)})['id'] == other_project['id']
        # Queue cancellation creates neither a worker nor a worktree.
        rpc('resume_worker', {'worker_id': released['id']})
        cancelled = rpc('spawn_worker', {'project_id': project['id'], 'title': 'cancel me', 'agent': 'shell', 'queue': True})
        run(str(BIN / 'sdk'), '--socket', sock, 'queue', 'cancel', cancelled['id'])
        assert not rpc('list_queue') and not (state / 'worktrees' / cancelled['id']).exists()
        w = workers[0]
        # New status field and the one-release compatibility alias agree.
        worker_status = rpc('get_worker_status', {'worker_id': w['id']})
        assert worker_status['status'] == 'working', worker_status
        assert worker_status['column'] == worker_status['status'], worker_status
        request = {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call',
                   'params': {'name': 'get_worker_status', 'arguments': {'worker_id': w['id']}}}
        mcp = subprocess.run([str(BIN / 'sigmadock-mcp')], env=env,
                             input=json.dumps(request)+'\n', capture_output=True, text=True, check=True)
        mcp_status = json.loads(json.loads(mcp.stdout)['result']['content'][0]['text'])
        assert mcp_status['status'] == 'working', mcp_status
        assert mcp_status['column'] == mcp_status['status'], mcp_status
        rpc('resize', {'worker_id': w['id'], 'rows': 40, 'cols': 100})
        rpc('resize', {'worker_id': w['id'], 'rows': 0, 'cols': 100}, error=True)
        resized_output = rpc('output', {'worker_id': w['id'], 'cursor': 0})
        assert (resized_output['cols'], resized_output['rows']) == (100, 40)
        rpc('input', {'worker_id': w['id'], 'bytes': list(b"printf 'SIGMA_PTY_OK\\n'\n")})
        wait_for(lambda: b'SIGMA_PTY_OK' in bytes(rpc('output', {'worker_id': w['id'], 'cursor': 0})['bytes']))
        assert 'one' in run(str(BIN / 'sdk'), '--socket', sock, 'ls')
        # Review of the same session has independent output cursors.
        assert rpc('output', {'worker_id': w['id'], 'cursor': 0})['cursor'] > 0
        Path(w['worktree'], 'dirty').write_text('keep me')
        rpc('input', {'worker_id': w['id'], 'bytes': list(b'exit\n')})
        wait_for(lambda: exited(w))
        rpc('archive_worker', {'worker_id': w['id'], 'cleanup': True}, error=True)
        assert Path(w['worktree'], 'dirty').read_text() == 'keep me'
        Path(w['worktree'], 'dirty').unlink()
        rpc('archive_worker', {'worker_id': w['id'], 'cleanup': True})
        assert not Path(w['worktree']).exists()
        assert w['branch'] in run('git', '-C', str(repo), 'branch', '--list')
        for extra in workers[2:]:
            rpc('stop_worker', {'worker_id': extra['id']})
            wait_for(lambda: exited(extra))
            rpc('archive_worker', {'worker_id': extra['id'], 'cleanup': True})
        w = workers[1]
        rpc('input', {'worker_id': w['id'], 'bytes': list(b'exit\n')})
        wait_for(lambda: exited(w))
        saved = rpc('session_context', {'worker_id': w['id']})
        assert saved and saved['recorded_at'] and saved['state'] == 'exited'
        # Persisted exited worker can restart in the same worktree.
        daemon.terminate(); daemon.wait(timeout=5)
        daemon = start()
        assert exited(w)
        recovered = rpc('session_context', {'worker_id': w['id']})
        assert recovered['recorded_at'] == saved['recorded_at'] and recovered['text'] == saved['text']
        assert any(entry['worker']['id'] == w['id'] and entry['runtime'] == 'stopped' for entry in rpc('list_unfinished'))
        rpc('clear_session_context', {'worker_id': w['id']})
        assert rpc('session_context', {'worker_id': w['id']}) is None
        rpc('resume_worker', {'worker_id': w['id']})
        assert rpc('get_worker_status', {'worker_id': w['id']})['worker']['facts']['session'] == 'running'
        rpc('resume_worker', {'worker_id': w['id']}, error=True)
        rpc('input', {'worker_id': w['id'], 'bytes': list(b"printf 'BEFORE_CLEAR_CHECKPOINT\n'\n")})
        wait_for(lambda: 'BEFORE_CLEAR_CHECKPOINT' in (rpc('session_context', {'worker_id': w['id']}) or {}).get('text', ''))
        rpc('clear_session_context', {'worker_id': w['id']})
        rpc('input', {'worker_id': w['id'], 'bytes': list(b"printf 'AFTER_CLEAR_CHECKPOINT\n'\n")})
        wait_for(lambda: 'AFTER_CLEAR_CHECKPOINT' in (rpc('session_context', {'worker_id': w['id']}) or {}).get('text', ''))
        assert 'BEFORE_CLEAR_CHECKPOINT' not in rpc('session_context', {'worker_id': w['id']})['text']
        # MCP is local, does not offer autonomous spawn by default.
        request = {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/list', 'params': {}}
        mcp = subprocess.run([str(BIN / 'sigmadock-mcp')], env=env, input=json.dumps(request)+'\n', capture_output=True, text=True, check=True)
        assert 'spawn_worker' not in [t['name'] for t in json.loads(mcp.stdout)['result']['tools']]
        rpc('stop_worker', {'worker_id': w['id']})
        wait_for(lambda: exited(w))
        # Simulate stale running metadata only after proving the real process exited.
        daemon.terminate(); daemon.wait(timeout=5)
        with sqlite3.connect(state / 'state.sqlite') as database:
            recorded = json.loads(database.execute('SELECT data FROM workers WHERE id=?', (w['id'],)).fetchone()[0])
            recorded['facts']['session'] = 'running'
            database.execute('UPDATE workers SET data=? WHERE id=?', (json.dumps(recorded), w['id']))
        daemon = start()
        assert rpc('get_worker_status', {'worker_id': w['id']})['worker']['facts']['session'] == 'lost'
        assert 'unknown' in rpc('resume_worker', {'worker_id': w['id']}, error=True)['message']
        rpc('resume_worker', {'worker_id': w['id'], 'acknowledge_unknown': True})
        rpc('stop_worker', {'worker_id': w['id']})
        wait_for(lambda: exited(w))
        rpc('archive_worker', {'worker_id': w['id'], 'cleanup': True})
        assert len(rpc('list_workers')) == 0
        # A failed queue head stays visible and prevents a later task jumping FIFO.
        rpc('set_max_workers', {'max_workers': 1})
        holder = rpc('spawn_worker', {'project_id': project['id'], 'title': 'hold queue', 'agent': 'shell'})
        workers.append(holder)
        failed = rpc('spawn_worker', {'project_id': project['id'], 'title': 'bad base', 'agent': 'shell', 'base': 'missing-branch', 'queue': True})
        pending = rpc('spawn_worker', {'project_id': project['id'], 'title': 'after failed', 'agent': 'shell', 'queue': True})
        rpc('stop_worker', {'worker_id': holder['id']})
        wait_for(lambda: rpc('list_queue')[0]['last_error'] is not None)
        assert [task['id'] for task in rpc('list_queue')] == [failed['id'], pending['id']]
        rpc('retry_queued', {'id': failed['id']}, error=True)
        daemon.terminate(); daemon.wait(timeout=5)
        # No --max-workers override: persisted setting is authoritative on restart.
        daemon = subprocess.Popen([str(BIN / 'sigmadockd'), '--state-dir', str(state)], env=env, stdout=log, stderr=log)
        wait_for(lambda: Path(sock).exists() and daemon.poll() is None)
        wait_for(lambda: rpc('ping')['version'] == 2)
        assert rpc('capacity')['max_workers'] == 1
        assert [task['id'] for task in rpc('list_queue')] == [failed['id'], pending['id']]
        rpc('cancel_queued', {'id': failed['id']})
        wait_for(lambda: any(worker['id'] == pending['id'] for worker in rpc('list_workers')))
        pending_worker = rpc('get_worker_status', {'worker_id': pending['id']})['worker']
        workers.append(pending_worker)
        assert pending_worker['berth'] == 1 and not rpc('list_queue')
        with sqlite3.connect(state / 'state.sqlite') as database:
            assert database.execute('PRAGMA user_version').fetchone()[0] == 4
        print('PASS: stable berths, lowered capacity, global FIFO, fresh bases, cancellation, queue restart, PTY I/O, recovery, project safety, CLI and MCP')
    finally:
        if daemon is not None and daemon.poll() is None:
            for worker in workers:
                try:
                    rpc('stop_worker', {'worker_id': worker['id']})
                except (OSError, AssertionError):
                    pass
            daemon.terminate()
            daemon.wait(timeout=5)
        log.close()
