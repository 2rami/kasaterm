#!/usr/bin/env python3
import importlib.util
import os
import tempfile
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
        os.utime(p, (stamp, stamp))
    return path


class PlanTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="prune-"))
        self.now = 1_800_000_000

    def plan(self, targets, cap_gib, busy="", sizes=None):
        sizes = sizes or {}
        return prune.plan(targets, cap_gib * GIB, 30 * 60, busy, self.now, measure=lambda p: sizes.get(p, 10 * GIB))

    def test_under_the_cap_nothing_is_removed(self):
        a = target(self.root, "a", 5, self.now)
        self.assertEqual(self.plan([a], 15)[1], [])

    def test_over_the_cap_the_longest_unused_go_first_until_under(self):
        old, mid, new = (target(self.root, n, h, self.now) for n, h in (("old", 9), ("mid", 5), ("new", 2)))
        _, doomed, left = self.plan([new, old, mid], 15)
        self.assertEqual([r["path"] for r in doomed], [old, mid])
        self.assertEqual(left, 10 * GIB)

    def test_a_folder_just_built_or_used_by_a_running_build_stays(self):
        fresh = target(self.root, "fresh", 0.1, self.now)
        env = target(self.root, "env", 9, self.now)
        cwd = target(self.root, "cwd", 8, self.now)
        idle = target(self.root, "idle", 7, self.now)
        busy = f"cargo test CARGO_TARGET_DIR={env}\nrustc --crate-name x PWD={cwd.parent}/crates/app"
        _, doomed, _ = self.plan([fresh, env, cwd, idle], 5, busy=busy)
        self.assertEqual([r["path"] for r in doomed], [idle])

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
