#!/usr/bin/env python3
"""Validate CI delivery, scoped orchestrators and notes with local fake services."""
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'debug'

def wait_for(predicate, timeout=15):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError('timed out waiting for feedback test')

with tempfile.TemporaryDirectory(prefix='sigma-feedback-', dir='/tmp') as directory:
    temp = Path(directory)
    fake_bin = temp / 'bin'; fake_bin.mkdir()
    harness = fake_bin / 'claude'
    harness.write_text('#!' + sys.executable + '\n' + r'''
import json, pathlib, subprocess, sys
pathlib.Path('launch.json').write_text(json.dumps(sys.argv[1:]))
if '--mcp-config' in sys.argv:
    config=json.loads(pathlib.Path(sys.argv[sys.argv.index('--mcp-config')+1]).read_text())['mcpServers']['sigma']
    request={'jsonrpc':'2.0','id':1,'method':'tools/call','params':{'name':'list_workers','arguments':{}}}
    result=subprocess.run([config['command']]+config['args'],input=json.dumps(request)+'\n',text=True,capture_output=True,check=True)
    pathlib.Path('mcp-result.json').write_text(result.stdout)
print('FAKE_AGENT_READY',flush=True)
with open('received.txt','w') as out:
    while True:
        data=sys.stdin.readline()
        if not data:break
        out.write(data);out.flush()
''')
    harness.chmod(0o755)
    sock = str(temp / 'state' / 'daemon.sock')
    env = dict(os.environ, PATH=str(fake_bin)+os.pathsep+os.environ['PATH'], SIGMA_DOCK_SOCKET=sock)
    branches = []
    sha = 'a' * 40
    class Forge(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass
        def do_GET(self):
            path = self.path.split('?')[0]
            if path.endswith('/pulls'):
                value = [{'number': n+1, 'head': {'ref': branch}} for n, branch in enumerate(branches)]
            elif '/pulls/' in path and path.rsplit('/', 1)[1].isdigit():
                value = {'number': int(path.rsplit('/', 1)[1]), 'head': {'sha': sha}, 'state': 'open', 'mergeable': False}
            elif path.endswith('/status'):
                value = {'state': 'success', 'statuses': []}
            elif path.endswith('/reviews'):
                value = []
            elif path.endswith('/actions/runs'):
                value = {'total_count': 1, 'workflow_runs': [{'id': 7, 'workflow_id': 'test.yml', 'event': 'push', 'commit_sha': sha, 'status': 'failure'}]}
            elif path.endswith('/actions/runs/7/jobs'):
                value = [{'id': 9, 'name': 'tests', 'attempt': 1, 'status': 'failure'}]
            elif path.endswith('/actions/jobs/9/logs'):
                body = b'assertion failed at src/main.rs:10\n'
                self.send_response(206); self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
                return
            else:
                self.send_error(404)
                return
            body = json.dumps(value).encode()
            self.send_response(200); self.send_header('Content-Type', 'application/json'); self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Forge)
    thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
    log = open(temp / 'daemon.log', 'w')
    daemon = None
    workers = []
    def rpc(method, params=None, error=False):
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(20); stream.connect(sock)
            stream.sendall(json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params or {}}).encode()+b'\n')
            reply = json.loads(stream.makefile('rb').readline())
        if error:
            assert 'error' in reply, reply
            return reply['error']
        assert 'error' not in reply, reply
        return reply['result']
    def start():
        p = subprocess.Popen([str(BIN/'sigma-dockerd'), '--state-dir', str(temp/'state'), '--idle-seconds', '1'], env=env, stdout=log, stderr=log)
        def ready():
            if p.poll() is not None:
                raise AssertionError((temp/'daemon.log').read_text())
            try:
                return rpc('ping')['version'] == 1
            except OSError:
                return False
        wait_for(ready)
        return p
    def project(name):
        repo = temp/name; repo.mkdir()
        for args in [('init','-b','main'),('config','user.name','Test'),('config','user.email','test@localhost'),('commit','--allow-empty','-m','initial')]:
            subprocess.run(['git','-C',str(repo)]+list(args),check=True,capture_output=True)
        return rpc('add_project', {'path': str(repo)})
    def mcp(scope, name, arguments):
        request = {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call', 'params': {'name': name, 'arguments': arguments}}
        result = subprocess.run([str(BIN/'sigma-dock-mcp'), '--project-id', scope], env=env, input=json.dumps(request)+'\n', text=True, capture_output=True, check=True)
        return json.loads(result.stdout)['result']
    try:
        daemon = start()
        p = project('first'); other_project = project('second')
        notes = rpc('write_planning_notes', {'project_id': p['id'], 'text': 'Plan: repair failing tests', 'expected_revision': 0})
        assert notes['revision'] == 1
        rpc('write_planning_notes', {'project_id': p['id'], 'text': 'stale', 'expected_revision': 0}, error=True)
        other = rpc('spawn_worker', {'project_id': other_project['id'], 'title': 'other', 'agent': 'claude'}); workers.append(other)
        worker = rpc('spawn_worker', {'project_id': p['id'], 'title': 'repair', 'agent': 'claude'}); workers.append(worker)
        branches.append(worker['branch'])
        rpc('configure_forge', {'worker_id': worker['id'], 'forge': {'kind': 'forgejo', 'api_url': 'http://127.0.0.1:'+str(server.server_address[1])+'/api/v1', 'owner': 'owner', 'repo': 'repo', 'token_env': 'SIGMA_TEST_NO_TOKEN', 'actions': True}})
        rpc('refresh_facts', {'worker_id': worker['id']})
        assert rpc('get_worker_status', {'worker_id': worker['id']})['worker']['facts']['checks'] == 'failed'
        preview = rpc('ci_feedback', {'worker_id': worker['id']})
        assert preview['includes_job_logs'] and 'assertion failed' in preview['text']
        received = Path(worker['worktree'])/'received.txt'
        rpc('configure_feedback', {'worker_id': worker['id'], 'auto_ci': True})
        # Exercise the automatic gate directly, then prove retry deduplication and persistence.
        wait_for(lambda: rpc('get_worker_status', {'worker_id': worker['id']})['worker']['facts']['session'] == 'idle')
        rpc('send_ci_feedback', {'worker_id': worker['id'], 'automatic': True})
        wait_for(lambda: received.exists() and 'assertion failed' in received.read_text())
        rpc('send_ci_feedback', {'worker_id': worker['id'], 'automatic': True}, error=True)
        assert received.read_text().count('CI feedback for commit') == 1
        assert mcp(p['id'], 'get_worker_status', {'worker_id': other['id']})['isError'] is True
        denied = mcp(p['id'], 'message_worker', {'worker_id': worker['id'], 'message': 'x', 'project_id': other_project['id']})
        assert denied['isError'] is True
        notes_reply = mcp(p['id'], 'read_planning_notes', {})
        assert json.loads(notes_reply['content'][0]['text'])['text'] == 'Plan: repair failing tests'
        orchestrator = rpc('start_orchestrator', {'project_id': p['id'], 'agent': 'claude'}); workers.append(orchestrator)
        rpc('start_orchestrator', {'project_id': p['id'], 'agent': 'claude'}, error=True)
        result_path = Path(orchestrator['worktree'])/'mcp-result.json'
        wait_for(result_path.exists)
        result = json.loads(result_path.read_text())['result']
        scoped_workers = json.loads(result['content'][0]['text'])
        assert all(w['project_id'] == p['id'] for w in scoped_workers)
        launch = json.loads((Path(orchestrator['worktree'])/'launch.json').read_text())
        assert '--mcp-config' in launch and any('Plan: repair failing tests' in arg for arg in launch)
        for w in workers:
            rpc('stop_worker', {'worker_id': w['id']})
        wait_for(lambda: all(rpc('get_worker_status', {'worker_id': w['id']})['worker']['facts']['session'] == 'exited' for w in workers))
        daemon.terminate(); daemon.wait(timeout=5); daemon = start()
        assert rpc('read_planning_notes', {'project_id': p['id']})['revision'] == 1
        assert rpc('get_worker_status', {'worker_id': worker['id']})['worker']['feedback']['last_ci_head'] == sha
        assert rpc('get_worker_status', {'worker_id': orchestrator['id']})['worker']['role'] == 'orchestrator'
        print('PASS: CI preview/delivery, one-attempt guard, project MCP scope, orchestrator configuration and persisted notes')
    finally:
        if daemon is not None and daemon.poll() is None:
            for w in workers:
                try: rpc('stop_worker', {'worker_id': w['id']})
                except (OSError, AssertionError): pass
            daemon.terminate(); daemon.wait(timeout=5)
        server.shutdown(); server.server_close(); log.close()
