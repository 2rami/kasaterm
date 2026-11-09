import copy
import http.server
import json
import os
import pathlib
import re
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import urllib.request
import uuid

if sys.platform != 'darwin':
    raise SystemExit('This fixture exercises the native macOS event path')

repo = pathlib.Path(__file__).resolve().parents[5]
binary = pathlib.Path(os.environ.get('CARGO_TARGET_DIR', str(repo / 'target'))) / 'debug/kasaterm'
root = pathlib.Path(tempfile.mkdtemp(prefix='kt-shared-', dir='/tmp'))
print('artifacts:', root, flush=True)
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
raw = ''.join(json.dumps({'type': role, 'message': {'content': text}}, ensure_ascii=False) + '\n'
              for role, text in [('user', '공유방 읽기 시험'), ('assistant', '원본 작업은 계속되고 이 화면은 읽기만 해요.')])
session = dict(id='s1', source_key='', title='함께 보는 대화', harness='claude', publisher_machine_id='',
               revision=1, generation='g1', bytes=len(raw.encode()), cursor=0, updated_at_ms=1,
               caught_up=True, checkpoint=None)
room = dict(id='r1', title='함께 쓰는 작업방', owner_account='alice', owner_name='Alice', my_role='owner',
            allow_fork=True, revision=1, updated_at_ms=1,
            members=[dict(account='alice', display_name='Alice', role='owner')], sessions=[session], invites=[])
requests = []
snapshot = json.loads(pathlib.Path(__file__).with_name('claude_snapshot.json').read_text())
fork_id = '0123456789abcdef0123456789abcdef'
shared_fork = dict(id=fork_id, room_id='r1', session_id='s1', checkpoint_id='cp1', created_at_ms=1,
                   checkpoint=dict(format_version=1, native=snapshot, captured_at_ms=1, display_raw=raw, project=None, omissions=[]))


class Relay(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        requests.append(('GET', self.path))
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path == '/relay/shared-rooms':
            value = dict(ok=True, rooms=[copy.deepcopy(room)])
        elif parsed.path.endswith('/content'):
            query = urllib.parse.parse_qs(parsed.query)
            start = int(query['offset'][0])
            end = min(int(query['end'][0]), len(raw.encode()))
            value = dict(ok=True, content=dict(generation=session['generation'], revision=session['revision'],
                         offset=start, next=end, end=end, raw=raw.encode()[start:end].decode()))
        elif parsed.path == '/relay/shared-rooms/r1':
            value = dict(ok=True, room=copy.deepcopy(room))
        elif parsed.path == '/relay/shared-forks/' + fork_id:
            value = dict(ok=True, fork=shared_fork)
        else:
            self.send_error(404)
            return
        body = json.dumps(value, ensure_ascii=False).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        self.wfile.write(body)


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Relay)
threading.Thread(target=server.serve_forever, daemon=True).start()
base_env = {k: v for k, v in os.environ.items() if not k.startswith(('KASATERM_', 'CMUX_', 'CLAUDE_', 'CODEX_'))}
base_env.pop('NO_COLOR', None)
rc = root / 'rc'
rc.mkdir()
(rc / '.zshrc').write_text("unset HISTFILE\nPROMPT=$'%{\\e]133;A\\a%}fixture> %{\\e]133;B\\a%}'\n")
shell = root / 'shell'
shell.write_text('#!/bin/sh\nexport ZDOTDIR="${KASATERM_TEST_SHELL_RC:?}"\nexec /bin/zsh -d -i\n')
shell.chmod(0o700)
program = root / 'claude'
source = root / 'harness.c'
source.write_text(r'''
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    const char *sid = NULL;
    for (int i = 1; i + 1 < argc; i++) if (!strcmp(argv[i], "--session-id")) sid = argv[i + 1];
    if (!sid) return 2;
    FILE *f = fopen(getenv("KASATERM_TEST_FORK_STARTED"), "a");
    if (!f) return 3;
    fprintf(f, "{\"sid\":\"%s\",\"pid\":%d,\"surface\":\"%s\"}\n", sid, getpid(), getenv("KASATERM_PANE_ID"));
    fclose(f);
    printf("\033[?2004h\033[2J\033[HClaude Code fixture\r\n\r\n────────────────────────────\r\n❯ \r\n────────────────────────────\033[4;3H");
    fflush(stdout);
    for (;;) pause();
}
''')
subprocess.run(['cc', str(source), '-o', str(program)], check=True, capture_output=True)


class App:
    def __init__(self, name, size, theme):
        self.dir = root / name
        self.dir.mkdir()
        self.sock = self.dir / 'app.sock'
        self.id = 0
        self.base = None
        (self.dir / 'settings.json').write_text(json.dumps({'lid_away': False, 'sidebar_body': 'map', 'theme': theme}))
        credential = dict(relay=f'http://127.0.0.1:{server.server_port}', account='alice', device_id='fixture-desktop', token='fixture-only')
        (self.dir / 'credential.json').write_text(json.dumps(credential))
        env = dict(base_env, TMPDIR=str(self.dir), NO_PROXY='127.0.0.1,localhost',
                   KASATERM_SESSION_FILE=str(self.dir / 'session.json'), KASATERM_SETTINGS_FILE=str(self.dir / 'settings.json'),
                   KASATERM_WINDOW_FILE=str(self.dir / 'window.json'), KASATERM_STUDENTS_DIR=str(self.dir / 'students'),
                   KASATERM_PET_DIR=str(self.dir / 'pet'), KASATERM_DEVICE_FILE=str(self.dir / 'device.json'),
                   KASATERM_COLLAB_ROOT=str(self.dir / 'collab'), KASATERM_KASANET_KEY=str(self.dir / 'kasanet.key'),
                   CLAUDE_CONFIG_DIR=str(self.dir / 'collab/native-claude'),
                   KASATERM_KASANET='off', KASATERM_MACHINES='[]', KASATERM_AUTORESTORE='fresh',
                   KASATERM_SOCKET_PATH=str(self.sock), KASATERM_AUTOQUIT_MS='180000', KASATERM_WINDOW_SIZE=size,
                   KASATERM_AUTODECKTIP='1',
                   KASATERM_NO_FOCUS='1', KASATERM_NO_AUTOSUGGEST='1', KASATERM_NOTIFICATION_PROBE='1',
                   KASATERM_TEST_COPY_PATH=str(self.dir / 'clipboard.txt'), KASATERM_TEST_PASTE_TEXT='READ_ONLY_PASTE',
                   KASATERM_MACHINE_ID=uuid.uuid4().hex, KASATERM_SELF_LABEL='Shared room fixture',
                   KASATERM_SHELL=str(shell), KASATERM_TEST_SHELL_RC=str(rc),
                   KASATERM_SHARED_FIXTURE_CLAUDE=str(program), KASATERM_TEST_FORK_STARTED=str(self.dir / 'launches.jsonl'),
                   KASATERM_TEST_SHARED_READY_MS='1800',
                   KASATERM_SHARED_FIXTURE_CREDENTIAL=str(self.dir / 'credential.json'), KASATERM_TEST_SHARED_DIR=str(self.dir))
        self.log = (self.dir / 'launcher.log').open('w')
        self.proc = subprocess.Popen([str(binary)], cwd=self.dir, env=env, stdout=self.log, stderr=subprocess.STDOUT)
        (self.dir / 'pid').write_text(str(self.proc.pid))
        try:
            self.wait(lambda r: r.get('nav_hits'), timeout=60)
        except BaseException:
            self.close()
            raise

    def wake(self):
        log = self.dir / 'kasaterm-app.log'
        if self.base is None and log.exists():
            ports = re.findall(r'HTTP on 127\.0\.0\.1:(\d+)', log.read_text(errors='replace'))
            if ports:
                assert ports[-1] != '8765'
                self.base = 'http://127.0.0.1:' + ports[-1]
        if self.base:
            try:
                with opener.open(self.base + '/shared-rooms/status', timeout=6) as response:
                    response.read()
            except (OSError, TimeoutError):
                pass

    def report(self):
        try:
            return json.loads((self.dir / 'report.json').read_text())
        except (OSError, ValueError):
            return {}

    def wait(self, predicate, timeout=30):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            assert self.proc.poll() is None, f'app exited: {self.dir}'
            self.wake()
            value = self.report()
            if predicate(value):
                return value
            time.sleep(.2)
        raise AssertionError(f'condition timed out: {self.report()}')

    def command(self, action, **fields):
        self.id += 1
        path = self.dir / 'command.tmp'
        path.write_text(json.dumps(dict(id=self.id, action=action, **fields)))
        path.replace(self.dir / 'command.json')
        return self.wait(lambda r: r.get('command') == self.id)

    def http(self, path, body=None):
        request = urllib.request.Request(self.base + path, data=None if body is None else json.dumps(body).encode(),
                                         headers={'Content-Type': 'application/json'})
        with opener.open(request, timeout=10) as response:
            return json.load(response)

    def bind_started_child(self):
        path = self.dir / 'launches.jsonl'
        self.wait(lambda _: path.exists(), timeout=40)
        launches = [json.loads(line) for line in path.read_text().splitlines()]
        assert len(launches) == 1, launches
        child = launches[0]
        self.wait(lambda r: any(op['status'] == 'failed' for op in r.get('operations', [])), timeout=12)
        query = urllib.parse.urlencode(dict(surface=child['surface'], sid=child['sid'], pid=child['pid']))
        self.http('/agent-identity?' + query, {})
        env = dict(base_env, KASATERM_SOCKET_PATH=str(self.sock), KASATERM_COLLAB_ROOT=str(self.dir / 'collab'))
        result = subprocess.run(['kasaterm-cli', 'report-cwd', child['surface'], str(self.dir), child['sid']],
                                env=env, capture_output=True, text=True, timeout=10)
        assert result.returncode == 0, result.stderr
        assert child['sid'] != snapshot['parent_session_id']
        self.wait(lambda r: r.get('bound_sessions', {}).get(child['surface']) == child['sid'])
        return child

    def click(self, name, nav=False):
        key = 'nav_hits' if nav else 'hits'
        r = self.wait(lambda r: any(h['action'] == name for h in r.get(key, [])))
        hit = next(h['rect'] for h in r[key] if h['action'] == name)
        self.command('click', x=hit[0] + hit[2] / 2, y=hit[1] + hit[3] / 2)

    def capture(self, name):
        env = dict(base_env, KASATERM_SOCKET_PATH=str(self.sock))
        output = subprocess.run(['kasaterm-cli', 'capture', '--window', str(self.dir / (name + '.png'))],
                                env=env, capture_output=True, text=True, timeout=30)
        assert output.returncode == 0, output.stdout + output.stderr
        subprocess.run(['sips', '-s', 'format', 'jpeg', '-s', 'formatOptions', '60', '-Z', '1100',
                        str(self.dir / (name + '.png')), '--out', str(self.dir / (name + '.jpg'))], check=True, capture_output=True)

    def close(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=8)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        self.log.close()


reports = []
try:
    for name, size, theme in [('wide', '1100,760', 'light'), ('narrow', '650,420', 'dark')]:
        app = App(name, size, theme)
        try:
            before = app.wait(lambda r: any(h['action'] == 'open:r1' for h in r.get('nav_hits', [])))
            app.click('open:r1', nav=True)
            view = app.wait(lambda r: r.get('active') and r.get('reader_items') == 2)
            assert view['pty_count'] == before['pty_count']
            assert view['local_sidebar_count'] == before['local_sidebar_count']
            assert [n for n in view['rooms'] if n is not None] == before['rooms']
            app.capture('read')
            app.command('key', text='READ_ONLY_INPUT')
            app.command('key', text='v', flags=1 << 20)
            assert app.report()['pty_count'] == before['pty_count']
            assert app.report()['input_revisions'] == before['input_revisions']
            app.click('back')
            app.click('new')
            form = app.wait(lambda r: r.get('form_text') == '공유방')
            footer = [h['rect'] for h in form['hits'] if h['action'] in ('submit', 'cancel')]
            for body in [h['rect'] for h in form['hits'] if h['action'] in ('field', 'toggle-fork')]:
                assert all(body[1] + body[3] <= f[1] for f in footer), (body, footer)
            app.capture('form')
            app.command('key', text='\x1b')
            app.wait(lambda r: r.get('form_text') is None)
            app.click('open:r1')
            app.wait(lambda r: r.get('reader_items') == 2)
            app.command('key', text='w', flags=1 << 20)
            closed = app.wait(lambda r: not r.get('active'))
            assert closed['pty_count'] == before['pty_count']
            assert closed['rooms'] == before['rooms']
            if name == 'wide':
                request = dict(fork_id=fork_id, destination=dict(tab=True, include_project=False), request_id='same-request')
                operation = app.http('/shared-rooms/fork', request)
                assert operation['ok'], operation
                child = app.bind_started_child()
                app.http('/shared-rooms/fork', request)
                ready = app.wait(lambda r: any(op['status'] == 'ready' for op in r.get('operations', [])), timeout=40)
                assert ready['pty_count'] == before['pty_count'] + 1
                retry = app.http('/shared-rooms/fork', request)
                assert retry['operation_id'] == operation['operation_id']
                assert retry['child_session_id'] == child['sid']
                assert len((app.dir / 'launches.jsonl').read_text().splitlines()) == 1
                assert not list((app.dir / 'collab/native-claude/projects').rglob('*.jsonl'))
                app.capture('fork')
            reports.append(dict(case=name, result='passed', pty_count=closed['pty_count']))
        finally:
            app.close()
finally:
    server.shutdown()
    server.server_close()
    (root / 'results.json').write_text(json.dumps(dict(cases=reports, requests=requests), ensure_ascii=False, indent=2))
print(json.dumps(reports, ensure_ascii=False), flush=True)
