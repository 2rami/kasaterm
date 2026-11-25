"""검사 자리 — release 시험을 controller 와 ssh 로 닿는 맥 가운데 지금 여유가 큰 곳에서 돌린다.

미니(controller)는 메모리 16GB 에 나쵸·봇·미리보기가 얹혀 일꾼 하나로 70분 남짓 걸렸다(2026-11-25). 그래서 검사 때마다
기기마다 코어·부하·메모리 여유를 재서 일꾼을 더 많이 낼 수 있는 곳에 맡긴다. 맥북은 사람이 쓰는 기기라 코어 둘을 남기고,
전원에 꽂혀 있고 디스크가 넉넉할 때만, `nice 10` 으로 돌린다. 원격은 그 기기 전용 사본(`~/.local/share/kasaterm-verify`)과
이어 쓰는 target 에서 돌아 바뀐 크레이트만 다시 굽는다. 시험만 하고 결과물은 만들지 않으니 굽기·서명·공증은 그대로 미니다.

원격 검사는 띄워 두고 tick 마다 상태만 읽는다(Pending). 준비(clone·fetch·도구)가 실패하거나 닿지 않은 채 20분이 지나면
그 계획은 controller 에서 검사한다. 시험이 실패하면 한 번 더 돌리고, 또 실패하면 새 계획이 올 때까지 그 실패를 그대로 둔다.
"""

import base64
import json
import os
from pathlib import Path
import re
import time

from tools.release.common import Pending, Refused

# 이 맥북(미니 → 맥북 역터널)과 모묘모 맥북(서울 관문 역터널, 미니 열쇠는 127.0.0.1 에서만·포워딩 없이 허락).
HOSTS = ("macbook-hifumi", "homehub-wan")
HOSTS_ENV = "KASATERM_VERIFY_HOSTS"
REPO_URL = "https://github.com/2rami/kasaterm.git"
SSH = ("ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=8", "-o", "ServerAliveInterval=10", "-o", "ServerAliveCountMax=3")
# release 빌드의 큰 크레이트 rustc 하나가 2~3GB 를 먹는다. 미니 16GB 에서 일꾼을 늘렸다가 스왑에 빠진 적이 있어 넉넉히 잡는다.
GB_PER_JOB = 3
MAX_JOBS = 8
PERSONAL_RESERVE = 2
MIN_DISK_GB = 40
UNREACHABLE_GRACE_MS = 20 * 60 * 1000
REMOTE_ATTEMPTS = 2
RECORD = "verify-remote.json"
SAFE_ARG = re.compile(r"[A-Za-z0-9._+-]+\Z")

PROBE = r"""
root="$HOME/.local/share/kasaterm-verify"
echo "ncpu=$(sysctl -n hw.ncpu)"
echo "load=$(sysctl -n vm.loadavg | awk '{print $3}')"
echo "mem=$(sysctl -n hw.memsize)"
echo "free_pct=$(sysctl -n kern.memorystatus_level)"
echo "power=$(pmset -g batt 2>/dev/null | head -n 1)"
echo "disk_kb=$(df -k "$HOME" | awk 'NR==2{print $4}')"
"""

JOB = r"""
set -u
commit=$1 jobs=$2 toolchain=$3
shift 3
root="$HOME/.local/share/kasaterm-verify"
run="$root/run"
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
setup() { printf '%s\n' "$1" > "$run/why"; printf 'setup\n' > "$run/exit"; exit 0; }
git lfs version >/dev/null 2>&1 || setup "git-lfs 가 없다"
command -v rustup >/dev/null 2>&1 || setup "rustup 이 없다"
if [ ! -d "$root/repo/.git" ]; then
  rm -rf "${root:?}/repo"
  git clone -q --filter=blob:none --no-checkout "@REPO@" "$root/repo" || setup "clone 실패"
fi
cd "$root/repo" || setup "사본이 없다"
git lfs install --local >/dev/null 2>&1 || setup "git lfs install 실패"
git fetch -q --filter=blob:none origin "$commit" || setup "계획 커밋을 못 받았다"
# --no-checkout 사본의 첫 checkout 에서는 .lfsconfig 가 아직 작업 트리에 없어 GitHub LFS 로 간다 — 계획 커밋의 주소를 먼저 박는다.
git show "$commit:.lfsconfig" > "$run/lfsconfig" 2>/dev/null && url=$(git config -f "$run/lfsconfig" lfs.url) \
  && git config lfs.url "$url" || setup "LFS 주소를 못 읽었다"
git checkout -q -f --detach "$commit" || setup "checkout 실패"
git clean -ffdxq
[ "$(git rev-parse HEAD)" = "$commit" ] || setup "HEAD 가 계획 커밋이 아니다"
[ -z "$(git status --porcelain)" ] || setup "사본이 깨끗하지 않다"
# 설치는 되고 rustup 스스로 업데이트만 실패해도 1 로 끝난다(2026-11-25 맥북) — 끝난 뒤 목록으로 판정한다.
rustup toolchain list | grep -q "^$toolchain-" || rustup toolchain install --no-self-update --profile minimal "$toolchain" >/dev/null 2>&1
rustup toolchain list | grep -q "^$toolchain-" || setup "rust $toolchain 을 못 깔았다"
printf '%s\n' "$commit" > "$run/head"
date +%s > "$run/test_started"
CARGO_TARGET_DIR="$root/target" CARGO_BUILD_JOBS="$jobs" KASATERM_SETTINGS_FILE="$run/settings.json" \
  nice -n 10 cargo "+$toolchain" test "$@" --release > "$run/test.log" 2>&1
printf '%s\n' "$?" > "$run/exit"
"""

# 한 기기에 검사 자리는 하나다. 다른 계획의 검사가 남아 있으면 그 계획은 이미 버려진 것이라 멈추고 새로 띄운다.
START = r"""
set -u
plan=$1 commit=$2 jobs=$3 toolchain=$4
shift 4
root="$HOME/.local/share/kasaterm-verify"
run="$root/run"
mkdir -p "$root"
pid=$(cat "$run/pid" 2>/dev/null || true)
if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
  if [ "$(cat "$run/plan" 2>/dev/null)" = "$plan" ] && [ "$(cat "$run/commit" 2>/dev/null)" = "$commit" ]; then
    echo "state=running"; exit 0
  fi
  kill -TERM -- "-$pid" 2>/dev/null
  i=0; while kill -0 "$pid" 2>/dev/null && [ $i -lt 20 ]; do sleep 1; i=$((i+1)); done
  kill -KILL -- "-$pid" 2>/dev/null
fi
rm -rf "${root:?}/run"
mkdir -p "$run"
printf '%s\n' "$plan" > "$run/plan"
printf '%s\n' "$commit" > "$run/commit"
printf '{}\n' > "$run/settings.json"
cat > "$run/job.sh" <<'KASATERM_JOB'
@JOB@
KASATERM_JOB
nohup perl -e 'setpgrp(0, 0); exec @ARGV' sh "$run/job.sh" "$commit" "$jobs" "$toolchain" "$@" </dev/null >"$run/job.log" 2>&1 &
printf '%s\n' "$!" > "$run/pid"
echo "state=started"
""".replace("@JOB@", JOB.strip()).replace("@REPO@", REPO_URL)

STATUS = r"""
run="$HOME/.local/share/kasaterm-verify/run"
[ -f "$run/plan" ] || { echo "state=missing"; exit 0; }
echo "plan=$(cat "$run/plan")"
echo "commit=$(cat "$run/commit")"
[ -f "$run/head" ] && echo "head=$(cat "$run/head")"
if [ -f "$run/exit" ]; then
  echo "exit=$(cat "$run/exit")"
  [ -f "$run/why" ] && echo "why=$(head -n 1 "$run/why")"
  [ -f "$run/test.log" ] && tail -n 60 "$run/test.log" | sed 's/^/log=/'
elif [ -f "$run/pid" ] && kill -0 "$(cat "$run/pid")" 2>/dev/null; then
  echo "state=running"
else
  echo "state=dead"
fi
"""

CANCEL = r"""
run="$HOME/.local/share/kasaterm-verify/run"
[ "$(cat "$run/plan" 2>/dev/null)" = "$1" ] || exit 0
pid=$(cat "$run/pid" 2>/dev/null) && kill -TERM -- "-$pid" 2>/dev/null
echo "state=cancelled"
"""


def now_ms():
    return int(time.time() * 1000)


def configured_hosts(env=os.environ):
    raw = env.get(HOSTS_ENV)
    names = HOSTS if raw is None else tuple(raw.replace(",", " ").split())
    return tuple(h for h in names if SAFE_ARG.fullmatch(h))


def remote_command(script, *args):
    """ssh 는 인자를 원격 셸 한 줄로 이어 붙인다 — 스크립트는 base64 로, 인자는 따옴표가 필요 없는 글자만 싣는다."""
    if not all(SAFE_ARG.fullmatch(a) for a in args):
        raise Refused(f"원격 검사 인자에 실을 수 없는 글자가 있다: {args}")
    encoded = base64.b64encode(script.encode()).decode()
    return f"echo {encoded} | base64 --decode | sh -s -- {' '.join(args)}".rstrip()


def parse(text):
    facts, log = {}, []
    for line in (text or "").splitlines():
        key, sep, value = line.partition("=")
        if not sep:
            continue
        if key == "log":
            log.append(value)
        else:
            facts[key] = value.strip()
    if log:
        facts["log"] = log
    return facts


def capacity(facts, personal):
    """(일꾼 수, 까닭). 0 이면 맡기지 않는다."""
    try:
        ncpu, load = int(facts["ncpu"]), float(facts["load"])
        mem, free_pct = int(facts["mem"]), int(facts["free_pct"])
    except (KeyError, ValueError):
        return 0, "사용량을 못 읽었다"
    if personal:
        if "AC Power" not in facts.get("power", ""):
            return 0, "배터리로 돈다"
        try:
            disk_kb = int(facts.get("disk_kb", ""))
        except ValueError:
            return 0, "디스크 여유를 못 읽었다"
        if disk_kb < MIN_DISK_GB * 1024 * 1024:
            return 0, f"디스크 여유가 {MIN_DISK_GB}GB 아래다"
    cpu = int(ncpu - load) - (PERSONAL_RESERVE if personal else 0)
    memory = int(mem * free_pct / 100 / 2**30 // GB_PER_JOB)
    return max(0, min(cpu, memory, MAX_JOBS)), f"코어 {ncpu}·5분 부하 {load:.1f}·메모리 여유 {free_pct}%"


def explicit_jobs(env=os.environ):
    raw = env.get("CARGO_BUILD_JOBS", "")
    return int(raw) if raw.isdigit() and int(raw) > 0 else None


def choose(runner, hosts, fixed=None):
    """{"host": None(이 기기)|이름, "jobs", "seen": 기기마다 판정}. 원격은 이 기기보다 일꾼을 더 낼 때만 고른다."""
    r = runner.run(["sh", "-c", PROBE], timeout=20)
    jobs, why = capacity(parse(r.out) if r.ok else {}, personal=False)
    if fixed:
        jobs, why = fixed, "CARGO_BUILD_JOBS 로 고정"
    jobs = max(1, jobs)
    seen = [{"host": "controller", "jobs": jobs, "why": why}]
    best = {"host": None, "jobs": jobs}
    for host in hosts:
        r = runner.run([*SSH, host, remote_command(PROBE)], timeout=30)
        if not r.ok:
            seen.append({"host": host, "jobs": 0, "why": "닿지 않는다"})
            continue
        jobs, why = capacity(parse(r.out), personal=True)
        seen.append({"host": host, "jobs": jobs, "why": why})
        if jobs > best["jobs"]:
            best = {"host": host, "jobs": jobs}
    return {**best, "seen": seen}


def toolchain(runner, cargo):
    """controller 의 rust 판 — 원격도 같은 판으로 시험해야 판정이 같다."""
    r = runner.run([str(Path(cargo).with_name("rustc")), "-V"], timeout=30)
    m = re.match(r"rustc (\d+\.\d+\.\d+)", r.out or "")
    return m.group(1) if r.ok and m else None


def _load(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def _save(path, record):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(record, ensure_ascii=False, sort_keys=True, indent=2) + "\n")
    tmp.replace(path)


def place(runner, workdir, plan, cargo, packages, hosts=None, fixed=None, clock=now_ms, timeout_ms=7200 * 1000):
    """{"done": 증거} — 원격 검사가 통과했다. {"jobs", "choice"} — 이 기기에서 그 일꾼 수로 검사한다.
    원격이 도는 중이면 Pending, 원격 시험이 실패했으면 Refused."""
    hosts = configured_hosts() if hosts is None else hosts
    path = Path(workdir) / RECORD
    record = _load(path) or {}
    if record.get("fallback"):
        return _local(runner, fixed, record["fallback"])
    attempts = record.get("attempts", 0)
    if record.get("failure") and attempts >= REMOTE_ATTEMPTS:
        raise Refused(record["failure"])
    if record.get("host") and not record.get("failure"):
        outcome = _poll(runner, path, record, plan, packages, clock, timeout_ms)
        if outcome is not None:
            return outcome if "done" in outcome else _local(runner, fixed, record["fallback"])
    choice = choose(runner, hosts, fixed)
    host = choice["host"]
    version = toolchain(runner, cargo) if host else None
    if not host:
        return {"jobs": choice["jobs"], "choice": choice}
    if not version:
        return _local(runner, fixed, "controller 의 rust 판을 못 읽었다")
    started = runner.run([*SSH, host, remote_command(START, plan["plan_id"], plan["commit"], str(choice["jobs"]), version,
                                                     *[a for p in packages for a in ("-p", p)])], timeout=60)
    if not started.ok or parse(started.out).get("state") not in ("started", "running"):
        return _local(runner, fixed, f"{host} 에서 검사를 못 띄웠다")
    _save(path, {"host": host, "jobs": choice["jobs"], "toolchain": version, "started_ms": clock(), "seen_ms": clock(),
                 "attempts": attempts + 1, "choice": choice})
    raise Pending(f"{host} 에서 검사 시작(일꾼 {choice['jobs']})")


def _local(runner, fixed, why):
    choice = choose(runner, (), fixed)
    return {"jobs": choice["jobs"], "choice": {**choice, "fallback": why}}


def _poll(runner, path, record, plan, packages, clock, timeout_ms):
    """None 이면 새로 고른다(원격 검사가 사라졌다). {"done"} 이면 통과, {"fallback"} 이면 이 기기로."""
    host, jobs = record["host"], record["jobs"]
    r = runner.run([*SSH, host, remote_command(STATUS)], timeout=30)
    if not r.ok:
        if clock() - record["seen_ms"] > UNREACHABLE_GRACE_MS:
            record["fallback"] = f"{host} 에 {UNREACHABLE_GRACE_MS // 60000}분 닿지 않았다"
            _save(path, record)
            return {"fallback": True}
        raise Pending(f"{host} 검사를 확인하지 못했다 — 다음 tick 에 다시 본다")
    record["seen_ms"] = clock()
    _save(path, record)
    st = parse(r.out)
    if st.get("plan") != plan["plan_id"] or st.get("commit") != plan["commit"] or st.get("state") in ("missing", "dead"):
        return None
    minutes = (clock() - record["started_ms"]) // 60000
    if "exit" not in st:
        if clock() - record["started_ms"] > timeout_ms:
            runner.run([*SSH, host, remote_command(CANCEL, plan["plan_id"])], timeout=30)
            record["failure"] = f"검사 시간 초과({host}, {minutes}분)"
            record["attempts"] = REMOTE_ATTEMPTS
            _save(path, record)
            raise Refused(record["failure"])
        raise Pending(f"{host} 에서 검사 중(일꾼 {jobs}) · {minutes}분째")
    if st["exit"] == "setup":
        record["fallback"] = f"{host} 준비 실패 — {st.get('why') or '까닭 없음'}"
        _save(path, record)
        return {"fallback": True}
    argv = " ".join(["cargo", f"+{record['toolchain']}", "test", *[a for p in packages for a in ("-p", p)], "--release"])
    if st["exit"] == "0" and st.get("head") == plan["commit"]:
        return {"done": {"host": host, "jobs": jobs, "toolchain": record["toolchain"], "ran": [argv], "minutes": minutes,
                         "seen": record["choice"]["seen"], "dry": False}}
    tail = "\n".join(st.get("log", [])[-4:])
    record["failure"] = f"검사 실패({host}): {argv} — {tail}"
    _save(path, record)
    raise Refused(record["failure"])
