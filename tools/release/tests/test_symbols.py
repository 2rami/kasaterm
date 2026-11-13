import json
import tempfile
import unittest
from pathlib import Path

from tools.release import symbols
from tools.release.proc import Result, Runner


class FakeRunner(Runner):
    """실행 목록은 정해 둔 것, 받기는 산출물이 있는 실행만 PDB 를 떨군다."""

    def __init__(self, runs, artifacts):
        super().__init__("local")
        self.runs, self.artifacts, self.downloads = runs, artifacts, []

    def execute(self, argv, cwd, timeout, env):
        if argv[1:3] == ["run", "list"]:
            return Result(0, json.dumps(self.runs))
        if argv[1:3] == ["run", "download"]:
            run_id = int(argv[3])
            self.downloads.append(run_id)
            if run_id not in self.artifacts:
                return Result(1, "", "no valid artifacts found to download")
            out = Path(argv[argv.index("--dir") + 1])
            for name in self.artifacts[run_id]:
                (out / name).write_bytes(b"pdb")
            return Result(0)
        return Result(127, "", "unexpected")


def run(run_id, title, branch="main", conclusion="success", status="completed"):
    return {"databaseId": run_id, "displayTitle": title, "headBranch": branch, "status": status, "conclusion": conclusion}


class SweepTest(unittest.TestCase):
    def setUp(self):
        self.state = Path(tempfile.mkdtemp(prefix="symbols-"))

    def test_finished_windows_runs_are_kept_per_tag_and_not_fetched_again(self):
        runs = [run(3, "release v0.2.223 (windows)"), run(2, "v0.2.223", branch="v0.2.223"), run(1, "release v0.2.222 (windows)")]
        runner = FakeRunner(runs, {3: ["kasaterm.pdb", "kasaterm_cli.pdb"], 1: ["kasaterm.pdb"]})
        self.assertEqual(symbols.sweep(self.state, runner=runner, gh="gh"), {"saved": ["v0.2.223", "v0.2.222"]})
        self.assertTrue((self.state / "symbols/v0.2.223/kasaterm_cli.pdb").is_file())
        self.assertTrue((self.state / "symbols/v0.2.222/kasaterm.pdb").is_file())
        self.assertEqual(runner.downloads, [3, 1], "a tag already kept is not downloaded from its other run")

        again = FakeRunner(runs, {})
        self.assertEqual(symbols.sweep(self.state, runner=again, gh="gh"), {})
        self.assertEqual(again.downloads, [])

    def test_runs_without_the_artifact_are_remembered_and_unfinished_runs_wait(self):
        runs = [run(5, "release v0.2.224 (windows)", status="in_progress", conclusion=""), run(4, "release v0.2.210 (macos)"),
                run(6, "release v0.2.209 (windows)", conclusion="failure")]
        runner = FakeRunner(runs, {})
        self.assertEqual(symbols.sweep(self.state, runner=runner, gh="gh"), {})
        self.assertEqual(runner.downloads, [4])
        later = FakeRunner(runs, {})
        symbols.sweep(self.state, runner=later, gh="gh")
        self.assertEqual(later.downloads, [], "a run that had no symbols is not asked every tick")

    def test_an_unreadable_run_list_is_reported_without_raising(self):
        class Down(FakeRunner):
            def execute(self, argv, cwd, timeout, env):
                return Result(1, "", "HTTP 502")
        self.assertIn("error", symbols.sweep(self.state, runner=Down([], {}), gh="gh"))


if __name__ == "__main__":
    unittest.main()
