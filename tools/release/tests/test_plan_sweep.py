import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path

from tools.release import plan_sweep, policy


class PlanSweepTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="plan-sweep-")).resolve()
        os.chmod(self.root, 0o700)
        (self.root / "plans").mkdir()

    def plan(self, plan_id, age_h):
        d = self.root / "plans" / plan_id
        (d / "work" / "wt").mkdir(parents=True)
        (d / "work" / "wt" / "big.bin").write_bytes(b"x" * 1024)
        (d / "work" / "out").mkdir()
        for name in ("plan.json", "state.json"):
            (d / name).write_text("{}")
            stamp = 2_000_000_000 - age_h * 3600
            os.utime(d / name, (stamp, stamp))
        return d

    def ledger(self, requests, active=None):
        policy.atomic_json(self.root / "queue.json", {"schema": "kasaterm-preview-queue/1", "requests": requests, "active": active})

    def test_old_finished_plans_lose_work_but_keep_their_records(self):
        plans = [self.plan(f"p{i}", i) for i in range(8)]
        self.ledger({"a" * 40: {"state": "done", "plan_id": "p7"}})
        result = plan_sweep.sweep(self.root, keep=5, git="true")
        self.assertEqual(sorted(result["swept"]), ["p5", "p6", "p7"])
        for d in plans[:5]:
            self.assertTrue((d / "work" / "wt" / "big.bin").exists(), d.name)
        for d in plans[5:]:
            self.assertFalse((d / "work").exists(), d.name)
            self.assertTrue((d / "plan.json").exists() and (d / "state.json").exists())
        self.assertEqual(plan_sweep.sweep(self.root, keep=5, git="true"), {})

    def test_unfinished_requests_and_the_active_plan_stay(self):
        plans = [self.plan(f"p{i}", i) for i in range(6)]
        self.ledger({
            "a" * 40: {"state": "waiting_main", "plan_id": "p3", "cancelled_plans": ["p4"]},
            "b" * 40: {"state": "done", "plan_id": "p5"},
        }, active="b" * 40)
        self.assertEqual(plan_sweep.sweep(self.root, keep=1, git="true"), {"swept": ["p1", "p2"]})
        for name in ("p0", "p3", "p4", "p5"):
            self.assertTrue((self.root / "plans" / name / "work").exists(), name)

    def test_a_linked_work_folder_is_never_followed(self):
        outside = Path(tempfile.mkdtemp(prefix="plan-sweep-outside-"))
        (outside / "keep.txt").write_text("x")
        d = self.plan("p9", 99)
        shutil.rmtree(d / "work")
        (d / "work").symlink_to(outside)
        self.ledger({})
        self.assertEqual(plan_sweep.sweep(self.root, keep=0, git="true"), {})
        self.assertTrue((outside / "keep.txt").exists())

    def test_failures_are_reported_not_raised(self):
        os.chmod(self.root, 0o755)
        self.assertIn("error", plan_sweep.sweep_quietly(self.root))


if __name__ == "__main__":
    unittest.main()
