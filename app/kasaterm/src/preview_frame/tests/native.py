import sys
import json,os,pathlib,struct,subprocess,tempfile,time,zlib
if sys.platform != 'darwin':
    raise SystemExit('This fixture uses the macOS AppKit input path')
root=pathlib.Path(tempfile.mkdtemp(prefix='kt-preview-frame-',dir='/tmp'))
print('artifacts:',root,flush=True)
repo=pathlib.Path(__file__).resolve().parents[5]
binary=pathlib.Path(os.environ.get('CARGO_TARGET_DIR',str(pathlib.Path(__file__).resolve().parents[5]/'target')))/'debug/kasaterm'
def chunk(kind,data):return struct.pack('!I',len(data))+kind+data+struct.pack('!I',zlib.crc32(kind+data)&0xffffffff)
w,h=1920,1080
colors=[bytes((240,40,40,255)),bytes((40,230,40,255)),bytes((40,40,240,255)),bytes((245,245,245,255))]
raw=b''.join(b'\0'+colors[(y>=h//2)*2]*(w//2)+colors[(y>=h//2)*2+1]*(w//2) for y in range(h))
png=root/'quadrants.png'
png.write_bytes(b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('!2I5B',w,h,8,6,0,0,0))+chunk(b'IDAT',zlib.compress(raw))+chunk(b'IEND',b''))
base={k:v for k,v in os.environ.items() if not k.startswith(('KASATERM_','CMUX_'))}
base.pop('NO_COLOR',None)
reports=[]
for kind in ['image','video']:
 case=root/kind;case.mkdir()
 (case/'settings.json').write_text(json.dumps({'lid_away':False,'sidebar_body':'map'}))
 env={**base,'TMPDIR':str(case),'KASATERM_SESSION_FILE':str(case/'session.json'),'KASATERM_SETTINGS_FILE':str(case/'settings.json'),'KASATERM_WINDOW_FILE':str(case/'window.json'),'KASATERM_STUDENTS_DIR':str(case/'students'),'KASATERM_PET_DIR':str(case/'pet'),'KASATERM_DEVICE_FILE':str(case/'device.json'),'KASATERM_COLLAB_ROOT':str(case/'collab'),'KASATERM_KASANET_KEY':str(case/'kasanet.key'),'KASATERM_KASANET':'off','KASATERM_MACHINES':'[]','KASATERM_AUTORESTORE':'fresh','KASATERM_SOCKET_PATH':str(case/'app.sock'),'KASATERM_NOTIFICATION_PROBE':'1','KASATERM_AUTOQUIT_MS':'32000','KASATERM_WINDOW_SIZE':'1000,700','KASATERM_WINDOW_POS':'40,100','KASATERM_NO_FOCUS':'1','KASATERM_NO_AUTOSUGGEST':'1','KASATERM_INLINE_RESULT_DIR':str(case/'images'),'KASATERM_INLINE_MEDIA_DIR':str(case/'media'),'KASATERM_TEST_PREVIEW_FRAME_REPORT':str(case/'frame.json'),'KASATERM_AUTOCAPTURE_MS':'15000','KASATERM_AUTOCAPTURE_PATH':str(case/'resized.png')}
 file=png if kind=='image' else repo/'app/kasaterm/src/media_player/fixtures/quadrants.mp4'
 block=subprocess.check_output([str(binary),'--inline-image' if kind=='image' else '--inline-media','--block',str(file)],env=env,text=True)
 (case/'block.txt').write_text(block)
 show=case/'show.sh';show.write_text("printf '\\033[2J\\033[H'\ncat '"+str(case/'block.txt')+"'\n")
 env['KASATERM_AUTOSEND']='bash '+str(show);env['KASATERM_AUTOSEND_MS']='1000'
 opening='native:@400,140' if kind=='image' else 'native:@498,231'
 env['KASATERM_AUTOCLICKS']=opening+';drag:@300,199>@350,229;drag:@799,554>@899,624;native:@868,237'
 env['KASATERM_AUTOCLICKS_MS']='5000';env['KASATERM_AUTOCLICKS_GAP_MS']='3000'
 # All interactions finish before capture; slow debug PNG writes cannot reorder a later drag.
 env['KASATERM_AUTOCAPTURE_MS']='12500'
 snapshots=[];last=object()
 with (case/'launcher.log').open('w') as log:
  proc=subprocess.Popen([str(binary)],cwd=case,env=env,stdout=log,stderr=subprocess.STDOUT)
  (case/'pid').write_text(str(proc.pid))
  until=time.monotonic()+55
  try:
   while proc.poll() is None and time.monotonic()<until:
    try:
     state=json.loads((case/'frame.json').read_text())
     if state!=last:snapshots.append(state);last=state
    except (FileNotFoundError,json.JSONDecodeError):pass
    time.sleep(.08)
   if proc.poll() is None:raise TimeoutError(kind)
  finally:
   if proc.poll() is None:
    proc.terminate()
    try:proc.wait(timeout=5)
    except subprocess.TimeoutExpired:proc.kill();proc.wait()
 assert proc.returncode==0,(kind,proc.returncode)
 (case/'states.json').write_text(json.dumps(snapshots,indent=2))
 visible=[s for s in snapshots if s and s.get('kind')==kind and s.get('ready')]
 near=lambda a,b:all(abs(x-y)<2 for x,y in zip(a,b))
 checks={
  'loaded':bool(visible),
  'quarter_default':any(near(s['frame'],[250,175,500,350]) for s in visible),
  'title_drag_moves':any(near(s['frame'],[300,205,500,350]) for s in visible),
  'corner_drag_resizes':any(near(s['frame'],[300,205,600,420]) for s in visible),
  'same_session':len({s['generation'] for s in visible})==1,
  'closed':bool(snapshots) and snapshots[-1] is None,
  'capture_exists':(case/'resized.png').exists(),
 }
 if kind=='video':checks['video_decoded']=any(s.get('decoded') for s in visible)
 report={'kind':kind,'checks':checks};reports.append(report)
 print(json.dumps(report),flush=True)
 (case/'checks.json').write_text(json.dumps(report,indent=2))
 assert all(checks.values()),report
(root/'success.json').write_text(json.dumps(reports,indent=2))
print('ALL PREVIEW FRAME UI CASES PASSED',root,flush=True)
