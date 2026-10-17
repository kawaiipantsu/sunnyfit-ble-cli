#!/usr/bin/env python3
"""Real processes: shared service, private workout clocks, reconnect, standalone selection."""
import fcntl
import json
import os
from pathlib import Path
import pty
import signal
import sqlite3
import struct
import subprocess
import tempfile
import termios
import threading
import time

BIN = Path(os.environ.get('SUNNY_TEST_BINARY', 'target/debug/sunnyfit-ble-cli')).resolve()


class Viewer:
    def __init__(self, root, name, env, socket, extra=()):
        self.root = root / name
        self.root.mkdir()
        self.screen = bytearray()
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 110, 0, 0))
        self.output = (self.root / 'stdout.jsonl').open('wb')
        self.process = subprocess.Popen([
            str(BIN), '--workout', '--stream-socket', str(socket),
            '--debug-stdout', '-v', '--sinks', 'local' if '--demo' in extra else 'local,sqlite',
            '--sqlite-path', str(root / 'must-not-exist.sqlite'),
            '--output-dir', str(self.root / 'workouts'), *extra,
        ], env=dict(env, TERM='xterm-256color'), stdin=slave,
            stderr=slave, stdout=self.output)
        os.close(slave)
        self.reader = threading.Thread(target=self.drain, daemon=True)
        self.reader.start()

    def drain(self):
        while True:
            try:
                data = os.read(self.master, 65536)
                if not data:
                    break
                self.screen.extend(data)
            except OSError:
                break

    def events(self):
        return [json.loads(line) for line in (self.root / 'stdout.jsonl').read_text().splitlines() if line.endswith('}')]

    def wait_for(self, predicate, seconds=12):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            rows = self.events()
            if predicate(rows):
                return rows
            assert self.process.poll() is None, 'viewer exited unexpectedly'
            time.sleep(.1)
        raise AssertionError('viewer stream condition timed out')

    def key(self, key):
        os.write(self.master, key)

    def close(self):
        if self.process.poll() is None:
            self.key(b'q')
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.reader.join(timeout=2)
        os.close(self.master)
        self.output.close()
        assert self.process.returncode == 0, 'viewer did not exit cleanly'


def metrics(rows):
    return [r['workout'] for r in rows if r['type'] == 'metrics']


with tempfile.TemporaryDirectory(prefix='sunnyfit-live-') as temp:
    root = Path(temp)
    env = {k: v for k, v in os.environ.items() if not k.startswith('SUNNY_')}
    env['XDG_CONFIG_HOME'] = str(root / 'config')
    socket = root / 'run/stream.sock'
    dbpath = root / 'service.sqlite'
    service = None
    viewers = []

    def start_service():
        proc = subprocess.Popen([
            str(BIN), '--demo', '--systemd', '--stream-socket', str(socket),
            '--sinks', 'sqlite', '--sqlite-path', str(dbpath),
            '--log-file', str(root / 'service.log'),
        ], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.monotonic() + 10
        while not socket.exists() and time.monotonic() < deadline:
            assert proc.poll() is None, 'collector failed to start'
            time.sleep(.05)
        assert socket.exists()
        return proc

    def stop_service():
        service.send_signal(signal.SIGTERM)
        service.wait(timeout=15)
        assert service.returncode == 0

    try:
        # Without a local service, auto proceeds to ordinary Bluetooth selection.
        fallback = subprocess.run([
            str(BIN), '--workout', '--stream-socket', str(socket),
        ], env=env, capture_output=True, text=True, timeout=10)
        assert fallback.returncode != 0 and 'choose a bike' in fallback.stderr.lower(), fallback.stderr
        required = subprocess.run([
            str(BIN), '--workout', '--workout-source', 'service', '--stream-socket', str(socket),
        ], env=env, capture_output=True, text=True, timeout=10)
        assert required.returncode != 0 and 'service stream unavailable' in required.stderr

        service = start_service()
        time.sleep(3)
        assert socket.stat().st_mode & 0o777 == 0o660
        first = Viewer(root, 'first', env, socket)
        viewers.append(first)
        rows = first.wait_for(lambda rows: len(metrics(rows)) >= 3)
        values = metrics(rows)
        assert values[0]['source'] == 'service'
        assert values[0]['elapsed_s'] < 2
        snapshot = next(r['snapshot'] for r in rows if r['type'] == 'service_snapshot')
        assert snapshot['workout']['elapsed_s'] >= 2
        assert values[-1]['current']['cadence_rpm'] > 0
        assert any(r['type'] == 'service_event' and r['event']['type'] == 'notification' for r in rows)
        assert all(r['type'] not in ('notification', 'write_request') for r in rows)
        assert b'SERVICE STREAM' in first.screen
        # No duplicate SQL connection/write, despite the viewer's sink configuration.
        assert not (root / 'must-not-exist.sqlite').exists()
        blocked = subprocess.run([
            str(BIN), '--workout', '--workout-source', 'direct', '--stream-socket', str(socket),
        ], env=env, capture_output=True, text=True, timeout=10)
        assert blocked.returncode != 0 and 'already owns this bike' in blocked.stderr

        second = Viewer(root, 'second', env, socket, ('--workout-source', 'service'))
        viewers.append(second)
        second.wait_for(lambda rows: len(metrics(rows)) >= 2)
        first.key(b' ')
        first.wait_for(lambda rows: metrics(rows)[-1]['paused'])
        paused_active = metrics(first.events())[-1]['active_s']
        other_active = metrics(second.events())[-1]['active_s']
        time.sleep(2.2)
        assert metrics(first.events())[-1]['active_s'] == paused_active
        assert metrics(second.events())[-1]['active_s'] > other_active + 1
        first.key(b' ')
        first.wait_for(lambda rows: not metrics(rows)[-1]['paused'])
        first.close()
        viewers.remove(first)
        assert service.poll() is None
        summary = json.loads(next((first.root / 'workouts').glob('*/summary.json')).read_text())
        assert summary['source'] == 'service_stream' and summary['databases'] == []
        assert summary['demo'] and 'redis' not in summary
        replay = subprocess.run([str(BIN), 'replay', str(next((first.root / 'workouts').glob('*/events.jsonl')))],
                                env=env, capture_output=True, text=True, timeout=10)
        assert replay.returncode == 0 and 'cadence_rpm' in replay.stdout

        old_run = snapshot['source_run_id']
        stop_service()
        second.wait_for(lambda rows: any(r['type'] == 'connection' and r['state'] == 'waiting for service' for r in rows))
        # Stream ownership is retained by the service role while reconnecting.
        assert second.process.poll() is None
        previous_distance = metrics(second.events())[-1]['distance_m']
        service = start_service()
        second.wait_for(lambda rows: any(r['type'] == 'service_snapshot' and r['snapshot']['source_run_id'] != old_run for r in rows))
        second.wait_for(lambda rows: metrics(rows)[-1]['connection'] == 'ready' and metrics(rows)[-1]['current']['cadence_rpm'] is not None)
        new_distance = metrics(second.events())[-1]['distance_m']
        assert previous_distance <= new_distance < previous_distance + 40
        second.close()
        viewers.remove(second)
        assert service.poll() is None
        stop_service()
        service = None
        assert not socket.exists()
        with sqlite3.connect(dbpath) as db:
            assert db.execute("SELECT count(*) FROM `workout-sessions` WHERE session_kind='collector'").fetchone()[0] == 2
            assert db.execute("SELECT count(*) FROM `workout-sessions` WHERE status='finished' AND session_kind='collector'").fetchone()[0] == 2
        # A standalone TUI remains available with no service; demo avoids physical BLE.
        solo = Viewer(root, 'standalone', env, socket, ('--demo',))
        viewers.append(solo)
        solo.wait_for(lambda rows: len(metrics(rows)) >= 2)
        assert metrics(solo.events())[-1]['source'] == 'bluetooth'
        assert all(r['type'] != 'service_snapshot' for r in solo.events())
        solo.close()
        viewers.remove(solo)
    finally:
        for viewer in viewers:
            viewer.close()
        if service and service.poll() is None:
            service.terminate()
            service.wait(timeout=15)
print('PASS: automatic attachment, standalone selection, multiple viewers, independent pause/history, no duplicate sinks, raw replay and service restart')
