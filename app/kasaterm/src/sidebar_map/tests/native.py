import sys
import json,os,pathlib,shutil,subprocess,tempfile
if sys.platform != 'darwin':
    raise SystemExit('This fixture uses the macOS AppKit input path')
root=pathlib.Path(tempfile.mkdtemp(prefix='kt-minimap-look-',dir='/tmp'))
print('artifacts:',root,flush=True)
repo=pathlib.Path(__file__).resolve().parents[5]
binary=pathlib.Path(os.environ.get('CARGO_TARGET_DIR',str(pathlib.Path(__file__).resolve().parents[5]/'target')))/'debug/kasaterm'
shutil.copytree(repo/'app/kasaterm/assets/students',root/'students')
(root/'settings.json').write_text(json.dumps({'lid_away':False,'sidebar_body':'map'}))
shots=root/'shots';shots.mkdir()
base={k:v for k,v in os.environ.items() if not k.startswith(('KASATERM_','CMUX_'))};base.pop('NO_COLOR',None)
env={**base,'TMPDIR':str(root),'KASATERM_SESSION_FILE':str(root/'session.json'),'KASATERM_SETTINGS_FILE':str(root/'settings.json'),'KASATERM_WINDOW_FILE':str(root/'window.json'),'KASATERM_STUDENTS_DIR':str(root/'students'),'KASATERM_PET_DIR':str(root/'pet'),'KASATERM_DEVICE_FILE':str(root/'device.json'),'KASATERM_COLLAB_ROOT':str(root/'collab'),'KASATERM_KASANET_KEY':str(root/'kasanet.key'),'KASATERM_KASANET':'off','KASATERM_MACHINES':'[]','KASATERM_AUTORESTORE':'fresh','KASATERM_SOCKET_PATH':str(root/'app.sock'),'KASATERM_NOTIFICATION_PROBE':'1','KASATERM_AUTOQUIT_MS':'60000','KASATERM_WINDOW_SIZE':'1200,900','KASATERM_WINDOW_POS':'40,100','KASATERM_NO_FOCUS':'1','KASATERM_NO_AUTOSUGGEST':'1','KASATERM_AUTOROOMCARDS_DIR':str(shots),'KASATERM_SIDEBAR_FIXTURE':'1','KASATERM_AUTODECKTIP':'1'}
with (root/'launcher.log').open('w') as log:
 p=subprocess.Popen([str(binary)],cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT)
 (root/'pid').write_text(str(p.pid))
 try:p.wait(timeout=90)
 except subprocess.TimeoutExpired:
  p.terminate()
  try:p.wait(timeout=5)
  except subprocess.TimeoutExpired:p.kill();p.wait()
  raise
assert p.returncode==0,p.returncode
checks={}
for n in range(9):
 d=json.loads((shots/f'{n:02}.json').read_text())
 checks.update({f'{n}:{key}':value for key,value in d['checks'].items()})
failed=[k for k,v in checks.items() if v is not True]
print('minimap resize/hide with portraits',len(checks),'checks',failed,root,flush=True)
assert not failed,failed
(root/'success.json').write_text(json.dumps({'checks':len(checks)}))
