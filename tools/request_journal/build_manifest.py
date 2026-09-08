"""Publish verified build evidence outside the signed application bundle."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def now():
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def digest(value):
    return hashlib.sha256(value).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()


def git(project, *args):
    return subprocess.run(["git", "--no-pager", "-C", str(project), "-c", "core.fsmonitor=false", *args],
                          check=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=30).stdout


def credential_path(path):
    return any(part == ".env" or part.startswith(".env.") or part == "usemap.env"
               or part.endswith((".pem", ".key", ".p12", ".pfx")) or "secret" in part.lower()
               for part in Path(path).parts)


def file_hash(path):
    before = path.stat()
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(block)
    after = path.stat()
    same = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
    if not same:
        raise ValueError("File changed while its evidence was being read")
    return {"sha256": hasher.hexdigest(), "size": after.st_size}


def source_snapshot(project):
    root = Path(project).resolve()
    result = {"source_root": str(root), "project": str(root), "observed_at": now(),
              "head": None, "dirty": None, "snapshot_stable": False, "configuration": "not_inspected"}
    try:
        common = Path(git(root, "rev-parse", "--git-common-dir").decode().strip())
        result["project"] = str((common if common.is_absolute() else root / common).resolve().parent)
        before = git(root, "rev-parse", "HEAD").decode().strip()
        status_before = git(root, "status", "--porcelain=v1", "-z")
        listed = git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--",
                     "app", "crates", "web", "scripts", "assets", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml")
        paths = set(os.fsdecode(p) for p in listed.split(b"\0") if p)
        paths.update(p for p in ("Cargo.lock", "web/arona-ui/package-lock.json") if (root / p).is_file())
        inputs, excluded = [], 0
        for relative in sorted(paths):
            if credential_path(relative):
                excluded += 1
                continue
            path = root / relative
            if path.is_symlink():
                inputs.append([relative, "symlink", os.readlink(path)])
            elif path.is_file():
                inputs.append([relative, file_hash(path)["sha256"]])
            else:
                inputs.append([relative, "missing"])
        after = git(root, "rev-parse", "HEAD").decode().strip()
        status_after = git(root, "status", "--porcelain=v1", "-z")
        result.update(head=after, dirty=bool(status_after), status_digest=digest(status_after),
                      input_digest=digest(canonical(inputs)), input_count=len(inputs),
                      excluded_credential_paths=excluded,
                      snapshot_stable=before == after and status_before == status_after)
    except (OSError, ValueError, subprocess.SubprocessError):
        result["error"] = "source_snapshot_unavailable"
    return result


def atomic_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".build-proof-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(canonical(value))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def begin(project, profile, output):
    snapshot = source_snapshot(project)
    snapshot.update(profile=profile, started_at=now())
    atomic_json(output, snapshot)
    return snapshot


def _inventory(bundle):
    result = []
    for path in sorted(bundle.rglob("*")):
        if path.is_symlink():
            result.append((path.relative_to(bundle).as_posix(), "link", os.readlink(path)))
        elif path.is_file():
            stat = path.stat()
            result.append((path.relative_to(bundle).as_posix(), stat.st_ino, stat.st_size, stat.st_mtime_ns))
    return result


def bundle_hashes(bundle):
    bundle = Path(bundle).resolve()
    before = _inventory(bundle)
    files = {}
    for record in before:
        relative = record[0]
        if credential_path(relative):
            raise ValueError("Credential-like bundle input is outside the evidence scope")
        path = bundle / relative
        files[relative] = ({"sha256": digest(os.readlink(path).encode()), "kind": "symlink"}
                           if path.is_symlink() else dict(file_hash(path), kind="file"))
    if before != _inventory(bundle):
        raise ValueError("Bundle changed while evidence was being captured")
    return {"sha256": digest(canonical(files)), "files": files}


def verify_signature(bundle):
    if sys.platform != "darwin":
        return {"verified": False, "status": "unsupported_platform"}
    result = subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(bundle)],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
    return {"verified": result.returncode == 0, "status": "verified" if result.returncode == 0 else "verification_failed", "checked_at": now()}


def finish(snapshot_path, bundle, output, *, signature_verifier=verify_signature):
    before = json.loads(Path(snapshot_path).read_text())
    bundle = Path(bundle).resolve()
    signature = signature_verifier(bundle)
    if signature.get("verified") is not True:
        raise ValueError("Bundle signature was not verified; no ready manifest was published")
    hashes = bundle_hashes(bundle)
    signature = signature_verifier(bundle)
    if signature.get("verified") is not True:
        raise ValueError("Bundle changed after signing; no ready manifest was published")
    component_paths = {"app": "Contents/MacOS/kasaterm", "cli": "Contents/MacOS/kasaterm-cli",
                       "web_service": "Contents/MacOS/kasa-serve-web", "pet": "Contents/Resources/kasapet"}
    if any(path not in hashes["files"] for path in component_paths.values()):
        raise ValueError("Required bundle component is missing")
    after = source_snapshot(before["source_root"])
    stable = before.get("snapshot_stable") and after.get("snapshot_stable") and not before.get("excluded_credential_paths") and not after.get("excluded_credential_paths") and all(
        before.get(k) == after.get(k) for k in ("head", "input_digest", "status_digest"))
    source_status = ("stable_dirty" if after.get("dirty") else "stable_clean") if stable else "uncertain"
    source = {"status": source_status, "observed_head": after.get("head"),
              "source_commit": after.get("head") if source_status == "stable_clean" else None,
              "dirty": after.get("dirty"), "before": before, "after": after,
              "configuration": "not_inspected"}
    manifest = {"schema_version": 1, "success": True, "project": before["project"],
                "started_at": before["started_at"], "completed_at": now(), "profile": before["profile"],
                "bundle_path": str(bundle), "bundle_sha256": hashes["sha256"], "files": hashes["files"],
                "components": {name: dict(hashes["files"][path], path=path) for name, path in component_paths.items()},
                "signature": signature, "source": source}
    manifest["id"] = digest(canonical(manifest))
    # Both destinations are outside the signed tree. A copied installed bundle
    # can be identified by its bytes without changing its signature afterward.
    atomic_json(Path(output).parent / "build-manifests" / (manifest["id"] + ".json"), manifest)
    atomic_json(output, manifest)
    return manifest


def validate_manifest(manifest, bundle=None):
    if not isinstance(manifest, dict) or not isinstance(manifest.get("signature"), dict) or manifest.get("schema_version") != 1 or manifest.get("success") is not True or manifest["signature"].get("verified") is not True:
        return False
    expected = manifest.get("id")
    if not expected or digest(canonical({k: v for k, v in manifest.items() if k != "id"})) != expected:
        return False
    try:
        return bundle_hashes(bundle or manifest["bundle_path"])["sha256"] == manifest.get("bundle_sha256")
    except (OSError, ValueError, KeyError):
        return False


def main(argv=None):
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    start = sub.add_parser("begin")
    start.add_argument("--project", required=True)
    start.add_argument("--profile", required=True)
    start.add_argument("--output", required=True)
    done = sub.add_parser("finish")
    done.add_argument("--snapshot", required=True)
    done.add_argument("--bundle", required=True)
    done.add_argument("--output", required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "begin":
            begin(args.project, args.profile, args.output)
        else:
            finish(args.snapshot, args.bundle, args.output)
    except (OSError, ValueError, KeyError, subprocess.SubprocessError):
        print("build provenance unavailable; no new ready manifest published", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
