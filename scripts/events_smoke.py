#!/usr/bin/env python3
"""Exercise local push events/reconnect and compare idle RPC schedules with six shell workers."""
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

with tempfile.TemporaryDirectory(prefix='sigmadock-events-', dir='/tmp') as directory:
    temp = Path(directory)
    repo = temp / 'repo'
    subprocess.run(['git', 'init', '-b', 'main', str(repo)], check=True, capture_output=True)
    for key, value in [('user.name', 'Test'), ('user.email', 'test@localhost')]:
        subprocess.run(['git', '-C', str(repo), 'config', key, value], check=True)
    (repo / 'README').write_text('test\n')
    subprocess.run(['git', '-C', str(repo), 'add', '.'], check=True)
    subprocess.run(['git', '-C', str(repo), 'commit', '-m', 'initial'], check=True, capture_output=True)
    sock = str(temp / 'state' / 'daemon.sock')
    calls = 0
    streams = []
    log = open(temp / 'daemon.log', 'w')
    daemon = None

    def rpc(method, params=None):
        global calls
        calls += 1
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(10)
            stream.connect(sock)
            stream.sendall(json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params or {}}).encode()+b'\n')
            reply = json.loads(stream.makefile('rb').readline())
        assert 'error' not in reply, reply
        return reply['result']

    def start():
        process = subprocess.Popen([str(BIN / 'sigmadockd'), '--state-dir', str(temp / 'state'), '--max-workers', '6', '--idle-seconds', '3600'], stdout=log, stderr=log)
        end = time.monotonic()+10
        while time.monotonic() < end:
            try:
                rpc('ping')
                return process
            except (OSError, AssertionError):
                assert process.poll() is None, (temp / 'daemon.log').read_text()
                time.sleep(.05)
        raise AssertionError('daemon start timed out')

    def subscribe():
        stream = socket.socket(socket.AF_UNIX)
        stream.settimeout(6)
        stream.connect(sock)
        reader = stream.makefile('rb')
        stream.sendall(b'{"jsonrpc":"2.0","id":1,"method":"subscribe","params":{}}\n')
        assert json.loads(reader.readline())['result']['version'] == rpc('ping')['version']
        events = queue.Queue()
        def read():
            try:
                while line := reader.readline():
                    event = json.loads(line)
                    assert event['method'] == 'event', event
                    events.put(event['params'])
            except (OSError, ValueError):
                pass
            finally:
                events.put(None)
                reader.close()
        threading.Thread(target=read, daemon=True).start()
        streams.append(stream)
        return events

    def wait(events, kind, worker=None):
        end = time.monotonic()+5
        while time.monotonic() < end:
            event = events.get(timeout=max(.01, end-time.monotonic()))
            assert event is not None, 'subscription disconnected'
            if event['type'] == kind and (worker is None or event.get('worker_id') == worker):
                return event
        raise AssertionError((kind, worker))

    def snapshot():
        rpc('ping')
        rpc('list_workers', {'include_archived': True})
        rpc('list_unfinished')
        rpc('list_projects')
        rpc('capacity')

    try:
        daemon = start()
        events = subscribe()
        wait(events, 'resync')
        project = rpc('add_project', {'path': str(repo)})
        wait(events, 'projects_changed')
        workers = [rpc('spawn_worker', {'project_id': project['id'], 'title': str(i), 'agent': 'shell'}) for i in range(6)]
        wait(events, 'capacity_changed')
        target = workers[0]['id']
        rpc('resize', {'worker_id': target, 'rows': 24, 'cols': 80})
        geometry = wait(events, 'output_available', target)
        while geometry['rows'] != 24:
            geometry = wait(events, 'output_available', target)
        assert geometry['cols'] == 80, geometry
        rpc('input', {'worker_id': target, 'bytes': list(b"printf 'PUSH_EVENT_OK\\n'\n")})
        wait(events, 'output_available', target)
        assert b'PUSH_EVENT_OK' in bytes(rpc('output', {'worker_id': target, 'cursor': 0})['bytes'])
        # Settle shell prompts and invalidations before measuring idle clients.
        time.sleep(.5)
        while not events.empty():
            events.get_nowait()
        duration = 3
        before = calls
        end = time.monotonic()+duration
        next_snapshot = next_preview = next_terminal = 0
        while time.monotonic() < end:
            now = time.monotonic()
            if now >= next_snapshot:
                snapshot(); next_snapshot = now+2
            if now >= next_preview:
                for worker in workers:
                    rpc('output', {'worker_id': worker['id'], 'cursor': 10**9})
                next_preview = now+.75
            if now >= next_terminal:
                rpc('output', {'worker_id': target, 'cursor': 10**9})
                next_terminal = now+.025
            time.sleep(.001)
        old_calls = calls-before
        before = calls
        end = time.monotonic()+duration
        while time.monotonic() < end:
            try:
                event = events.get(timeout=max(.01, end-time.monotonic()))
            except queue.Empty:
                break
            assert event is not None
            if event['type'] == 'output_available':
                rpc('output', {'worker_id': event['worker_id'], 'cursor': 10**9})
            elif event['type'] != 'heartbeat':
                snapshot()
        new_calls = calls-before
        assert new_calls == 0, new_calls
        print(f'Idle RPC schedule benchmark (6 shell workers + open terminal, {duration}s): {old_calls/duration:.2f}/s before; {new_calls/duration:.2f}/s after. Subscription heartbeats excluded; 30s snapshot/preview/terminal catch-up amortizes to 12/30 RPC/s.')
        rpc('stop_worker', {'worker_id': target})
        wait(events, 'worker_changed', target)
        wait(events, 'capacity_changed')
        daemon.terminate(); daemon.wait(timeout=5)
        deadline = time.monotonic()+5
        while events.get(timeout=max(.01, deadline-time.monotonic())) is not None:
            assert time.monotonic() < deadline, 'old subscription did not close'
        # Old subscriptions close on shutdown; new connections request a full resync.
        daemon = start()
        reconnected = subscribe()
        wait(reconnected, 'resync')
        assert rpc('get_worker_status', {'worker_id': target})['worker']['facts']['session'] == 'exited'
        print('PASS: project/worker/capacity/output/geometry events, existing RPCs and restart resync')
    finally:
        for stream in streams:
            try:
                stream.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            stream.close()
        if daemon is not None and daemon.poll() is None:
            daemon.terminate(); daemon.wait(timeout=5)
        log.close()
