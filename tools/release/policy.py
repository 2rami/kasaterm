"""Owner-enabled, revocable authorization for the single Mac preview publisher."""

import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import stat
import tempfile
import time

from tools.release.common import Refused, version_tuple

REPOSITORY = "2rami/kasaterm"
REMOTE_URL = "https://github.com/2rami/kasaterm.git"
PREVIEW_FEED = "https://2rami.github.io/kasaterm/appcast-preview.xml"
TEAM = "L366799VND"
SCHEMA = "kasaterm-preview-policy/1"
FIELDS = {"schema", "enabled", "revision", "repository", "remote_url", "branch", "channel", "feed",
          "platforms", "controller", "minor", "team", "keychain", "notary_profile", "unlock_signing",
          "lfs_storage", "created_at_ms", "policy_id", "policy_hash"}


def scope_hash(value):
    return digest({key: item for key, item in value.items()
                   if key not in ("enabled", "revision", "created_at_ms", "policy_id", "policy_hash")})


def lfs_storage(path):
    if not isinstance(path, str) or not Path(path).is_absolute():
        raise Refused("an explicit absolute LFS storage directory is required")
    directory = Path(path)
    if directory.is_symlink() or directory.resolve() != directory:
        raise Refused("LFS storage must use its canonical non-symlink path")
    for item in (directory, directory / "objects"):
        info = item.stat()
        if item.is_symlink() or not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
            raise Refused("LFS cache and objects must be owned by this user and not publicly writable")
    return str(directory)


def digest(value):
    return "sha256:" + hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False,
                                                separators=(",", ":")).encode()).hexdigest()


def identity():
    # An inherited pane variable must not impersonate the controller machine.
    try:
        return (Path(pwd.getpwuid(os.getuid()).pw_dir) / ".config/kasaterm/machine-id").read_text().strip()
    except (OSError, KeyError):
        return ""


def private_dir(path, create=False):
    path = Path(path).absolute()
    if path.is_symlink():
        raise Refused("state directory must not be a symlink")
    if create:
        path.mkdir(parents=True, exist_ok=True, mode=0o700)
    info = path.stat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) & 0o077:
        raise Refused("state directory must be owned by this user and private (0700)")
    return path.resolve()


def read_private(path):
    path = Path(path)
    if path.is_symlink():
        raise Refused("state file must not be a symlink")
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600:
            raise Refused("state file must be owned by this user with mode 0600")
        with os.fdopen(fd, "r") as source:
            fd = None
            return json.load(source)
    finally:
        if fd is not None:
            os.close(fd)


def atomic_json(path, value):
    path = Path(path)
    if path.is_symlink():
        raise Refused("refusing to replace a symlink")
    fd, name = tempfile.mkstemp(prefix="." + path.name + "-", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as target:
            os.fchmod(target.fileno(), 0o600)
            json.dump(value, target, ensure_ascii=False, indent=2)
            target.flush()
            os.fsync(target.fileno())
        os.replace(name, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def seal(value):
    value = {key: item for key, item in value.items() if key not in ("policy_hash", "policy_id")}
    hashed = digest(value)
    return {**value, "policy_id": hashed[7:31], "policy_hash": hashed}


def validate(value):
    if not isinstance(value, dict) or set(value) != FIELDS or seal(value) != value:
        raise Refused("policy fields or hash have changed")
    fixed = {"schema": SCHEMA, "repository": REPOSITORY, "remote_url": REMOTE_URL, "branch": "main",
             "channel": "preview", "feed": PREVIEW_FEED, "platforms": ["macos"], "team": TEAM}
    if any(value.get(key) != item for key, item in fixed.items()):
        raise Refused("policy permits only this repository's macOS preview feed")
    if type(value["enabled"]) is not bool or type(value["unlock_signing"]) is not bool:
        raise Refused("policy switches must be booleans")
    if type(value["revision"]) is not int or value["revision"] < 1:
        raise Refused("invalid policy revision")
    if not isinstance(value["controller"], str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", value["controller"]):
        raise Refused("invalid controller identity")
    if not isinstance(value["minor"], str) or not re.fullmatch(r"\d+\.\d+", value["minor"]):
        raise Refused("minor must be major.minor")
    if (not isinstance(value["keychain"], str) or not Path(value["keychain"]).is_absolute()
            or not isinstance(value["notary_profile"], str)
            or not re.fullmatch(r"[A-Za-z0-9_.-]{1,80}", value["notary_profile"])):
        raise Refused("keychain path and named notary profile are required")
    if type(value["created_at_ms"]) is not int or value["created_at_ms"] < 1:
        raise Refused("invalid policy creation time")
    if not isinstance(value["lfs_storage"], str) or not Path(value["lfs_storage"]).is_absolute():
        raise Refused("an explicit absolute LFS storage directory is required")
    return value


def load(state_dir, require_enabled=True):
    directory = private_dir(state_dir)
    value = validate(read_private(directory / "policy.json"))
    if require_enabled and not value["enabled"]:
        raise Refused("preview publisher policy is disabled")
    return value


def enable(state_dir, controller, minor, keychain=None, notary_profile="AC_NOTARY", unlock_signing=False,
           controller_identity=identity, lfs_store=None):
    if controller_identity() != controller:
        raise Refused("enable must run on the selected controller")
    proposed = Path(state_dir).resolve()
    if any((parent / ".git").exists() for parent in (proposed, *proposed.parents)):
        raise Refused("publisher policy must live outside source checkouts")
    directory = private_dir(state_dir, create=True)
    old = load(directory, require_enabled=False) if (directory / "policy.json").exists() else None
    value = seal({"schema": SCHEMA, "enabled": True, "revision": (old["revision"] + 1) if old else 1,
                  "repository": REPOSITORY, "remote_url": REMOTE_URL, "branch": "main", "channel": "preview",
                  "feed": PREVIEW_FEED, "platforms": ["macos"], "controller": controller, "minor": minor,
                  "team": TEAM, "keychain": str(Path(keychain or Path.home() / "Library/Keychains/codesign.keychain-db").absolute()),
                  "notary_profile": notary_profile, "unlock_signing": bool(unlock_signing),
                  "lfs_storage": lfs_storage(str(Path(lfs_store).resolve()) if lfs_store else (old or {}).get("lfs_storage")),
                  "created_at_ms": int(time.time() * 1000)})
    validate(value)
    atomic_json(directory / "policy.json", value)
    return value


def disable(state_dir):
    value = load(state_dir, require_enabled=False)
    value.update(enabled=False, revision=value["revision"] + 1)
    value = seal(value)
    atomic_json(Path(state_dir) / "policy.json", value)
    return value


class Authorizer:
    def __init__(self, state_dir, expected_hash, requested_commit, planned_commit, controller_identity=identity):
        self.state_dir, self.expected_hash = Path(state_dir), expected_hash
        self.requested_commit, self.planned_commit = requested_commit, planned_commit
        self.controller_identity = controller_identity

    def __call__(self, plan, state, backend, stage):
        from tools.release import fastpatch
        current = load(self.state_dir)
        if current["policy_hash"] != self.expected_hash:
            raise Refused("policy changed; a new authorization is required")
        if self.controller_identity() != current["controller"] or plan.get("controller") != current["controller"]:
            raise Refused("publisher is not the policy controller")
        try:
            valid_hash = fastpatch.plan_hash_ok(plan)
        except (KeyError, TypeError):
            valid_hash = False
        if stage not in ("tag", "release", "feed") or not valid_hash:
            raise Refused("invalid publishing stage or plan hash")
        if plan.get("commit") != self.planned_commit or not re.fullmatch(r"[0-9a-f]{40}", self.requested_commit):
            raise Refused("plan no longer names the exact verified commit")
        if (plan.get("remote") != "origin" or plan.get("branch") != "main" or plan.get("channel") != "preview"
                or plan.get("platforms") != ["macos"] or plan.get("device_ids") or plan.get("devices") or plan.get("ios_build")
                or plan.get("stages") != fastpatch.STAGES
                or plan.get("feed", {}).get("source") != PREVIEW_FEED or plan.get("feed", {}).get("windows") is not None):
            raise Refused("publication is outside the preview-only policy scope")
        version = version_tuple(plan.get("version"))
        minor = tuple(int(part) for part in current["minor"].split("."))
        base = version_tuple((plan.get("base") or {}).get("tag"))
        if not version or not base or version[:2] != minor or version <= base or plan.get("tag") != "v" + plan["version"]:
            raise Refused("only increasing patch versions of the enabled minor are permitted")
        signing = plan.get("mac_artifact") or {}
        if (signing.get("mode") != "local" or (signing.get("identity") or {}).get("team") != TEAM
                or signing.get("keychain") != current["keychain"]
                or signing.get("notary_profile") != current["notary_profile"] or signing.get("problems")):
            raise Refused("signing identity or profile differs from policy")
        remote = backend.git("remote", "get-url", "origin")
        if not remote.ok or remote.out.strip() != REMOTE_URL or backend.slug != REPOSITORY:
            raise Refused("publisher checkout has a different remote")
        record = {"kind": SCHEMA, "policy_id": current["policy_id"], "policy_hash": current["policy_hash"],
                  "revision": current["revision"], "plan_id": plan["plan_id"], "commit": plan["commit"],
                  "requested_commit": self.requested_commit, "controller": current["controller"],
                  "scope_hash": scope_hash(current)}
        previous = state.get("authorization")
        if previous and any(previous.get(key) != value for key, value in record.items()):
            raise Refused("saved publication authorization differs from this policy and plan")
        facts = backend.resume_facts(plan)
        if facts.get("tag_parent"):
            if facts["tag_parent"] != plan["commit"]:
                raise Refused("existing tag belongs to a different source commit")
        else:
            if facts.get("main") != plan["commit"]:
                raise Refused("origin/main advanced before tagging; wait for a new plan")
            if backend.feed_base(plan) != plan["feed_base"]:
                raise Refused("preview feed changed since planning")
        if backend.runner.mode == "live":
            # Once the external transaction starts, local revocation cannot recall CI work already dispatched.
            state.setdefault("publication", {"commit_point": "external_tag_submission_authorized", "tag": plan["tag"],
                                             "source_commit": plan["commit"], "at_ms": int(time.time() * 1000),
                                             "ci_may_finish_after_disable": True})
        return {**record, "stage": stage, "checked_at_ms": int(time.time() * 1000)}
