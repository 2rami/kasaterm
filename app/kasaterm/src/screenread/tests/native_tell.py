"""Render multi-paragraph tell text through a real isolated PTY and wgpu window."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[5]
TARGET = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target'))
RUN = Path(tempfile.mkdtemp(prefix='kasaterm-tell-paragraphs-'))
DRAW = '''import os,sys,time
sys.stdout.write('\\x1b[2J\\x1b[H')
rows=['› ⟦유즈⟧ 여러 문단의 보고', '  첫 문단은 같은 색으로 보여요.', '',
      '  실패 결과', '  - 문단 사이 빈 줄 뒤에도 같은 배경이에요.', '', '',
      '  | 항목 | 결과 |', '  | 문단 | 정상 |', '',
      '  마지막 문단도 발신자 색을 유지해요.', '']
for row in rows:
    sys.stdout.write('\\x1b[48;2;72;77;84m'+row+'\\x1b[K\\r\\n')
sys.stdout.write('\\x1b[0m\\r\\nOrdinary output remains unchanged.\\r\\n')
sys.stdout.flush()
time.sleep(30)
'''
results=[]
for appearance in (True, False):
    rig=RUN/('face' if appearance else 'plain')
    rig.mkdir();(rig/'tmp').mkdir()
    (rig/'draw.py').write_text(DRAW)
    (rig/'settings.json').write_text(json.dumps({'shim_inject':False,'font_size':16,'onboarding_complete':True,
        'claude_account':'','character_appearance':appearance}))
    env={k:v for k,v in os.environ.items() if k in {'HOME','USER','LOGNAME','PATH','LANG'}}
    env.update(KASATERM_SESSION_FILE=str(rig/'session.json'),KASATERM_SETTINGS_FILE=str(rig/'settings.json'),
        KASATERM_WINDOW_FILE=str(rig/'window.json'),KASATERM_SOCKET_PATH=str(rig/'app.sock'),
        KASATERM_DEVICE_FILE=str(rig/'device.json'),KASATERM_STUDENTS_DIR=str(rig/'students'),
        KASATERM_PET_DIR=str(rig/'pet'),KASATERM_COLLAB_ROOT=str(rig/'collab'),KASATERM_MACHINES='[]',
        KASATERM_KASANET='off',KASATERM_CWD=str(rig),KASATERM_SHELL='/bin/sh',KASATERM_AUTORESTORE='fresh',
        KASATERM_WINDOW_SIZE='1100,750',KASATERM_AUTOQUIT_MS='12000',TMPDIR=str(rig/'tmp')+'/',
        KASATERM_AUTOSEND=shlex.join([sys.executable,str(rig/'draw.py')]),KASATERM_AUTOSEND_MS='1500',
        KASATERM_AUTOCAPTURE_MS='8000',KASATERM_AUTOCAPTURE_PATH=str(rig/'window.png'))
    with (rig/'app.log').open('wb') as log:
        app=subprocess.Popen([str(TARGET/'debug/kasaterm')],env=env,cwd=rig,stdout=log,stderr=subprocess.STDOUT)
        try: app.wait(timeout=35)
        finally:
            if app.poll() is None: app.terminate();app.wait(timeout=10)
    assert (rig/'window.png').is_file(),rig
    results.append({'appearance':appearance,'capture':str(rig/'window.png'),'exit':app.returncode})
(RUN/'report.json').write_text(json.dumps(results,indent=2))
print(RUN,flush=True)
