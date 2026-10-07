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
                return rpc('ping')['version'] == 1
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
        print('PASS: worker isolation, PTY I/O, capacity, persistence, resume, safe cleanup, CLI and MCP')
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
