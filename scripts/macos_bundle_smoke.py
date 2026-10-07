#!/usr/bin/env python3
"""Verify app bootstrap and worker survival; does not qualify rendered UI behavior."""
import argparse
import json
import os
import plistlib
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('app', type=Path)
args = parser.parse_args()
bin_dir = args.app.resolve() / 'Contents/MacOS'

contents = args.app.resolve() / 'Contents'
with (contents / 'Info.plist').open('rb') as file:
    icon_name = plistlib.load(file).get('CFBundleIconFile')
assert icon_name, 'Info.plist has no CFBundleIconFile'
icon = contents / 'Resources' / (icon_name if icon_name.endswith('.icns') else icon_name + '.icns')
assert icon.is_file() and icon.read_bytes()[:4] == b'icns', f'App icon missing or invalid: {icon}'

def wait(predicate):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if predicate(): return
        time.sleep(0.1)
    raise AssertionError('Timed out waiting for bundled daemon')

with tempfile.TemporaryDirectory(prefix='sigma-app-', dir='/tmp') as folder:
    root = Path(folder)
    repo = root / 'repo'
    subprocess.run(['git','init','-b','main',str(repo)],check=True,capture_output=True)
    subprocess.run(['git','-C',str(repo),'config','user.name','Bundle Test'],check=True)
    subprocess.run(['git','-C',str(repo),'config','user.email','test@localhost'],check=True)
    (repo / 'README').write_text('bundle test\n')
    subprocess.run(['git','-C',str(repo),'add','.'],check=True)
    subprocess.run(['git','-C',str(repo),'commit','-m','Initial'],check=True,capture_output=True)
    sock = str(root / 'daemon.sock')
    env = dict(os.environ, SIGMA_DOCK_STATE_DIR=str(root / 'state'), SIGMA_DOCK_SOCKET=sock,
               PATH='/usr/bin:/bin', SHELL='/bin/sh')
    def rpc(method, params=None):
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(10)
            stream.connect(sock)
            stream.sendall(json.dumps({'jsonrpc':'2.0','id':1,'method':method,'params':params or {}}).encode()+b'\n')
            reply = json.loads(stream.makefile('rb').readline())
        assert 'error' not in reply, reply
        return reply['result']
    def ready():
        try: return rpc('ping')['version'] == 2
        except OSError: return False
    app = None
    daemon_ids = []
    with (root / 'ui.log').open('w') as log:
        try:
            app = subprocess.Popen([str(bin_dir / 'sigma-dock')],env=env,stdout=log,stderr=log)
            wait(ready)
            daemon_ids = [rpc('ping')['pid']]
            project = rpc('add_project', {'path':str(repo)})
            worker = rpc('spawn_worker', {'project_id':project['id'],'title':'Bundle smoke','agent':'shell','base':'main'})
            if app.poll() is None: app.terminate()
            app.wait(timeout=10)
            assert ready(), 'Closing the app stopped the daemon'
            rpc('input', {'worker_id':worker['id'],'bytes':list(b"printf 'BUNDLE_WORKER_OK\\n'\n")})
            wait(lambda: b'BUNDLE_WORKER_OK' in bytes(rpc('output', {'worker_id':worker['id'],'cursor':0})['bytes']))
            rpc('stop_worker', {'worker_id':worker['id']})
            print('PASS: bundled daemon cold start, minimal Finder PATH and workers surviving app closure')
        finally:
            if app is not None and app.poll() is None:
                app.kill(); app.wait(timeout=10)
            if not daemon_ids:
                try: daemon_ids = [rpc('ping')['pid']]
                except (OSError, KeyError): pass
            for pid in daemon_ids:
                try: os.kill(pid,signal.SIGTERM)
                except ProcessLookupError: pass
            deadline=time.monotonic()+10
            while Path(sock).exists() and time.monotonic()<deadline: time.sleep(0.1)
