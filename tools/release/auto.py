"""One-shot preview publisher ticks; launchd supplies scheduling, policy supplies authorization."""

import argparse
import contextlib
import fcntl
import json
import os
from pathlib import Path
import plistlib
import re
import stat
import subprocess
import sys
import time

from tools.release import fastpatch, policy
from tools.release.backend import RealBackend
from tools.release.common import Refused, sha256_file, validate_channel_manifest
from tools.release.proc import Http, Runner

READY_PREFIX = "refs/tags/preview-ready/"
DEFAULT_STATE = Path.home() / ".local/state/kasaterm-preview"
LABEL = "com.kasaterm.preview-publisher"
SHA = re.compile(r"[0-9a-f]{40}")


class Locked(Refused):
    pass


@contextlib.contextmanager
def locked(state_dir):
    root = policy.private_dir(state_dir)
    fd = os.open(root / "publisher.lock", os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600:
            raise Refused("publisher lock is not private")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Locked("another publisher tick is active")
        yield
    finally:
        os.close(fd)


def parse_refs(text):
    head, requests = None, []
    for line in text.splitlines():
        pieces = line.split()
        if len(pieces) != 2:
            raise Refused("invalid remote reference response")
        value, ref = pieces
        if ref == "refs/heads/main":
            if not SHA.fullmatch(value):
                raise Refused("invalid remote main")
            head = value
        elif ref.startswith(READY_PREFIX):
            name = ref[len(READY_PREFIX):]
            if not SHA.fullmatch(name) or value != name:
                raise Refused("ready tag must directly reference the SHA in its name")
            requests.append(name)
    if head is None:
        raise Refused("origin/main is unavailable")
    return head, sorted(set(requests))


def git(repo, *args, env=None):
    base = {**os.environ, "GIT_TERMINAL_PROMPT": "0", "GIT_LFS_SKIP_SMUDGE": "1"}
    result = subprocess.run(["git", "-C", str(repo), *args],
                            env={**base, **(env or {})}, capture_output=True, text=True, timeout=120)
    if result.returncode:
        raise Refused("git " + args[0] + " failed (no interactive login attempted)")
    return result.stdout.strip()


def configure_lfs(repo, config):
    storage = policy.lfs_storage(config["lfs_storage"])
    git(repo, "config", "--local", "lfs.storage", storage)
    if git(repo, "config", "--local", "--get", "lfs.storage") != storage:
        raise Refused("dedicated checkout did not retain the selected LFS cache")


def enqueue(repo, commit):
    if not SHA.fullmatch(commit):
        raise Refused("enqueue requires a full lowercase commit SHA")
    if git(repo, "remote", "get-url", "origin") != policy.REMOTE_URL:
        raise Refused("enqueue supports only the configured kasaterm origin")
    head, ready = parse_refs(git(repo, "ls-remote", "origin", "refs/heads/main", READY_PREFIX + "*"))
    git(repo, "fetch", "--no-tags", "origin", "main")
    if git(repo, "rev-parse", commit + "^{commit}") != commit:
        raise Refused("ready SHA is not a commit")
    git(repo, "merge-base", "--is-ancestor", commit, head)
    if commit in ready:
        return {"commit": commit, "state": "already_queued"}
    # No local tag is created, and a conflicting remote ref is never forced.
    git(repo, "push", "origin", commit + ":" + READY_PREFIX + commit)
    _, ready = parse_refs(git(repo, "ls-remote", "origin", "refs/heads/main", READY_PREFIX + "*"))
    if commit not in ready:
        raise Refused("ready ref was not confirmed after push")
    return {"commit": commit, "state": "queued"}


def contains(repo, older, newer):
    """Whether newer includes older. A commit this checkout never received (a ready ref dropped from main) is not included."""
    for sha in (older, newer):
        if not sha or subprocess.run(["git", "-C", str(repo), "cat-file", "-e", sha + "^{commit}"],
                                     capture_output=True, timeout=60).returncode:
            return False
    result = subprocess.run(["git", "-C", str(repo), "merge-base", "--is-ancestor", older, newer], capture_output=True, timeout=120)
    if result.returncode not in (0, 1):
        raise Refused("cannot compare commit ancestry")
    return result.returncode == 0


def lineage(repo, base, head):
    """Commits after the last publication up to head, newest first. A side branch that never saw base is left out so a
    release never drops what the previous one shipped."""
    return git(repo, "rev-list", "--topo-order", *(["--ancestry-path", base + ".." + head] if base else [head])).splitlines()


class Engine:
    def __init__(self, state_dir):
        self.root = policy.private_dir(state_dir)
        self.repo = self.root / "checkout"
        self.plans = self.root / "plans"

    def controller(self):
        return policy.identity()

    def queue(self, _policy):
        result = subprocess.run(["git", "ls-remote", policy.REMOTE_URL, "refs/heads/main", READY_PREFIX + "*"],
                                env={**os.environ, "GIT_TERMINAL_PROMPT": "0"}, capture_output=True, text=True, timeout=60)
        if result.returncode:
            raise Refused("cannot read ready queue")
        return parse_refs(result.stdout)

    def prepare(self):
        config = policy.load(self.root)
        if self.repo.is_symlink():
            raise Refused("publisher checkout must not be a symlink")
        fresh = not self.repo.exists()
        if fresh:
            result = subprocess.run(["git", "clone", "--no-checkout", policy.REMOTE_URL, str(self.repo)],
                                    env={**os.environ, "GIT_TERMINAL_PROMPT": "0", "GIT_LFS_SKIP_SMUDGE": "1"},
                                    capture_output=True, timeout=180)
            if result.returncode:
                raise Refused("dedicated publisher clone failed")
        if git(self.repo, "remote", "get-url", "origin") != policy.REMOTE_URL:
            raise Refused("dedicated checkout origin changed")
        configure_lfs(self.repo, config)
        git(self.repo, "fetch", "--no-tags", "origin", "+refs/heads/main:refs/remotes/origin/main")
        if fresh:
            git(self.repo, "checkout", "--detach", "refs/remotes/origin/main")

    def orphaned_publication(self, requested, planned):
        self.prepare()
        refs = git(self.repo, "ls-remote", "origin", "refs/tags/v*")
        rows = [line.split() for line in refs.splitlines()]
        if len(rows) > 4096:
            raise Refused("release tag inventory exceeds the audit bound")
        for row in rows:
            if len(row) != 2 or not re.fullmatch(r"refs/tags/v\d+\.\d+\.\d+", row[1]):
                continue
            sha, ref = row
            if not SHA.fullmatch(sha):
                raise Refused("invalid release tag object")
            try:
                git(self.repo, "rev-parse", "--verify", sha + "^{commit}")
            except Refused:
                git(self.repo, "fetch", "--no-tags", "origin", ref)
                git(self.repo, "rev-parse", "--verify", sha + "^{commit}")
            if not git(self.repo, "ls-tree", sha, "--", ".github/release-channel.json"):
                continue
            manifest = json.loads(git(self.repo, "show", sha + ":.github/release-channel.json"))
            validate_channel_manifest(manifest, ref.removeprefix("refs/tags/"), git(self.repo, "rev-parse", sha + "^"))
            if manifest.get("channel") == "preview" and manifest.get("source_commit") in (requested, planned):
                return ref.removeprefix("refs/tags/")
        return None

    def lineage(self, base, head):
        self.prepare()
        return lineage(self.repo, base, head)

    def contains(self, older, newer):
        return contains(self.repo, older, newer)

    def make_plan(self, config, commit):
        self.prepare()
        # Refuse unexpected edits in our own clone rather than resetting them away.
        if git(self.repo, "status", "--porcelain"):
            raise Refused("dedicated publisher checkout is dirty")
        git(self.repo, "checkout", "--detach", commit)
        signing_env = {**os.environ, "KASATERM_SIGN_KEYCHAIN": config["keychain"],
                       "KASATERM_NOTARY_PROFILE": config["notary_profile"]}
        plan = fastpatch.make_plan(self.repo, channel="preview", feed=policy.PREVIEW_FEED, feed_win=None,
                                   devices=[], controller=config["controller"], signing_env=signing_env)
        fastpatch.save_plan(plan, self.plans)
        return plan

    def load_plan(self, plan_id):
        if not isinstance(plan_id, str) or not re.fullmatch(r"[0-9a-f]{16}", plan_id):
            raise Refused("invalid saved plan identity")
        directory = self.plans / plan_id
        if self.plans.is_symlink() or directory.is_symlink():
            raise Refused("saved plan must not use a symlink")
        policy.read_private(directory / "plan.json")
        if (directory / "state.json").exists():
            policy.read_private(directory / "state.json")
        return fastpatch.load(plan_id, self.plans)

    def backend(self, plan, mode, config):
        # Publisher ticks are serialized; shared dependency outputs avoid a
        # full cold rebuild for every patch without reusing verification evidence.
        target = self.root / "target"
        if mode != "dry" or target.exists():
            target = policy.private_dir(target, create=mode != "dry")
        return RealBackend(self.repo, Runner(mode), Http(), fastpatch.plan_dir(plan["plan_id"], self.plans) / "work",
                           lambda _plan, _state: {}, plan["tools"], unlock=config["unlock_signing"], cargo_target=target)

    def facts(self, plan, config):
        backend = self.backend(plan, "dry", config)
        facts = backend.resume_facts(plan)
        if backend.remote_ref("refs/tags/" + plan["tag"]) and not facts.get("tag_parent"):
            raise Refused("remote tag exists but its source is unverified; preserve the original plan")
        return facts

    def run_local(self, plan, config):
        configure_lfs(self.repo, config)
        return fastpatch.run(plan["plan_id"], self.backend(plan, "local", config), self.plans)

    def run_live(self, plan, config, authorizer):
        return fastpatch.run(plan["plan_id"], self.backend(plan, "live", config), self.plans,
                             publisher_authorizer=authorizer)

    def save_state(self, plan, state):
        fastpatch.save_state(plan["plan_id"], state, self.plans)

    def revalidate_started(self, plan, state, config):
        backend = self.backend(plan, "local", config)
        tagged = backend.remote_ref("refs/tags/" + plan["tag"])
        backend.reconcile_tag(plan, tagged)
        tagged_state = state.get("stages", {}).get("tag", {}).get("detail") or {}
        built = state.get("stages", {}).get("build", {}).get("detail") or {}
        record = tagged_state.get("notarized") or {}
        dmg = Path(record.get("dmg") or "")
        if (tagged != tagged_state.get("commit") or tagged != built.get("commit")
                or not record.get("dmg_sha256") or not dmg.is_file() or sha256_file(dmg) != record["dmg_sha256"]):
            raise Refused("original tagged artifact evidence is missing or changed; cannot reauthorize")
        verified = backend.verify_notarized(plan, plan["mac_artifact"], dmg)
        return {"tag_commit": tagged, "dmg_sha256": record["dmg_sha256"], **verified}

    def observe(self, plan, config):
        return self.backend(plan, "dry", config).observe(plan)


def complete(state):
    return all(state.get("stages", {}).get(name, {}).get("status") == "done"
               for name in ("verify", "build", "tag", "release", "feed"))


def load_ledger(root):
    path = Path(root) / "queue.json"
    ledger = policy.read_private(path) if path.exists() else {"schema": "kasaterm-preview-queue/1", "requests": {}, "active": None}
    if (not isinstance(ledger, dict) or ledger.get("schema") != "kasaterm-preview-queue/1"
            or not isinstance(ledger.get("requests"), dict)
            or any(not SHA.fullmatch(sha) or not isinstance(entry, dict) for sha, entry in ledger["requests"].items())
            or ledger.get("active") is not None and ledger["active"] not in ledger["requests"]):
        raise Refused("invalid queue state")
    return ledger


def recover(state_dir, plan_id, replan=False, engine=None):
    root = policy.private_dir(state_dir)
    engine = engine or Engine(root)
    with locked(root):
        config = policy.load(root)
        if engine.controller() != config["controller"]:
            raise Refused("recovery must run on the enabled controller")
        ledger = load_ledger(root)
        matches = [(sha, entry) for sha, entry in ledger["requests"].items() if entry.get("plan_id") == plan_id]
        if len(matches) != 1 or ledger.get("active") not in (None, matches[0][0]):
            raise Refused("recovery requires one exact saved plan and no different active publication")
        request, entry = matches[0]
        plan, state = engine.load_plan(plan_id)
        try:
            valid_hash = fastpatch.plan_hash_ok(plan)
        except (KeyError, TypeError):
            valid_hash = False
        if (not valid_hash or plan.get("plan_id") != plan_id
                or plan.get("commit") != entry.get("planned_commit")):
            raise Refused("saved recovery plan identity changed")
        backend = engine.backend(plan, "dry", config)
        facts = engine.facts(plan, config)
        tagged = bool(facts.get("tag_parent"))
        if state.get("stages", {}).get("tag", {}).get("status") == "done" and not tagged:
            raise Refused("previously published tag is missing; do not create another release")
        if replan:
            if tagged:
                raise Refused("an existing tag must resume its original plan; cannot replan")
            if policy.load(root)["policy_hash"] != config["policy_hash"]:
                raise Refused("policy changed during recovery")
            history = entry.get("cancelled_plans", []) + [plan_id]
            ledger["requests"][request] = {"state": "queued", "cancelled_plans": history}
            ledger["active"] = None
            result = {"state": "queued_for_replanning", "request": request, "discarded_plan": plan_id}
        else:
            if entry.get("policy_scope") != policy.scope_hash(config):
                raise Refused("reauthorization requires exactly the same policy scope")
            old = state.get("authorization")
            expected = {"plan_id": plan_id, "commit": entry["planned_commit"], "requested_commit": request,
                        "scope_hash": policy.scope_hash(config)}
            if old and (any(old.get(key) != value for key, value in expected.items())
                        or old.get("policy_hash") not in (entry["policy_hash"], config["policy_hash"])):
                raise Refused("saved authorization identity changed")
            if state.get("approval"):
                raise Refused("cannot recover a Nacho-approved plan as a publisher plan")
            authorizer = policy.Authorizer(root, config["policy_hash"], request, entry["planned_commit"], engine.controller)
            check_state = {key: value for key, value in state.items() if key != "authorization"}
            record = authorizer(plan, check_state, backend, "tag")
            if tagged:
                if not old or not fastpatch.live_ready(plan, state):
                    raise Refused("tagged recovery requires original verification and build evidence")
                evidence = engine.revalidate_started(plan, state, config)
            else:
                # Unpublished artifacts are rebuilt under the new authorization rather than trusting stale signatures.
                for stage in ("verify", "build"):
                    state["stages"].pop(stage, None)
                evidence = {"reverify_and_rebuild_required": True}
            if policy.load(root)["policy_hash"] != config["policy_hash"]:
                raise Refused("policy changed during recovery")
            state.setdefault("reauthorizations", []).append({"previous": old, "current": record, "evidence": evidence})
            state["authorization"] = record
            engine.save_state(plan, state)
            entry.update(policy_hash=config["policy_hash"], policy_id=config["policy_id"], state="prepared")
            ledger["active"] = request
            result = {"state": "reauthorized", "request": request, "plan_id": plan_id, "evidence": evidence}
        policy.atomic_json(root / "queue.json", ledger)
        return result


def status(state_dir, observe=False, engine=None):
    config = policy.load(state_dir, require_enabled=False)
    ledger = load_ledger(state_dir)
    result = {"policy": config, "queue": ledger,
              "revocation_scope": "Disables future controller stages; already-started external CI may finish the exact tagged release."}
    request = ledger.get("active")
    if request:
        engine = engine or Engine(state_dir)
        plan, state = engine.load_plan(ledger["requests"][request]["plan_id"])
        result["publication"] = state.get("publication")
        result["recovery"] = "enable the same scope, then explicitly reauthorize this plan" if not config["enabled"] else None
        if observe:
            if engine.controller() != config["controller"]:
                raise Refused("observation must run on the policy controller")
            result["remote"] = engine.observe(plan, config)
    return result


def tick(state_dir, engine=None):
    config = policy.load(state_dir)
    root = policy.private_dir(state_dir)
    engine = engine or Engine(root)
    if engine.controller() != config["controller"]:
        raise Refused("this machine is not the enabled controller")
    with locked(root):
        config = policy.load(root)
        ledger_path = root / "queue.json"
        ledger = load_ledger(root)
        def save():
            ledger["checked_at_ms"] = int(time.time() * 1000)
            policy.atomic_json(ledger_path, ledger)
        head, ready = engine.queue(config)
        ledger["remote_main"] = head
        published = [entry for entry in ledger["requests"].values() if entry.get("state") == "done" and entry.get("completed_at_ms")]
        last = max(published, key=lambda entry: entry["completed_at_ms"])["planned_commit"] if published else None
        # Only exact registered commits ship: an unregistered tip (phone, docs, relay) must not hold back the desktop
        # release, and must not ride along in it either.
        after = engine.lineage(last, head)
        candidates = [sha for sha in ready if ledger["requests"].get(sha, {}).get("state") != "done"]
        for sha in list(candidates):
            if sha not in after and sha != ledger.get("active") and last and engine.contains(sha, last):
                ledger["requests"].setdefault(sha, {}).update(state="done", reason="already covered by a completed publication")
                candidates.remove(sha)
        eligible = [sha for sha in after if sha in candidates]
        request = ledger.get("active")
        if request:
            entry = ledger["requests"][request]
            plan, prior = engine.load_plan(entry["plan_id"])
            facts = engine.facts(plan, config)
            if not facts.get("tag_parent"):
                stale = policy.untagged_stale(plan, facts, engine.backend(plan, "dry", config))
                # A built plan still ships even if newer work arrived; an unbuilt one (failed checks) yields to it.
                if not stale and eligible and eligible[0] != request and not fastpatch.live_ready(plan, prior):
                    stale = "a newer ready commit supersedes this unbuilt plan"
                if stale:
                    entry.update(state="waiting_main", reason=stale, observed_main=head)
                    ledger["active"] = None
                    request = None
                    save()
        if request is None:
            request = eligible[0] if eligible else None
            for sha in candidates:
                if sha != request:
                    reason = "a newer ready commit goes first" if sha in eligible else "not on origin/main after the last publication"
                    ledger["requests"].setdefault(sha, {}).update(state="waiting_main", reason=reason, observed_main=head)
            if request is None:
                save()
                return {"state": "waiting_ready" if candidates else "idle", "remote_main": head}
            old = ledger["requests"].get(request, {})
            if old.get("policy_hash") and old["policy_hash"] != config["policy_hash"]:
                raise Refused("queued request belongs to an older policy revision")
            orphan = engine.orphaned_publication(request, request)
            if orphan:
                ledger["requests"].setdefault(request, {}).update(state="blocked", reason="existing preview tag requires its original plan", tag=orphan)
                save()
                raise Refused("existing preview tag requires its original plan; refusing a new release")
            plan = engine.make_plan(config, request)
            entry = {"state": "prepared", "plan_id": plan["plan_id"], "planned_commit": request,
                     "policy_hash": config["policy_hash"], "policy_id": config["policy_id"], "policy_scope": policy.scope_hash(config)}
            ledger["requests"][request] = entry
            ledger["active"] = request
            save()
        entry = ledger["requests"][request]
        plan, prior = engine.load_plan(entry["plan_id"])
        if entry["policy_hash"] != config["policy_hash"]:
            raise Refused("active publication belongs to an older policy revision")
        authorizer = policy.Authorizer(root, entry["policy_hash"], request, entry["planned_commit"], engine.controller)
        try:
            authorizer(plan, prior, engine.backend(plan, "dry", config), "tag")
            state = engine.run_local(plan, config)
            if not fastpatch.live_ready(plan, state):
                entry.update(state="blocked", reason="verification or build is not ready")
                save()
                return {"state": "blocked", "request": request, "plan_id": plan["plan_id"]}
            state = engine.run_live(plan, config, authorizer)
            entry["publication"] = state.get("publication")
            if complete(state):
                entry.update(state="done", completed_at_ms=int(time.time() * 1000))
                entry.pop("reason", None)
                ledger["active"] = None
            else:
                entry.update(state="waiting", reason="publication has unfinished stages")
            save()
            return {"state": entry["state"], "request": request, "plan_id": plan["plan_id"]}
        except Refused as error:
            _, failed = engine.load_plan(plan["plan_id"])
            entry["publication"] = failed.get("publication")
            entry.update(state="blocked", reason=str(error))
            save()
            raise


def service_spec(source, state_dir, python, interval=60):
    source, python = Path(source).resolve(), Path(python).resolve()
    root = policy.private_dir(state_dir)
    cache = Path(policy.load(root, require_enabled=False)["lfs_storage"])
    if root == source or source in root.parents:
        raise Refused("policy/state must live outside the source repository")
    for folder in ("Desktop", "Documents", "Downloads"):
        protected = Path.home() / folder
        if any(path == protected or protected in path.parents for path in (source, root, cache)):
            raise Refused("background publisher source/state/LFS cache must be outside TCC-protected user folders")
    if not python.is_file() or not (source / "tools/release/auto.py").is_file() or interval < 30:
        raise Refused("absolute Python/source paths and interval >=30 are required")
    return {"Label": LABEL, "ProgramArguments": [str(python), "-m", "tools.release.auto", "--state-dir", str(root), "tick"],
            "WorkingDirectory": str(source), "RunAtLoad": True, "StartInterval": interval, "ProcessType": "Background",
            "ThrottleInterval": 30, "StandardOutPath": str(root / "publisher.log"), "StandardErrorPath": str(root / "publisher.log"),
            "EnvironmentVariables": {"PYTHONDONTWRITEBYTECODE": "1", "PATH": ":".join([str(python.parent), str(Path.home()/".cargo/bin"),
                str(Path.home()/".local/bin"), "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"])}}


def install(source, state_dir, python, interval=60, apply=False):
    config = policy.load(state_dir)
    if policy.identity() != config["controller"]:
        raise Refused("install must run on the enabled controller")
    spec = service_spec(source, state_dir, python, interval)
    target = Path.home() / "Library/LaunchAgents" / (LABEL + ".plist")
    if not apply:
        return {"state": "preview", "path": str(target), "spec": spec}
    if sys.platform != "darwin" or target.is_symlink():
        raise Refused("LaunchAgent installation requires macOS and a non-symlink target")
    if target.exists():
        previous = plistlib.loads(target.read_bytes())
        if previous.get("Label") != LABEL or previous.get("WorkingDirectory") != spec["WorkingDirectory"]:
            raise Refused("existing LaunchAgent belongs to another source")
        raise Refused("LaunchAgent already exists; disable and explicitly remove its registration before reinstalling")
    target.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as output:
        output.write(plistlib.dumps(spec))
        output.flush()
        os.fsync(output.fileno())
    result = subprocess.run(["launchctl", "bootstrap", f"gui/{os.getuid()}", str(target)], capture_output=True)
    if result.returncode:
        target.unlink()
        raise Refused("LaunchAgent bootstrap failed; new registration file removed")
    return {"state": "installed", "label": LABEL}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-dir", type=Path, default=DEFAULT_STATE)
    commands = parser.add_subparsers(dest="command", required=True)
    enable = commands.add_parser("enable", help="explicitly create/revise the standing preview policy; does not publish")
    enable.add_argument("--controller", required=True)
    enable.add_argument("--minor", required=True)
    enable.add_argument("--keychain")
    enable.add_argument("--notary-profile", default="AC_NOTARY")
    enable.add_argument("--unlock-signing", action="store_true")
    enable.add_argument("--lfs-storage", type=Path, required=True, help="existing canonical controller LFS store containing objects/")
    commands.add_parser("disable", help="stop future controller stages; already-started external CI may still finish")
    commands.add_parser("status", help="read local policy/queue only")
    commands.add_parser("observe", help="read active release/CI/feed even while disabled; never resumes publication")
    recovery = commands.add_parser("reauthorize", help="explicitly reauthorize one original plan under the same policy scope")
    recovery.add_argument("plan_id")
    replan = commands.add_parser("replan", help="discard only an untagged saved plan and queue fresh verification")
    replan.add_argument("plan_id")
    commands.add_parser("tick", help="one controller cycle; may publish only under the enabled policy")
    queue = commands.add_parser("enqueue", help="push only an idempotent preview-ready/<SHA> tag for a verified main commit")
    queue.add_argument("commit")
    queue.add_argument("--repo", type=Path, default=Path.cwd())
    service = commands.add_parser("install", help="preview LaunchAgent; --apply registers the periodic controller")
    service.add_argument("--source", type=Path, required=True)
    service.add_argument("--python", type=Path, default=Path(sys.executable))
    service.add_argument("--interval", type=int, default=60)
    service.add_argument("--apply", action="store_true")
    args = parser.parse_args(argv)
    try:
        if args.command == "enable":
            result = policy.enable(args.state_dir, args.controller, args.minor, args.keychain, args.notary_profile, args.unlock_signing,
                                   lfs_store=args.lfs_storage)
        elif args.command == "disable":
            result = policy.disable(args.state_dir)
        elif args.command == "enqueue":
            result = enqueue(args.repo, args.commit)
        elif args.command == "install":
            result = install(args.source, args.state_dir, args.python, args.interval, args.apply)
        elif args.command in ("status", "observe"):
            result = status(args.state_dir, observe=args.command == "observe")
        elif args.command in ("reauthorize", "replan"):
            result = recover(args.state_dir, args.plan_id, replan=args.command == "replan")
        else:
            if not policy.load(args.state_dir, require_enabled=False)["enabled"]:
                result = {"state": "disabled"}
            else:
                result = tick(args.state_dir)
        print(json.dumps(result, ensure_ascii=False))
        return 0
    except Locked:
        print(json.dumps({"state": "locked"}))
        return 0
    except (Refused, OSError, ValueError, subprocess.SubprocessError) as error:
        print(json.dumps({"state": "blocked", "error": str(error)}, ensure_ascii=False))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
