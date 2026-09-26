"""명령·HTTP 를 한 자리로 — 무엇을 실제로 하고 무엇을 「할 것」으로만 적을지 여기서 가른다.

명령마다 종류를 단다:
- read    읽기(원격 태그·CI 상태·피드·서명 신원). 어느 모드에서든 돈다.
- local   이 기기 안에서만 남는 일(격리 워크트리·검사·굽기). `dry` 에서는 적기만 한다.
- publish 밖으로 나가는 일(태그 push). `live` 에서만 돈다.

검사는 이 둘을 가짜로 바꿔 끼워 실제 명령 구성·시간 초과·부분 실패를 본다.
"""

import json
import subprocess
import urllib.error
import urllib.request

MODES = ("dry", "local", "live")
KINDS = ("read", "local", "publish")


class Result:
    def __init__(self, code, out="", err="", timed_out=False, skipped=False):
        self.code, self.out, self.err, self.timed_out, self.skipped = code, out, err, timed_out, skipped

    @property
    def ok(self):
        return self.code == 0 and not self.timed_out

    def tail(self, n=6):
        text = (self.err or "") + ("\n" if self.err and self.out else "") + (self.out or "")
        return "\n".join(text.strip().splitlines()[-n:])


def runs(mode, kind):
    return kind == "read" or (kind == "local" and mode in ("local", "live")) or (kind == "publish" and mode == "live")


class Runner:
    """실제 subprocess. `calls` 에 돈 것과 건너뛴 것을 다 남긴다 — 상태 파일·dry-run 이 이것을 보인다."""

    def __init__(self, mode="dry"):
        assert mode in MODES
        self.mode = mode
        self.calls = []

    def run(self, argv, cwd=None, timeout=60, env=None, kind="read"):
        assert kind in KINDS
        go = runs(self.mode, kind)
        self.calls.append({"argv": list(argv), "cwd": str(cwd) if cwd else None, "kind": kind, "ran": go})
        if not go:
            return Result(0, skipped=True)
        return self.execute(argv, cwd, timeout, env)

    def execute(self, argv, cwd, timeout, env):
        try:
            p = subprocess.run(argv, cwd=cwd, env=env, timeout=timeout, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        except subprocess.TimeoutExpired as e:
            return Result(None, (e.stdout or b"").decode(errors="replace"), (e.stderr or b"").decode(errors="replace"),
                          timed_out=True)
        except FileNotFoundError as e:
            return Result(127, "", str(e))
        return Result(p.returncode, p.stdout.decode(errors="replace"), p.stderr.decode(errors="replace"))


class Http:
    """(상태, 본문 bytes). 연결 실패는 상태 0 — 부르는 쪽이 「못 닿음」과 「거절」을 가른다."""

    def request(self, method, url, headers=None, body=None, timeout=10):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method, headers=dict(headers or {}))
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return r.status, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.read()
        except (urllib.error.URLError, OSError, ValueError):
            return 0, b""

    def get(self, url, headers=None, timeout=10):
        return self.request("GET", url, headers, None, timeout)
