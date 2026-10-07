#!/usr/bin/env python3
"""Exercise fork CLI/RPC, durable queue snapshots, source isolation and rollback."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'debug'


def run(*args, cwd=None, env=None):
    return subprocess.run(args, cwd=cwd, env=env, check=True, capture_output=True, text=True).stdout


def wait(predicate):
    end = time.monotonic() + 20
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError('timed out')


with tempfile.TemporaryDirectory(prefix='sigmadock-fork-', dir='/tmp') as directory:
    root = Path(directory)
    repo, state = root / 'repo', root / 'state'
    repo.mkdir()
    run('git', 'init', '-b', 'main', str(repo))
    for key, value in [('user.email', 'test@localhost'), ('user.name', 'Test'), ('commit.gpgsign', 'false')]:
        run('git', '-C', str(repo), 'config', key, value)
    (repo / 'tracked').write_text('base\n')
    (repo / '.gitignore').write_text('ignored\n')
    run('git', '-C', str(repo), 'add', '.')
    run('git', '-C', str(repo), 'commit', '-m', 'initial')
    harnesses = root / 'bin'
    harnesses.mkdir()
    fake = harnesses / 'codex'
    fake.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > fork-args\nexec /bin/sh\n')
    fake.chmod(0o755)
    sock = str(state / 'daemon.sock')
    env = dict(os.environ, SHELL='/bin/sh', SIGMA_DOCK_SOCKET=sock, PATH=str(harnesses) + os.pathsep + os.environ['PATH'])
    log = open(root / 'daemon.log', 'w')
    daemon = None

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
        process = subprocess.Popen([str(BIN / 'sigmadockd'), '--state-dir', str(state), '--max-workers', '2'], env=env, stdout=log, stderr=log)
        def ready():
            if process.poll() is not None:
                raise AssertionError((root / 'daemon.log').read_text())
            try:
                return rpc('ping')['version'] == 2
            except OSError:
                return False
        wait(ready)
        return process

    def git(path, *args):
        return run('git', '-C', str(path), *args)

    def worker(id):
        return rpc('get_worker_status', {'worker_id': id})['worker']

    def source_state(path):
        index = Path(git(path, 'rev-parse', '--git-path', 'index').strip())
        if not index.is_absolute():
            index = path / index
        return (index.read_bytes(), git(path, 'rev-parse', 'HEAD'), git(path, 'status', '--porcelain'), git(path, 'stash', 'list'), (path / 'tracked').read_bytes(), (path / 'untracked').read_bytes())

    try:
        daemon = start()
        project = rpc('add_project', {'path': str(repo)})
        source = rpc('spawn_worker', {'project_id': project['id'], 'title': 'source', 'agent': 'shell', 'base': 'HEAD'})
        wt = Path(source['worktree'])
        (wt / 'tracked').write_text('source commit\n')
        git(wt, 'add', '.')
        git(wt, 'commit', '-m', 'source branch ahead of main')
        head = git(wt, 'rev-parse', 'HEAD')
        (wt / 'tracked').write_text('staged\n')
        git(wt, 'add', 'tracked')
        (wt / 'tracked').write_text('working\n')
        (wt / 'untracked').write_bytes(b'\x00\xffrequest-time\n')
        (wt / 'ignored').write_text('private ignored file')
        before = source_state(wt)
        fork = json.loads(run(str(BIN / 'sdk'), 'fork', source['id'], '--title', 'fork', '--include-uncommitted', env=env))
        dest = Path(fork['worktree'])
        assert fork['forked_from'] == source['id']
        assert fork['agent'] == 'shell'
        assert git(dest, 'rev-parse', 'HEAD') == head
        assert git(dest, 'diff', '--cached') == git(wt, 'diff', '--cached')
        assert (dest / 'tracked').read_bytes() == b'working\n'
        assert (dest / 'untracked').read_bytes() == b'\x00\xffrequest-time\n'
        assert not (dest / 'ignored').exists()
        assert source_state(wt) == before
        assert rpc('capacity')['in_use'] == 2
        rpc('fork_worker', {'worker_id': source['id'], 'title': 'no room'}, error=True)
        assert source_state(wt) == before
        queued = json.loads(run(str(BIN / 'sdk'), 'fork', source['id'], '--title', 'queued fork', '--include-uncommitted', '--queue', env=env))
        assert queued['queued'] and source_state(wt) == before
        qid = queued['id']
        assert git(repo, 'rev-parse', '--verify', f'refs/sigmadock/forks/{qid}').strip()
        cancel = rpc('fork_worker', {'worker_id': source['id'], 'title': 'cancel', 'queue': True})
        rpc('cancel_queued', {'id': cancel['id']})
        assert not git(repo, 'for-each-ref', f"refs/sigmadock/forks/{cancel['id']}").strip()
        (wt / 'tracked').write_text('changed after queue\n')
        (wt / 'untracked').write_bytes(b'later')
        git(wt, 'add', '.')
        git(wt, 'commit', '-m', 'source advanced after queue')
        # Queue and lineage survive restart; explicit stop frees one berth.
        daemon.terminate()
        daemon.wait(timeout=15)
        daemon = start()
        assert worker(fork['id'])['forked_from'] == source['id']
        rpc('stop_worker', {'worker_id': fork['id']})
        wait(lambda: not rpc('list_queue'))
        started = worker(qid)
        queued_wt = Path(started['worktree'])
        assert started['forked_from'] == source['id']
        assert git(queued_wt, 'rev-parse', 'HEAD') == head
        assert (queued_wt / 'tracked').read_bytes() == b'working\n'
        assert (queued_wt / 'untracked').read_bytes() == b'\x00\xffrequest-time\n'
        assert not git(repo, 'for-each-ref', f'refs/sigmadock/forks/{qid}').strip()
        rpc('stop_worker', {'worker_id': qid})
        wait(lambda: rpc('capacity')['in_use'] == 1)
        # Invalid copied config fails before publication and removes the dirty new worktree.
        (wt / '.sigmadock.toml').write_text('[invalid')
        worktrees = git(repo, 'worktree', 'list', '--porcelain')
        rpc('fork_worker', {'worker_id': source['id'], 'title': 'invalid config', 'include_uncommitted': True}, error=True)
        assert git(repo, 'worktree', 'list', '--porcelain') == worktrees
        assert (wt / '.sigmadock.toml').read_text() == '[invalid'
        clean = rpc('fork_worker', {'worker_id': source['id'], 'title': 'HEAD only', 'agent': 'codex', 'prompt': 'Explore another approach'})
        clean_wt = Path(clean['worktree'])
        assert clean['agent'] == 'codex' and clean['prompt'] == 'Explore another approach'
        wait(lambda: (clean_wt / 'fork-args').exists())
        assert 'Explore another approach' in (clean_wt / 'fork-args').read_text()
        assert not (clean_wt / '.sigmadock.toml').exists()
        assert git(clean_wt, 'rev-parse', 'HEAD') == git(wt, 'rev-parse', 'HEAD')
        assert not git(repo, 'for-each-ref', 'refs/sigmadock/forks/').strip()
        rpc('stop_worker', {'worker_id': clean['id']})
        wait(lambda: rpc('capacity')['in_use'] <= 1)
        (wt / '.sigmadock.toml').unlink()
        (wt / 'ignored').unlink()
        rpc('archive_worker', {'worker_id': source['id'], 'cleanup': True})
        wait(lambda: not wt.exists())
        rpc('fork_worker', {'worker_id': source['id'], 'title': 'local after cleanup', 'include_uncommitted': True}, error=True)
        archived_fork = rpc('fork_worker', {'worker_id': source['id'], 'title': 'from archived branch'})
        assert archived_fork['forked_from'] == source['id']
        assert git(Path(archived_fork['worktree']), 'rev-parse', 'HEAD') == git(repo, 'rev-parse', source['branch'])
        print('fork smoke: passed (CLI, source isolation, lineage, capacity, durable queue, cancellation, rollback, HEAD-only)')
    finally:
        if daemon is not None and daemon.poll() is None:
            daemon.terminate()
            daemon.wait(timeout=15)
        log.close()
