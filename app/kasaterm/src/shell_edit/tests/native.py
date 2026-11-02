import json,os,pathlib,subprocess,tempfile,time,sys
if sys.platform != 'darwin':
    raise SystemExit('This fixture uses the macOS AppKit input path')
root=pathlib.Path(tempfile.mkdtemp(prefix='kt-shell-ui-',dir='/tmp'))
print('artifacts:',root,flush=True)
binary=pathlib.Path(os.environ.get('CARGO_TARGET_DIR',str(pathlib.Path(__file__).resolve().parents[5]/'target')))/'debug/kasaterm'
(root/'settings.json').write_text(json.dumps({'lid_away':False,'sidebar_body':'map'}))
state=root/'state.bin'
def quote(s):return "'"+str(s).replace("'","'\\''")+"'"
fixture=root/'fixture.zsh'
fixture_text='''unset HISTFILE
fc -p
HISTSIZE=100
bindkey -e
PROMPT='p> '
RPROMPT=''
precmd_functions=(_kasaterm_osc133)
_kt_ui_dump() {
  _kasaterm_editor_prompt_width
  printf '%s\\0%d\\0%d\\0%d\\0%d\\0%d\\0%d\\0' "$BUFFER" "$CURSOR" "$MARK" "$REGION_ACTIVE" "$COLUMNS" "$HISTNO" "$REPLY" >> '''+quote(state)+'''
}
zle -N _kt_ui_dump
bindkey '^X^D' _kt_ui_dump
print -s 'history_marker'
printf '\\e[2J\\e[H'
'''
if '--user-prompt' in sys.argv:
    fixture_text=fixture_text.replace("PROMPT='p> '\nRPROMPT=''\nprecmd_functions=(_kasaterm_osc133)\n",'')
fixture.write_text(fixture_text)
plan=[]
expected=[]
t=7000
def key(spec,wait=450):
    global t
    plan.append((t,spec));t+=wait
def dump(label,**want):
    key('ctrl+x',70);key('ctrl+d',500)
    expected.append((label,want))
def reset(text=''):
    key('cmd+a');key('backspace')
    if text:key('text:'+text,650)
key('text:'+'abcdefghij'*24,900)
dump('typed wrapped input',buffer='abcdefghij'*24,cursor=240)
key('up');dump('native up stays in draft',buffer='abcdefghij'*24,cursor=lambda s:240-s['columns'])
key('down');dump('native down returns to same column',buffer='abcdefghij'*24,cursor=240)
reset('alpha beta gamma')
key('alt+shift+left');dump('native option shift word',buffer='alpha beta gamma',cursor=11,mark=16,region=1)
key('backspace');dump('native selected delete',buffer='alpha beta ',cursor=11,region=0)
key('cmd+z');dump('native undo',buffer='alpha beta gamma')
key('cmd+shift+z');dump('native redo',buffer='alpha beta ')
reset('alpha beta')
key('alt+shift+left');key('cmd+c');dump('native copy keeps selection',buffer='alpha beta',cursor=6,mark=10,region=1)
key('text:X');dump('native typing replaces selection',buffer='alpha X',cursor=7,region=0)
reset('alpha beta')
key('cmd+a');key('cmd+v',650);dump('native multiline paste replaces selection',buffer='pasted\nline',cursor=11,region=0)
reset('abc def ghi')
key('cmd+left');key('alt+right');key('alt+right');key('cmd+backspace');dump('native command backspace keeps suffix',buffer=' ghi',cursor=0)
reset('abc def ghi')
key('cmd+left');key('alt+right');key('right');key('cmd+delete');dump('native command delete keeps prefix',buffer='abc ',cursor=4)
reset('first')
key('shift+enter');key('text:second');dump('native shift enter does not execute',buffer='first\nsecond',cursor=12)
key('up');dump('native multiline up',buffer='first\nsecond',cursor=lambda s:min(5,max(0,6-s['prompt']%s['columns'])))
key('down');dump('native multiline down',buffer='first\nsecond',cursor=12)
reset('한글 alpha 가나다')
key('alt+shift+left');key('backspace');dump('native Korean word selection delete',buffer='한글 alpha ',cursor=9)
reset('draft')
key('up');dump('native boundary protects draft',buffer='draft',cursor=0)
reset('')
key('up');dump('native empty history works',buffer='history_marker')
reset('alpha beta gamma')
mouse1=t+200
t=mouse1+2000
key('backspace');dump('native drag selection deletes exact range',buffer='alpha  gamma',cursor=6)
reset('alpha beta gamma')
key('cmd+=',14000)
mouse2=t+200
t=mouse2+2000
key('backspace');dump('native selection after zoom',buffer='alpha  gamma',cursor=6)
reset('alpha beta gamma')
key('alt+shift+left')
dump('final shell selection',buffer='alpha beta gamma',cursor=11,mark=16,region=1)
capture_ms=t+700
quit_ms=t+9000
base={k:v for k,v in os.environ.items() if not k.startswith(('KASATERM_','CMUX_'))}
base.pop('NO_COLOR',None)
env={**base,'TMPDIR':str(root),'KASATERM_SESSION_FILE':str(root/'session.json'),'KASATERM_SETTINGS_FILE':str(root/'settings.json'),'KASATERM_WINDOW_FILE':str(root/'window.json'),'KASATERM_STUDENTS_DIR':str(root/'students'),'KASATERM_PET_DIR':str(root/'pet'),'KASATERM_DEVICE_FILE':str(root/'device.json'),'KASATERM_COLLAB_ROOT':str(root/'collab'),'KASATERM_KASANET_KEY':str(root/'kasanet.key'),'KASATERM_KASANET':'off','KASATERM_MACHINES':'[]','KASATERM_AUTORESTORE':'fresh','KASATERM_SOCKET_PATH':str(root/'app.sock'),'KASATERM_NOTIFICATION_PROBE':'1','KASATERM_AUTOQUIT_MS':str(quit_ms),'KASATERM_WINDOW_SIZE':'1000,700','KASATERM_WINDOW_POS':'40,100','KASATERM_NO_FOCUS':'1','KASATERM_NO_AUTOSUGGEST':'1','KASATERM_AUTOSEND':'source '+quote(fixture),'KASATERM_AUTOSEND_MS':'1200','KASATERM_AUTOKEYS':';'.join(f'{at}:{key}' for at,key in plan),'KASATERM_AUTOSHELLEDIT_DIR':str(root),'KASATERM_TEST_COPY_PATH':str(root/'copy.txt'),'KASATERM_TEST_PASTE_TEXT':'pasted\nline','KASATERM_AUTOCLICKS':'drag:beta>a gamma;drag:beta>a gamma','KASATERM_AUTOCLICKS_MS':str(mouse1),'KASATERM_AUTOCLICKS_GAP_MS':str(mouse2-mouse1),'KASATERM_AUTOCAPTURE_MS':str(capture_ms),'KASATERM_AUTOCAPTURE_PATH':str(root/'selection.png')}
(root/'plan.json').write_text(json.dumps({'keys':plan,'mouse':[mouse1,mouse2],'quit_ms':quit_ms,'labels':[x[0] for x in expected]},ensure_ascii=False,indent=2))
with (root/'launcher.log').open('w') as log:
    proc=subprocess.Popen([str(binary)],cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT)
    (root/'pid').write_text(str(proc.pid))
    try:proc.wait(timeout=quit_ms/1000+25)
    except subprocess.TimeoutExpired:
        proc.terminate()
        try:proc.wait(timeout=5)
        except subprocess.TimeoutExpired:proc.kill();proc.wait()
        raise
assert proc.returncode==0,proc.returncode
raw=state.read_bytes().split(b'\0')
records=[]
for at in range(0,len(raw)-1,7):
    chunk=raw[at:at+7]
    records.append({'buffer':chunk[0].decode(),'cursor':int(chunk[1]),'mark':int(chunk[2]),'region':int(chunk[3]),'columns':int(chunk[4]),'history':int(chunk[5]),'prompt':int(chunk[6])})
(root/'states.json').write_text(json.dumps(records,ensure_ascii=False,indent=2))
assert len(records)==len(expected),(len(records),len(expected))
failures=[]
for (label,want),state in zip(expected,records):
    want={k:v(state) if callable(v) else v for k,v in want.items()}
    if any(state[k]!=v for k,v in want.items()):failures.append({'case':label,'actual':state,'expected':want})
report={'cases':len(expected),'checks':sum(len(v) for _,v in expected),'failures':failures}
(root/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2))
print(json.dumps(report,ensure_ascii=False),root,flush=True)
assert not failures,failures
assert (root/'copy.txt').read_text()=='beta'
print('ALL NATIVE SHELL EDIT CASES PASSED',root,flush=True)
