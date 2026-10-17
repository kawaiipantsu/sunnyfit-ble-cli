#!/usr/bin/env python3
"""Exercise the real binary, isolated Redis, signals, replay, and a pseudo-terminal."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import socket
import struct
import subprocess
import tempfile
import termios
import time

BIN = Path(os.environ.get('SUNNY_TEST_BINARY', 'target/debug/sunnyfit-ble-cli')).resolve()
with tempfile.TemporaryDirectory(prefix='sunnyfit-tests-') as temp:
    root = Path(temp)
    env = {k:v for k,v in os.environ.items() if not k.startswith('SUNNY_')}
    env['XDG_CONFIG_HOME']=str(root/'config')
    env.pop('SUNNY_REDIS_URL', None)
    def run(*args, **kw):
        return subprocess.run([str(BIN), *args], env=env, capture_output=True, text=True, timeout=20, **kw)
    p = run('--help')
    assert p.returncode == 0, p.stderr
    config = root/'config/sunnyfit-ble-cli/config.toml'
    assert config.is_file() and config.stat().st_mode & 0o777 == 0o600
    for args in [('--timeout-seconds','0'),('--weight-kg','NaN'),('--seconds','0'),('--device','bad')]:
        assert run(*args).returncode != 0
    secret='redis://example:TEST_SECRET@127.0.0.1:1/'
    p=subprocess.run([str(BIN),'--help'],env=dict(env,SUNNY_REDIS_URL=secret),capture_output=True,text=True)
    assert 'TEST_SECRET' not in p.stdout+p.stderr
    config.write_text('redis_url = "PRIVATE_SECRET"\nmalformed ???\n')
    p=run('config');assert p.returncode and 'PRIVATE_SECRET' not in p.stderr
    config.unlink();assert run('--help').returncode == 0
    for flags,raw in [([],False),(['--debug-stdout'],False),(['-v'],False),(['--debug-stdout','-v'],True)]:
        out=root/('mode-'+str(len(list(root.glob('mode-*')))))
        p=run('--demo','--seconds','2','--output-dir',str(out),*flags)
        assert p.returncode==0,p.stderr
        lines=[json.loads(s) for s in p.stdout.splitlines()]
        assert any(s['type']=='notification' for s in lines)==raw
        records=list(out.glob('*/events.jsonl'));assert len(records)==1
        events=[json.loads(s) for s in records[0].read_text().splitlines()]
        assert any(s['type']=='notification' for s in events)
        decoded=[s['sample'] for s in events if s['type']=='decoded']
        replay=run('replay',str(records[0]));assert replay.returncode==0,replay.stderr
        assert decoded==[json.loads(s)['sample'] for s in replay.stdout.splitlines()]
    out=root/'signal'
    p=subprocess.Popen([str(BIN),'--demo','--seconds','30','--output-dir',str(out)],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    time.sleep(1);p.send_signal(signal.SIGTERM);p.communicate(timeout=10);assert p.returncode==0
    assert list(out.glob('*/summary.json'))

    # A real TUI must restore the terminal and leave requested JSON stdout clean.
    master,slave=pty.openpty()
    fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',32,110,0,0))
    p=subprocess.Popen([str(BIN),'--demo','--workout','--debug-stdout','-v','--output-dir',str(root/'tui')],env=dict(env,TERM='xterm-256color'),stdin=slave,stderr=slave,stdout=subprocess.PIPE)
    os.close(slave);screen=b'';deadline=time.monotonic()+2
    while time.monotonic()<deadline:
        if select.select([master],[],[],0.1)[0]:screen+=os.read(master,65536)
    os.write(master,b'1234567g89\t\x1b[Z mq')
    stdout,_=p.communicate(timeout=10)
    while select.select([master],[],[],0.1)[0]:
        try:screen+=os.read(master,65536)
        except OSError:break
    os.close(master)
    assert p.returncode==0
    assert b'\x1b[?1049h' in screen and b'\x1b[?1049l' in screen
    assert b'DEMO' in screen and b'CADENCE' in screen
    for line in stdout.splitlines():json.loads(line)

    # Start a private Redis instance on an ephemeral localhost port.
    sock=socket.socket();sock.bind(('127.0.0.1',0));port=sock.getsockname()[1];sock.close()
    server=subprocess.Popen(['redis-server','--bind','127.0.0.1','--port',str(port),'--save','','--appendonly','no','--dir',str(root)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    sub=None
    try:
        for _ in range(40):
            if subprocess.run(['redis-cli','-p',str(port),'ping'],capture_output=True).returncode==0:break
            time.sleep(.1)
        log=open(root/'redis-events','wb')
        sub=subprocess.Popen(['redis-cli','-p',str(port),'--raw','PSUBSCRIBE','test:sunny:*'],stdout=log,stderr=subprocess.DEVNULL)
        time.sleep(.2)
        p=run('--demo','--seconds','3','--output-dir',str(root/'redis'),'--redis-url',f'redis://127.0.0.1:{port}/',
            '--raw-channel','test:sunny:raw','--metrics-channel','test:sunny:metrics','--status-channel','test:sunny:status','--workout-channel','test:sunny:workout')
        assert p.returncode==0,p.stderr
        time.sleep(.2);sub.terminate();sub.wait(timeout=5);log.close();sub=None
        data=(root/'redis-events').read_text()
        for name in ['raw','metrics','status','workout']:assert 'test:sunny:'+name in data,name
        summary=json.loads(next((root/'redis').glob('*/summary.json')).read_text())
        assert summary['redis']['published']>0 and summary['redis']['queue_dropped']==0 and summary['redis']['pending']==0
        # Offline replay must apply backpressure even with a one-record queue.
        recorded=next((root/'redis').glob('*/events.jsonl'))
        p=run('--redis-url',f'redis://127.0.0.1:{port}/','--redis-queue','1','replay',str(recorded))
        assert p.returncode==0,p.stderr
        stats=json.loads(next(line.removeprefix('Redis replay: ') for line in p.stderr.splitlines() if line.startswith('Redis replay: ')))
        assert stats['published']>5 and stats['pending']==0 and stats['queue_dropped']==0 and stats['failed_or_unconfirmed']==0
        # Redis unavailable at startup, then recovers: queued records must publish.
        server.terminate();server.wait(timeout=5)
        p=subprocess.Popen([str(BIN),'--demo','--seconds','7','--redis-url',f'redis://127.0.0.1:{port}/','--output-dir',str(root/'recovery')],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        time.sleep(2)
        server=subprocess.Popen(['redis-server','--bind','127.0.0.1','--port',str(port),'--save','','--appendonly','no','--dir',str(root)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        p.communicate(timeout=20);assert p.returncode==0
        summary=json.loads(next((root/'recovery').glob('*/summary.json')).read_text())
        assert summary['redis']['published']>5 and summary['redis']['pending']==0
    finally:
        if sub:sub.terminate();sub.wait(timeout=5)
        server.terminate();server.wait(timeout=5)
print('PASS: first-run config, overrides, output modes, secret redaction, replay, SIGTERM, TUI, Redis channels and Redis recovery')
