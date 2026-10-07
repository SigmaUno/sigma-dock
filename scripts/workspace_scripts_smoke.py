#!/usr/bin/env python3
"""Exercise repository approval, hooks, PTYs and process-group cleanup end to end."""
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'debug'


def run(*args, cwd=None, env=None):
    return subprocess.run(args, cwd=cwd, env=env, check=True, capture_output=True, text=True).stdout


def wait(predicate, timeout=15):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError('timed out')


with tempfile.TemporaryDirectory(prefix='sigmadock-scripts-', dir='/tmp') as directory:
    root = Path(directory)
    repo, state = root / 'repo', root / 'state'
    repo.mkdir()
    run('git', 'init', '-b', 'main', str(repo))
    for key, value in [('user.email', 'test@localhost'), ('user.name', 'Test'), ('commit.gpgsign', 'false')]:
        run('git', '-C', str(repo), 'config', key, value)
    (repo / '.env').write_text('SECRET=local-value\n')
    (repo / '.gitignore').write_text('.env\nsetup-done\nchild.pid\n')
    detached = "import os,signal,time; os.setsid(); signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(120)"
    command = f'{shlex.quote(sys.executable)} -c {shlex.quote(detached)} & child=$!; echo "$child" > child.pid; wait'
    config = f'''[scripts]
setup = """
printf 'setup started\\n'
sleep 0.2
test -f "$SIGMA_DOCK_ROOT_PATH/allow-setup" || exit 7
cp "$SIGMA_DOCK_ROOT_PATH/.env" .env
printf '%s|%s|%s|%s|%s' "$SIGMA_DOCK_ROOT_PATH" "$SIGMA_DOCK_WORKTREE_PATH" "$SIGMA_DOCK_WORKER_ID" "$SIGMA_DOCK_BRANCH" "$PORT" > setup-done
printf 'setup complete\\n'
"""
archive = 'printf "archive output\\n"; test -f "$SIGMA_DOCK_ROOT_PATH/allow-archive"'
run_mode = "nonconcurrent"
[scripts.run.web]
command = """{command}"""
default = true
[scripts.run.watch]
command = 'printf "watch output\\n"; sleep 120'
'''
    (repo / '.sigmadock.toml').write_text(config)
    run('git', '-C', str(repo), 'add', '.')
    run('git', '-C', str(repo), 'commit', '-m', 'initial')
    sock = str(state / 'daemon.sock')
    env = dict(os.environ, SHELL='/bin/sh', SIGMA_DOCK_SOCKET=sock)
    log = open(root / 'daemon.log', 'w')
    daemon = None
    child_pids = []

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
        process = subprocess.Popen([str(BIN / 'sigmadockd'), '--state-dir', str(state)], env=env, stdout=log, stderr=log)
        def ready():
            if process.poll() is not None:
                raise AssertionError((root / 'daemon.log').read_text())
            try:
                return rpc('ping')['version'] == 2
            except OSError:
                return False
        wait(ready)
        return process

    def worker(id):
        return rpc('get_worker_status', {'worker_id': id})['worker']

    def phase(id, expected):
        wait(lambda: worker(id)['workspace_scripts']['phase'] == expected)

    def stopped(pid):
        process = subprocess.run(['/bin/ps', '-p', str(pid), '-o', 'stat='], capture_output=True, text=True)
        if process.returncode not in (0, 1):
            raise AssertionError(f'cannot inspect child: {process.stderr}')
        return not process.stdout.strip() or process.stdout.strip().startswith('Z')

    try:
        daemon = start()
        project = rpc('add_project', {'path': str(repo)})
        first = rpc('spawn_worker', {'project_id': project['id'], 'title': 'hooks', 'agent': 'shell', 'base': 'HEAD'})
        id, wt = first['id'], Path(first['worktree'])
        assert first['workspace_scripts']['phase'] == 'awaiting_approval'
        assert rpc('get_worker_status', {'worker_id': id})['pid'] is None
        assert rpc('capacity')['in_use'] == 1
        preview = rpc('workspace_scripts', {'worker_id': id})
        assert not preview['approved'] and preview['text'] == config
        rpc('run_script', {'worker_id': id}, error=True)
        rpc('approve_scripts', {'worker_id': id, 'hash': 'wrong'}, error=True)
        run(str(BIN / 'sdk'), '--socket', sock, 'scripts', id, '--approve', preview['hash'])
        phase(id, 'setup_failed')
        assert not (wt / 'setup-done').exists()
        assert rpc('get_worker_status', {'worker_id': id})['pid'] is None
        setup = rpc('output', {'worker_id': id, 'script': 'setup'})
        assert b'setup started' in bytes(setup['bytes'])
        (repo / 'allow-setup').touch()
        run(str(BIN / 'sdk'), '--socket', sock, 'setup', id)
        phase(id, 'ready')
        assert (wt / '.env').read_text() == (repo / '.env').read_text()
        expected = '|'.join([project['path'], str(wt), id, first['branch'], str(first['port'])])
        assert (wt / 'setup-done').read_text() == expected
        assert rpc('get_worker_status', {'worker_id': id})['pid'] is not None
        assert rpc('output', {'worker_id': id})['generation'] != setup['generation']
        run(str(BIN / 'sdk'), '--socket', sock, 'run', id)
        wait(lambda: (wt / 'child.pid').exists())
        child = int((wt / 'child.pid').read_text())
        child_pids.append(child)
        time.sleep(0.3)  # Child has detached and installed its TERM handler.
        assert not stopped(child)
        run(str(BIN / 'sdk'), '--socket', sock, 'run', id, 'watch')
        wait(lambda: worker(id)['workspace_scripts']['runs'].get('watch', {}).get('running'))
        wait(lambda: stopped(child))
        assert not worker(id)['workspace_scripts']['runs']['web']['running']
        assert b'watch output' in bytes(rpc('output', {'worker_id': id, 'script': 'run:watch'})['bytes'])
        run(str(BIN / 'sdk'), '--socket', sock, 'run', id, '--stop')
        wait(lambda: not worker(id)['workspace_scripts']['runs']['watch']['running'])
        # Changing a config invalidates trust; a stale preview cannot approve it.
        (wt / '.sigmadock.toml').write_text(config.replace('nonconcurrent', 'concurrent') + '# changed\n')
        rpc('run_script', {'worker_id': id}, error=True)
        rpc('approve_scripts', {'worker_id': id, 'hash': preview['hash']}, error=True)
        changed = rpc('workspace_scripts', {'worker_id': id})
        assert not changed['approved']
        rpc('approve_scripts', {'worker_id': id, 'hash': changed['hash']})
        (wt / 'child.pid').unlink()
        rpc('run_script', {'worker_id': id, 'name': 'web'})
        wait(lambda: (wt / 'child.pid').exists())
        child_pids.append(int((wt / 'child.pid').read_text()))
        rpc('run_script', {'worker_id': id, 'name': 'watch'})
        wait(lambda: all(worker(id)['workspace_scripts']['runs'].get(name, {}).get('running') for name in ['web', 'watch']))
        rpc('run_script', {'worker_id': id, 'stop': True})
        wait(lambda: not any(run['running'] for run in worker(id)['workspace_scripts']['runs'].values()))
        run('git', '-C', str(wt), 'add', '.sigmadock.toml')
        run('git', '-C', str(wt), 'commit', '-m', 'config change')
        rpc('stop_worker', {'worker_id': id})
        wait(lambda: worker(id)['facts']['session'] == 'exited')
        time.sleep(2.2)
        rpc('archive_worker', {'worker_id': id, 'cleanup': True})
        phase(id, 'archive_failed')
        assert wt.exists() and not worker(id)['archived']
        assert b'archive output' in bytes(rpc('output', {'worker_id': id, 'script': 'archive'})['bytes'])
        (wt / 'dirty').write_text('preserve me')
        rpc('archive_worker', {'worker_id': id, 'cleanup': True, 'force': True})
        phase(id, 'archive_failed')
        assert (wt / 'dirty').read_text() == 'preserve me'
        assert 'uncommitted' in worker(id)['workspace_scripts']['error']
        (wt / 'dirty').unlink()
        forced = run(str(BIN / 'sdk'), '--socket', sock, 'archive', id, '--cleanup', '--force')
        assert 'archive output' in forced
        assert not wt.exists() and worker(id)['archived']
        # The changed worker config didn't approve the original repository config.
        blocked = rpc('spawn_worker', {'project_id': project['id'], 'title': 'skip', 'agent': 'shell', 'base': 'HEAD'})
        second = blocked['id']
        assert blocked['workspace_scripts']['phase'] == 'awaiting_approval'
        run(str(BIN / 'sdk'), '--socket', sock, 'setup', second, '--skip')
        phase(second, 'ready')
        assert not (Path(blocked['worktree']) / 'setup-done').exists()
        rpc('run_script', {'worker_id': second}, error=True)
        # Approval survives restart; a setup interrupted by a crash never starts an agent.
        original = rpc('workspace_scripts', {'worker_id': second})
        rpc('approve_scripts', {'worker_id': second, 'hash': original['hash']})
        rpc('stop_worker', {'worker_id': second})
        time.sleep(2.4)
        daemon.terminate()
        daemon.wait(timeout=10)
        daemon = start()
        assert rpc('workspace_scripts', {'worker_id': second})['approved']
        third = rpc('spawn_worker', {'project_id': project['id'], 'title': 'approved', 'agent': 'shell', 'base': 'HEAD'})
        assert third['workspace_scripts']['phase'] == 'setting_up'
        phase(third['id'], 'ready')
        # Invalid config reports a parser error without reserving a new worker.
        (repo / '.sigmadock.toml').write_text('[scripts]\nsetpu="typo"\n')
        run('git', '-C', str(repo), 'add', '.sigmadock.toml')
        run('git', '-C', str(repo), 'commit', '-m', 'invalid config')
        before = len(rpc('list_workers'))
        rpc('spawn_worker', {'project_id': project['id'], 'title': 'invalid config', 'agent': 'shell', 'base': 'HEAD'}, error=True)
        assert len(rpc('list_workers')) == before
        print('PASS: exact-hash approval, setup gating/failure/retry/skip, script environment, named/default run PTYs, concurrent runs and nonconcurrent handoff, detached TERM-ignoring child cleanup, archive failure/force, persistence and validation')
    finally:
        if daemon is not None and daemon.poll() is None:
            daemon.terminate()
            daemon.wait(timeout=10)
        for pid in child_pids:
            if not stopped(pid):
                os.kill(pid, 9)
        log.close()
