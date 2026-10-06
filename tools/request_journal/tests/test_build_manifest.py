import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

from tools.request_journal import build_manifest as proof
from tools.request_journal.store import Store


class BuildManifestTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.project = self.root / "repo"
        self.project.mkdir()
        self.command("init", "-q")
        self.command("config", "user.email", "test@example.invalid")
        self.command("config", "user.name", "Test")
        (self.project / ".gitignore").write_text("/dist/\n")
        (self.project / "Cargo.toml").write_text("[workspace]\n")
        (self.project / "app").mkdir()
        (self.project / "app/main.rs").write_text("initial source")
        self.command("add", ".")
        self.command("commit", "-qm", "initial")
        self.bundle = self.project / "dist/kasaterm.app"
        for relative in ("Contents/MacOS/kasaterm", "Contents/MacOS/kasaterm-cli", "Contents/MacOS/kasa-serve-web", "Contents/Resources/kasapet"):
            path = self.bundle / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(relative.encode())
        self.snapshot = self.root / "snapshot.json"
        self.output = self.project / "dist/kasaterm.build.json"

    def tearDown(self):
        self.tmp.cleanup()

    def command(self, *args):
        return subprocess.run(["git", "-C", str(self.project), *args], check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout

    def finish(self):
        return proof.finish(self.snapshot, self.bundle, self.output, signature_verifier=lambda p: {"verified": True, "status": "fixture_signature"})

    def test_signed_bundle_is_unchanged_and_source_commit_is_evidence_not_running_state(self):
        before = proof.bundle_hashes(self.bundle)
        proof.begin(self.project, "release", self.snapshot)
        manifest = self.finish()
        self.assertEqual(proof.bundle_hashes(self.bundle), before)
        self.assertEqual(manifest["source"]["status"], "stable_clean")
        self.assertEqual(manifest["source"]["source_commit"], self.command("rev-parse", "HEAD").decode().strip())
        self.assertTrue(proof.validate_manifest(manifest, self.bundle))
        self.assertTrue((self.output.parent / "build-manifests" / (manifest["id"] + ".json")).is_file())
        store = Store(self.root / "journal/db.sqlite")
        store.record_build(manifest)
        self.assertIsNone(store.runtime_context(str(self.project.resolve()))["current_run"])

    def test_changed_sources_keep_verified_artifact_but_cannot_claim_a_source_commit(self):
        proof.begin(self.project, "release", self.snapshot)
        (self.project / "app/main.rs").write_text("changed while building")
        manifest = self.finish()
        self.assertTrue(manifest["success"])
        self.assertEqual(manifest["source"]["status"], "uncertain")
        self.assertIsNone(manifest["source"]["source_commit"])
        self.assertTrue(manifest["components"]["app"]["sha256"])

    def test_an_ignored_lockfile_change_still_prevents_a_clean_source_claim(self):
        (self.project / ".gitignore").write_text("/dist/\n/Cargo.lock\n")
        self.command("add", ".gitignore")
        self.command("commit", "-qm", "ignore generated lock")
        lock = self.project / "Cargo.lock"
        lock.write_text('version = "0.2.1"\n')
        proof.begin(self.project, "release", self.snapshot)
        lock.write_text('version = "0.2.2"\n')
        source = self.finish()["source"]
        self.assertFalse(source["before"]["dirty"])
        self.assertFalse(source["after"]["dirty"])
        self.assertEqual(source["before"]["head"], source["after"]["head"])
        self.assertEqual(source["before"]["status_digest"], source["after"]["status_digest"])
        self.assertNotEqual(source["before"]["input_digest"], source["after"]["input_digest"])
        self.assertEqual(source["status"], "uncertain")
        self.assertIsNone(source["source_commit"])

    def test_signature_failure_does_not_publish_ready_manifest(self):
        proof.begin(self.project, "release", self.snapshot)
        with self.assertRaises(ValueError):
            proof.finish(self.snapshot, self.bundle, self.output, signature_verifier=lambda p: {"verified": False})
        self.assertFalse(self.output.exists())

    def test_modified_bundle_invalidates_existing_manifest(self):
        proof.begin(self.project, "release", self.snapshot)
        manifest = self.finish()
        (self.bundle / "Contents/MacOS/kasaterm").write_bytes(b"replaced after proof")
        self.assertFalse(proof.validate_manifest(manifest, self.bundle))
        self.assertFalse(proof.validate_manifest([]))

    def test_environment_files_are_never_read_for_source_fingerprints(self):
        env_file = self.project / "app/usemap.env"
        env_file.write_text("TEST_FIXTURE_ONLY=example")
        original = proof.file_hash
        def checked(path):
            self.assertNotEqual(path, env_file)
            return original(path)
        with mock.patch.object(proof, "file_hash", checked):
            snapshot = proof.source_snapshot(self.project)
        self.assertEqual(snapshot["excluded_credential_paths"], 1)


if __name__ == "__main__":
    unittest.main()


class RepositoryInputNamesTests(unittest.TestCase):
    def test_no_tracked_build_input_is_named_like_a_credential(self):
        # 이름에 secret·.key 따위가 든 입력은 지문에서 빠지고, 빠진 것이 하나라도 있으면 굽기 증명서가 「불확실」이 되어
        # preview controller 가 막힌다(2026-10-06 op_secret.rs 로 실제로 막혔다). 비밀이 아닌 코드는 이름을 바꿔라.
        root = Path(__file__).resolve().parents[3]
        listed = subprocess.run(
            ["git", "ls-files", "-z", "--cached", "--", "app", "crates", "web", "scripts", "assets",
             "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml"],
            cwd=root, capture_output=True, check=True).stdout
        named = [p for p in listed.decode().split("\0") if p and proof.credential_path(p)]
        self.assertEqual(named, [])

