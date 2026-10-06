import base64
import copy
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

from tools.release import backend, deps, feed_publish
from tools.release.backend import RealBackend
from tools.release.common import CHANNEL_MANIFEST, Refused, channel_manifest, sha256_file
from tools.release.proc import Result, Runner


OPENSSL = deps.find_openssl(Runner("local"))
REPO = Path(__file__).resolve().parents[3]


def sh(repo, *args):
    return subprocess.run(args, cwd=repo, check=True, capture_output=True).stdout.decode().strip()


def xml(version, signature, length, platform="macos"):
    suffix = ".dmg" if platform == "macos" else "-windows-x86_64.msi"
    return (f'<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle"><channel><item>'
            f'<sparkle:version>{version}</sparkle:version><enclosure length="{length}" '
            f'url="https://github.com/2rami/kasaterm/releases/download/v{version}/kasaterm-v{version}{suffix}" '
            f'sparkle:edSignature="{signature}"/></item></channel></rss>').encode()


@unittest.skipUnless(OPENSSL.get("path"), "Ed25519 OpenSSL required")
class FeedPublishTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="feed-cas-test-")
        self.root = Path(self.temporary.name)
        self.remote, self.writer, self.repo = [self.root / name for name in ("origin.git", "writer", "publisher")]
        self.key = self.root / "key.pem"
        sh(self.root, OPENSSL["path"], "genpkey", "-algorithm", "ED25519", "-out", str(self.key))
        public = subprocess.run([OPENSSL["path"], "pkey", "-in", str(self.key), "-pubout", "-outform", "DER"],
                                check=True, capture_output=True).stdout[-32:]
        self.public = base64.b64encode(public).decode()
        sh(self.root, "git", "init", "-q", "--bare", "--initial-branch=main", str(self.remote))
        sh(self.root, "git", "init", "-q", "--initial-branch=main", str(self.writer))
        sh(self.writer, "git", "config", "user.name", "Fixture")
        sh(self.writer, "git", "config", "user.email", "fixture@example.invalid")
        sh(self.writer, "git", "remote", "add", "origin", str(self.remote))
        for name in ("docs", "scripts", ".github"):
            (self.writer / name).mkdir()
        (self.writer / "Cargo.toml").write_text('[workspace.package]\nversion = "0.2.1"\n')
        (self.writer / "scripts/build-app.sh").write_text(f'<key>SUPublicEDKey</key>\n<string>{self.public}</string>\n')
        (self.writer / "docs/appcast.xml").write_bytes(b"stable untouched")
        (self.writer / "docs/appcast-win.xml").write_bytes(b"Windows untouched")
        (self.writer / "docs/appcast-preview.xml").write_bytes(b"<rss><channel/></rss>")
        self.source = self.commit("source")
        (self.writer / "Cargo.toml").write_text('[workspace.package]\nversion = "0.2.2"\n')
        (self.writer / CHANNEL_MANIFEST).write_text(json.dumps(channel_manifest("preview", "v0.2.2", self.source)))
        self.tagged = self.commit("release version")
        sh(self.writer, "git", "tag", "v0.2.2")
        sh(self.writer, "git", "push", "-q", "origin", "main", "refs/tags/v0.2.2")
        sh(self.root, "git", "clone", "-q", str(self.remote), str(self.repo))
        self.asset = self.root / "kasaterm-v0.2.2.dmg"
        self.asset.write_bytes(b"verified and notarized artifact fixture")
        self.feed = self.root / "candidate.xml"
        self.feed.write_bytes(xml("0.2.2", self.sign(self.asset), self.asset.stat().st_size))
        self.candidate = feed_publish.Candidate.read("macos", self.feed, self.asset, sha256_file(self.asset))
        self.release = {"isDraft": False, "isPrerelease": True,
                        "assets": [{"name": self.asset.name, "size": self.asset.stat().st_size, "digest": self.candidate.sha256}]}
        self.publisher = feed_publish.Publisher(self.repo, "v0.2.2", "preview", self.tagged,
                                                [self.candidate], openssl=OPENSSL["path"])
        self.publisher.release_view = lambda: copy.deepcopy(self.release)

    def tearDown(self):
        self.temporary.cleanup()

    def commit(self, message):
        sh(self.writer, "git", "add", ".")
        sh(self.writer, "git", "commit", "-qm", message)
        return sh(self.writer, "git", "rev-parse", "HEAD")

    def sign(self, path):
        signature = subprocess.run([OPENSSL["path"], "pkeyutl", "-sign", "-inkey", str(self.key), "-rawin", "-in", str(path)],
                                   check=True, capture_output=True).stdout
        return base64.b64encode(signature).decode()

    def advance(self, relative="other.txt", raw=b"concurrent work"):
        sh(self.writer, "git", "fetch", "-q", "origin", "main")
        sh(self.writer, "git", "merge", "--ff-only", "origin/main")
        (self.writer / relative).write_bytes(raw)
        result = self.commit("concurrent change")
        sh(self.writer, "git", "push", "-q", "origin", "main")
        return result

    def remote_file(self, relative):
        return subprocess.run(["git", "--git-dir", str(self.remote), "show", "refs/heads/main:" + relative],
                              check=True, capture_output=True).stdout

    def test_publish_preserves_local_index_worktree_and_other_channels(self):
        (self.repo / "Cargo.toml").write_text("local edit")
        sh(self.repo, "git", "add", "Cargo.toml")
        (self.repo / "Cargo.toml").write_text("unstaged edit")
        before = (sh(self.repo, "git", "rev-parse", "HEAD"), (self.repo / ".git/index").read_bytes(),
                  sh(self.repo, "git", "status", "--porcelain"))
        result = self.publisher.publish()
        self.assertEqual(result["state"], "published")
        self.assertEqual(self.remote_file("docs/appcast-preview.xml"), self.candidate.raw)
        self.assertEqual(self.remote_file("docs/appcast.xml"), b"stable untouched")
        self.assertEqual(self.remote_file("docs/appcast-win.xml"), b"Windows untouched")
        self.assertEqual(before, (sh(self.repo, "git", "rev-parse", "HEAD"), (self.repo / ".git/index").read_bytes(),
                                  sh(self.repo, "git", "status", "--porcelain")))
        self.assertEqual((self.repo / "Cargo.toml").read_text(), "unstaged edit")
        self.assertEqual(self.publisher.publish()["state"], "already_published")

    def test_main_race_retries_only_feed_commit_and_preserves_concurrent_change(self):
        push = self.publisher.push
        with mock.patch.object(self.publisher, "push", side_effect=lambda commit: (self.advance(), push(commit))[1]) as calls:
            with self.assertRaises(feed_publish.CasExhausted):
                self.publisher.publish(attempts=1)
        self.assertEqual(calls.call_count, 1)
        result = self.publisher.publish()
        self.assertEqual(result["state"], "published")
        self.assertEqual(self.remote_file("other.txt"), b"concurrent work")

    def test_single_race_is_recovered_in_same_invocation(self):
        push = self.publisher.push
        count = 0

        def race(commit):
            nonlocal count
            count += 1
            if count == 1:
                self.advance()
            return push(commit)

        with mock.patch.object(self.publisher, "push", side_effect=race):
            result = self.publisher.publish()
        self.assertEqual((result["attempts"], count), (2, 2))
        self.assertEqual(self.remote_file("other.txt"), b"concurrent work")

    def test_newer_feed_winning_race_is_never_downgraded(self):
        newer = xml("0.2.3", "newer-signature", 12)
        push = self.publisher.push
        with mock.patch.object(self.publisher, "push", side_effect=lambda commit: (self.advance("docs/appcast-preview.xml", newer), push(commit))[1]) as calls:
            with self.assertRaisesRegex(Refused, "더 새 판"):
                self.publisher.publish()
        self.assertEqual(calls.call_count, 1)
        self.assertEqual(self.remote_file("docs/appcast-preview.xml"), newer)

    def test_same_version_different_signature_is_never_overwritten(self):
        self.advance("docs/appcast-preview.xml", xml("0.2.2", "different", self.asset.stat().st_size))
        with mock.patch.object(self.publisher, "push") as push, self.assertRaisesRegex(Refused, "같은 판"):
            self.publisher.publish()
        push.assert_not_called()

    def test_retry_keeps_feed_bytes_but_rechecks_artifact_hash(self):
        push = self.publisher.push

        def race(commit):
            self.advance()
            self.asset.write_bytes(b"x" * self.asset.stat().st_size)
            return push(commit)

        with mock.patch.object(self.publisher, "push", side_effect=race) as calls, self.assertRaisesRegex(Refused, "해시"):
            self.publisher.publish()
        self.assertEqual(calls.call_count, 1)
        self.assertEqual(self.remote_file("docs/appcast-preview.xml"), b"<rss><channel/></rss>")

    def test_changed_source_feed_cannot_replace_fixed_candidate(self):
        self.feed.write_bytes(b"changed after snapshot")
        self.publisher.publish()
        self.assertEqual(self.remote_file("docs/appcast-preview.xml"), self.candidate.raw)

    def test_bad_signature_and_remote_digest_fail_before_push(self):
        invalid = feed_publish.Candidate("macos", xml("0.2.2", base64.b64encode(bytes(64)).decode(), self.asset.stat().st_size),
                                        self.asset, self.candidate.sha256)
        self.publisher.candidates = (invalid,)
        with mock.patch.object(self.publisher, "push") as push, self.assertRaisesRegex(Refused, "EdDSA"):
            self.publisher.publish()
        push.assert_not_called()
        self.publisher.candidates = (self.candidate,)
        self.release["assets"][0]["digest"] = "sha256:" + "0" * 64
        with self.assertRaisesRegex(Refused, "릴리스 산출물"):
            self.publisher.publish()

    def test_remote_channel_and_tag_identity_are_rechecked_after_race(self):
        for changed in ("channel", "tag"):
            with self.subTest(changed=changed):
                self.tearDown()
                self.setUp()
                push = self.publisher.push

                def race(commit):
                    self.advance()
                    if changed == "channel":
                        self.release["isPrerelease"] = False
                    else:
                        sh(self.root, "git", "--git-dir", str(self.remote), "update-ref", "refs/tags/v0.2.2", self.source)
                    return push(commit)

                with mock.patch.object(self.publisher, "push", side_effect=race) as calls, self.assertRaises(Refused):
                    self.publisher.publish()
                self.assertEqual(calls.call_count, 1)

    def test_continuous_main_races_are_bounded(self):
        push = self.publisher.push
        count = 0

        def race(commit):
            nonlocal count
            count += 1
            self.advance(raw=f"change {count}".encode())
            return push(commit)

        with mock.patch.object(self.publisher, "push", side_effect=race), self.assertRaises(feed_publish.CasExhausted):
            self.publisher.publish(attempts=3)
        self.assertEqual(count, 3)
        self.assertEqual(self.remote_file("other.txt"), b"change 3")

    def test_push_rejection_without_main_movement_is_not_retried(self):
        with mock.patch.object(self.publisher, "push", return_value=False) as push, self.assertRaisesRegex(Refused, "main은 그대로"):
            self.publisher.publish()
        push.assert_called_once()

    def test_lost_success_response_reconciles_same_feed(self):
        push = self.publisher.push
        with mock.patch.object(self.publisher, "push", side_effect=lambda commit: (push(commit), False)[1]) as calls:
            result = self.publisher.publish(attempts=1)
        self.assertEqual(result["state"], "already_published")
        calls.assert_called_once()

    def test_preview_rejects_windows_and_malformed_feed(self):
        with self.assertRaisesRegex(Refused, "macOS"):
            feed_publish.Publisher(self.repo, "v0.2.2", "preview", self.tagged,
                                   [self.candidate, feed_publish.Candidate("windows", b"", self.asset, self.candidate.sha256)])
        for raw in (b"<bad/>", self.candidate.raw.replace(b"</item>", b"</item><item/>")):
            with self.assertRaises(Refused):
                feed_publish.item(raw)

    def test_ci_depth_one_checkout_can_verify_tag_parent(self):
        shallow = self.root / "shallow"
        sh(self.root, "git", "clone", "-q", "--depth=1", self.remote.as_uri(), str(shallow))
        self.assertEqual(sh(shallow, "git", "rev-parse", "--is-shallow-repository"), "true")
        self.publisher.repo = shallow
        self.assertEqual(self.publisher.publish()["state"], "published")

    def stable_pair(self):
        (self.writer / CHANNEL_MANIFEST).write_text(json.dumps(channel_manifest("stable", "v0.2.2", self.tagged)))
        (self.writer / "docs/appcast.xml").write_bytes(b"<rss><channel/></rss>")
        (self.writer / "docs/appcast-win.xml").write_bytes(b"<rss><channel/></rss>")
        stable = self.commit("stable fixture")
        sh(self.writer, "git", "push", "-q", "origin", "main")
        sh(self.root, "git", "--git-dir", str(self.remote), "update-ref", "refs/tags/v0.2.2", stable)
        msi = self.root / "kasaterm-v0.2.2-windows-x86_64.msi"
        msi.write_bytes(b"verified Windows installer")
        windows = feed_publish.Candidate("windows", xml("0.2.2", self.sign(msi), msi.stat().st_size, "windows"), msi, sha256_file(msi))
        self.release["isPrerelease"] = False
        self.release["assets"].append({"name": msi.name, "size": msi.stat().st_size, "digest": windows.sha256})
        self.publisher.channel, self.publisher.commit = "stable", stable
        self.publisher.candidates = (self.candidate, windows)
        return windows

    def test_stable_pair_is_validated_before_either_feed_changes(self):
        self.stable_pair()
        self.advance("docs/appcast-win.xml", xml("0.2.3", "newer", 1, "windows"))
        with self.assertRaisesRegex(Refused, "더 새 판"):
            self.publisher.publish()
        self.assertEqual(self.remote_file("docs/appcast.xml"), b"<rss><channel/></rss>")

    def test_stable_pair_publishes_in_one_commit_without_preview_change(self):
        windows = self.stable_pair()
        result = self.publisher.publish()
        self.assertEqual(self.remote_file("docs/appcast.xml"), self.candidate.raw)
        self.assertEqual(self.remote_file("docs/appcast-win.xml"), windows.raw)
        self.assertEqual(self.remote_file("docs/appcast-preview.xml"), b"<rss><channel/></rss>")
        changed = sh(self.root, "git", "--git-dir", str(self.remote), "diff-tree", "--no-commit-id", "--name-only", "-r", result["commit"])
        self.assertEqual(set(changed.splitlines()), {"docs/appcast.xml", "docs/appcast-win.xml"})

    def test_signatures_are_rechecked_on_each_attempt(self):
        push = self.publisher.push
        with mock.patch.object(self.publisher, "push", side_effect=lambda commit: (self.advance(), push(commit))[1]), \
                mock.patch.object(feed_publish, "ed25519_ok", side_effect=[True, False]) as verify:
            with self.assertRaisesRegex(Refused, "EdDSA"):
                self.publisher.publish()
        self.assertEqual(verify.call_count, 2)
        self.assertEqual(self.remote_file("docs/appcast-preview.xml"), b"<rss><channel/></rss>")

    def pages_site(self):
        return {"build_type": "legacy", "source": {"branch": "main", "path": "/docs"},
                "html_url": feed_publish.PAGES_URL}

    def test_pages_requests_once_and_waits_for_build_and_public_feed(self):
        published = self.publisher.publish()
        builds = [{"status": "built", "commit": self.tagged},
                  {"status": "building", "commit": published["commit"]},
                  {"status": "built", "commit": published["commit"]}]
        http = mock.Mock()
        sleep = mock.Mock()
        http.get.side_effect = [(200, xml("0.2.1", "stale", 2)), (200, self.candidate.raw)]
        with mock.patch.object(self.publisher, "pages_api", side_effect=[
                self.pages_site(), {"status": "queued"}, *builds, builds[-1]]) as api:
            result = self.publisher.deploy_pages(published, attempts=4, delay=1, http=http, wait=sleep)
        self.assertEqual((result["state"], result["commit"], result["attempts"]), ("deployed", published["commit"], 4))
        self.assertEqual(api.call_args_list.count(mock.call("POST", "/builds")), 1)
        self.assertEqual(sleep.call_count, 3)
        self.assertEqual(http.get.call_count, 2)
        for call in http.get.call_args_list:
            self.assertEqual(call.args[0], feed_publish.PAGES_URL + "appcast-preview.xml")
            self.assertEqual(call.kwargs["headers"], {"Cache-Control": "no-cache"})

    def test_pages_already_published_retries_deployment_without_new_commit(self):
        self.publisher.publish()
        published = self.publisher.publish()
        self.assertEqual(published["state"], "already_published")
        http = mock.Mock()
        http.get.return_value = (200, self.candidate.raw)
        with mock.patch.object(self.publisher, "pages_api", side_effect=[
                self.pages_site(), {"status": "queued"}, {"status": "built", "commit": published["commit"]}]), \
                mock.patch.object(self.publisher, "push") as push:
            self.assertEqual(self.publisher.deploy_pages(published, attempts=1, http=http)["state"], "deployed")
        push.assert_not_called()

    def test_pages_accepts_descendant_build_preserving_verified_feed(self):
        published = self.publisher.publish()
        descendant = self.advance()
        http = mock.Mock()
        http.get.return_value = (200, self.candidate.raw)
        with mock.patch.object(self.publisher, "pages_api", side_effect=[
                self.pages_site(), {"status": "queued"}, {"status": "built", "commit": descendant}]):
            self.assertEqual(self.publisher.deploy_pages(published, attempts=1, http=http)["commit"], descendant)

    def test_pages_descendant_check_works_after_depth_one_ci_checkout(self):
        shallow = self.root / "pages-shallow"
        sh(self.root, "git", "clone", "-q", "--depth=1", self.remote.as_uri(), str(shallow))
        self.publisher.repo = shallow
        published = self.publisher.publish()
        descendant = self.advance()
        http = mock.Mock()
        http.get.return_value = (200, self.candidate.raw)
        with mock.patch.object(self.publisher, "pages_api", side_effect=[
                self.pages_site(), {"status": "queued"}, {"status": "built", "commit": descendant}]):
            self.assertEqual(self.publisher.deploy_pages(published, attempts=1, http=http)["commit"], descendant)

    def test_pages_different_configuration_never_requests_or_reconfigures(self):
        published = self.publisher.publish()
        for key, value in (("build_type", "workflow"), ("source", {"branch": "other", "path": "/"}),
                           ("html_url", "https://unexpected.invalid/")):
            site = self.pages_site()
            site[key] = value
            with self.subTest(key=key), mock.patch.object(self.publisher, "pages_api", return_value=site) as api, \
                    self.assertRaisesRegex(Refused, "설정을 자동 변경하지"):
                self.publisher.deploy_pages(published, attempts=1)
            self.assertEqual(api.call_args_list, [mock.call("GET")])

    def test_pages_unverified_publication_never_requests(self):
        for published in ({"state": "blocked", "commit": self.tagged},
                          {"state": "published", "commit": self.tagged},
                          {"state": "published", "commit": "invalid"}):
            with self.subTest(published=published), mock.patch.object(self.publisher, "pages_api") as api, \
                    self.assertRaises(Refused):
                self.publisher.deploy_pages(published, attempts=1)
            api.assert_not_called()

    def test_pages_failed_build_is_failure_without_public_success(self):
        published = self.publisher.publish()
        http = mock.Mock()
        errored = {"status": "errored", "commit": published["commit"]}
        with mock.patch.object(self.publisher, "pages_api", side_effect=[
                self.pages_site(), {"status": "queued"}, errored, {"status": "queued"}, errored, {"status": "queued"},
                errored]) as api, self.assertRaisesRegex(feed_publish.PagesUnfinished, "Pages 빌드 실패"):
            self.publisher.deploy_pages(published, attempts=5, delay=1, http=http, wait=mock.Mock())
        self.assertEqual(api.call_args_list.count(mock.call("POST", "/builds")), 1 + feed_publish.PAGES_REBUILDS)
        http.get.assert_not_called()
        self.assertEqual(self.remote_file("docs/appcast-preview.xml"), self.candidate.raw)

    def test_pages_failed_or_stuck_build_is_requested_again(self):
        # 2026-10-02 v0.2.28: 피드 커밋의 Pages 빌드가 15분 「빌드 중」이다가 실패했고, 다음 빌드는 20초에 됐다.
        published = self.publisher.publish()
        stuck = {"status": "building", "commit": published["commit"], "created_at": "2026-10-02T06:33:39Z"}
        built = {"status": "built", "commit": published["commit"]}
        for name, middle in (("errored", [{"status": "errored", "commit": published["commit"]}]),
                             ("stuck", [stuck] * (feed_publish.PAGES_STALL + 1))):
            http = mock.Mock()
            http.get.return_value = (200, self.candidate.raw)
            with self.subTest(name), mock.patch.object(self.publisher, "pages_api", side_effect=[
                    self.pages_site(), {"status": "queued"}, *middle, {"status": "queued"}, built]) as api:
                result = self.publisher.deploy_pages(published, attempts=60, delay=1, http=http, wait=mock.Mock())
            self.assertEqual(result["state"], "deployed")
            self.assertEqual(api.call_args_list.count(mock.call("POST", "/builds")), 2)

    def test_pages_old_build_and_public_failure_have_bounded_waits(self):
        published = self.publisher.publish()
        for built, response in ((self.tagged, (200, self.candidate.raw)),
                                (published["commit"], (404, b"missing")),
                                (published["commit"], (200, b"malformed")),
                                (published["commit"], (200, xml("0.2.2", "wrong-signature", self.asset.stat().st_size)))):
            http = mock.Mock()
            sleep = mock.Mock()
            http.get.return_value = response
            with self.subTest(built=built, response=response), mock.patch.object(self.publisher, "pages_api", side_effect=[
                    self.pages_site(), {"status": "queued"}, *[{"status": "built", "commit": built}] * 2]) as api, \
                    self.assertRaisesRegex(Refused, "확인 2회 소진"):
                self.publisher.deploy_pages(published, attempts=2, delay=1, http=http, wait=sleep)
            self.assertEqual(api.call_args_list.count(mock.call("POST", "/builds")), 1)
            self.assertEqual(sleep.call_count, 1)
            self.assertEqual(http.get.call_count, 0 if built == self.tagged else 2)

    def test_pages_rejects_changed_feed_even_when_build_succeeded(self):
        published = self.publisher.publish()
        descendant = self.advance("docs/appcast-preview.xml", xml("0.2.3", "newer", 3))
        with mock.patch.object(self.publisher, "pages_api", side_effect=[
                self.pages_site(), {"status": "queued"}, {"status": "built", "commit": descendant}]), \
                self.assertRaisesRegex(Refused, "후보에서 바뀌었다"):
            self.publisher.deploy_pages(published, attempts=1, http=mock.Mock())

    def test_pages_stable_pair_requires_both_public_feeds(self):
        windows = self.stable_pair()
        published = self.publisher.publish()
        http = mock.Mock()
        http.get.side_effect = [(200, self.candidate.raw), (200, windows.raw)]
        with mock.patch.object(self.publisher, "pages_api", side_effect=[
                self.pages_site(), {"status": "queued"}, {"status": "built", "commit": published["commit"]}]):
            result = self.publisher.deploy_pages(published, attempts=1, http=http)
        self.assertEqual(result["feeds"], [feed_publish.PAGES_URL + "appcast.xml", feed_publish.PAGES_URL + "appcast-win.xml"])
        self.assertEqual(http.get.call_count, 2)
        self.assertEqual(self.remote_file("docs/appcast-preview.xml"), b"<rss><channel/></rss>")

    def test_pages_api_failure_and_bad_response_are_not_success(self):
        for response in (Result(1, "", "permission denied"), Result(0, "not JSON"), Result(0, "[]")):
            with self.subTest(response=response.out), mock.patch.object(feed_publish.Runner, "run", return_value=response), \
                    self.assertRaises(Refused):
                self.publisher.pages_api("POST", "/builds")


class RetryClassificationTests(unittest.TestCase):
    def setUp(self):
        self.jobs = [{"name": name, "status": "completed", "conclusion": conclusion, "databaseId": number}
                     for number, (name, conclusion) in enumerate((
                         ("resolve", "success"), ("build-dmg", "success"), ("build-msi", "skipped"), ("appcast", "failure")), 10)]
        self.jobs[-1]["steps"] = [{"name": name, "conclusion": conclusion} for name, conclusion in (
            ("Checkout main", "success"), ("Sparkle signing tools", "success"), ("Generate signed appcasts", "success"),
            ("Publish verified appcasts", "failure"), ("Post Checkout main", "success"))]
        self.log = "appcast\tPublish verified appcasts\t2026-09-29T00:00:00.000Z KASATERM_APPCAST_CAS_EXHAUSTED\n"
        self.runner = mock.Mock()
        self.runner.run.side_effect = lambda args, **_: (Result(0, json.dumps({"jobs": self.jobs}))
                                                       if "--json" in args else Result(0, self.log))
        self.backend = RealBackend(".", self.runner, None, ".", None, {"gh": {"path": "gh"}})
        self.run = {"databaseId": 42, "conclusion": "failure"}
        self.plan = {"platforms": ["macos"]}

    def test_only_cas_exhaustion_gets_explicit_job_only_recovery(self):
        hint = self.backend.appcast_retry_hint(self.run, self.plan)
        self.assertIn("gh run rerun 42 --job 13 --repo 2rami/kasaterm", hint)
        self.assertTrue(all("rerun" not in call.args[0] for call in self.runner.run.call_args_list))

    def test_unfinished_pages_check_gets_the_same_job_only_recovery(self):
        self.log = "appcast\tPublish verified appcasts\t2026-10-02T06:43:57.000Z KASATERM_APPCAST_PAGES_UNFINISHED\n"
        hint = self.backend.appcast_retry_hint(self.run, self.plan)
        self.assertIn("Pages 게시 확인 미완", hint)
        self.assertIn("gh run rerun 42 --job 13 --repo 2rami/kasaterm", hint)

    def test_controller_reruns_only_the_publish_job_a_bounded_number_of_times(self):
        reruns = lambda: [call for call in self.runner.run.call_args_list if call.args[0][1:3] == ["run", "rerun"]]
        for attempt, expected in ((1, True), (backend.APPCAST_AUTO_RERUNS, True), (backend.APPCAST_AUTO_RERUNS + 1, False),
                                  (None, False)):
            with self.subTest(attempt=attempt):
                self.setUp()
                self.run["attempt"] = attempt
                said = self.backend.rerun_appcast(self.run, self.plan)
                self.assertEqual(bool(said), expected)
                self.assertEqual(len(reruns()), int(expected))
                if expected:
                    self.assertEqual(reruns()[0].args[0], ["gh", "run", "rerun", "42", "--job", "13", "--repo", "2rami/kasaterm"])
                    self.assertEqual(reruns()[0].kwargs["kind"], "publish")
        self.setUp()
        self.run["attempt"] = 1
        self.jobs[1]["conclusion"] = "failure"
        self.assertIsNone(self.backend.rerun_appcast(self.run, self.plan))
        self.assertEqual(reruns(), [])

    def test_tests_signing_or_validation_failure_never_suggested_for_rerun(self):
        for kind in ("build", "sign", "validation", "cancelled", "extra", "unknown"):
            with self.subTest(kind=kind):
                self.setUp()
                if kind == "build":
                    self.jobs[1]["conclusion"] = "failure"
                elif kind == "sign":
                    self.jobs[-1]["steps"][2]["conclusion"] = "failure"
                elif kind == "validation":
                    self.log = "appcast EdDSA 서명이 고정한 산출물과 다르다"
                elif kind == "cancelled":
                    self.run["conclusion"] = "cancelled"
                elif kind == "extra":
                    self.jobs.append({"name": "other", "conclusion": "failure"})
                else:
                    self.runner.run.side_effect = lambda *_args, **_kwargs: Result(1)
                self.assertEqual(self.backend.appcast_retry_hint(self.run, self.plan), "")

    def test_workflow_separates_signing_from_bounded_publication(self):
        workflow = (REPO / ".github/workflows/release.yml").read_text()
        publish = workflow.split("      - name: Publish verified appcasts\n", 1)[1]
        self.assertIn("python3 -m tools.release.feed_publish", publish)
        self.assertIn("--attempts 5", publish)
        self.assertIn("--deploy-pages", publish)
        self.assertEqual(workflow.count("      pages: write"), 1)
        self.assertIn("    permissions:\n      contents: write\n      pages: write\n", workflow.split("  appcast:\n", 1)[1])
        for forbidden in ("ED_KEY", "git pull", "git reset", "--force", "notarytool", "cargo test"):
            self.assertNotIn(forbidden, publish)

    def test_cli_does_not_deploy_pages_before_verified_publication(self):
        args = ["--tag", "v0.2.2", "--channel", "preview", "--commit", "a" * 40,
                "--mac", "feed", "--dmg", "dmg", "--dmg-sha256", "b" * 64, "--deploy-pages"]
        with mock.patch.object(feed_publish.Candidate, "read"), mock.patch.object(feed_publish, "Publisher") as publisher:
            publisher.return_value.publish.side_effect = Refused("fixture verification failure")
            with self.assertRaises(SystemExit) as error:
                feed_publish.main(args)
            self.assertEqual(error.exception.code, 1)
            publisher.return_value.deploy_pages.assert_not_called()

    def test_cli_marks_an_unfinished_pages_check_for_the_controller(self):
        args = ["--tag", "v0.2.2", "--channel", "preview", "--commit", "a" * 40,
                "--mac", "feed", "--dmg", "dmg", "--dmg-sha256", "b" * 64, "--deploy-pages"]
        with mock.patch.object(feed_publish.Candidate, "read"), mock.patch.object(feed_publish, "Publisher") as publisher, \
                mock.patch("sys.stderr", new_callable=io.StringIO) as err:
            publisher.return_value.publish.return_value = {"state": "published", "commit": "c" * 40}
            publisher.return_value.deploy_pages.side_effect = feed_publish.PagesUnfinished("fixture Pages timeout")
            with self.assertRaises(SystemExit) as error:
                feed_publish.main(args)
        self.assertEqual(error.exception.code, 1)
        self.assertEqual(err.getvalue().splitlines()[0], "KASATERM_APPCAST_PAGES_UNFINISHED")

    def test_cli_pages_failure_cannot_print_publication_success(self):
        args = ["--tag", "v0.2.2", "--channel", "preview", "--commit", "a" * 40,
                "--mac", "feed", "--dmg", "dmg", "--dmg-sha256", "b" * 64, "--deploy-pages"]
        with mock.patch.object(feed_publish.Candidate, "read"), mock.patch.object(feed_publish, "Publisher") as publisher, \
                mock.patch("builtins.print") as output:
            publisher.return_value.publish.return_value = {"state": "published", "commit": "c" * 40}
            publisher.return_value.deploy_pages.side_effect = Refused("fixture Pages failure")
            with self.assertRaises(SystemExit) as error:
                feed_publish.main(args)
            self.assertEqual(error.exception.code, 1)
            output.assert_not_called()


if __name__ == "__main__":
    unittest.main()
