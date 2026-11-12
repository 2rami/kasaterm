"""Exercise account planning and child environments using only synthetic accounts and launchers."""
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import tempfile
import time

ROOT=Path(__file__).resolve().parents[5]
TARGET=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))
BIN=TARGET/'debug/kasaterm'
CLI=TARGET/'debug/kasaterm-cli'
RUN=Path(tempfile.mkdtemp(prefix='kt-account-',dir='/tmp'))


def rpc(path,method,params):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(15)
        s.connect(str(path))
        s.sendall((json.dumps({'id':'fixture','method':method,'params':params})+'\n').encode())
        return json.loads(s.makefile('rb').readline())


def row(id,used):
    return {'id':id,'label':id,'logged_in':True,'usage':used,'usage_stale':False,'usage_state':'ready'}


def main():
    for folder in ['home','tmp','collab','fakebin']:
        (RUN/folder).mkdir()
    settings={'shim_inject':True,'claude_persona':False,'claude_account':'','codex_account':'','claude_accounts':[], 'codex_accounts':[]}
    (RUN/'settings.json').write_text(json.dumps(settings))
    fixture={'agents':{'preferred_agent':'claude'},'claude':{'account':'acct-active','codex_account':'',
        'accounts':[row('acct-active',47),row('acct-low',0)],'codex_accounts':[row('',17),row('codex-low',5)]}}
    choices=RUN/'collab/choices.json'
    choices.write_text(json.dumps(fixture))
    env={k:v for k,v in os.environ.items() if k in {'USER','LOGNAME','LANG','PATH'}}
    env.update(HOME=str(RUN/'home'),KASATERM_SESSION_FILE=str(RUN/'session.json'),KASATERM_SETTINGS_FILE=str(RUN/'settings.json'),
        KASATERM_WINDOW_FILE=str(RUN/'window.json'),KASATERM_SOCKET_PATH=str(RUN/'app.sock'),KASATERM_DEVICE_FILE=str(RUN/'device.json'),
        KASATERM_STUDENTS_DIR=str(RUN/'students'),KASATERM_PET_DIR=str(RUN/'pet'),KASATERM_COLLAB_ROOT=str(RUN/'collab'),
        KASATERM_KASANET='off',KASATERM_MACHINES='[]',KASATERM_CWD=str(RUN),KASATERM_SHELL='/bin/sh',
        KASATERM_AUTORESTORE='fresh',KASATERM_AUTOQUIT_MS='23000',KASATERM_WINDOW_SIZE='1100,750',
        KASATERM_TEST_ACCOUNT_CHOICES=str(choices),TMPDIR=str(RUN/'tmp')+'/')
    log=(RUN/'launcher.log').open('wb')
    app=subprocess.Popen([str(BIN)],env=env,cwd=RUN,stdout=log,stderr=subprocess.STDOUT)
    try:
        until=time.monotonic()+25
        while not (RUN/'app.sock').exists():
            assert app.poll() is None and time.monotonic()<until
            time.sleep(.1)
        def call(method,params):
            r=rpc(RUN/'app.sock',method,params)
            assert r['ok'],r
            return r['result']
        plan=json.loads(subprocess.run([str(CLI),'agent-plan'],env=env,capture_output=True,timeout=10,check=True).stdout)['result']
        assert (plan['provider'],plan['account_id'])==('claude','acct-low'),plan
        port=int((RUN/'app.mcp_port').read_text())
        assert port!=8765
        base=f'http://127.0.0.1:{port}'
        remote=json.loads(subprocess.run([str(CLI),'--api',base,'agent-plan'],env=env,capture_output=True,timeout=10,check=True).stdout)['result']
        assert remote['account_id']=='acct-low' and remote['account_selection']==1
        mismatch=subprocess.run([str(CLI),'--api',base,'agent-plan','--machine','wrong-target'],env=env,capture_output=True,timeout=10)
        assert mismatch.returncode!=0

        # An unrelated process cannot request another pane's account binding.
        assert not rpc(RUN/'app.sock','agent.account_env',{'surface_id':'%0','pid':os.getpid(), 'selection':{'provider':'claude','account_id':'acct-low'}})['ok']
        shims=list((RUN/'runtime').glob('kasaterm-shim-*'))
        assert len(shims)==1,shims
        shim=shims[0]
        assert shim.is_relative_to(RUN)
        for name in ['claude','codex']:
            (shim/name).write_text('#!'+sys.executable+'\n'+f'''import json,os,time
from pathlib import Path
keys=['KASATERM_AUTO_ACCOUNT_ID','KASATERM_AUTO_ACCOUNT_PROVIDER','CLAUDE_SECURESTORAGE_CONFIG_DIR','KASATERM_CODEX_ACCOUNT_DIR']
Path({str(RUN/(name+'.json'))!r}).write_text(json.dumps(dict({{k:os.environ.get(k) for k in keys}},pid=os.getpid())))
print('FIXTURE {name}: '+os.environ.get('KASATERM_AUTO_ACCOUNT_ID','MISSING'),flush=True)
time.sleep(60)
''')
            (shim/name).chmod(0o700)
        call('surface.send_text',{'surface_id':'%0','text':"export CLAUDE_SECURESTORAGE_CONFIG_DIR='original-shell-value'\n"})
        time.sleep(.2)
        command=f"{shlex.quote(str(CLI))} account-run claude acct-low -- claude"
        call('surface.launch',{'surface_id':'%0','command':command})
        until=time.monotonic()+7
        while not (RUN/'claude.json').exists():
            assert time.monotonic()<until,call('collab.peek',{'surface_id':'%0','lines':20})
            time.sleep(.1)
        c=json.loads((RUN/'claude.json').read_text())
        assert c['CLAUDE_SECURESTORAGE_CONFIG_DIR']==str(RUN/'claude-accounts/acct-low'),c
        assert not rpc(RUN/'app.sock','agent.account_env',{'surface_id':'%0','pid':c['pid'],'selection':{'provider':'claude','account_id':'acct-active'}})['ok']
        second=call('surface.split',{'direction':'right','from':'%0','focus':False})['surface']['id']
        fixture['claude']['accounts'][1]['usage']=60
        choices.write_text(json.dumps(fixture))
        plan=call('agent.plan',{})
        assert (plan['provider'],plan['account_id'])==('codex','codex-low'),plan
        call('surface.launch',{'surface_id':second,'command':f"{shlex.quote(str(CLI))} account-run codex codex-low -- codex"})
        until=time.monotonic()+7
        while not (RUN/'codex.json').exists():
            assert time.monotonic()<until
            time.sleep(.1)
        c=json.loads((RUN/'codex.json').read_text())
        assert c['KASATERM_CODEX_ACCOUNT_DIR']==str(RUN/'codex-accounts/codex-low'),c
        call('surface.capture',{'surface_id':'','path':str(RUN/'window.png'),'max_width':1100})
        app.wait(timeout=30)
        saved=json.loads((RUN/'session.json').read_text())
        def walk(v):
            if isinstance(v,dict):
                if 'auto_account' in v: yield v['auto_account']
                for child in v.values(): yield from walk(child)
            elif isinstance(v,list):
                for child in v: yield from walk(child)
        bindings=list(walk(saved))
        assert {'provider':'claude','account_id':'acct-low'} in bindings,bindings
        assert {'provider':'codex','account_id':'codex-low'} in bindings,bindings
        after=json.loads((RUN/'settings.json').read_text())
        assert after.get('claude_account','')=='' and after.get('codex_account','')==''
        report={'ok':True,'rig':str(RUN),'saved_accounts':bindings,'settings_accounts_unchanged':True,'real_provider_calls':0}
        (RUN/'report.json').write_text(json.dumps(report,indent=2))
        print(json.dumps(report),flush=True)
    finally:
        if app.poll() is None:
            app.terminate()
        app.wait(timeout=10)
        log.close()


if __name__=='__main__': main()
