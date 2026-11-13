#!/usr/bin/env python3
"""카사텀 cargo 빌드 산출물이 상한을 넘으면 오래 안 쓴 것부터 통째로 지운다.

학생마다 격리 빌드를 띄워 target 이 쌓이고 아무도 치우지 않아, 맥북 디스크가 0 이 되자 카사텀이
stderr 한 줄을 못 써 panic 하고 잠금 오염 연쇄로 죽었다(2026-11-13). 그래서 빌드 끝에 한 번 이것을 돈다.

대상은 cargo 가 만든 target 폴더(뿌리에 CACHEDIR.TAG)만이다:
- 레포의 `target`
- `~/Library/Caches/kasaterm-builds/<일>/` 아래 깊이 2
- `/private/tmp/kt-*`·`/private/tmp/kasaterm-*` 아래 깊이 2
합이 상한을 넘으면 마지막으로 쓴 때가 오래된 것부터 지워 상한 밑으로 내린다.

지금 쓰는 것은 남긴다. 경로는 모두 실제 경로로 맞춰 비교한다(`/tmp` ↔ `/private/tmp`, 심볼릭 링크, 상대 경로):
- 최근 `--idle-min` 분 안에 바뀐 것
- 이 사용자의 어느 프로세스든 작업 폴더·실행 파일·열린 파일이 그 target 이나 그 작업 공간 안에 있는 것(`lsof`)
- 어느 프로세스의 `CARGO_TARGET_DIR` 이 그 target 인 것(`ps -E`, 상대 경로는 그 프로세스의 `PWD` 기준)
- 다른 target 안의 심볼릭 링크가 가리키는 것(가리키는 쪽 target 도 같이 지울 때만 지운다)
지우기 직전에 그 target 하나를 다시 재고, 정리는 잠금으로 한 번에 하나만 돈다. 프로세스 조회가 실패하면
아무것도 지우지 않는다. 빌드와 정리가 함께 잡는 잠금(lease)은 아직 없어, 다시 잰 뒤 지우기 사이의 짧은 틈은 남는다.
"""

import argparse
import fcntl
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

CARGO_TAG = "Signature: 8a477f597d28d172789f06886806bc55"
TARGET_ENV = re.compile(r"(?:^|\s)CARGO_TARGET_DIR=(\S+)")
PWD_ENV = re.compile(r"(?:^|\s)PWD=(\S+)")


class Unknown(RuntimeError):
    """누가 무엇을 쓰는지 못 쟀다 — 그럴 땐 지우지 않는다."""


def real(path):
    return os.path.realpath(path)


def under(path, root):
    return path == root or path.startswith(root.rstrip("/") + "/")


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
            newest = max(newest, child.lstat().st_mtime)
            if child.is_dir() and not child.is_symlink():
                for grand in child.iterdir():
                    newest = max(newest, grand.lstat().st_mtime)
        except OSError:
            pass
    return newest


def links_into(path, depth=3):
    """이 target 안 심볼릭 링크들이 가리키는 실제 경로."""
    out, stack = set(), [(path, depth)]
    while stack:
        here, left = stack.pop()
        try:
            children = list(here.iterdir())
        except OSError:
            continue
        for child in children:
            if child.is_symlink():
                out.add(real(child))
            elif left > 0 and child.is_dir():
                stack.append((child, left - 1))
    return out


def size_bytes(path):
    out = subprocess.run(["du", "-sk", str(path)], capture_output=True, text=True)
    try:
        return int(out.stdout.split()[0]) * 1024
    except (IndexError, ValueError):
        return 0


def env_paths(ps_text):
    """`ps -E` 한 줄마다 CARGO_TARGET_DIR(상대면 그 프로세스 PWD 기준)과 PWD."""
    paths = set()
    for line in ps_text.splitlines():
        pwd = PWD_ENV.search(line)
        pwd = pwd.group(1) if pwd else None
        if pwd:
            paths.add(pwd)
        target = TARGET_ENV.search(line)
        if target:
            value = target.group(1)
            paths.add(value if os.path.isabs(value) else os.path.join(pwd or "/", value))
    return paths


def lsof_paths(lsof_text):
    """`lsof -F n` 의 경로 줄 — 작업 폴더(cwd)·실행 파일(txt)·열린 파일."""
    return {line[1:] for line in lsof_text.splitlines() if line.startswith("n/")}


def used_paths():
    ps = subprocess.run(["ps", "-axEww", "-o", "command="], capture_output=True, text=True, timeout=60)
    if ps.returncode != 0 or not ps.stdout.strip():
        raise Unknown("ps 를 못 읽었다")
    lsof = subprocess.run(["lsof", "-n", "-P", "-w", "-F", "n", "-u", str(os.getuid())],
                          capture_output=True, text=True, timeout=300)
    if lsof.returncode not in (0, 1) or not lsof.stdout.strip():
        raise Unknown("lsof 를 못 읽었다")
    return {real(p) for p in env_paths(ps.stdout) | lsof_paths(lsof.stdout)}


def in_use(path, used):
    """그 target 안이나 그 작업 공간(target 의 부모) 안을 누가 쓰고 있나."""
    target, workspace = real(path), real(Path(path).parent)
    return any(under(p, target) or under(p, workspace) for p in used)


def plan(targets, cap, idle_s, used, now, measure=size_bytes):
    rows = [{"path": p, "real": real(p), "bytes": measure(p), "used": last_used(p)} for p in targets]
    referrers = {row["real"]: [other for other in rows if other is not row
                               and any(under(link, row["real"]) for link in links_into(other["path"]))] for row in rows}
    total = sum(r["bytes"] for r in rows)
    doomed = []
    for row in sorted(rows, key=lambda r: r["used"]):
        if total <= cap:
            break
        if now - row["used"] < idle_s or in_use(row["path"], used):
            continue
        if any(other not in doomed for other in referrers[row["real"]]):
            continue
        doomed.append(row)
        total -= row["bytes"]
    return rows, doomed, total


def still_idle(row, idle_s, measure_used=used_paths, now=None):
    """지우기 직전 다시 잰다 — 계획 뒤에 시작한 빌드가 그 target 을 잡았을 수 있다."""
    now = time.time() if now is None else now
    return (is_cargo_target(row["path"]) and real(row["path"]) == row["real"] and last_used(row["path"]) == row["used"]
            and now - row["used"] >= idle_s and not in_use(row["path"], measure_used()))


def locked():
    path = Path.home() / "Library/Caches/kasaterm-builds/.prune.lock"
    path.parent.mkdir(parents=True, exist_ok=True)
    handle = open(path, "w")
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        handle.close()
        return None
    return handle


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cap-gib", type=float, default=float(os.environ.get("KASATERM_BUILD_CAP_GIB", "15")))
    parser.add_argument("--idle-min", type=float, default=30)
    parser.add_argument("--root", action="append", type=Path, help="이 폴더만 본다(깊이 2) — 시험용")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    lock = locked()
    if lock is None:
        print("다른 정리가 도는 중 — 건너뜀")
        return 0
    repo = Path(__file__).resolve().parent.parent
    roots = [(r, 2) for r in args.root] if args.root else default_roots(repo)
    idle_s = args.idle_min * 60
    try:
        rows, doomed, _ = plan(find_targets(roots), int(args.cap_gib * 2**30), idle_s, used_paths(), time.time())
    except Unknown as why:
        print(f"누가 무엇을 쓰는지 못 재서 지우지 않음 — {why}")
        return 1
    removed = []
    for row in doomed:
        if args.dry_run:
            continue
        try:
            if not still_idle(row, idle_s):
                continue
        except Unknown as why:
            print(f"다시 재지 못해 멈춤 — {why}")
            break
        shutil.rmtree(row["path"], ignore_errors=True)
        if not row["path"].exists():
            removed.append(row)
    shown = doomed if args.dry_run else removed
    report = {"targets": len(rows), "total_gib": round(sum(r["bytes"] for r in rows) / 2**30, 1),
              "cap_gib": args.cap_gib, "dry_run": args.dry_run,
              "removed": [{"path": str(r["path"]), "gib": round(r["bytes"] / 2**30, 1)} for r in shown]}
    print(json.dumps(report, ensure_ascii=False) if args.json else
          f"빌드 산출물 {report['total_gib']}GiB / 상한 {args.cap_gib}GiB — "
          + (", ".join(f"{r['path']} {r['gib']}GiB" for r in report["removed"]) + (" 지울 것" if args.dry_run else " 지움")
             if report["removed"] else "지울 것 없음"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
