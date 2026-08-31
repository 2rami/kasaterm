#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import tempfile
import tomllib
from collections.abc import Iterable
from pathlib import Path
from typing import Any


def project_slug(path: str) -> str:
    return path.replace("/", "-").replace(".", "-")


def fnv1a(value: str) -> int:
    result = 0xCBF29CE484222325
    for byte in value.encode():
        result ^= byte
        result = (result * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return result


def team_name(path: str) -> str:
    slug = project_slug(path).strip("-")
    collapsed = re.sub(r"-+", "-", slug)
    tail = collapsed[-40:].strip("-") or "room"
    return f"kt-{tail}-{fnv1a(project_slug(path)) & 0xFFFF:04x}"


def under(path: str, root: str) -> bool:
    return path == root or path.startswith(root + os.sep)


def merge_values(left: Any, right: Any) -> Any:
    if left == right:
        return left
    if isinstance(left, dict) and isinstance(right, dict):
        merged = dict(left)
        for key, value in right.items():
            merged[key] = merge_values(merged[key], value) if key in merged else value
        return merged
    if isinstance(left, list) and isinstance(right, list):
        merged = list(left)
        for value in right:
            if value not in merged:
                merged.append(value)
        return merged
    return right


class Migration:
    def __init__(
        self,
        source: Path,
        target: Path,
        home: Path,
        backup: Path,
        apply: bool,
        check_clean: bool,
    ):
        self.source = str(source)
        self.target = str(target)
        self.home = home
        self.backup = backup
        self.apply = apply
        self.check_clean = check_clean
        self.operations: list[dict[str, str]] = []
        self.cwds = {self.source}
        escaped = re.escape(self.source)
        self.path_pattern = re.compile(escaped + r"(?=$|[/\\\s\"'`:,;\)\]\}])")

    def say(self, message: str) -> None:
        print(("APPLY " if self.apply else "DRY   ") + message)

    def replace_text(self, value: str) -> str:
        return self.path_pattern.sub(lambda _: self.target, value)

    def transform(self, value: Any) -> Any:
        if isinstance(value, str):
            return self.replace_text(value)
        if isinstance(value, list):
            return [self.transform(item) for item in value]
        if not isinstance(value, dict):
            return value

        out: dict[str, Any] = {}
        priority: dict[str, int] = {}
        for raw_key, raw_value in value.items():
            key = self.replace_text(raw_key) if isinstance(raw_key, str) else raw_key
            item = self.transform(raw_value)
            rank = 2 if key == raw_key else 1
            if key not in out:
                out[key] = item
                priority[key] = rank
                continue
            if rank >= priority[key]:
                out[key] = merge_values(out[key], item)
                priority[key] = rank
            else:
                out[key] = merge_values(item, out[key])
        return out

    def collect_cwds(self, value: Any) -> None:
        if isinstance(value, list):
            for item in value:
                self.collect_cwds(item)
            return
        if not isinstance(value, dict):
            return
        cwd = value.get("cwd")
        if isinstance(cwd, str) and under(cwd, self.source):
            self.cwds.add(cwd)
        for item in value.values():
            self.collect_cwds(item)

    @staticmethod
    def cwd_values(value: Any) -> Iterable[str]:
        if isinstance(value, list):
            for item in value:
                yield from Migration.cwd_values(item)
            return
        if not isinstance(value, dict):
            return
        cwd = value.get("cwd")
        if isinstance(cwd, str):
            yield cwd
        for item in value.values():
            yield from Migration.cwd_values(item)

    def project_cwd(self, directory: Path) -> str | None:
        known = next((cwd for cwd in self.cwds if project_slug(cwd) == directory.name), None)
        if known is not None:
            return known
        files = sorted(directory.glob("*.jsonl"), key=lambda path: path.stat().st_mtime, reverse=True)
        for path in files[:20]:
            try:
                sample = path.read_bytes()[: 2 * 1024 * 1024].decode("utf-8", errors="ignore")
            except OSError:
                continue
            for line in sample.splitlines()[:200]:
                try:
                    value = json.loads(line)
                except json.JSONDecodeError:
                    continue
                for cwd in self.cwd_values(value):
                    if project_slug(cwd) == directory.name:
                        return cwd
        return None

    def repository_cwd(self, slug: str) -> str | None:
        current_root = Path(self.source) if Path(self.source).is_dir() else Path(self.target)
        if not current_root.is_dir():
            return None
        for root, dirs, _ in os.walk(current_root):
            dirs[:] = [name for name in dirs if name not in {".git", ".cache", "node_modules", "target"}]
            current = Path(root)
            relative = current.relative_to(current_root)
            old_cwd = Path(self.source) / relative
            if project_slug(str(old_cwd)) == slug:
                return str(old_cwd)
        return None

    def relative_backup(self, path: Path) -> Path:
        try:
            rel = path.relative_to(self.home)
            return self.backup / "files" / rel
        except ValueError:
            digest = hashlib.sha256(str(path).encode()).hexdigest()[:12]
            return self.backup / "files-outside-home" / digest / path.name

    def backup_file(self, path: Path) -> None:
        destination = self.relative_backup(path)
        if destination.exists():
            return
        destination.parent.mkdir(parents=True, exist_ok=True)
        try:
            os.link(path, destination)
        except OSError:
            shutil.copy2(path, destination)

    def record(self, kind: str, source: Path, target: Path | None = None) -> None:
        row = {"kind": kind, "source": str(source)}
        if target is not None:
            row["target"] = str(target)
        self.operations.append(row)
        if self.apply:
            self.write_manifest()

    def write_manifest(self) -> None:
        self.backup.mkdir(parents=True, exist_ok=True)
        body = {
            "source": self.source,
            "target": self.target,
            "operations": self.operations,
        }
        self.atomic_write(self.backup / "state-migration-manifest.json", json.dumps(body, indent=2) + "\n")

    @staticmethod
    def atomic_write(path: Path, text: str, source_mode: int | None = None) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
        try:
            with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
                handle.write(text)
                handle.flush()
                os.fsync(handle.fileno())
            if source_mode is not None:
                os.chmod(temporary, stat.S_IMODE(source_mode))
            os.replace(temporary, path)
        finally:
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass

    def json_candidates(self) -> list[Path]:
        fixed_slots = [
            self.home / ".claude.json",
            self.home / ".claude/settings.json",
            self.home / ".claude/plugins/known_marketplaces.json",
            self.home / ".config/kasaterm/session.json",
            self.home / ".config/kasaterm/window.json",
            self.home / ".config/kasaterm/settings.json",
            self.home / ".config/kasaterm/claude-mcp.json",
        ]
        fixed = [target for path in fixed_slots if (target := self.config_target(path)) is not None]
        patterns = [
            (self.home / ".config/kasaterm", "session-restored-*.json"),
            (self.home / ".config/kasaterm", "daemon*.sock.state"),
            (self.home / ".config/kasaterm/agent-roster", "*.json"),
            (self.home / ".claude/sessions", "*.json"),
            (self.home / ".claude/tasks", "**/*.json"),
            (self.home / ".claude/teams", "**/*.json"),
        ]
        found = fixed
        for root, pattern in patterns:
            if root.is_dir():
                found.extend(root.glob(pattern))
        return sorted({path for path in found if path.is_file() and not path.is_symlink()})

    @staticmethod
    def config_target(path: Path) -> Path | None:
        if path.is_symlink():
            try:
                target = path.resolve(strict=True)
            except (OSError, RuntimeError) as error:
                raise RuntimeError(f"설정 symlink가 깨졌습니다: {path}: {error}") from error
            if not target.is_file():
                raise RuntimeError(f"설정 symlink가 파일을 가리키지 않습니다: {path} -> {target}")
            return target
        return path if path.is_file() else None

    def validate_and_discover(self) -> tuple[list[tuple[Path, Any, Any]], tuple[Path, str, str] | None]:
        patches: list[tuple[Path, Any, Any]] = []
        for path in self.json_candidates():
            try:
                text = path.read_text(encoding="utf-8")
            except (OSError, UnicodeError, json.JSONDecodeError) as error:
                raise RuntimeError(f"JSON을 읽을 수 없습니다: {path}: {error}") from error
            if not self.path_pattern.search(text):
                continue
            try:
                original = json.loads(text)
            except json.JSONDecodeError as error:
                raise RuntimeError(f"경로가 든 JSON을 읽을 수 없습니다: {path}: {error}") from error
            self.collect_cwds(original)
            changed = self.transform(original)
            if changed != original:
                patches.append((path, original, changed))

        toml_slot = self.home / ".codex/config.toml"
        toml_path = self.config_target(toml_slot)
        toml_patch: tuple[Path, str, str] | None = None
        if toml_path is not None:
            original = toml_path.read_text(encoding="utf-8")
            tomllib.loads(original)
            changed = self.replace_text(original)
            if changed != original:
                tomllib.loads(changed)
                toml_patch = (toml_path, original, changed)
        return patches, toml_patch

    @staticmethod
    def files_equal(left: Path, right: Path) -> bool:
        if left.stat().st_size != right.stat().st_size:
            return False
        with left.open("rb") as a, right.open("rb") as b:
            while True:
                one = a.read(1024 * 1024)
                two = b.read(1024 * 1024)
                if one != two:
                    return False
                if not one:
                    return True

    def check_merge(self, source: Path, target: Path) -> None:
        if not target.exists():
            return
        if source.is_dir() and target.is_dir():
            for child in source.iterdir():
                self.check_merge(child, target / child.name)
            return
        if source.is_file() and target.is_file() and self.files_equal(source, target):
            return
        raise RuntimeError(f"자동 병합할 수 없는 경로 충돌입니다: {source} -> {target}")

    def move_pairs(self) -> list[tuple[Path, Path, str]]:
        pairs: list[tuple[Path, Path, str]] = []
        old_slug = project_slug(self.source)
        new_slug = project_slug(self.target)
        projects = self.home / ".claude/projects"
        if projects.is_dir():
            for child in projects.iterdir():
                if child.name == old_slug or child.name.startswith(old_slug + "-"):
                    if child.is_symlink() or not child.is_dir():
                        raise RuntimeError(f"Claude project 경로가 실제 디렉터리가 아닙니다: {child}")
                    cwd = self.source if child.name == old_slug else self.project_cwd(child)
                    cwd = cwd or self.repository_cwd(child.name)
                    if cwd is None:
                        if any(child.glob("*.jsonl")):
                            raise RuntimeError(f"Claude project 폴더의 원래 cwd를 확인할 수 없습니다: {child}")
                        self.say(f"대화 없는 옛 project 폴더 보존: {child}")
                        continue
                    if not under(cwd, self.source):
                        continue
                    self.cwds.add(cwd)
                    pairs.append((child, projects / (new_slug + child.name[len(old_slug):]), "claude-project"))

        names: dict[str, str] = {}
        for cwd in self.cwds:
            if not under(cwd, self.source):
                continue
            replacement = self.target + cwd[len(self.source):]
            names[team_name(cwd)] = team_name(replacement)
        for root_name in (".claude/tasks", ".claude/teams"):
            root = self.home / root_name
            for old_name, new_name in names.items():
                source = root / old_name
                if source.exists() and old_name != new_name:
                    pairs.append((source, root / new_name, "claude-" + Path(root_name).name))
        return pairs

    def roster_pairs(self) -> list[tuple[Path, Path]]:
        root = self.home / ".config/kasaterm/agent-roster"
        if not root.is_dir():
            return []
        old_slug = project_slug(self.source)
        pairs = []
        slugs = {
            project_slug(cwd): project_slug(self.target + cwd[len(self.source):])
            for cwd in self.cwds
            if under(cwd, self.source)
        }
        slugs.setdefault(old_slug, project_slug(self.target))
        for old_name, new_name in slugs.items():
            for suffix in (".json", ".json.lock"):
                path = root / f"{old_name}{suffix}"
                if path.exists():
                    pairs.append((path, root / f"{new_name}{suffix}"))
        return pairs

    def merge_dir(self, source: Path, target: Path, kind: str) -> None:
        if not target.exists():
            os.replace(source, target)
            self.record(kind, source, target)
            return
        for child in list(source.iterdir()):
            destination = target / child.name
            if child.is_dir():
                destination.mkdir(exist_ok=True)
                self.merge_dir(child, destination, kind)
            elif not destination.exists():
                os.replace(child, destination)
                self.record(kind, child, destination)
            else:
                self.backup_file(child)
                os.unlink(child)
                self.record(kind + "-duplicate", child, destination)
        source.rmdir()

    def merge_roster(self, source: Path, target: Path) -> None:
        if not target.exists():
            os.replace(source, target)
            self.record("agent-roster", source, target)
            return
        self.backup_file(source)
        self.backup_file(target)
        if source.suffix == ".json" and target.suffix == ".json":
            old = json.loads(source.read_text(encoding="utf-8"))
            new = json.loads(target.read_text(encoding="utf-8"))
            if not isinstance(old, dict) or not isinstance(new, dict):
                raise RuntimeError(f"agent-roster 병합 형식이 아닙니다: {source}, {target}")
            for key, value in old.items():
                current = new.get(key)
                old_ts = value.get("ts", 0) if isinstance(value, dict) else 0
                new_ts = current.get("ts", 0) if isinstance(current, dict) else 0
                if current is None or old_ts > new_ts:
                    new[key] = value
            mode = target.stat().st_mode
            self.atomic_write(target, json.dumps(new, ensure_ascii=False, separators=(",", ":")), mode)
        os.unlink(source)
        self.record("agent-roster-merge", source, target)

    def run(self) -> None:
        patches, toml_patch = self.validate_and_discover()
        move_pairs = self.move_pairs()
        roster_pairs = self.roster_pairs()
        for source, target, _ in move_pairs:
            self.check_merge(source, target)
        for source, target in roster_pairs:
            if target.exists() and source.suffix == ".json":
                json.loads(source.read_text(encoding="utf-8"))
                json.loads(target.read_text(encoding="utf-8"))

        for path, _, _ in patches:
            self.say(f"경로 치환: {path}")
        if toml_patch:
            self.say(f"경로 치환: {toml_patch[0]}")
        for source, target, kind in move_pairs:
            self.say(f"{kind} 이동: {source} -> {target}")
        for source, target in roster_pairs:
            self.say(f"agent-roster 이동: {source} -> {target}")

        if not self.apply:
            file_count = len(patches) + int(toml_patch is not None)
            move_count = len(move_pairs) + len(roster_pairs)
            print(f"검사 완료: 파일 {file_count}개, 경로 이동 {move_count}개")
            if self.check_clean and (file_count or move_count):
                raise RuntimeError("이전 경로가 아직 남아 있습니다")
            return

        self.backup.mkdir(parents=True, exist_ok=True)
        for path, _, changed in patches:
            self.backup_file(path)
            mode = path.stat().st_mode
            self.atomic_write(path, json.dumps(changed, ensure_ascii=False, indent=2) + "\n", mode)
            self.record("rewrite-json", path)
        if toml_patch:
            path, _, changed = toml_patch
            self.backup_file(path)
            mode = path.stat().st_mode
            self.atomic_write(path, changed, mode)
            self.record("rewrite-toml", path)

        for source, target, kind in move_pairs:
            target.parent.mkdir(parents=True, exist_ok=True)
            self.merge_dir(source, target, kind)
        for source, target in roster_pairs:
            self.merge_roster(source, target)
        self.write_manifest()
        print(f"상태 이전 완료: 백업 {self.backup}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--target", type=Path, required=True)
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("--backup-dir", type=Path, required=True)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--check-clean", action="store_true")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    migration = Migration(
        args.source,
        args.target,
        args.home,
        args.backup_dir,
        args.apply,
        args.check_clean,
    )
    try:
        migration.run()
    except (OSError, RuntimeError, json.JSONDecodeError, tomllib.TOMLDecodeError) as error:
        raise SystemExit(f"오류: {error}") from error


if __name__ == "__main__":
    main()
