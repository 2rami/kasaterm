"""Windows 판 기호(PDB) 보관.

Windows MSI 는 GitHub 러너가 굽고, 러너는 `kasaterm.pdb` 를 릴리스 자산이 아니라 워크플로 산출물(`ARTIFACT`, 보존 90일)로만
남긴다. controller 가 돌 때마다 끝난 release.yml 실행의 산출물을 `<state>/symbols/<태그>/` 로 받아 둔다 — 집 PC 덤프의
`kasaterm.exe + 오프셋` 을 함수 이름으로 바꾸는 데 쓴다. 출시와 따로 돌고, 실패해도 출시를 막지 않는다.
"""

import json
import os
import re
import shutil
import tempfile
from pathlib import Path

from tools.release import deps
from tools.release.proc import Runner

ARTIFACT = "kasaterm-windows-pdb"
SLUG = "2rami/kasaterm"
TAG = re.compile(r"v\d+\.\d+\.\d+")
# 산출물이 없던 실행(mac 만 구운 실행, 이 길 이전 판)은 다시 묻지 않는다 — tick 은 1분마다 돈다.
CHECKED = ".no-artifact.json"


def run_tag(run):
    """Windows 덤 실행은 제목(`release vX.Y.Z (windows)`)에, 태그 push 실행은 브랜치 이름에 태그가 있다."""
    for text in (run.get("displayTitle") or "", run.get("headBranch") or ""):
        found = TAG.search(text)
        if found:
            return found.group(0)
    return None


def gh_path():
    return next((p for p in deps.GH_CANDIDATES if os.access(p, os.X_OK)), shutil.which("gh") or "gh")


def sweep(state_dir, runner=None, gh=None, slug=SLUG, limit=30):
    runner = runner or Runner("local")
    gh = gh or gh_path()
    root = Path(state_dir) / "symbols"
    root.mkdir(mode=0o700, exist_ok=True)
    checked_path = root / CHECKED
    try:
        checked = set(json.loads(checked_path.read_text()))
    except (OSError, ValueError):
        checked = set()
    listed = runner.run([gh, "run", "list", "--repo", slug, "--workflow", "release.yml", "--json",
                         "databaseId,status,conclusion,headBranch,displayTitle", "--limit", str(limit)], timeout=60)
    if not listed.ok:
        return {"error": f"실행 목록을 못 읽었다 — {listed.tail(2) or '시간 초과'}"}
    saved = []
    for run in json.loads(listed.out or "[]"):
        tag, run_id = run_tag(run), run.get("databaseId")
        if not tag or not run_id or run.get("status") != "completed" or run.get("conclusion") != "success":
            continue
        if run_id in checked or (root / tag / "kasaterm.pdb").exists():
            continue
        with tempfile.TemporaryDirectory(dir=root, prefix=".download-") as scratch:
            got = runner.run([gh, "run", "download", str(run_id), "--repo", slug, "--name", ARTIFACT, "--dir", scratch],
                             timeout=600, kind="local")
            pdb = Path(scratch) / "kasaterm.pdb"
            if not got.ok or not pdb.is_file():
                checked.add(run_id)
                continue
            (root / tag).mkdir(mode=0o700, exist_ok=True)
            for item in Path(scratch).iterdir():
                if item.is_file():
                    os.replace(item, root / tag / item.name)
            saved.append(tag)
    checked_path.write_text(json.dumps(sorted(checked)))
    return {"saved": saved} if saved else {}


def sweep_quietly(state_dir):
    """tick 끝에 붙인다 — 기호를 못 받아도 그 tick 의 출시 결과는 그대로다."""
    try:
        return sweep(state_dir)
    except (OSError, ValueError) as error:
        return {"error": str(error)}
