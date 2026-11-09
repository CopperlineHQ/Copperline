# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise gate failures, process supervision and artifacts without emulating."""

import contextlib
from dataclasses import replace
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "performance", Path(__file__).resolve().parents[1] / "check-performance.py"
)
performance = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = performance
spec.loader.exec_module(performance)


def log(wall=5, render=False, frames=1498):
    rendered = f", {frames} presented (1300 unchanged, 371 renders skipped)" if render else ""
    return (
        f"bench: 30.006s emulated in {wall:.3f}s wall, {frames} frames (300.0/s), realtime x6.00{rendered}\n"
        "bench frame ms: mean=2.33 p50=2.47 p90=2.87 p95=3.09 p99=3.50 max=5.18\n"
        "bench budget: 0 of 1841 frames over the 20ms PAL budget\n"
        + ("bench render checksum: 0x2c934000000\n" if render else "")
    )


def sample(wall=5, render=False):
    return performance.parse_sample(log(wall, render), 30, render)


class SampleTests(unittest.TestCase):
    def test_accepts_real_summary_with_more_step_calls_than_frames(self):
        self.assertEqual(sample().frames, 1498)
        self.assertEqual(sample(render=True).presented, 1498)

    def test_missing_duplicate_nonfinite_incomplete_and_wrong_modes_fail(self):
        for output, render in (
            ("", False), (log() * 2, False), (log().replace("5.000", "nan"), False),
            (log().replace("30.006", "0.000"), False), (log(frames=0), False),
            (log().replace("1841", "1000"), False), (log(render=True), False),
            (log(), True), (log(render=True).replace("1498 presented", "0 presented"), True),
            (log(render=True).replace("bench render checksum", "missing"), True),
        ):
            with self.subTest(output=output, render=render), self.assertRaises(ValueError):
                performance.parse_sample(output, 30, render)

    def test_gate_passes_stable_pairs_but_fails_a_slow_candidate(self):
        self.assertTrue(performance.evaluate([(sample(), sample(5.4))] * 5, 10)["passed"])
        result = performance.evaluate([(sample(), sample(6))] * 5, 10)
        self.assertFalse(result["passed"])
        self.assertAlmostEqual(result["regression_percent"], 20)

    def test_uses_paired_median_with_an_isolated_outlier(self):
        pairs = [(sample(), sample(5.2))] * 4 + [(sample(10), sample(10.4))]
        self.assertAlmostEqual(performance.evaluate(pairs, 10)["regression_percent"], 4)

    def test_empty_partial_changed_workload_and_noisy_samples_fail(self):
        pairs = [(sample(), sample())] * 5
        for broken in (
            [], pairs[:4], pairs[:4] + [(sample(), replace(sample(), frames=1499))],
            [(sample(5 + i), sample(5 + i)) for i in range(5)],
            [(sample(), sample(5 + i)) for i in range(5)],
        ):
            with self.subTest(pairs=broken), self.assertRaises(ValueError):
                performance.evaluate(broken, 10)

    def test_render_signature_changes_fail(self):
        with self.assertRaisesRegex(ValueError, "checksums differ"):
            performance.evaluate([(sample(render=True), replace(sample(render=True), checksum="0x0"))] * 5, 10)


class PolicyTests(unittest.TestCase):
    def test_committed_policy_and_rom_hashes_validate(self):
        self.assertEqual(len(performance.load_policy(performance.POLICY)["workloads"]), 2)

    def test_disabled_gate_mutable_ref_empty_workloads_and_changed_rom_fail(self):
        original = json.loads(performance.POLICY.read_text())
        for field, value in (
            ("baseline_revision", "v1"), ("pairs", 0), ("pairs", 6), ("pairs", 5.5),
            ("seconds", float("nan")), ("max_regression_percent", 100),
            ("workloads", []), ("rom_sha256", {}),
            ("rom_sha256", {**original["rom_sha256"], performance.ROM_NAMES[0]: "0" * 64}),
        ):
            policy = {**original, field: value}
            with self.subTest(field=field, value=value), patch.object(Path, "read_text", return_value=json.dumps(policy)):
                with self.assertRaises(ValueError):
                    performance.load_policy(performance.POLICY)


class ProcessTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.policy = performance.load_policy(performance.POLICY)

    def binary(self, name, source):
        binary = self.root / name
        binary.write_text(f"#!{sys.executable}\n" + source)
        binary.chmod(0o755)
        return binary

    def run_sample(self, binary):
        return performance.run_sample(binary, self.root / "test.toml", self.policy,
                                      False, self.root / "sample.log")

    def test_child_has_clean_environment_and_fresh_working_directory(self):
        binary = self.binary("check-env", "import os\nfrom pathlib import Path\n"
                             "assert not any(k.startswith('COPPERLINE_') for k in os.environ)\n"
                             "assert os.environ['TZ'] == 'UTC'\n"
                             "assert not Path('marker').exists()\nPath('marker').touch()\n"
                             f"print({log()!r})\n")
        with patch.dict(os.environ, {"COPPERLINE_DBG_TRACE": "1", "COPPERLINE_REAL_PACING_BUDGET": "1"}):
            self.run_sample(binary)
            self.run_sample(binary)

    def test_nonzero_exit_is_never_a_passing_sample_and_keeps_log(self):
        binary = self.binary("fail", f"print({log()!r})\nraise SystemExit(3)\n")
        with self.assertRaisesRegex(ValueError, "exited 3"):
            self.run_sample(binary)
        self.assertIn("bench:", (self.root / "sample.log").read_text())

    def test_timeout_kills_child_and_keeps_partial_output(self):
        binary = self.binary("stall", "import time\nprint('started', flush=True)\ntime.sleep(60)\n")
        self.policy["sample_timeout_seconds"] = 0.2
        with self.assertRaisesRegex(ValueError, "timed out"):
            self.run_sample(binary)
        self.assertIn("started", (self.root / "sample.log").read_text())

    def test_comparison_exit_status_and_reports_cover_real_subprocesses(self):
        for wall, expected in ((5, 0), (6, 1)):
            with self.subTest(candidate_wall=wall):
                baseline = self.binary("baseline", "import sys\n"
                                       f"print({log()!r} if '--render' not in sys.argv else {log(render=True)!r})\n")
                candidate = self.binary("candidate", "import sys\n"
                                        f"print({log(wall)!r} if '--render' not in sys.argv else {log(wall, True)!r})\n")
                output = self.root / f"results-{wall}"
                args = SimpleNamespace(baseline_binary=baseline, candidate_binary=candidate,
                                       policy=performance.POLICY, output=output)
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(performance.compare(args, self.policy), expected)
                report = json.loads((output / "report.json").read_text())
                self.assertEqual(report["passed"], expected == 0)
                self.assertEqual(len(report["cases"]), 4)
                self.assertTrue(all(len(case["pairs"]) == 5 for case in report["cases"]))
                self.assertIn("AROS performance gate", (output / "summary.md").read_text())
                self.assertEqual(len(list(output.glob("*.log"))), 48)


if __name__ == "__main__":
    unittest.main()
