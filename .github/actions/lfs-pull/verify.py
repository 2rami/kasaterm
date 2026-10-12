import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys


POINTER_VERSION = b"version https://git-lfs.github.com/spec/v1"


def safe_relative(name):
    if not isinstance(name, str) or not name or "\\" in name or ":" in name:
        raise ValueError("invalid LFS path")
    path = PurePosixPath(name)
    if path.is_absolute() or any(part in ("", ".", "..") for part in name.split("/")):
        raise ValueError("invalid LFS path")
    return path


def required_files():
    skips = os.environ.get("SKIP", "").split()
    for prefix in skips:
        if not prefix.endswith("/") or any(char in prefix for char in "*?[]!,"):
            raise ValueError("skip must contain relative directory names ending in /")
        safe_relative(prefix[:-1])
    result = subprocess.run(
        ["git", "-c", "lfs.fetchinclude=", "-c", "lfs.fetchexclude=",
         "lfs", "ls-files", "--json", "HEAD"],
        check=True, stdout=subprocess.PIPE,
    )
    manifest = json.loads(result.stdout)
    if not isinstance(manifest, dict) or not isinstance(manifest.get("files"), list):
        raise ValueError("invalid LFS manifest")
    files = []
    seen = set()
    root = Path.cwd().resolve()
    for item in manifest["files"]:
        if not isinstance(item, dict):
            raise ValueError("invalid LFS entry")
        name, oid, size = item.get("name"), item.get("oid"), item.get("size")
        safe_relative(name)
        if (name in seen or item.get("oid_type") != "sha256"
                or not isinstance(oid, str) or re.fullmatch(r"[0-9a-f]{64}", oid) is None
                or type(size) is not int or size < 0):
            raise ValueError("invalid LFS identity")
        seen.add(name)
        if not any(name.startswith(prefix) for prefix in skips):
            root.joinpath(*PurePosixPath(name).parts).resolve().relative_to(root)
            files.append((name, oid, size))
    if not files:
        raise ValueError("no required LFS files remain to verify")
    return files


def verify(files, checkout):
    if checkout:
        # Explicit paths keep skipped directories out of local materialization too.
        for offset in range(0, len(files), 20):
            subprocess.run(
                ["git", "lfs", "checkout", "--"]
                + [item[0] for item in files[offset:offset + 20]],
                check=False,
            )
    failures = []
    root = Path.cwd().resolve()
    for name, oid, size in files:
        target = root.joinpath(*PurePosixPath(name).parts)
        try:
            target.resolve().relative_to(root)
            if not stat.S_ISREG(target.lstat().st_mode):
                raise ValueError("not a regular file")
            with target.open("rb") as source:
                first = source.read(128)
                if first.splitlines()[:1] == [POINTER_VERSION]:
                    raise ValueError("LFS pointer remains")
                source.seek(0)
                digest = hashlib.sha256()
                actual_size = 0
                for chunk in iter(lambda: source.read(1024 * 1024), b""):
                    actual_size += len(chunk)
                    digest.update(chunk)
            if actual_size != size:
                raise ValueError("size mismatch")
            if digest.hexdigest() != oid:
                raise ValueError("SHA256 mismatch")
        except (OSError, ValueError) as error:
            failures.append("{!r}: {}".format(name, error))
    if failures:
        level = "warning" if checkout else "error"
        print("::{}::{} required LFS file(s) failed verification".format(level, len(failures)))
        for failure in failures[:20]:
            print(failure)
        return 1
    print("Verified {} required LFS files (size + SHA256; no pointers).".format(len(files)))
    return 0


def main():
    sys.stdout.reconfigure(encoding="utf-8", errors="backslashreplace")
    sys.stderr.reconfigure(encoding="utf-8", errors="backslashreplace")
    parser = argparse.ArgumentParser()
    parser.add_argument("--checkout", action="store_true")
    args = parser.parse_args()
    try:
        return verify(required_files(), args.checkout)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print("::error::Cannot establish required LFS assets: {}".format(error))
        return 2


if __name__ == "__main__":
    sys.exit(main())
