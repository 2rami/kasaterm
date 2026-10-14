#!/usr/bin/env python3
"""Record a program's PTY output as a .kcast corpus file for the differential test.

    python3 tools/record.py --out corpus/vim.kcast.gz --size 100x30 --script steps.json -- vim file

Each read() of the PTY master becomes one output event, like kasaterm's pane reader, and window
size changes are recorded where they happen. Terminal queries are answered by kasaterm.js
(tools/responder.mjs) so the program runs as it would inside kasaterm.

The child gets a clean environment (`env -i` style): nothing from the recording pane — kasaterm
pane ids, sockets, tokens — leaks into the program or into the recording.

Script steps (JSON list): {"wait": ms} · {"send": "text"} · {"type": "text", "gap": ms} ·
{"resize": [cols, rows]} · {"expect": "regex", "timeout": ms}
"""
import argparse
import base64
import fcntl
import gzip
import json
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time

HERE = os.path.dirname(os.path.abspath(__file__))
CLEAN_PATH = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"


def set_size(fd, cols, rows):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


class Responder:
    def __init__(self):
        self.proc = subprocess.Popen(["node", os.path.join(HERE, "responder.mjs")],
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE)

    def _frame(self, payload):
        self.proc.stdin.write(payload)
        self.proc.stdin.flush()
        head = self.proc.stdout.read(4)
        n = struct.unpack("<I", head)[0]
        return self.proc.stdout.read(n) if n else b""

    def resize(self, cols, rows):
        return self._frame(b"r" + struct.pack("<HH", cols, rows))

    def output(self, data):
        return self._frame(b"o" + struct.pack("<I", len(data)) + data)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--size", default="100x30")
    ap.add_argument("--script", help="JSON steps file")
    ap.add_argument("--cwd", default=None)
    ap.add_argument("--env", action="append", default=[], help="extra KEY=VALUE for the child")
    ap.add_argument("--title", default="")
    ap.add_argument("--max-seconds", type=float, default=120)
    ap.add_argument("cmd", nargs=argparse.REMAINDER)
    args = ap.parse_args()
    cmd = args.cmd[1:] if args.cmd and args.cmd[0] == "--" else args.cmd
    cols, rows = (int(v) for v in args.size.split("x"))
    steps = json.load(open(args.script)) if args.script else []

    env = {
        "HOME": os.environ["HOME"], "USER": os.environ.get("USER", ""), "LOGNAME": os.environ.get("USER", ""),
        "PATH": CLEAN_PATH, "TERM": "xterm-256color", "COLORTERM": "truecolor", "LANG": "ko_KR.UTF-8",
        "LC_ALL": "ko_KR.UTF-8", "SHELL": "/bin/zsh", "TMPDIR": "/tmp/",
    }
    for kv in args.env:
        k, v = kv.split("=", 1)
        env[k] = v

    responder = Responder()
    responder.resize(cols, rows)
    pid, fd = pty.fork()
    if pid == 0:
        if args.cwd:
            os.chdir(args.cwd)
        os.execvpe(cmd[0], cmd, env)
    set_size(fd, cols, rows)

    start = time.monotonic()
    events = []
    seen = b""

    def now():
        return round(time.monotonic() - start, 4)

    def pump(timeout):
        nonlocal seen
        r, _, _ = select.select([fd], [], [], timeout)
        if not r:
            return True
        try:
            data = os.read(fd, 65536)
        except OSError:
            return False
        if not data:
            return False
        events.append([now(), "o", base64.b64encode(data).decode()])
        seen = (seen + data)[-200000:]
        answer = responder.output(data)
        if answer:
            os.write(fd, answer)
        return True

    def run_for(ms):
        end = time.monotonic() + ms / 1000
        while time.monotonic() < end:
            if not pump(min(0.05, max(0, end - time.monotonic()))):
                return False
        return True

    alive = True
    for step in steps:
        if not alive:
            break
        if "wait" in step:
            alive = run_for(step["wait"])
        elif "send" in step:
            os.write(fd, step["send"].encode())
            alive = run_for(step.get("after", 50))
        elif "type" in step:
            for ch in step["type"]:
                os.write(fd, ch.encode())
                alive = run_for(step.get("gap", 30))
        elif "resize" in step:
            c, r = step["resize"]
            set_size(fd, c, r)
            responder.resize(c, r)
            events.append([now(), "r", c, r])
            os.kill(pid, signal.SIGWINCH)
            alive = run_for(step.get("after", 300))
        elif "expect" in step:
            pat = re.compile(step["expect"].encode())
            mark = len(seen)
            deadline = time.monotonic() + step.get("timeout", 10000) / 1000
            while alive and time.monotonic() < deadline and not pat.search(seen[max(0, mark - 4096):]):
                alive = pump(0.05)
            if not pat.search(seen[max(0, mark - 4096):]):
                print(f"expect timed out: {step['expect']!r}", file=sys.stderr)
    deadline = start + args.max_seconds
    while alive and time.monotonic() < deadline:
        alive = pump(0.2)
        try:
            if os.waitpid(pid, os.WNOHANG)[0]:
                while pump(0.1):
                    pass
                break
        except ChildProcessError:
            break
    try:
        os.kill(pid, signal.SIGHUP)
    except ProcessLookupError:
        pass
    responder.proc.kill()

    header = {"version": 1, "cols": cols, "rows": rows, "title": args.title, "command": " ".join(cmd)}
    opener = gzip.open if args.out.endswith(".gz") else open
    with opener(args.out, "wt", encoding="utf-8") as f:
        f.write(json.dumps(header, ensure_ascii=False) + "\n")
        for e in events:
            f.write(json.dumps(e) + "\n")
    total = sum(len(base64.b64decode(e[2])) for e in events if e[1] == "o")
    print(f"{args.out}: {len(events)} events, {total} bytes", file=sys.stderr)


if __name__ == "__main__":
    main()
