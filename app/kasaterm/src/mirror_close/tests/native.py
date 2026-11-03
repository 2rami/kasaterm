import argparse
import json
import os
import pathlib
import re
import shlex
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid

if sys.platform != 'darwin':
    raise SystemExit('This fixture uses native macOS confirmation sheets')
parser = argparse.ArgumentParser()
parser.add_argument('--case', action='append')
args = parser.parse_args()
repo = pathlib.Path(__file__).resolve().parents[5]
binary = pathlib.Path(os.environ.get('CARGO_TARGET_DIR', str(repo / 'target'))) / 'debug/kasaterm'
root = pathlib.Path(tempfile.mkdtemp(prefix='kt-remote-close-', dir='/tmp'))
print('artifacts:', root, flush=True)
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
base_env = {k: v for k, v in os.environ.items() if not k.startswith(('KASATERM_', 'CMUX_', 'CLAUDE_', 'CODEX_'))}
base_env.pop('NO_COLOR', None)
rc = root / 'rc'
rc.mkdir()
(rc / '.zshrc').write_text("unset HISTFILE\nPROMPT=$'%{\\e]133;A\\a%}fixture> %{\\e]133;B\\a%}'\npreexec() { printf '\\033]133;C\\007'; }\nprecmd() { printf '\\033]133;D;0\\007'; }\n")
shell = root / 'shell'
shell.write_text('#!/bin/sh\nexport ZDOTDIR="${KASATERM_TEST_SHELL_RC:?}"\nexec /bin/zsh -d -i\n')
shell.chmod(0o700)


def wait_for(predicate, timeout=30):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = predicate()
        if value:
            return value
        time.sleep(.2)
    raise TimeoutError('fixture condition did not arrive')


class App:
    def __init__(self, folder, label, extra=None):
        self.folder = folder
        folder.mkdir()
        self.sock = folder / 'app.sock'
        assert len(str(self.sock).encode()) < 104
        (folder / 'settings.json').write_text(json.dumps({'lid_away': False, 'sidebar_body': 'map'}))
        env = {**base_env, 'TMPDIR': str(folder), 'NO_PROXY': '127.0.0.1,localhost',
               'KASATERM_SESSION_FILE': str(folder / 'session.json'), 'KASATERM_SETTINGS_FILE': str(folder / 'settings.json'),
               'KASATERM_WINDOW_FILE': str(folder / 'window.json'), 'KASATERM_STUDENTS_DIR': str(folder / 'students'),
               'KASATERM_PET_DIR': str(folder / 'pet'), 'KASATERM_DEVICE_FILE': str(folder / 'device.json'),
               'KASATERM_COLLAB_ROOT': str(folder / 'collab'), 'KASATERM_KASANET_KEY': str(folder / 'kasanet.key'),
               'KASATERM_KASANET': 'off', 'KASATERM_MACHINES': '[]', 'KASATERM_AUTORESTORE': 'fresh',
               'KASATERM_MACHINE_ID': uuid.uuid4().hex, 'KASATERM_SELF_LABEL': label,
               'KASATERM_SOCKET_PATH': str(self.sock), 'KASATERM_NOTIFICATION_PROBE': '1',
               'KASATERM_AUTOQUIT_MS': '120000', 'KASATERM_WINDOW_SIZE': '1000,700',
               'KASATERM_AUTODECKTIP': '1',
               'KASATERM_WINDOW_POS': '40,100', 'KASATERM_NO_FOCUS': '1', 'KASATERM_NO_AUTOSUGGEST': '1',
               'KASATERM_SHELL': str(shell), 'KASATERM_TEST_SHELL_RC': str(rc), **(extra or {})}
        self.log = (folder / 'launcher.log').open('w')
        self.proc = subprocess.Popen([str(binary)], cwd=folder, env=env, stdout=self.log, stderr=subprocess.STDOUT)
        (folder / 'pid').write_text(str(self.proc.pid))
        self.base = None

    def ready(self):
        def port():
            if self.proc.poll() is not None:
                raise RuntimeError(f'app exited: {self.folder}')
            log = self.folder / 'kasaterm-app.log'
            matches = re.findall(r'HTTP on 127\.0\.0\.1:(\d+)', log.read_text(errors='replace') if log.exists() else '')
            return matches[-1] if matches and self.sock.exists() else None
        number = wait_for(port)
        assert number != '8765', 'fixture must not use the user app port'
        self.base = 'http://127.0.0.1:' + number
        wait_for(lambda: self.panes())
        return self

    def http(self, path, payload=None):
        data = None if payload is None else json.dumps(payload).encode()
        request = urllib.request.Request(self.base + path, data=data, headers={'Content-Type': 'application/json'})
        with opener.open(request, timeout=15) as response:
            return json.load(response)

    def rpc(self, method, params=None):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(25)
            connection.connect(str(self.sock))
            connection.sendall((json.dumps({'id': 'fixture', 'method': method, 'params': params or {}}) + '\n').encode())
            response = json.loads(connection.makefile('rb').readline())
        assert response.get('ok'), response
        return response.get('result')

    def panes(self):
        rows = self.http('/term/panes')
        return [r for r in rows if not r.get('closed') and not r.get('detached')]

    def close(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        self.log.close()


cases = [
    ('idle-pane', 'pane', None, False, False),
    ('user-shell', 'pane', None, False, False),
    ('idle-tab', 'tab', None, False, False),
    ('idle-sidebar', 'remote-pane', None, False, False),
    ('idle-room', 'session', None, False, False),
    ('busy-cancel', 'pane', 'sleep 120', False, False),
    ('busy-confirm', 'pane', 'sleep 120', False, True),
    ('became-busy', 'pane', 'sleep 120', False, False),
    ('sibling-busy', 'pane', 'sleep 120', True, False),
    ('first-tab', 'tab', 'sleep 120', True, False),
    ('builtin-busy', 'pane', 'while :; do :; done', False, False),
]
reports = []
for name, kind, command, sibling, accept in cases:
    if name == 'user-shell' and not args.case:
        continue
    if args.case and name not in args.case:
        continue
    folder = root / name
    folder.mkdir()
    source = App(folder / 'src', 'fixture-source', {'KASATERM_SHELL': '/bin/zsh'} if name == 'user-shell' else None)
    viewer = None
    try:
        source.ready()
        pane = source.panes()[0]['id']
        if name == 'idle-room':
            reply = source.http('/spawn-shell?window=new', {})
            assert reply['ok'], reply
            pane = reply['surface']
        protected = None
        if sibling:
            protected = source.rpc('surface.new_tab', {'outer': pane, 'focus': False})['surface']['id']
        marker = folder / 'job-started'
        def start_job():
            target = protected or pane
            source.rpc('surface.send_text', {'surface_id': target, 'text': 'print ready > ' + shlex.quote(str(marker)) + '; ' + command + '\r'})
            wait_for(marker.exists)
        if command and name != 'became-busy':
            start_job()
        time.sleep(1)
        before = {p['id']: p for p in source.panes()}
        board = source.rpc('collab.snapshot', {'scope': 'local'})
        address = next(row['address'] for row in board['panes'] if row['address']['surface_id'] == pane)
        stale = {**address, 'instance_id': 'stale-fixture-instance'}
        refused = source.http('/cmd', {'method': 'surface.close_idle_checked', 'params': {'kind': 'tab', 'addresses': [stale]}})
        assert refused.get('ok') is False and pane in {p['id'] for p in source.panes()}, refused
        sheets = folder / 'sheets'
        sheets.mkdir()
        extra = {'KASATERM_MACHINES': json.dumps([{'label': 'fixture-source', 'base': source.base}]),
                 'KASATERM_TEST_BOARD_REMOTE': '1', 'KASATERM_TEST_MIRROR_LABEL': 'fixture-source',
                 'KASATERM_TEST_MIRROR_PANE': pane, 'KASATERM_TEST_MIRROR_ROOM': '1',
                 'KASATERM_TEST_MIRROR_CLOSE_MS': '8000', 'KASATERM_TEST_MIRROR_CLOSE_KIND': kind,
                 'KASATERM_UPDATE_RIG_SHOTS': str(sheets), 'KASATERM_UPDATE_RIG_PRESS_MS': '1500',
                 'KASATERM_AUTOCAPTURE_MS': '16000', 'KASATERM_AUTOCAPTURE_PATH': str(folder / 'after.png')}
        if accept:
            extra['KASATERM_UPDATE_RIG_BUTTON'] = '닫기'
        viewer = App(folder / 'view', 'fixture-viewer', extra)
        viewer.ready()
        deadline = time.monotonic() + 22
        snapshots = []
        while time.monotonic() < deadline:
            if name == 'became-busy' and not marker.exists() and time.monotonic() > deadline - 18:
                start_job()
            snapshots.append({'source': source.panes(), 'viewer': viewer.panes()})
            time.sleep(.3)
        after = {p['id']: p for p in source.panes()}
        (folder / 'machines.json').write_text(json.dumps(viewer.http('/machines'), indent=2))
        log = (viewer.folder / 'kasaterm-app.log').read_text(errors='replace')
        should_confirm = bool(command) and name != 'first-tab'
        should_close = not should_confirm or accept
        checks = {'stale_address_rejected': refused.get('ok') is False,
                  'requested': '[mirror-view-test] close=' in log,
                  'source_outcome': (pane not in after) == should_close,
                  'confirmation_only_for_jobs': bool(list(sheets.glob('*.png'))) == should_confirm,
                  'captured': (folder / 'after.png').exists()}
        if protected:
            checks['sibling_preserved'] = protected in after and after[protected]['surface_key'] == before[protected]['surface_key']
        if should_close:
            checks['viewer_source_removed'] = not any(p.get('mirror_of') and p.get('surface_key') == before[pane]['surface_key'] for p in viewer.panes())
        (folder / 'snapshots.json').write_text(json.dumps(snapshots, indent=2))
        report = {'case': name, 'checks': checks}
        reports.append(report)
        (folder / 'report.json').write_text(json.dumps(report, indent=2))
        print(json.dumps(report), flush=True)
        assert all(checks.values()), report
    finally:
        if viewer:
            viewer.close()
        source.close()
(root / 'success.json').write_text(json.dumps(reports, indent=2))
print('ALL REMOTE SHELL CLOSE CASES PASSED', root, flush=True)
