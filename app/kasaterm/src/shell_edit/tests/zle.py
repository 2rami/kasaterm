import base64,fcntl,json,os,pathlib,pty,re,select,struct,subprocess,tempfile,time,termios
root=pathlib.Path(tempfile.mkdtemp(prefix='kt-zle-',dir='/tmp'))
source=pathlib.Path(__file__).resolve().parents[1]/'zsh.zsh'
master,slave=pty.openpty()
fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',24,40,0,0))
env={k:v for k,v in os.environ.items() if not k.startswith(('KASATERM_','CMUX_','ZDOTDIR'))}
env.update(TERM='xterm-256color',ZDOTDIR=str(root),LANG='en_US.UTF-8',LC_ALL='en_US.UTF-8')
p=subprocess.Popen(['/bin/zsh','-dfi'],stdin=slave,stdout=slave,stderr=slave,env=env,start_new_session=True)
os.close(slave)
transcript=bytearray()
def drain(wait=.03):
    end=time.monotonic()+wait
    while time.monotonic()<end:
        r,_,_=select.select([master],[],[],max(0,end-time.monotonic()))
        if r:
            try: transcript.extend(os.read(master,65536))
            except OSError: break
def send(data):
    os.write(master,data if isinstance(data,bytes) else data.encode())
def quote(s): return "'"+str(s).replace("'","'\\''")+"'"
state=root/'state'
fixture=root/'fixture.zsh'
fixture.write_text('''unset HISTFILE
HISTSIZE=100
PROMPT='p> '
RPROMPT=''
source '''+quote(source)+'''
_kt_fixture_seed() {
  IFS= read -r -d '' BUFFER < '''+quote(root/'seed')+'''
  CURSOR=$(<'''+quote(root/'cursor')+''')
  MARK=0; REGION_ACTIVE=0; _kasaterm_editor_select=0
  _kasaterm_editor_goal=-1
  _kasaterm_editor_history=0
}
_kt_fixture_dump() {
  printf '%s\\0%d\\0%d\\0%d\\0%d\\0' "$BUFFER" "$CURSOR" "$MARK" "$REGION_ACTIVE" "$HISTNO" >> '''+quote(state)+'''
}
zle -N _kt_fixture_seed
zle -N _kt_fixture_dump
bindkey $'\\e[11000~' _kt_fixture_dump
bindkey $'\\e[11001~' _kt_fixture_seed
print -s 'history_marker'
''')
checks=[]
seen=0
def current(keys=b''):
    global seen
    send(keys+b'\x1b[11000~')
    until=time.monotonic()+4
    while time.monotonic()<until:
        drain()
        raw=state.read_bytes() if state.exists() else b''
        records=raw.split(b'\0')
        if len(records)>seen+5:
            v=records[seen:seen+5];seen+=5
            return {'buffer':v[0].decode(),'cursor':int(v[1]),'mark':int(v[2]),'region':int(v[3]),'history':int(v[4])}
    raise RuntimeError('ZLE did not respond: '+repr(bytes(transcript[-1600:])))
def seed(text,cursor=None):
    (root/'seed').write_bytes(text.encode()+b'\0')
    (root/'cursor').write_text(str(len(text) if cursor is None else cursor))
    return current(b'\x1b[11001~')
def expect(name,s,**want):
    assert all(s[k]==v for k,v in want.items()),(name,s,want)
    checks.append(name)
def mouse(start,end,prefix,replacement='',mode='replace'):
    return (f'\x1b[9001~{start};{end};{mode};{prefix.encode().hex()};{replacement.encode().hex()}\x1b[201~').encode()
try:
    drain(.2)
    send('source '+quote(fixture)+'\r')
    drain(.2)
    s=seed('abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ01234567')
    expect('wrapped up',current(b'\x1b[A'),buffer=s['buffer'],cursor=20,history=s['history'])
    expect('wrapped down',current(b'\x1b[B'),buffer=s['buffer'],cursor=60)
    seed('abcdef')
    expect('shift left selection',current(b'\x1b[1;2D'*3),cursor=3,mark=6,region=1)
    expect('backspace deletes selected range',current(b'\x7f'),buffer='abc',cursor=3,region=0)
    expect('undo selected delete',current(b'\x1b[122;9u'),buffer='abcdef')
    expect('redo selected delete',current(b'\x1b[122;10u'),buffer='abc')
    seed('alpha beta gamma')
    expect('option shift word',current(b'\x1b[1;4D'),cursor=11,mark=16,region=1)
    expect('word selection typing replaces',current(b'X'),buffer='alpha beta X',cursor=12,region=0)
    seed('alpha beta',0)
    expect('option right uses word end',current(b'\x1bf'),cursor=5)
    expect('next option right skips space',current(b'\x1bf'),cursor=10)
    seed('alpha beta',0)
    expect('option shift right selects word without following space',current(b'\x1b[1;4C'),cursor=5,mark=0,region=1)
    expect('right word selection delete',current(b'\x7f'),buffer=' beta',cursor=0)
    seed('alpha beta')
    expect('select all',current(b'\x1b[97;9u'),cursor=10,mark=0,region=1)
    expect('select all delete',current(b'\x7f'),buffer='',cursor=0)
    seed('alpha beta')
    current(b'\x1b[97;9u')
    expect('paste replaces selection',current(b'\x1b[200~new\nline\x1b[201~'),buffer='new\nline',cursor=8,region=0)
    seed('abc def ghi',7)
    expect('command backspace keeps suffix',current(b'\x1b[127;9u'),buffer=' ghi',cursor=0)
    seed('abc def ghi',4)
    expect('command delete keeps prefix',current(b'\x1b[3;9~'),buffer='abc ',cursor=4)
    seed('abc def')
    expect('option backspace',current(b'\x1b\x7f'),buffer='abc ',cursor=4)
    seed('abc def',4)
    expect('forward delete',current(b'\x1b[3~'),buffer='abc ef',cursor=4)
    seed('abc')
    expect('shift enter is not execution',current(b'\x1b[13;2u'),buffer='abc\n',cursor=4)
    seed('한글 alpha 가나다')
    expect('unicode option shift',current(b'\x1b[1;4D'),cursor=9,mark=12,region=1)
    expect('unicode delete',current(b'\x7f'),buffer='한글 alpha ',cursor=9)
    seed('copy me')
    current(b'\x1b[97;9u')
    start=len(transcript)
    current(b'\x1b[99;9u')
    drain(.1)
    payload=re.findall(rb'\x1b\]52;c;([^\x07]*)\x07',bytes(transcript[start:]))
    assert payload and base64.b64decode(payload[-1])==b'copy me',payload
    checks.append('copy selection OSC52')
    expect('cut deletes selection',current(b'\x1b[120;9u'),buffer='',cursor=0)
    seed('abcdef')
    current(b'\x1b[1;2D'*3)
    expect('plain left collapses selection',current(b'\x1b[D'),cursor=3,region=0)
    seed('abc def ghi')
    expect('mouse selected delete',current(mouse(4,7,'abc def')),buffer='abc  ghi',cursor=4)
    seed('abc def ghi')
    expect('mouse typing replacement',current(mouse(4,7,'abc def','X\nY')),buffer='abc X\nY ghi',cursor=7)
    expect('mouse replacement undo',current(b'\x1b[122;9u'),buffer='abc def ghi')
    seed('abc XYZ ghi')
    expect('stale selection never deletes new input',current(mouse(4,7,'abc def')),buffer='abc XYZ ghi',cursor=11,region=0)
    seed('한글 가나다 world')
    expect('unicode mouse replacement',current(mouse(3,6,'한글 가나다','새 글')),buffer='한글 새 글 world',cursor=6)
    seed('abc def')
    expect('row selection ignores blank screen padding',current(mouse(0,10,'abc def   ')),buffer='',cursor=0)
    seed('abc def ghi')
    current(mouse(4,7,'abc def',mode='select'))
    expect('mouse range becomes native selection',current(),cursor=7,mark=4,region=1)
    expect('native delete after mouse range',current(b'\x1b[3~'),buffer='abc  ghi',cursor=4)
    seed('alpha')
    expect('suggestion accepts matching live input',current(mouse(0,5,'alpha',' beta',mode='suggest-right')),buffer='alpha beta',cursor=10)
    seed('alpha')
    current(b'\x1b[97;9u')
    expect('suggestion never replaces a live selection',current(mouse(0,5,'alpha',' beta',mode='suggest-right')),buffer='alpha',cursor=5,region=0)
    seed('different')
    expect('stale suggestion never edits another draft',current(mouse(0,5,'alpha',' beta',mode='suggest-right')),buffer='different',cursor=9)
    seed('alpha',2)
    expect('suggestion inside text only moves caret',current(mouse(0,2,'al',' beta',mode='suggest-right')),buffer='alpha',cursor=3)
    seed('replace me')
    expect('large selection paste is preserved',current(mouse(0,10,'replace me','x'*20000)),buffer='x'*20000,cursor=20000)
    seed('first\nsecond\nthird',10)
    expect('option up first reaches this paragraph',current(b'\x1b[1;3A'),cursor=6)
    expect('option up at boundary reaches previous paragraph',current(b'\x1b[1;3A'),cursor=0)
    expect('option down first reaches this paragraph end',current(b'\x1b[1;3B'),cursor=5)
    expect('option down at boundary reaches next paragraph end',current(b'\x1b[1;3B'),cursor=12)
    seed('한'*20)
    expect('wide wrapped vertical goal',current(b'\x1b[A\x1b[B'),buffer='한'*20,cursor=20)
    seed('abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ01234567')
    expect('repeated wrap movement',current(b'\x1b[A\x1b[B'*3),cursor=60)
    seed('new draft')
    expect('up at draft boundary never recalls history',current(b'\x1b[A'),buffer='new draft',cursor=0)
    expect('down at draft boundary goes to end',current(b'\x1b[B'),buffer='new draft',cursor=9)
    seed('')
    expect('empty prompt history',current(b'\x1b[A'),buffer='history_marker')
    print('PASSED',len(checks),'checks',root,flush=True)
    (root/'checks.json').write_text(json.dumps(checks,ensure_ascii=False,indent=2))
finally:
    (root/'terminal.bin').write_bytes(transcript)
    p.terminate()
    try:p.wait(timeout=3)
    except subprocess.TimeoutExpired:p.kill();p.wait()
    os.close(master)
