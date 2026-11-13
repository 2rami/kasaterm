#!/usr/bin/env python3
import importlib.util
import os
import tempfile
import time
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("prune", Path(__file__).with_name("prune-build-outputs.py"))
prune = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(prune)

GIB = 2**30


def target(root, name, age_h, now):
    path = root / name / "target"
    (path / "debug").mkdir(parents=True)
    (path / "CACHEDIR.TAG").write_text(prune.CARGO_TAG + "\n")
    stamp = now - age_h * 3600
    for p in (path / "debug", path / "CACHEDIR.TAG", path):
        os.utime(p, (stamp, stamp), follow_symlinks=False)
    return path


def used(ps="", lsof=""):
    """used_paths 와 같은 변환 — 프로세스 조회 결과를 실제 경로로."""
    return {prune.real(p) for p in prune.env_paths(ps) | prune.lsof_paths(lsof)}


class PlanTest(unittest.TestCase):
    def setUp(self):
        self.base = Path(tempfile.mkdtemp(prefix="prune-"))
        self.root = Path(prune.real(self.base / "real"))
        self.root.mkdir()
        self.now = int(time.time())

    def plan(self, targets, cap_gib, in_use=frozenset(), sizes=None):
        sizes = sizes or {}
        return prune.plan(targets, cap_gib * GIB, 30 * 60, set(in_use), self.now,
                          measure=lambda p: sizes.get(p, 10 * GIB))

    def doomed(self, *args, **kwargs):
        return [r["path"] for r in self.plan(*args, **kwargs)[1]]

    def test_under_the_cap_nothing_is_removed(self):
        a = target(self.root, "a", 5, self.now)
        self.assertEqual(self.doomed([a], 15), [])

    def test_over_the_cap_the_longest_unused_go_first_until_under(self):
        old, mid, new = (target(self.root, n, h, self.now) for n, h in (("old", 9), ("mid", 5), ("new", 2)))
        rows, doomed, left = self.plan([new, old, mid], 15)
        self.assertEqual([r["path"] for r in doomed], [old, mid])
        self.assertEqual(left, 10 * GIB)

    def test_a_build_seen_through_the_tmp_alias_protects_its_target(self):
        # /tmp → /private/tmp 처럼 다른 이름으로 본 작업 공간도 같은 곳이다.
        kt = target(self.root, "kt-x", 9, self.now)
        alias = self.base / "tmp"
        alias.symlink_to(self.root)
        self.assertEqual(self.doomed([kt], 1, in_use=used(ps=f"cargo build PWD={alias}/kt-x")), [])

    def test_a_relative_target_dir_is_read_from_the_builds_working_folder(self):
        ws = self.root / "ws"
        custom = ws / "out"
        (custom / "debug").mkdir(parents=True)
        (custom / "CACHEDIR.TAG").write_text(prune.CARGO_TAG)
        os.utime(custom, (self.now - 9 * 3600,) * 2)
        os.utime(custom / "debug", (self.now - 9 * 3600,) * 2)
        os.utime(custom / "CACHEDIR.TAG", (self.now - 9 * 3600,) * 2)
        seen = used(ps=f"cargo test PWD=/somewhere/else CARGO_TARGET_DIR={ws}/out\n")
        self.assertEqual(self.doomed([custom], 1, in_use=seen), [])
        relative = used(ps=f"cargo test PWD={ws} CARGO_TARGET_DIR=out")
        self.assertIn(prune.real(custom), relative)

    def test_a_running_binary_or_open_file_inside_a_target_protects_it(self):
        app = target(self.root, "app", 9, self.now)
        idle = target(self.root, "idle", 8, self.now)
        seen = used(lsof=f"p123\nftxt\nn{app}/debug/kasaterm\n")
        self.assertEqual(self.doomed([app, idle], 5, in_use=seen), [idle])

    def test_a_target_another_one_links_into_stays_until_that_one_goes_too(self):
        # 실제로 있던 꼴: report-senders/source/target/ios → tell-mobile/source/target/ios.
        shared = target(self.root / "tell-mobile", "source", 9, self.now)
        (shared / "ios").mkdir()
        sender = target(self.root / "report-senders", "source", 5, self.now)
        (sender / "ios").symlink_to(shared / "ios")
        for p in (sender / "ios", sender, shared / "ios", shared):
            os.utime(p, (self.now - (5 if sender in (p, p.parent) else 9) * 3600,) * 2, follow_symlinks=False)
        for cap in (15, 1):
            doomed = self.doomed([shared, sender], cap)
            self.assertNotIn(shared, doomed, "the linked-into target cannot go while the one linking to it stays")
            self.assertEqual(doomed, [sender], "removing the linking target only drops its symlink")

    def test_a_target_picked_up_after_planning_is_left_alone(self):
        a = target(self.root, "a", 9, self.now)
        rows, doomed, _ = self.plan([a], 1)
        self.assertEqual([r["path"] for r in doomed], [a])
        busy = lambda: used(ps=f"cargo build PWD={a.parent}")
        self.assertFalse(prune.still_idle(doomed[0], 30 * 60, measure_used=busy, now=self.now))
        self.assertTrue(prune.still_idle(doomed[0], 30 * 60, measure_used=set, now=self.now))
        (a / "debug" / "new.o").write_text("x")
        self.assertFalse(prune.still_idle(doomed[0], 30 * 60, measure_used=set, now=self.now), "touched since planning")

    def test_only_cargo_tagged_real_folders_are_targets(self):
        real = target(self.root, "real", 3, self.now)
        (self.root / "plain" / "target").mkdir(parents=True)
        other = self.root / "other" / "target"
        other.mkdir(parents=True)
        (other / "CACHEDIR.TAG").write_text("Signature: something else\n")
        (self.root / "link").symlink_to(real.parent)
        self.assertEqual(prune.find_targets([(self.root, 2)]), [real])


if __name__ == "__main__":
    unittest.main()
