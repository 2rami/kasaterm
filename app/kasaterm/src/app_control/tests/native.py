"""Isolated restore controls; helper relaunch is covered by app_restart's fake app tests."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[5]
TARGET = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target'))
BIN = TARGET / 'debug/kasaterm'
CLI = TARGET / 'debug/kasaterm-cli'
RUN = Path(tempfile.mkdtemp(prefix='kasaterm-app-controls-'))


def rpc(path, method, params):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(10)
        s.connect(str(path))
        s.sendall((json.dumps({'id':'fixture','method':method,'params':params})+'\n').encode())
        return json.loads(s.makefile('rb').readline())


def phase(name, automatic=False, missing=False):
    rig = RUN / name
    rig.mkdir()
    (rig / 'tmp').mkdir()
    (rig / 'settings.json').write_text(json.dumps({'shim_inject':False,'font_size':13,'onboarding_complete':True}))
    state = {'active_session':0,'sessions':[{'active_window':0,'windows':[
        {'leaf':{'pane_id':'%31','cwd':str(rig),'shell':'/bin/sh','title':'Saved first'}},
        {'leaf':{'pane_id':'%32','cwd':str(rig),'shell':'/bin/sh','title':'Saved second'}}]}]}
    if not missing:
        (rig / 'session.json').write_text(json.dumps(state))
    machine = uuid.uuid4().hex
    job = 'rscontrolfixture'
    env = {k:v for k,v in os.environ.items() if k in {'HOME','USER','LOGNAME','PATH','LANG'}}
    env.update(KASATERM_SESSION_FILE=str(rig/'session.json'),KASATERM_SETTINGS_FILE=str(rig/'settings.json'),
        KASATERM_WINDOW_FILE=str(rig/'window.json'),KASATERM_SOCKET_PATH=str(rig/'app.sock'),
        KASATERM_DEVICE_FILE=str(rig/'device.json'),KASATERM_STUDENTS_DIR=str(rig/'students'),
        KASATERM_PET_DIR=str(rig/'pet'),KASATERM_COLLAB_ROOT=str(rig/'collab'),KASATERM_MACHINES='[]',
        KASATERM_KASANET='off',KASATERM_MACHINE_ID=machine,KASATERM_CWD=str(rig),KASATERM_SHELL='/bin/sh',
        KASATERM_SERVER_RESTORE_TEST='1',KASATERM_WINDOW_SIZE='1100,750',KASATERM_AUTOQUIT_MS='45000',
        TMPDIR=str(rig/'tmp')+'/')
    if automatic:
        env['KASATERM_TEST_CONTROL_RESTORE']='2'
    log=(rig/'launcher.log').open('wb')
    app=subprocess.Popen([str(BIN)],env=env,cwd=rig,stdout=log,stderr=subprocess.STDOUT)
    try:
        until=time.monotonic()+30
        while not (rig/'app.sock').exists():
            assert app.poll() is None and time.monotonic()<until
            time.sleep(.2)
        time.sleep(1)
        def inspect():
            output=subprocess.run([str(CLI),'app','inspect'],env=env,capture_output=True,timeout=15,check=True)
            result=json.loads(output.stdout)
            assert result['ok'],result
            return result['result']
        before=inspect()
        (rig/'before.json').write_text(json.dumps(before,ensure_ascii=False,indent=2))
        if not automatic:
            assert before['restore']['selected_count']==2,before
            # A different machine must not be forwarded again by the receiving HTTP host.
            port=int((rig/'app.mcp_port').read_text())
            assert port!=8765
            req=urllib.request.Request(f'http://127.0.0.1:{port}/app/control',data=json.dumps({'action':'inspect','machine_id':'wrong'}).encode(),headers={'Content-Type':'application/json'})
            try:
                urllib.request.urlopen(req,timeout=5)
                raise AssertionError('wrong target accepted')
            except urllib.error.HTTPError as err:
                assert err.code==409
            captured=rpc(rig/'app.sock','surface.capture',{'surface_id':'','path':str(rig/'before.png'),'max_width':1100})
            assert captured['ok'],captured
            bad=rpc(rig/'app.sock','app.control',{'action':'restore','expect':'0.old'})
            assert not bad['ok']
            assert inspect()['restore']['selected_count']==2
            current=inspect()
            restored=json.loads(subprocess.run([str(CLI),'app','restore','--expect',current['revision']],env=env,capture_output=True,timeout=15,check=True).stdout)
            assert restored['ok'],restored
        until=time.monotonic()+20
        while True:
            after=inspect()
            if missing or (after['restore'] is None and not after['restoring']):
                break
            assert time.monotonic()<until,after
            time.sleep(.3)
        (rig/'after.json').write_text(json.dumps(after,ensure_ascii=False,indent=2))
        if not missing:
            assert after['restart']['plain_shells']==2,after
            image=rpc(rig/'app.sock','surface.capture',{'surface_id':'','path':str(rig/'after.png'),'max_width':1100})
            assert image['ok'],image
        if automatic:
            until=time.monotonic()+5
            while True:
                status=rpc(rig/'app.sock','app.restart_job',{'job_id':job})
                assert status['ok'],status
                expected='failed' if missing else 'verified'
                if status['result']['state']==expected:
                    break
                assert time.monotonic()<until,status
                time.sleep(.2)
            (rig/'job.json').write_text(json.dumps(status,indent=2))
        current=inspect()
        refusal=rpc(rig/'app.sock','app.control',{'action':'restart','expect':current['revision'],'interrupt':True})
        assert not refusal['ok'],'development app must not relaunch outside isolation'
        assert app.poll() is None
        return {'phase':name,'pass':True,'rig':str(rig),'restored_shells':after['restart']['plain_shells']}
    finally:
        if app.poll() is None:
            app.terminate()
        app.wait(timeout=10)
        log.close()


if __name__=='__main__':
    results=[]
    for name,auto,missing in [('manual',False,False),('automatic',True,False),('missing',True,True)]:
        result=phase(name,auto,missing)
        results.append(result)
        print(json.dumps(result),flush=True)
    (RUN/'report.json').write_text(json.dumps(results,indent=2))
    print(RUN,flush=True)
