#!/usr/bin/env python3
"""카사텀 cargo 빌드 산출물이 상한을 넘으면 오래 안 쓴 것부터 통째로 지운다.

학생마다 격리 빌드를 띄워 target 이 쌓이고 아무도 치우지 않아, 맥북 디스크가 0 이 되자 카사텀이
stderr 한 줄을 못 써 panic 하고 잠금 오염 연쇄로 죽었다(2026-11-13). 그래서 빌드 끝에 한 번 이것을 돈다.

대상은 cargo 가 만든 target 폴더(뿌리에 CACHEDIR.TAG)만이다:
- 레포의 `target`
- `~/Library/Caches/kasaterm-builds/<일>/` 아래 깊이 2
- `/private/tmp/kt-*`·`/private/tmp/kasaterm-*` 아래 깊이 2
합이 상한을 넘으면 마지막으로 쓴 때가 오래된 것부터 지워 상한 밑으로 내린다. 지금 쓰는 것은 남긴다:
최근 `--idle-min` 분 안에 바뀐 것, 돌고 있는 cargo·rustc·build-app 의 명령줄·환경(PWD·CARGO_TARGET_DIR)에
그 폴더나 그 폴더의 작업 공간 경로가 든 것.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

CARGO_TAG = "Signature: 8a477f597d28d172789f06886806bc55"
BUILDERS = ("cargo", "rustc", "build-app.sh", "rust-analyzer")


def default_roots(repo):
    home = Path.home()
    roots = [(repo / "target", 0), (home / "Library/Caches/kasaterm-builds", 2)]
    tmp = Path("/private/tmp")
    if tmp.is_dir():
        for entry in tmp.iterdir():
            if entry.name.startswith(("kt-", "kasaterm-")) and entry.is_dir() and not entry.is_symlink():
                roots.append((entry, 2))
    return roots


def is_cargo_target(path):
    try:
        return (path / "CACHEDIR.TAG").read_text(errors="replace").startswith(CARGO_TAG)
    except OSError:
        return False


def find_targets(roots):
    found = []
    for root, depth in roots:
        stack = [(root, depth)]
        while stack:
            path, left = stack.pop()
            if path.is_symlink() or not path.is_dir():
                continue
            if is_cargo_target(path):
                found.append(path)
                continue
            if left > 0:
                try:
                    stack.extend((child, left - 1) for child in path.iterdir())
                except OSError:
                    pass
    return sorted(set(found))


def last_used(path):
    """target 뿌리와 그 아래 두 겹(debug·release·<triple>/debug)의 가장 늦은 mtime — 빌드가 돌면 이 자리들이 바뀐다."""
    newest = path.stat().st_mtime
    for child in path.iterdir():
        try:
            newest = max(newest, child.stat().st_mtime)
            if child.is_dir() and not child.is_symlink():
                for grand in child.iterdir():
                    newest = max(newest, grand.lstat().st_mtime)
        except OSError:
            pass
    return newest


def size_bytes(path):
    out = subprocess.run(["du", "-sk", str(path)], capture_output=True, text=True)
    try:
        return int(out.stdout.split()[0]) * 1024
    except (IndexError, ValueError):
        return 0


def busy_text():
    """돌고 있는 빌드 프로세스의 명령줄과 환경(macOS `ps -E`) — 경로 문자열이 들었나만 본다."""
    out = subprocess.run(["ps", "-axEww", "-o", "command="], capture_output=True, text=True)
    return "\n".join(line for line in out.stdout.splitlines() if any(b in line for b in BUILDERS))


def in_use(path, busy):
    """그 target 경로(CARGO_TARGET_DIR 포함)나 그 작업 공간 안에서 도는 빌드(PWD=작업 공간…)가 있으면 쓰는 중이다."""
    return str(path) in busy or f"PWD={path.parent}" in busy


def plan(targets, cap, idle_s, busy, now, measure=size_bytes):
    rows = [{"path": p, "bytes": measure(p), "used": last_used(p)} for p in targets]
    total = sum(r["bytes"] for r in rows)
    doomed = []
    for row in sorted(rows, key=lambda r: r["used"]):
        if total <= cap:
            break
        if now - row["used"] < idle_s or in_use(row["path"], busy):
            continue
        doomed.append(row)
        total -= row["bytes"]
    return rows, doomed, total


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cap-gib", type=float, default=float(os.environ.get("KASATERM_BUILD_CAP_GIB", "15")))
    parser.add_argument("--idle-min", type=float, default=30)
    parser.add_argument("--root", action="append", type=Path, help="이 폴더만 본다(깊이 2) — 시험용")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    repo = Path(__file__).resolve().parent.parent
    roots = [(r, 2) for r in args.root] if args.root else default_roots(repo)
    rows, doomed, left = plan(find_targets(roots), int(args.cap_gib * 2**30), args.idle_min * 60, busy_text(), time.time())
    removed = []
    for row in doomed:
        if args.dry_run:
            continue
        shutil.rmtree(row["path"], ignore_errors=True)
        if not row["path"].exists():
            removed.append(row)
    report = {"targets": len(rows), "total_gib": round(sum(r["bytes"] for r in rows) / 2**30, 1),
              "cap_gib": args.cap_gib, "left_gib": round(left / 2**30, 1), "dry_run": args.dry_run,
              "removed": [{"path": str(r["path"]), "gib": round(r["bytes"] / 2**30, 1)} for r in (doomed if args.dry_run else removed)]}
    print(json.dumps(report, ensure_ascii=False) if args.json else
          f"빌드 산출물 {report['total_gib']}GiB / 상한 {args.cap_gib}GiB — "
          + (", ".join(f"{r['path']} {r['gib']}GiB" for r in report["removed"]) + (" 지울 것" if args.dry_run else " 지움")
             if report["removed"] else "지울 것 없음"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
