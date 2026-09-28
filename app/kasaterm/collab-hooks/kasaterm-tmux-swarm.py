#!/usr/bin/env python3
"""Claude Code 팀원 창을 숨은 tmux 서버 대신 kasaterm pane 에 세운다.

teammateMode=tmux 인 Claude Code 는 tmux 밖이면 `tmux -L claude-swarm-<pid>` 로 자기 전용 서버를
띄워 팀원을 거기 숨긴다 — 사람은 `tmux -L … a` 로 따로 붙어야 보인다(2026-09-28). pane PATH 의
`tmux` 셰임이 그 소켓 호출만 여기로 보내고, 여기서 kasaterm-cli 로 옮긴다. 팀원 pane id 는
kasaterm 의 `%N` 그대로다 — Claude 는 받은 id 를 되돌려 줄 뿐이라 번역표가 필요 없다.

명령 표면은 Claude Code 2.1.283 의 TmuxBackend(외부 세션 경로)를 바이너리에서 읽은 것이다.
모르는 명령은 실패로 돌려 Claude 쪽 오류로 보이게 한다 — 조용히 0 을 주면 팀원이 빈 pane 에 묶인다.
"""
import fcntl
import json
import os
import shlex
import subprocess
import sys
import tempfile
import time

WINDOW = "swarm-view"
# 값을 받는 tmux 플래그. 나머지 한 글자 플래그는 켜기/끄기다.
VALUED = set("tFnsclTxye")
# 쪼갠 pane 에 팀원 명령이 들어가기까지. Claude 는 쪼개고 곧바로 명령을 넣는다.
SPAWN_GRACE = 120


def parse(args):
    flags, pos, cmd = {}, [], []
    i = 0
    while i < len(args):
        a = args[i]
        if a == "--":
            cmd = args[i + 1:]
            break
        if a.startswith("-") and len(a) > 1 and not pos:
            for j, c in enumerate(a[1:], 1):
                if c in VALUED:
                    rest = a[j + 1:]
                    if not rest:
                        i += 1
                        rest = args[i] if i < len(args) else ""
                    flags[c] = rest
                    break
                flags[c] = True
        else:
            pos.append(a)
        i += 1
    return flags, pos, cmd


def cli_path():
    # 셰임 디렉터리의 CLI 를 먼저 — 검증용 앱과 본판이 섞여 있어도 이 pane 의 앱으로 간다.
    shim = os.environ.get("KASATERM_TMUX_SHIM_DIR", "")
    path = os.path.join(shim, "kasaterm-cli")
    return path if shim and os.access(path, os.X_OK) else "kasaterm-cli"


def cli(*args):
    try:
        out = subprocess.run([cli_path(), *args], capture_output=True, text=True, timeout=20)
    except (OSError, subprocess.TimeoutExpired) as e:
        return None, str(e)
    lines = out.stdout.strip().splitlines()
    try:
        resp = json.loads(lines[-1])
    except (ValueError, IndexError):
        return None, (out.stderr or out.stdout).strip() or f"kasaterm-cli {args[0]} 실패"
    if not resp.get("ok"):
        err = resp.get("error")
        return None, err.get("message", "") if isinstance(err, dict) else str(err)
    return resp.get("result") or {}, ""


class Swarm:
    """한 리더(소켓 이름)의 팀원 pane 목록. 앱 한 번의 셰임 디렉터리 안에 둔다 — 앱을 다시
    켜면 pane 번호가 처음부터 다시 매겨지므로, 그 전 목록이 남의 pane 을 가리키면 안 된다."""

    def __init__(self, socket_name):
        root = os.environ.get("KASATERM_TMUX_SHIM_DIR") or tempfile.gettempdir()
        self.dir = os.path.join(root, "swarm")
        os.makedirs(self.dir, mode=0o700, exist_ok=True)
        self.base = os.path.join(self.dir, socket_name)
        self.path = self.base + ".json"
        self.lock = open(self.base + ".lock", "w")
        fcntl.flock(self.lock, fcntl.LOCK_EX)
        try:
            with open(self.path) as f:
                self.panes = json.load(f).get("panes", [])
        except (OSError, ValueError):
            self.panes = []

    def save(self):
        tmp = self.path + ".tmp"
        with open(tmp, "w") as f:
            json.dump({"panes": self.panes}, f)
        os.replace(tmp, self.path)

    def shell_file(self, pane):
        return f"{self.base}.{pane.lstrip('%')}.shell"

    def mine(self, p, now):
        """닫힌 pane 번호는 다음 pane 에 다시 붙는다(실측) — 번호만 보고 닫거나 명령을 치면
        남의 pane 이 당한다. 팀원 스크립트가 적어 둔 그 pane 셸이 살아 있을 때만 우리 것이다.
        셸을 적기 전(쪼갠 직후 명령을 넣기까지)은 잠깐이라 갓 만든 것만 믿는다."""
        if p["id"] not in now:
            return False
        try:
            with open(self.shell_file(p["id"])) as f:
                shell = int(f.read().strip())
        except (OSError, ValueError):
            return time.time() - p.get("created", 0) < SPAWN_GRACE
        try:
            os.kill(shell, 0)
        except ProcessLookupError:
            return False
        except PermissionError:
            pass
        return True

    def live(self):
        res, _ = cli("list", "surfaces")
        if res is None:
            return list(self.panes)
        now = {s.get("id") for s in res.get("surfaces", [])}
        keep = [p for p in self.panes if self.mine(p, now)]
        for p in self.panes:
            if p not in keep:
                try:
                    os.unlink(self.shell_file(p["id"]))
                except OSError:
                    pass
        self.panes = keep
        self.save()
        return self.panes

    def ids(self):
        return [p["id"] for p in self.live()]

    def owns(self, pane):
        return pane in self.ids()

    def add(self, surface):
        pane = surface["id"]
        try:
            os.unlink(self.shell_file(pane))
        except OSError:
            pass
        self.panes.append({"id": pane, "created": time.time()})
        self.save()
        return pane


def fail(msg, code=1):
    print(f"kasaterm tmux: {msg}", file=sys.stderr)
    return code


def new_pane(swarm, beside, direction):
    res, err = cli("split", direction, beside)
    if res is None:
        # 창이 좁아 못 쪼개면 그 pane 의 탭으로 — 팀원을 못 띄우는 것보다 한 번 더 누르는 게 낫다.
        res, tab_err = cli("tab", beside)
        if res is None:
            return None, f"{err} / {tab_err}"
    surface = res.get("surface") or {}
    if not surface.get("id"):
        return None, f"새 pane id 를 못 받았다: {json.dumps(res, ensure_ascii=False)}"
    return swarm.add(surface), ""


def print_format(flags, pane):
    if flags.get("P"):
        print(str(flags.get("F") or "#{pane_id}").replace("#{pane_id}", pane))


def respawn(swarm, pane, command):
    """tmux 는 pane 의 프로세스를 갈아 끼우지만 kasaterm pane 에는 셸이 있으니 그 셸에 실행을 시킨다.
    명령을 입력창에 직접 치지 않는다 — Claude 가 붙이는 `env …` 에 API 키가 실려 올 수 있고,
    쳐 넣으면 셸 기록과 화면에 남는다. 스스로 지워지는 파일로 넘긴다."""
    fd, path = tempfile.mkstemp(prefix="teammate-", suffix=".sh", dir=swarm.dir)
    with os.fdopen(fd, "w") as f:
        f.write(f'rm -f -- "$0"\necho "$PPID" > {shlex.quote(swarm.shell_file(pane))}\n{command}\n')
    # 정상 종료면 셸도 닫아 pane 이 걷히게 하고, 실패면 오류를 볼 수 있게 셸을 남긴다(tmux 의
    # remain-on-exit failed 와 같은 뜻).
    _, err = cli("send", "--surface", pane, f" sh {shlex.quote(path)} && exit\n")
    if err:
        os.unlink(path)
        return fail(f"{pane} 에 팀원 명령을 못 보냈다: {err}")
    return 0


def kill(swarm, pane):
    cli("close", pane)
    # 닫힌 pane 은 되살리기 목록에 산 채로 남는다 — 팀원은 여기서 끝이어야 한다.
    cli("closed", pane)
    swarm.panes = [p for p in swarm.panes if p["id"] != pane]
    swarm.save()
    try:
        os.unlink(swarm.shell_file(pane))
    except OSError:
        pass


def run(argv):
    if len(argv) < 3 or argv[0] != "-L":
        return fail("claude-swarm 소켓 호출만 받는다")
    swarm = Swarm(argv[1])
    cmd, rest = argv[2], argv[3:]
    flags, pos, tail = parse(rest)
    target = flags.get("t") or ""

    if cmd == "has-session":
        return 0 if swarm.ids() else 1

    if cmd in ("new-session", "new-window"):
        leader = os.environ.get("KASATERM_PANE_ID", "")
        if not leader:
            return fail("KASATERM_PANE_ID 가 없다 — kasaterm pane 밖에서 불렀다")
        pane, err = new_pane(swarm, leader, "auto")
        if not pane:
            return fail(err)
        print_format(flags, pane)
        return 0

    if cmd == "list-windows":
        if not swarm.ids():
            return fail("no server running", 1)
        print(WINDOW)
        return 0

    if cmd == "list-panes":
        ids = swarm.ids()
        if not ids:
            return fail(f"can't find window: {target}")
        print("\n".join(ids))
        return 0

    if cmd == "split-window":
        if not swarm.owns(target):
            return fail(f"{target} 는 이 팀의 pane 이 아니다")
        pane, err = new_pane(swarm, target, "down" if flags.get("v") else "right")
        if not pane:
            return fail(err)
        print_format(flags, pane)
        return 0

    if cmd == "respawn-pane":
        if not swarm.owns(target):
            return fail(f"{target} 는 이 팀의 pane 이 아니다")
        if not tail:
            return fail("respawn-pane 에 실행할 명령이 없다")
        return respawn(swarm, target, " ".join(tail))

    if cmd == "select-pane":
        title = flags.get("T")
        if title and swarm.owns(target):
            cli("rename", target, title)
        return 0

    if cmd == "kill-pane":
        if not swarm.owns(target):
            return fail(f"can't find pane: {target}")
        kill(swarm, target)
        return 0

    if cmd in ("kill-session", "kill-server"):
        for pane in swarm.ids():
            kill(swarm, pane)
        return 0

    if cmd in ("attach", "attach-session", "a"):
        ids = swarm.ids()
        if not ids:
            return fail("이 리더가 띄운 팀원이 없다")
        cli("focus", ids[0])
        print(f"팀원은 kasaterm pane 에 있다: {' '.join(ids)}")
        return 0

    # 테두리 색·제목 줄·배치는 tmux 의 그림이다. kasaterm 은 pane 이름으로 팀원을 가른다.
    if cmd in ("set-option", "select-layout", "resize-pane"):
        return 0

    return fail(f"{cmd} 는 아직 kasaterm 으로 옮기지 않았다 ({' '.join(argv)})")


if __name__ == "__main__":
    sys.exit(run(sys.argv[1:]))
