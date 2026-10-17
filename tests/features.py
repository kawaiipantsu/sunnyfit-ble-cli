#!/usr/bin/env python3
"""Exercise new CLI paths without hardware, credentials, or production services."""
import csv,io,json,os,subprocess,tempfile,time,sqlite3,struct,xml.etree.ElementTree as ET
from pathlib import Path
BIN=Path(os.environ.get('SUNNY_TEST_BINARY','target/debug/sunnyfit-ble-cli')).resolve()
with tempfile.TemporaryDirectory(prefix='sunnyfit-features-') as temp:
 root=Path(temp);env={k:v for k,v in os.environ.items() if not k.startswith('SUNNY_')};env['XDG_CONFIG_HOME']=str(root/'config')
 def run(*args,**kw):return subprocess.run([str(BIN),*map(str,args)],env=env,capture_output=True,text=True,timeout=25,**kw)
 p=run('--demo','--systemd','--sinks','sqlite','--sqlite-path',root/'state.sqlite','--log-file',root/'service.log','--stream-socket',root/'stream.sock','--debug-stdout','-v','--seconds','3')
 assert p.returncode==0,p.stderr
 assert not p.stdout and not p.stderr
 assert not (root/'config/sunnyfit-ble-cli/past-workouts').exists()
 db=sqlite3.connect(root/'state.sqlite')
 assert {r[0]for r in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}=={'workout-sessions','workout-statistics','workout-devices'}
 session=db.execute("SELECT session_id,status FROM `workout-sessions` WHERE session_kind='collector'").fetchone();assert session[1]=='finished'
 assert db.execute('SELECT count(*) FROM `workout-statistics` WHERE heart_rate_average_bpm IS NOT NULL').fetchone()[0]==0
 assert any(json.loads(x)['event']=='stopped'for x in (root/'service.log').read_text().splitlines())
 p=run('--sqlite-path',root/'state.sqlite','sessions');assert p.returncode==0,p.stderr
 for fmt in ['csv','tcx','fit']:
  p=run('--sqlite-path',root/'state.sqlite','export','--session',session[0],'--format',fmt,'--output',root/('ride.'+fmt));assert p.returncode==0,p.stderr
 rows=list(csv.DictReader(io.StringIO((root/'ride.csv').read_text())));assert rows and all(r['heart_rate_bpm']==''for r in rows)
 xml=(root/'ride.tcx').read_text();ET.fromstring(xml);assert '<HeartRateBpm>'not in xml and '<Position>'not in xml
 data=(root/'ride.fit').read_bytes();assert data[8:12]==b'.FIT' and struct.unpack('<I',data[4:8])[0]+16==len(data)
 # Persistent sink unavailable at first, then recovers while local acquisition continues.
 blocked=root/'blocked.sqlite';blocked.mkdir()
 p=subprocess.Popen([str(BIN),'--demo','--seconds','5','--sinks','local,sqlite','--sqlite-path',str(blocked),'--output-dir',str(root/'recovery')],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
 time.sleep(1.3);blocked.rmdir();stdout,stderr=p.communicate(timeout=20);assert p.returncode==0,stderr
 summary=json.loads(next((root/'recovery').glob('*/summary.json')).read_text());stats=summary['databases'][0]
 assert stats['written']>=3 and stats['pending']==0 and stats['retries']>0,stats
 # User-editable food catalogue is used for the session summary.
 food=root/'food.json';food.write_text(json.dumps({'foods':[{'name':'a mooncake','portion':'1 small mooncake','kcal':200}]}))
 p=run('--demo','--seconds','2','--met','3','--food-file',food,'--output-dir',root/'food-test');assert p.returncode==0,p.stderr
 summary=json.loads(next((root/'food-test').glob('*/summary.json')).read_text());assert 'mooncake'in summary['food_equivalent']
 p=run('validate-profile','fixtures/heart-rate.json');assert p.returncode==0,p.stderr
 p=run('profiles');assert p.returncode==0 and 'heart-rate'in p.stdout
print('PASS: headless service, three-table schema, SQLite export, unavailable HR, database recovery, editable food catalogue and HRS fixtures')
