"""끝난 출시 계획의 작업 폴더 거두기.

계획마다 `<state>/plans/<id>/work/` 에 격리 워크트리(`wt`, 0.7GB 남짓)와 dmg·자산이 남는데 지우는 곳이 없어, 맥미니에
38개 29GB 가 쌓였다(2026-11-13, 데이터 볼륨 98%). tick 끝에 붙여 최근 `KEEP` 개와 아직 안 끝난 요청이 가리키는 계획은
두고 나머지의 `work/` 만 지운다. `plan.json`·`state.json` 은 남긴다 — 대기열이 옛 계획을 다시 읽어도 깨지지 않게(계획이
사라져 대기열이 멈춘 적이 있다). 워크트리는 checkout 에서 `git worktree remove` 로 떼고, 다 지운 뒤 `prune` 한다.
출시와 따로 돌고, 실패해도 그 tick 의 결과를 바꾸지 않는다.
"""

import shutil
import subprocess

from tools.release import policy
from tools.release.common import Refused

KEEP = 5


def live_plans(ledger):
    """아직 끝나지 않은 요청(지금 굽는 것 포함)이 가리키는 계획 — 다시 시작하거나 되살릴 수 있어 남긴다."""
    active = ledger.get("active")
    live = set()
    for sha, entry in (ledger.get("requests") or {}).items():
        if not isinstance(entry, dict) or (entry.get("state") == "done" and sha != active):
            continue
        if entry.get("plan_id"):
            live.add(entry["plan_id"])
        live.update(entry.get("cancelled_plans") or [])
    return live


def _last_touched(plan):
    return max((plan / name).stat().st_mtime for name in ("plan.json", "state.json") if (plan / name).is_file())


def sweep(state_dir, keep=KEEP, git="git"):
    root = policy.private_dir(state_dir)
    plans = root / "plans"
    if not plans.is_dir() or plans.is_symlink():
        return {}
    queue = root / "queue.json"
    live = live_plans(policy.read_private(queue) if queue.exists() else {})
    found = [d for d in plans.iterdir() if d.is_dir() and not d.is_symlink() and (d / "plan.json").is_file()]
    found.sort(key=_last_touched, reverse=True)
    checkout = root / "checkout"
    swept = []
    for plan in found[keep:]:
        work = plan / "work"
        if plan.name in live or work.is_symlink() or not work.is_dir() or work.resolve().parent.parent != plans:
            continue
        for tree in sorted(work.glob("wt*")):
            if (tree / ".git").exists() and checkout.is_dir():
                subprocess.run([git, "-C", str(checkout), "worktree", "remove", "--force", str(tree)],
                               capture_output=True, timeout=300)
        shutil.rmtree(work, ignore_errors=True)
        if not work.exists():
            swept.append(plan.name)
    if swept and checkout.is_dir():
        subprocess.run([git, "-C", str(checkout), "worktree", "prune"], capture_output=True, timeout=120)
    return {"swept": swept} if swept else {}


def sweep_quietly(state_dir):
    """tick 끝에 붙인다 — 거두기에 실패해도 그 tick 의 출시 결과는 그대로다."""
    try:
        return sweep(state_dir)
    except (OSError, ValueError, subprocess.SubprocessError, Refused) as error:
        return {"error": str(error)}
