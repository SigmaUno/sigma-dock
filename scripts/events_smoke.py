#!/usr/bin/env python3
"""Real daemon events and a headless idle workload using the UI's request intervals."""
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'debug'

with tempfile.TemporaryDirectory(prefix='sigmadock-events-', dir='/tmp') as folder:
    temp = Path(folder)
    repo = temp / 'repo'
    repo.mkdir()
    for args in [('init', '-b', 'main'), ('config', 'user.name', 'Test'), ('config', 'user.email', 'test@localhost'), ('config', 'commit.gpgsign', 'false'), ('commit', '--allow-empty', '-m', 'initial')]:
        subprocess.run(['git', '-C', str(repo), *args], check=True, capture_output=True)
    sock = str(temp / 'state' / 'daemon.sock')
    env = dict(os.environ, SHELL='/bin/sh', SIGMA_DOCK_SOCKET=sock)
    log = open(temp / 'daemon.log', 'w')
    daemon = None
    subscription = None
    calls = 0
    events = queue.Queue()
    closed_readers = []
    def rpc(method, params=None):
        global calls
        calls += 1
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(10)
            stream.connect(sock)
            stream.sendall(json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params or {}}).encode() + b'\n')
            reply = json.loads(stream.makefile('rb').readline())
        assert 'error' not in reply, reply
        return reply['result']
    def subscribe():
        stream = socket.socket(socket.AF_UNIX)
        stream.settimeout(10)
        stream.connect(sock)
        stream.sendall(b'{"jsonrpc":"2.0","id":1,"method":"subscribe","params":{}}\n')
        reader = stream.makefile('rb')
        assert json.loads(reader.readline())['result']['version'] == 2
        first = json.loads(reader.readline())
        assert first['params']['type'] == 'resync', first
        stream.settimeout(None)
        closed = threading.Event()
        closed_readers.append(closed)
        def read_events():
            try:
                for line in reader:
                    event = json.loads(line)
                    assert event['jsonrpc'] == '2.0' and event['method'] == 'event'
                    events.put(event['params'])
            except (OSError, ValueError):
                pass
            finally:
                reader.close()
                closed.set()
        threading.Thread(target=read_events, daemon=True).start()
        return stream
    def close_subscription(stream):
        try: stream.shutdown(socket.SHUT_RDWR)
        except OSError: pass  # EOF may already have closed the peer's endpoint.
        stream.close()
    def wait_event(kind, worker=None, predicate=lambda event: True):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            event = events.get(timeout=max(0.01, deadline-time.monotonic()))
            if event['type'] == kind and (worker is None or event.get('worker_id') == worker) and predicate(event):
                return event
        raise AssertionError(f'missing {kind} event')
    def snapshot():
        for method, params in [('ping', {}), ('list_workers', {'include_archived': True}), ('list_unfinished', {}), ('list_projects', {}), ('capacity', {})]:
            rpc(method, params)
    def drain_events():
        changed, output = False, set()
        while True:
            try: event = events.get_nowait()
            except queue.Empty: break
            if event['type'] == 'output_available': output.add(event['worker_id'])
            elif event['type'] != 'heartbeat': changed = True
        return changed, output
    def wait_ready():
        deadline = time.monotonic() + 10
        while True:
            assert daemon.poll() is None, (temp / 'daemon.log').read_text()
            try:
                if rpc('ping')['version'] == 2: break
            except OSError:
                pass
            assert time.monotonic() < deadline
            time.sleep(0.05)
    try:
        daemon = subprocess.Popen([str(BIN / 'sigmadockd'), '--state-dir', str(temp / 'state'), '--max-workers', '6', '--idle-seconds', '3600'], env=env, stdout=log, stderr=log)
        wait_ready()
        subscription = subscribe()
        project = rpc('add_project', {'path': str(repo)})
        wait_event('projects_changed')
        workers = [rpc('spawn_worker', {'project_id': project['id'], 'title': str(index), 'agent': 'shell'}) for index in range(6)]
        wait_event('capacity_changed')
        time.sleep(0.5)
        # Old UI pattern: snapshot every 2s, six previews every 750ms, open terminal every 25ms.
        started = time.monotonic()
        before = calls
        next_snapshot = next_preview = next_terminal = started
        while time.monotonic() - started < 6:
            now = time.monotonic()
            if now >= next_snapshot:
                snapshot(); next_snapshot = time.monotonic() + 2
            if now >= next_preview:
                for worker in workers: rpc('output', {'worker_id': worker['id'], 'cursor': 0})
                next_preview = time.monotonic() + 0.75
            if now >= next_terminal:
                rpc('output', {'worker_id': workers[0]['id'], 'cursor': 0})
                next_terminal = time.monotonic() + 0.025
            time.sleep(0.002)
        baseline_elapsed = time.monotonic() - started
        baseline_calls = calls - before
        drain_events()
        # New UI pattern: events trigger snapshot/output; the 30s resync is outside this window.
        started = time.monotonic()
        before = calls
        while time.monotonic() - started < 6:
            changed, output = drain_events()
            if changed: snapshot()
            for worker in output: rpc('output', {'worker_id': worker, 'cursor': 0})
            time.sleep(0.05)
        event_elapsed = time.monotonic() - started
        event_calls = calls - before
        assert event_calls == 0, event_calls
        print(f'IDLE: six shell workers + one open-terminal workload, polling {baseline_calls / baseline_elapsed:.2f} RPC/s ({baseline_calls} calls); events {event_calls / event_elapsed:.2f} RPC/s ({event_calls} calls), each window ~6s; 30s resync contributes 5/30 RPC/s outside the window.')
        worker = workers[0]
        rpc('resize', {'worker_id': worker['id'], 'rows': 24, 'cols': 80})
        signal = wait_event('output_available', worker['id'], lambda event: event['signal']['cols'] == 80)['signal']
        assert signal['rows'] == 24
        cursor = signal['cursor']
        rpc('input', {'worker_id': worker['id'], 'bytes': list(b"printf 'EVENT_OUTPUT_OK\\n'\n")})
        wait_event('output_available', worker['id'], lambda event: event['signal']['cursor'] > cursor)
        assert b'EVENT_OUTPUT_OK' in bytes(rpc('output', {'worker_id': worker['id'], 'cursor': cursor})['bytes'])
        rpc('stop_worker', {'worker_id': worker['id']})
        wait_event('worker_changed', worker['id'])
        wait_event('output_available', worker['id'], lambda event: event['signal']['exited'])
        rpc('resume_worker', {'worker_id': worker['id']})
        newer = wait_event('output_available', worker['id'], lambda event: event['signal']['generation'] != signal['generation'])
        assert newer['signal']['generation'] > signal['generation']
        # Reconnect always begins with resync; daemon shutdown closes a live stream.
        close_subscription(subscription)
        subscription = subscribe()
        daemon.terminate(); daemon.wait(timeout=5)
        assert closed_readers[-1].wait(2), "event reader did not receive EOF on shutdown"
        close_subscription(subscription); subscription = None
        # A new daemon process must resync persisted workers before replaying new sessions.
        drain_events()
        daemon = subprocess.Popen([str(BIN / 'sigmadockd'), '--state-dir', str(temp / 'state'), '--max-workers', '6', '--idle-seconds', '3600'], env=env, stdout=log, stderr=log)
        wait_ready()
        subscription = subscribe()
        recovered = rpc('get_worker_status', {'worker_id': worker['id']})['worker']
        assert recovered['project_id'] == project['id']
        assert recovered['facts']['session'] == 'exited', recovered
        assert rpc('capacity')['in_use'] == 0
        rpc('resume_worker', {'worker_id': worker['id']})
        restarted = wait_event('output_available', worker['id'], lambda event: not event['signal']['exited'])['signal']
        assert (restarted['rows'], restarted['cols']) == (30, 120), restarted
        cursor = restarted['cursor']
        rpc('input', {'worker_id': worker['id'], 'bytes': list(b"printf 'RESTART_EVENT_OK\\n'\n")})
        wait_event('output_available', worker['id'], lambda event: event['signal']['cursor'] > cursor)
        assert b'RESTART_EVENT_OK' in bytes(rpc('output', {'worker_id': worker['id'], 'cursor': cursor})['bytes'])
        print('PASS: event handshake, idle workload, project/capacity/worker changes, output, resize, EOF, session generations, reconnect, graceful shutdown and daemon restart resync')
    finally:
        if subscription is not None:
            close_subscription(subscription)
        if daemon is not None and daemon.poll() is None:
            daemon.terminate(); daemon.wait(timeout=5)
        log.close()
