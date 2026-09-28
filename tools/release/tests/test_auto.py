import json
import os
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest import mock

from tools.release import auto, fastpatch, policy
from tools.release.common import Pending, Refused, sha256_bytes
from tools.release.proc import Result, Runner

A, B = "a" * 40, "b" * 40
CONTROLLER = "mini-controller"


def plan_for(config, commit=A):
    core = {"schema": fastpatch.SCHEMA, "commit": commit, "branch": "main", "remote": "origin",
            "version": "0.2.2", "tag": "v0.2.2", "channel": "preview", "platforms": ["macos"],
            "ios_build": None, "devices": [], "device_ids": [], "controller": CONTROLLER,
            "feed_base": "sha256:feed", "stages": list(fastpatch.STAGES)}
    return {**core, "plan_id": sha256_bytes(fastpatch.canonical(core))[7:23], "errors": [], "live_blocks": [],
            "base": {"tag": "v0.2.1"}, "feed": {"source": policy.PREVIEW_FEED, "windows": None},
            "mac_artifact": {"mode": "local", "identity": {"team": policy.TEAM}, "keychain": config["keychain"],
                             "notary_profile": config["notary_profile"], "problems": []}}


def rehash(plan):
    plan["plan_id"] = sha256_bytes(fastpatch.canonical(fastpatch.core_of(plan)))[7:23]
    return plan


class FakeBackend:
    slug = policy.REPOSITORY

    def __init__(self, engine, mode="live"):
        self.engine, self.runner = engine, Runner(mode)

    def git(self, *args):
        assert args == ("remote", "get-url", "origin")
        return Result(0, self.engine.remote_url)

    def feed_base(self, _plan):
        return self.engine.feed

    def resume_facts(self, plan):
        return {"main": self.engine.head, "tag_parent": self.engine.tags.get(plan["tag"])}

    def tag(self, plan, _state):
        self.engine.calls.append("tag")
        self.engine.tags[plan["tag"]] = plan["commit"]
        if self.engine.disable_after_tag:
            policy.disable(self.engine.root)
        return {}

    def release(self, _plan, _state):
        self.engine.calls.append("release")
        if self.engine.waiting:
            raise Pending("CI is still building")
        return {}

    def feed(self, _plan, _state):
        self.engine.calls.append("feed")
        return {}

    def devices(self, _plan, _state):
        return {}


class FakeEngine:
    def __init__(self, root):
        self.root = Path(root)
        self.plans = self.root / "plans"
        self.head, self.ready = A, [A]
        self.feed = "sha256:feed"
        self.remote_url = policy.REMOTE_URL
        self.tags, self.calls = {}, []
        self.waiting = False
        self.disable_after_tag = False
        self.metadata = False
        self.orphan = None
        self.local_ready = True
        self.created = 0
        self.signature_ok = True
        self.revalidated = 0

    def controller(self):
        return CONTROLLER

    def queue(self, _policy):
        return self.head, self.ready

    def can_advance(self, _older, _newer):
        return self.metadata

    def orphaned_publication(self, _requested, _planned):
        return self.orphan

    def make_plan(self, config, commit):
        self.created += 1
        plan = plan_for(config, commit)
        plan["feed_base"] = self.feed
        rehash(plan)
        fastpatch.save_plan(plan, self.plans)
        return plan

    def load_plan(self, plan_id):
        return fastpatch.load(plan_id, self.plans)

    def facts(self, plan, _config):
        return FakeBackend(self).resume_facts(plan)

    def backend(self, _plan, mode, _config):
        return FakeBackend(self, mode)

    def run_local(self, plan, _config):
        _, state = self.load_plan(plan["plan_id"])
        for stage in ("verify", "build"):
            state["stages"][stage] = {"status": "done" if self.local_ready else "failed"}
        fastpatch.save_state(plan["plan_id"], state, self.plans)
        return state

    def run_live(self, plan, _config, authorizer):
        return fastpatch.run(plan["plan_id"], FakeBackend(self), self.plans, publisher_authorizer=authorizer)

    def save_state(self, plan, state):
        fastpatch.save_state(plan["plan_id"], state, self.plans)

    def revalidate_started(self, _plan, _state, _config):
        self.revalidated += 1
        if not self.signature_ok:
            raise Refused("original signed artifact changed")
        return {"dmg_sha256": "sha256:fixture", "notarized": True}

    def observe(self, _plan, _config):
        return {"tag": next(iter(self.tags), None)}


class PolicyFixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name) / "publisher"
        self.lfs = Path(self.temp.name) / "lfs"
        (self.lfs / "objects").mkdir(parents=True)
        self.config = policy.enable(self.root, CONTROLLER, "0.2", controller_identity=lambda: CONTROLLER, lfs_store=self.lfs)
        self.engine = FakeEngine(self.root)

    def tearDown(self):
        self.temp.cleanup()

    def auth(self, plan=None):
        plan = plan or plan_for(self.config)
        return policy.Authorizer(self.root, self.config["policy_hash"], A, plan["commit"], lambda: CONTROLLER)

    def test_policy_has_private_owner_files_and_no_secret_fields(self):
        self.assertEqual((self.root / "policy.json").stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.root.stat().st_mode & 0o777, 0o700)
        self.assertEqual(set(self.config), policy.FIELDS)
        self.assertEqual(policy.load(self.root), self.config)

    def test_controller_identity_ignores_inherited_home_and_pane_environment(self):
        account = mock.Mock(pw_dir=str(self.root))
        machine = self.root / ".config/kasaterm/machine-id"
        machine.parent.mkdir(parents=True)
        machine.write_text(CONTROLLER)
        with mock.patch.object(policy.pwd, "getpwuid", return_value=account), mock.patch.dict(os.environ, {
                "HOME": "/not-the-account-home", "KASATERM_MACHINE_ID": "another-machine"}):
            self.assertEqual(policy.identity(), CONTROLLER)

    def test_permission_and_symlink_changes_are_refused(self):
        path = self.root / "policy.json"
        path.chmod(0o644)
        with self.assertRaises(Refused): policy.load(self.root)
        path.chmod(0o600)
        path.rename(self.root / "saved.json")
        path.symlink_to(self.root / "saved.json")
        with self.assertRaises(Refused): policy.load(self.root)

    def test_wrong_owner_is_refused(self):
        with mock.patch.object(policy.os, "getuid", return_value=os.getuid() + 1):
            with self.assertRaises(Refused): policy.load(self.root)

    def test_disabled_or_revised_policy_stops_existing_authorizer(self):
        authorizer = self.auth()
        plan = plan_for(self.config)
        policy.disable(self.root)
        with self.assertRaises(Refused): authorizer(plan, {}, FakeBackend(self.engine), "tag")
        policy.enable(self.root, CONTROLLER, "0.2", controller_identity=lambda: CONTROLLER)
        with self.assertRaisesRegex(Refused, "policy changed"):
            authorizer(plan, {}, FakeBackend(self.engine), "tag")

    def test_tampered_policy_hash_and_unknown_fields_are_refused(self):
        value = dict(self.config, team="DIFFERENT")
        policy.atomic_json(self.root / "policy.json", value)
        with self.assertRaises(Refused): policy.load(self.root)

    def test_resealed_wrong_types_and_in_repo_policy_are_refused(self):
        for key in ("controller", "minor", "keychain", "notary_profile", "created_at_ms"):
            with self.subTest(key=key), self.assertRaises(Refused):
                policy.validate(policy.seal({**self.config, key: []}))
        repo = self.root / "source"
        (repo / ".git").mkdir(parents=True)
        alias = self.root / "source-alias"
        alias.symlink_to(repo, target_is_directory=True)
        with self.assertRaisesRegex(Refused, "outside"):
            policy.enable(alias / "publisher", CONTROLLER, "0.2", controller_identity=lambda: CONTROLLER)
        value = policy.seal({**self.config, "private_key": "not-a-key"})
        policy.atomic_json(self.root / "policy.json", value)
        with self.assertRaises(Refused): policy.load(self.root)

    def test_policy_scope_rejects_stable_windows_new_minor_and_foreign_remote(self):
        for change in ({"channel": "stable"}, {"platforms": ["macos", "windows"]},
                       {"version": "0.3.0", "tag": "v0.3.0"}, {"device_ids": ["foreign"]},
                       {"feed": {"source": fastpatch.MAC_FEED, "windows": None}}):
            plan = rehash({**plan_for(self.config), **change})
            with self.subTest(change=change), self.assertRaises(Refused):
                self.auth(plan)(plan, {}, FakeBackend(self.engine), "tag")
        self.engine.remote_url = "https://example.invalid/another.git"
        with self.assertRaises(Refused): self.auth()(plan_for(self.config), {}, FakeBackend(self.engine), "tag")

    def test_plan_hash_and_exact_sha_are_checked(self):
        plan = plan_for(self.config)
        authorizer = self.auth(plan)
        plan["commit"] = B
        with self.assertRaises(Refused): authorizer(plan, {}, FakeBackend(self.engine), "tag")
        rehash(plan)
        with self.assertRaises(Refused): authorizer(plan, {}, FakeBackend(self.engine), "tag")

    def test_authorization_records_policy_plan_sha_and_rechecks_remote(self):
        plan = plan_for(self.config)
        authorizer = self.auth(plan)
        first = authorizer(plan, {}, FakeBackend(self.engine), "tag")
        self.assertEqual(first["policy_hash"], self.config["policy_hash"])
        self.assertEqual(first["commit"], A)
        self.assertEqual(first["plan_id"], plan["plan_id"])
        self.engine.tags[plan["tag"]] = A
        self.engine.feed = "sha256:already-published"
        resumed = authorizer(plan, {"authorization": first}, FakeBackend(self.engine), "feed")
        self.assertEqual(resumed["stage"], "feed")
        first["commit"] = B
        with self.assertRaises(Refused): authorizer(plan, {"authorization": first}, FakeBackend(self.engine), "feed")

    def test_remote_main_movement_before_tag_is_refused(self):
        self.engine.head = B
        with self.assertRaisesRegex(Refused, "advanced"):
            self.auth()(plan_for(self.config), {}, FakeBackend(self.engine), "tag")

    def test_queue_reference_mismatch_is_refused(self):
        with self.assertRaises(Refused):
            auto.parse_refs(f"{A}\trefs/heads/main\n{B}\t{auto.READY_PREFIX}{A}\n")
        self.assertEqual(auto.parse_refs(f"{A}\trefs/heads/main\n{A}\t{auto.READY_PREFIX}{A}\n"), (A, [A]))

    def test_enqueue_is_idempotent_and_uses_only_the_ready_ref(self):
        calls = []
        def fake_git(_repo, *args, **_kwargs):
            calls.append(args)
            if args[:3] == ("remote", "get-url", "origin"): return policy.REMOTE_URL
            if args[0] == "ls-remote": return f"{A}\trefs/heads/main\n{A}\t{auto.READY_PREFIX}{A}"
            if args[0] == "rev-parse": return A
            return ""
        with mock.patch.object(auto, "git", side_effect=fake_git):
            self.assertEqual(auto.enqueue(self.root, A)["state"], "already_queued")
        self.assertFalse(any(call[0] == "push" for call in calls))

    def test_enqueue_pushes_only_exact_sha_ready_ref(self):
        calls = []
        def fake_git(_repo, *args, **_kwargs):
            calls.append(args)
            if args[:3] == ("remote", "get-url", "origin"): return policy.REMOTE_URL
            if args[0] == "ls-remote":
                suffix = f"\n{A}\t{auto.READY_PREFIX}{A}" if any(call[0] == "push" for call in calls) else ""
                return f"{A}\trefs/heads/main" + suffix
            if args[0] == "rev-parse": return A
            return ""
        with mock.patch.object(auto, "git", side_effect=fake_git):
            self.assertEqual(auto.enqueue(self.root, A)["state"], "queued")
        self.assertEqual([call for call in calls if call[0] == "push"], [("push", "origin", A + ":" + auto.READY_PREFIX + A)])

    def test_single_flock_blocks_another_tick(self):
        with auto.locked(self.root):
            with self.assertRaises(auto.Locked):
                with auto.locked(self.root): pass

    def test_completed_queue_is_not_published_twice(self):
        self.assertEqual(auto.tick(self.root, self.engine)["state"], "done")
        self.assertEqual(auto.tick(self.root, self.engine)["state"], "idle")
        self.assertEqual(self.engine.calls, ["tag", "release", "feed"])
        self.assertEqual(self.engine.created, 1)

    def test_waiting_ci_is_not_completion_and_resumes_same_plan(self):
        self.engine.waiting = True
        first = auto.tick(self.root, self.engine)
        self.assertEqual(first["state"], "waiting")
        self.engine.head, self.engine.waiting = B, False
        second = auto.tick(self.root, self.engine)
        self.assertEqual(second["state"], "done")
        self.assertEqual(first["plan_id"], second["plan_id"])
        self.assertEqual(self.engine.created, 1)
        self.assertEqual(self.engine.calls.count("tag"), 1)

    def test_pre_tag_feed_change_replans_but_never_resigns_published_tag(self):
        self.engine.local_ready = False
        first = auto.tick(self.root, self.engine)
        self.engine.feed, self.engine.local_ready = "sha256:preview-now-exists", True
        second = auto.tick(self.root, self.engine)
        self.assertEqual(second["state"], "done")
        self.assertNotEqual(first["plan_id"], second["plan_id"])
        self.assertEqual(self.engine.created, 2)
        self.assertEqual(self.engine.calls.count("tag"), 1)

    def test_covered_metadata_queue_does_not_republish_same_source(self):
        self.engine.head, self.engine.ready = B, [A, B]
        self.assertEqual(auto.tick(self.root, self.engine)["state"], "done")
        self.engine.metadata = True
        self.assertEqual(auto.tick(self.root, self.engine)["state"], "idle")
        self.assertEqual(self.engine.created, 1)

    def test_invalid_queue_and_plan_id_are_rejected(self):
        policy.atomic_json(self.root / "queue.json", {"schema": "kasaterm-preview-queue/1", "requests": {}, "active": A})
        with self.assertRaisesRegex(Refused, "queue"):
            auto.tick(self.root, self.engine)
        with self.assertRaisesRegex(Refused, "plan identity"):
            auto.Engine(self.root).load_plan("../../policy")

    def test_revoke_between_stages_prevents_later_publication(self):
        self.engine.disable_after_tag = True
        with self.assertRaises(Refused): auto.tick(self.root, self.engine)
        self.assertEqual(self.engine.calls, ["tag"])

    def test_explicit_same_scope_reauthorization_resumes_original_tag(self):
        self.engine.waiting = True
        first = auto.tick(self.root, self.engine)
        policy.disable(self.root)
        policy.enable(self.root, CONTROLLER, "0.2", controller_identity=lambda: CONTROLLER)
        with self.assertRaisesRegex(Refused, "older policy"):
            auto.tick(self.root, self.engine)
        got = auto.recover(self.root, first["plan_id"], engine=self.engine)
        self.assertEqual(got["state"], "reauthorized")
        self.engine.waiting = False
        self.assertEqual(auto.tick(self.root, self.engine)["state"], "done")
        self.assertEqual(self.engine.revalidated, 1)
        self.assertEqual(self.engine.created, 1)
        self.assertEqual(self.engine.calls.count("tag"), 1)

    def test_reauthorization_refuses_scope_change_and_damaged_signature(self):
        self.engine.waiting = True
        first = auto.tick(self.root, self.engine)
        policy.disable(self.root)
        policy.enable(self.root, CONTROLLER, "0.2", controller_identity=lambda: CONTROLLER)
        self.engine.signature_ok = False
        with self.assertRaisesRegex(Refused, "signed artifact"):
            auto.recover(self.root, first["plan_id"], engine=self.engine)
        policy.enable(self.root, CONTROLLER, "0.3", controller_identity=lambda: CONTROLLER)
        with self.assertRaisesRegex(Refused, "same policy scope"):
            auto.recover(self.root, first["plan_id"], engine=self.engine)
        with self.assertRaisesRegex(Refused, "existing tag"):
            auto.recover(self.root, first["plan_id"], replan=True, engine=self.engine)

    def test_pre_tag_reauthorization_requires_fresh_verification(self):
        self.engine.local_ready = False
        first = auto.tick(self.root, self.engine)
        policy.disable(self.root)
        policy.enable(self.root, CONTROLLER, "0.2", controller_identity=lambda: CONTROLLER)
        auto.recover(self.root, first["plan_id"], engine=self.engine)
        _, state = self.engine.load_plan(first["plan_id"])
        self.assertNotIn("verify", state["stages"])
        self.assertNotIn("build", state["stages"])
        self.assertEqual(auto.tick(self.root, self.engine)["state"], "blocked")
        self.assertEqual(self.engine.calls, [])

    def test_untagged_replan_recovers_changed_feed_after_reenable(self):
        self.engine.local_ready = False
        first = auto.tick(self.root, self.engine)
        policy.disable(self.root)
        policy.enable(self.root, CONTROLLER, "0.2", controller_identity=lambda: CONTROLLER)
        self.engine.feed, self.engine.local_ready = "sha256:new-base", True
        auto.recover(self.root, first["plan_id"], replan=True, engine=self.engine)
        second = auto.tick(self.root, self.engine)
        self.assertEqual(second["state"], "done")
        self.assertNotEqual(first["plan_id"], second["plan_id"])

    def test_disable_exposes_commit_point_and_observe_never_resumes(self):
        self.engine.disable_after_tag = True
        with self.assertRaises(Refused): auto.tick(self.root, self.engine)
        got = auto.status(self.root, observe=True, engine=self.engine)
        self.assertFalse(got["policy"]["enabled"])
        self.assertTrue(got["publication"]["ci_may_finish_after_disable"])
        self.assertEqual(got["remote"]["tag"], "v0.2.2")
        self.assertEqual(self.engine.calls, ["tag"])

    def test_missing_lfs_cache_does_not_prevent_disable(self):
        self.lfs.rename(self.lfs.with_name("cache-offline"))
        self.assertFalse(policy.disable(self.root)["enabled"])

    def test_existing_unreadable_tag_cannot_be_discarded_as_unpublished(self):
        engine = auto.Engine(self.root)
        backend = mock.Mock()
        backend.resume_facts.return_value = {"main": A, "tag_parent": None}
        backend.remote_ref.return_value = B
        with mock.patch.object(engine, "backend", return_value=backend):
            with self.assertRaisesRegex(Refused, "preserve the original plan"):
                engine.facts(plan_for(self.config), self.config)

    def test_recovery_checks_actual_saved_artifact_hash_before_signature_probe(self):
        engine = auto.Engine(self.root)
        backend = mock.Mock()
        backend.remote_ref.return_value = B
        dmg = self.root / "artifact.dmg"
        dmg.write_bytes(b"modified bytes")
        state = {"stages": {"build": {"detail": {"commit": B}}, "tag": {"detail": {
            "commit": B, "notarized": {"dmg": str(dmg), "dmg_sha256": "sha256:old"}}}}}
        with mock.patch.object(engine, "backend", return_value=backend):
            with self.assertRaisesRegex(Refused, "artifact evidence"):
                engine.revalidate_started(plan_for(self.config), state, self.config)
        backend.verify_notarized.assert_not_called()
        state["stages"]["tag"]["detail"]["notarized"]["dmg_sha256"] = sha256_bytes(dmg.read_bytes())
        backend.verify_notarized.return_value = {"notarized": True}
        with mock.patch.object(engine, "backend", return_value=backend):
            engine.revalidate_started(plan_for(self.config), state, self.config)
        backend.verify_notarized.assert_called_once_with(plan_for(self.config), plan_for(self.config)["mac_artifact"], dmg)

    def test_git_helper_preserves_repository_hooks(self):
        with mock.patch.object(auto.subprocess, "run", return_value=mock.Mock(returncode=0, stdout="")) as run:
            auto.git(self.root, "push", "origin", "fixture")
        self.assertNotIn("core.hooksPath=/dev/null", run.call_args.args[0])

    def test_verification_failure_never_enters_live_backend(self):
        self.engine.local_ready = False
        self.assertEqual(auto.tick(self.root, self.engine)["state"], "blocked")
        self.assertEqual(self.engine.calls, [])

    def test_native_main_advance_waits_for_new_ready_without_replanning(self):
        self.engine.head = B
        self.assertEqual(auto.tick(self.root, self.engine)["state"], "waiting_ready")
        self.assertEqual(self.engine.created, 0)

    def test_metadata_only_advance_records_original_and_planned_sha(self):
        self.engine.head, self.engine.metadata = B, True
        result = auto.tick(self.root, self.engine)
        plan, state = self.engine.load_plan(result["plan_id"])
        self.assertEqual(plan["commit"], B)
        self.assertEqual(state["authorization"]["requested_commit"], A)
        self.assertEqual(state["authorization"]["commit"], B)

    def test_orphaned_preview_tag_does_not_create_another_release(self):
        self.engine.orphan = "v0.2.2"
        with self.assertRaisesRegex(Refused, "original plan"):
            auto.tick(self.root, self.engine)
        self.assertEqual(self.engine.created, 0)

    def test_default_fastpatch_still_requires_nacho_approval(self):
        plan = self.engine.make_plan(self.config, A)
        self.engine.run_local(plan, self.config)
        with self.assertRaises(Refused):
            fastpatch.run(plan["plan_id"], FakeBackend(self.engine), self.engine.plans)
        self.assertEqual(self.engine.calls, [])

    def test_launch_agent_has_absolute_paths_and_no_fake_nacho_environment(self):
        source = Path(__file__).resolve().parents[3]
        import sys
        spec = auto.service_spec(source, self.root, sys.executable)
        self.assertTrue(Path(spec["ProgramArguments"][0]).is_absolute())
        self.assertTrue(Path(spec["WorkingDirectory"]).is_absolute())
        self.assertEqual(spec["StartInterval"], 60)
        self.assertNotIn("KASATERM_RELEASE_INVOKER", spec["EnvironmentVariables"])
        self.assertNotIn("KeepAlive", spec)

    def test_launch_agent_refuses_lfs_cache_in_tcc_protected_folder(self):
        source = Path(__file__).resolve().parents[3]
        import sys
        protected = Path(self.temp.name).resolve() / "Desktop/lfs"
        (protected / "objects").mkdir(parents=True)
        policy.enable(self.root, CONTROLLER, "0.2", controller_identity=lambda: CONTROLLER, lfs_store=protected)
        with mock.patch.object(auto.Path, "home", return_value=Path(self.temp.name).resolve()):
            with self.assertRaisesRegex(Refused, "TCC"):
                auto.service_spec(source, self.root, sys.executable)


class MetadataGitFixture(unittest.TestCase):
    def test_fresh_clone_materializes_lfs_from_explicit_cache_without_network(self):
        binary = shutil.which("git-lfs")
        if not binary:
            self.skipTest("git-lfs is unavailable")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, checkout, cache = root / "source", root / "checkout", root / "cache"
            source.mkdir()
            contents = b"verified local LFS fixture\n"
            digest = sha256_bytes(contents)[7:]
            cached = cache / "objects" / digest[:2] / digest[2:4] / digest
            cached.parent.mkdir(parents=True)
            cached.write_bytes(contents)
            auto.git(source, "init", "--initial-branch=main")
            auto.git(source, "config", "user.name", "Fixture")
            auto.git(source, "config", "user.email", "fixture@example.invalid")
            (source / ".gitattributes").write_text("*.bin filter=lfs diff=lfs merge=lfs -text\n")
            (source / "asset.bin").write_text(f"version https://git-lfs.github.com/spec/v1\noid sha256:{digest}\nsize {len(contents)}\n")
            auto.git(source, "add", ".")
            auto.git(source, "commit", "-m", "fixture")
            auto.git(root, "clone", "--no-checkout", str(source), str(checkout))
            auto.configure_lfs(checkout, {"lfs_storage": str(cache)})
            auto.git(checkout, "config", "filter.lfs.process", binary + " filter-process")
            auto.git(checkout, "config", "filter.lfs.required", "true")
            auto.git(checkout, "config", "lfs.url", "http://127.0.0.1:1/must-not-contact")
            auto.git(checkout, "checkout", "--detach", "HEAD", env={"GIT_LFS_SKIP_SMUDGE": "0"})
            self.assertEqual((checkout / "asset.bin").read_bytes(), contents)
            worktree = root / "build-worktree"
            auto.git(checkout, "worktree", "add", "--detach", str(worktree), "HEAD", env={"GIT_LFS_SKIP_SMUDGE": "0"})
            self.assertEqual((worktree / "asset.bin").read_bytes(), contents)

    def test_only_version_metadata_and_docs_can_advance(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            auto.git(repo, "init", "--initial-branch=main")
            auto.git(repo, "config", "user.name", "Fixture")
            auto.git(repo, "config", "user.email", "fixture@example.invalid")
            cargo = repo / "Cargo.toml"
            cargo.write_text('[workspace.package]\nversion = "0.2.1"\n[workspace.dependencies]\ncrate = "1"\n')
            auto.git(repo, "add", ".")
            auto.git(repo, "commit", "-m", "fixture")
            first = auto.git(repo, "rev-parse", "HEAD")
            cargo.write_text(cargo.read_text().replace('version = "0.2.1"', 'version = "0.2.2"'))
            (repo / "docs").mkdir()
            (repo / "docs/note.md").write_text("Fixture documentation\n")
            auto.git(repo, "add", ".")
            auto.git(repo, "commit", "-m", "metadata")
            second = auto.git(repo, "rev-parse", "HEAD")
            self.assertTrue(auto.metadata_only(repo, first, second))
            cargo.write_text(cargo.read_text().replace('crate = "1"', 'crate = "2"'))
            auto.git(repo, "add", ".")
            auto.git(repo, "commit", "-m", "dependency")
            third = auto.git(repo, "rev-parse", "HEAD")
            self.assertFalse(auto.metadata_only(repo, first, third))
            self.assertFalse(auto.metadata_only(repo, third, first))


if __name__ == "__main__":
    unittest.main()
