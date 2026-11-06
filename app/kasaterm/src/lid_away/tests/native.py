import json, os, pathlib, subprocess, tempfile, time, shutil

root = pathlib.Path(tempfile.mkdtemp(prefix='kt-lid-hotspot-', dir='/tmp'))
repo = pathlib.Path(__file__).resolve().parents[5]
binary = pathlib.Path(os.environ.get('CARGO_TARGET_DIR', str(repo / 'target'))) / 'debug/kasaterm'
cli_binary = os.environ.get('KASATERM_TEST_CLI') or shutil.which('kasaterm-cli') or str(binary.with_name('kasaterm-cli'))
print('artifacts:', root, flush=True)
(root / 'students').mkdir()
(root / 'settings.json').write_text(json.dumps({'lid_away': True, 'lid_away_minutes': 10,
    'lid_away_hotspot': True, 'lid_away_hotspot_ssid': 'Fixture phone'}))
(root / 'lid').write_text('open')
(root / 'pmset').write_text('#!/usr/bin/python3\nimport sys\nfrom pathlib import Path\np=Path(__file__).with_name("power.log")\nwith p.open("a") as f:f.write(" ".join(sys.argv[1:])+"\\n")\n')
(root / 'networksetup').write_text('''#!/usr/bin/python3
import sys
from pathlib import Path
r=Path(__file__).parent
a=sys.argv[1:]
with (r/'network.log').open('a') as f:f.write(repr(a)+'\\n')
if a[0]=='-listallhardwareports': print('Hardware Port: Wi-Fi\\nDevice: en0\\n')
elif a[0]=='-listpreferredwirelessnetworks': print('Preferred networks on en0:\\n\\tFixture phone\\n\\tOther network')
elif a[0]=='-setairportnetwork': (r/'connected').write_text(a[2])
elif a[0]=='-getairportnetwork': print('Current Wi-Fi Network: '+((r/'connected').read_text() if (r/'connected').exists() else 'Other network'))
''')
for name in ['pmset', 'networksetup']: (root/name).chmod(0o755)
(root / 'fixture.rs').write_text('fn main(){std::thread::sleep(std::time::Duration::from_secs(180));}')
subprocess.run(['rustc', str(root/'fixture.rs'), '-o', str(root/'claude')], check=True)
base = {k:v for k,v in os.environ.items() if not k.startswith(('KASATERM_', 'CMUX_'))}
base.pop('NO_COLOR', None)
env = {**base, 'TMPDIR':str(root), 'KASATERM_SESSION_FILE':str(root/'session.json'),
    'KASATERM_SETTINGS_FILE':str(root/'settings.json'), 'KASATERM_WINDOW_FILE':str(root/'window.json'),
    'KASATERM_STUDENTS_DIR':str(root/'students'), 'KASATERM_PET_DIR':str(root/'pet'),
    'KASATERM_DEVICE_FILE':str(root/'device.json'), 'KASATERM_COLLAB_ROOT':str(root/'collab'),
    'KASATERM_KASANET_KEY':str(root/'key'), 'KASATERM_KASANET':'off', 'KASATERM_MACHINES':'[]',
    'KASATERM_AUTORESTORE':'fresh', 'KASATERM_SOCKET_PATH':str(root/'app.sock'),
    'KASATERM_NOTIFICATION_PROBE':'1', 'KASATERM_AUTOQUIT_MS':'150000',
    'KASATERM_WINDOW_SIZE':'1100,900', 'KASATERM_NO_FOCUS':'1', 'KASATERM_NO_AUTOSUGGEST':'1',
    'KASATERM_AUTOSETTINGS_MS':'5000', 'KASATERM_AUTOSETTINGS':'machines',
    'KASATERM_AUTOSETTINGS_SCROLL':'9999', 'KASATERM_LID_PROBE':str(root/'lid'),
    'KASATERM_LID_AWAY_PMSET':str(root/'pmset'), 'KASATERM_LID_AWAY_NETWORKSETUP':str(root/'networksetup')}

def cli(*args):
    p = subprocess.run([cli_binary, *args], env=env, capture_output=True, text=True, timeout=12)
    if p.returncode: raise RuntimeError(p.stdout + p.stderr)
    return p.stdout

def wait_for(check, timeout=15):
    end = time.monotonic()+timeout
    while time.monotonic()<end:
        if check(): return
        time.sleep(.25)
    raise AssertionError('condition timed out')

def power(): return (root/'power.log').read_text() if (root/'power.log').exists() else ''
def joins():
    text = (root/'network.log').read_text() if (root/'network.log').exists() else ''
    return text.count("'-setairportnetwork'")

with (root/'launcher.log').open('w') as log:
    app = subprocess.Popen([str(binary)], cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT)
    (root/'pid').write_text(str(app.pid))
    try:
        wait_for(lambda: (root/'app.sock').exists())
        time.sleep(2)
        cli('launch', '%0', str(root/'claude'))
        time.sleep(2)
        cli('turn', '--surface', '%0', 'start', '--permission-mode', 'bypassPermissions')
        wait_for(lambda: '-a disablesleep 1' in power())
        (root/'lid').write_text('closed')
        wait_for(lambda: joins() == 1)
        time.sleep(3)
        cli('capture', '--window', str(root/'settings.png'))
        cli('turn', '--surface', '%0', 'end')
        before = time.monotonic()
        wait_for(lambda: 'sleepnow' in power(), timeout=78)
        assert time.monotonic()-before >= 55, power()
        assert '-a disablesleep 0\nsleepnow' in power(), power()
        assert joins() == 1, 'one association per lid closure'
        arms = power().count('-a disablesleep 1')
        cli('turn', '--surface', '%0', 'start')
        time.sleep(6)
        assert power().count('-a disablesleep 1') == arms, 'closed wake must not rearm'
        assert joins() == 1, 'sleep retry must not reconnect'
        (root/'lid').write_text('open')
        time.sleep(2)
        cli('turn', '--surface', '%0', 'start')
        wait_for(lambda: power().count('-a disablesleep 1') > arms)
        print('PASS: lid connection, quiet sleep, DarkWake and reopen', flush=True)
        (root/'success.json').write_text(json.dumps({'joins':joins(), 'quiet_sleep':True, 'dark_wake':True, 'reopen':True}))
    finally:
        if app.poll() is None:
            app.terminate()
            try: app.wait(timeout=8)
            except subprocess.TimeoutExpired: app.kill(); app.wait()
