#!/usr/bin/env python3
"""Compare release copperline-bench binaries against the tracked AROS baseline.

No third-party Python packages are needed. Build both binaries before invoking
this tool: compilation and emulation must not compete for the host CPU.
"""

import argparse
from dataclasses import asdict, dataclass
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
POLICY = ROOT / "tools/performance/baseline.json"
ROM_NAMES = ("aros-amiga-m68k-rom.bin", "aros-amiga-m68k-ext.bin")
NUMBER = r"([0-9]+(?:\.[0-9]+)?)"
SUMMARY = re.compile(
    rf"^bench: {NUMBER}s emulated in {NUMBER}s wall, ([0-9]+) frames .*?$",
    re.MULTILINE,
)
PRESENTED = re.compile(r", ([0-9]+) presented \([0-9]+ unchanged, [0-9]+ renders skipped\)$")
BUDGET = re.compile(r"^bench budget: ([0-9]+) of ([0-9]+) frames over the 20ms PAL budget$", re.MULTILINE)
CHECKSUM = re.compile(r"^bench render checksum: (0x[0-9a-f]+)$", re.MULTILINE)


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_policy(path):
    policy = json.loads(path.read_text())
    if policy["schema_version"] != 1:
        raise ValueError("unsupported performance policy version")
    if not re.fullmatch(r"[0-9a-f]{40}", policy["baseline_revision"]):
        raise ValueError("baseline_revision must pin a complete Git commit SHA")
    for key, lower, upper in (
        ("seconds", 10, 120), ("pairs", 5, 15),
        ("max_regression_percent", 0, 25), ("sample_timeout_seconds", 1, 300),
    ):
        value = policy[key]
        if type(value) not in (int, float) or not math.isfinite(value) or not lower <= value <= upper:
            raise ValueError(f"invalid {key}: {value!r}")
    if type(policy["pairs"]) is not int or policy["pairs"] % 2 != 1:
        raise ValueError("pairs must be an odd integer")
    if set(policy["rom_sha256"]) != set(ROM_NAMES):
        raise ValueError("both AROS ROM hashes must be pinned")
    for name, digest in policy["rom_sha256"].items():
        if not re.fullmatch(r"[0-9a-f]{64}", digest) or sha256(ROOT / "assets/aros" / name) != digest:
            raise ValueError(f"AROS hash mismatch: {name}")
    workloads = policy["workloads"]
    if not workloads or len({w["name"] for w in workloads}) != len(workloads):
        raise ValueError("expected nonempty, uniquely named workloads")
    for workload in workloads:
        if not re.fullmatch(r"[a-z0-9-]+", workload["name"]):
            raise ValueError("invalid workload name")
        config = path.parent / workload["config"]
        if config.parent.resolve() != path.parent.resolve() or not config.is_file():
            raise ValueError(f"missing workload configuration: {config}")
    return policy


@dataclass(frozen=True)
class Sample:
    emulated_seconds: float
    wall_seconds: float
    frames: int
    presented: int | None
    checksum: str | None

    @property
    def cost(self):
        return self.wall_seconds / self.frames

    @property
    def signature(self):
        return (self.emulated_seconds, self.frames, self.presented, self.checksum)


def parse_sample(output, seconds, render):
    matches = list(SUMMARY.finditer(output))
    budgets = BUDGET.findall(output)
    checksums = CHECKSUM.findall(output)
    if len(matches) != 1 or len(budgets) != 1:
        raise ValueError("expected one complete benchmark summary and frame budget")
    summary = matches[0]
    emulated, wall = map(float, summary.group(1, 2))
    frames = int(summary.group(3))
    presented_match = PRESENTED.search(summary.group())
    presented = int(presented_match.group(1)) if presented_match else None
    if not seconds <= emulated <= seconds + 0.05 or not math.isfinite(wall) or wall <= 0:
        raise ValueError("benchmark did not complete the requested positive interval")
    if not seconds * 49 <= frames <= math.ceil(seconds * 51):
        raise ValueError("benchmark frame count is outside the PAL workload")
    over_budget, sampled_frames = map(int, budgets[0])
    # step_frame may return at an internal boundary before retiring a PAL
    # frame, so the bench's timing sample count can exceed its frame count.
    if sampled_frames < frames or over_budget > sampled_frames:
        raise ValueError("benchmark frame samples are incomplete")
    if render:
        if presented is None or not 0 < presented <= frames or len(checksums) != 1:
            raise ValueError("render workload did not present frames and emit a checksum")
    elif presented is not None or checksums:
        raise ValueError("core-only workload unexpectedly rendered")
    return Sample(emulated, wall, frames, presented, checksums[0] if render else None)


def clean_environment():
    # Diagnostics, pacing, ROM search and renderer overrides must not change
    # either the workload or its cost. An explicit config bypasses saved defaults.
    env = {key: value for key, value in os.environ.items() if not key.startswith("COPPERLINE_")}
    env["TZ"] = "UTC"
    return env


def run_sample(binary, config, policy, render, log):
    command = [str(binary), "--config", str(config), "--seconds", str(policy["seconds"]),
               "--rom", str(ROOT / "assets/aros" / ROM_NAMES[0]),
               "--ext", str(ROOT / "assets/aros" / ROM_NAMES[1])]
    if render:
        command.append("--render")
    # A fresh working directory keeps guest persistent data out of later runs.
    # The timeout bounds a stalled emulator; raw output survives every failure.
    with tempfile.TemporaryDirectory(prefix="copperline-performance-") as work:
        with log.open("w") as output:
            output.write(json.dumps(command) + "\n")
            output.flush()
            try:
                result = subprocess.run(command, cwd=work, env=clean_environment(),
                                        stdout=output, stderr=subprocess.STDOUT,
                                        timeout=policy["sample_timeout_seconds"], check=False)
            except subprocess.TimeoutExpired as error:
                raise ValueError(f"benchmark timed out; see {log}") from error
    if result.returncode:
        raise ValueError(f"benchmark exited {result.returncode}; see {log}")
    try:
        return parse_sample(log.read_text(), policy["seconds"], render)
    except ValueError as error:
        raise ValueError(f"{error}; see {log}") from error


def evaluate(pairs, max_regression_percent):
    if len(pairs) < 5:
        raise ValueError("at least five complete pairs are required")
    for index, label in enumerate(("baseline", "candidate")):
        if len({pair[index].signature for pair in pairs}) != 1:
            raise ValueError(f"{label} workload differs between trials")
    # Pairing cancels host drift shared by both binaries. Check noise in
    # the ratios used for the verdict, rather than rejecting that shared drift.
    ratios = [candidate.cost / baseline.cost for baseline, candidate in pairs]
    median = statistics.median(ratios)
    mad_fraction = statistics.median(abs(ratio - median) for ratio in ratios) / median
    regression = (median - 1) * 100
    changed = pairs[0][0].signature != pairs[0][1].signature
    result = {
        "baseline_median_ms_per_frame": statistics.median(pair[0].cost for pair in pairs) * 1000,
        "candidate_median_ms_per_frame": statistics.median(pair[1].cost for pair in pairs) * 1000,
        "paired_ratios": ratios,
        "paired_ratio_mad_percent": mad_fraction * 100,
        "regression_percent": regression,
        "workload_changed": changed,
        "passed": not changed and mad_fraction <= 0.05 and regression <= max_regression_percent,
    }
    if changed:
        result["error"] = "workload changed, baseline refresh needed; ratios are informational"
    elif mad_fraction > 0.05:
        result["error"] = "unstable paired timings (median absolute deviation exceeds 5%)"
    return result


def write_report(output, report):
    (output / "report.json").write_text(json.dumps(report, indent=2, allow_nan=False) + "\n")


def compare(args, policy):
    binaries = {label: path.resolve() for label, path in (
        ("baseline", args.baseline_binary), ("candidate", args.candidate_binary))}
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    report = {
        "policy": policy, "host": platform.platform(), "processor": platform.processor(),
        "binaries": {label: {"path": str(path), "sha256": sha256(path)}
                     for label, path in binaries.items()}, "cases": [], "passed": False,
    }
    write_report(output, report)
    failed = False
    for workload in policy["workloads"]:
        config = (args.policy.parent / workload["config"]).resolve()
        for render in (False, True):
            name = workload["name"] + ("-render" if render else "-core")
            case = {"name": name, "config_sha256": sha256(config), "pairs": []}
            report["cases"].append(case)
            pairs = []
            print(f"{name}: warming both binaries", flush=True)
            try:
                warmups = [run_sample(binary, config, policy, render,
                                      output / f"{name}-warmup-{label}.log")
                           for label, binary in binaries.items()]
                for trial in range(policy["pairs"]):
                    order = ("baseline", "candidate") if trial % 2 == 0 else ("candidate", "baseline")
                    pair = {}
                    for label in order:
                        pair[label] = run_sample(binaries[label], config, policy, render,
                                                 output / f"{name}-{trial + 1}-{label}.log")
                    pairs.append((pair["baseline"], pair["candidate"]))
                    case["pairs"].append({label: asdict(sample) for label, sample in pair.items()})
                    write_report(output, report)
                    print(f"{name} pair {trial + 1}: baseline {pair['baseline'].wall_seconds:.3f}s, "
                          f"candidate {pair['candidate'].wall_seconds:.3f}s", flush=True)
                if any(sample.signature != pairs[0][index].signature for index, sample in enumerate(warmups)):
                    raise ValueError("warmup workload differs from measured samples")
                case.update(evaluate(pairs, policy["max_regression_percent"]))
                failed |= not case["passed"]
                print(f"{name}: {case['regression_percent']:+.2f}% "
                      f"({'PASS' if case['passed'] else 'FAIL'})"
                      + (f": {case['error']}" if "error" in case else ""), flush=True)
            except ValueError as error:
                failed = True
                case.update({"passed": False, "error": str(error)})
                print(f"{name}: FAIL: {error}", file=sys.stderr, flush=True)
            write_report(output, report)
    report["passed"] = not failed
    write_report(output, report)
    summary = ["# AROS performance gate", "",
               f"Baseline: `{policy['baseline_revision']}`. "
               f"Limit: {policy['max_regression_percent']}% slower, median of {policy['pairs']} alternating pairs.",
               "", "| Workload | Baseline ms/frame | Candidate ms/frame | Median paired change | Result |",
               "| --- | ---: | ---: | ---: | --- |"]
    for case in report["cases"]:
        if "regression_percent" not in case:
            summary.append(f"| {case['name']} | - | - | - | FAIL: {case['error']} |")
        else:
            summary.append(f"| {case['name']} | {case['baseline_median_ms_per_frame']:.3f} | "
                           f"{case['candidate_median_ms_per_frame']:.3f} | "
                           f"{case['regression_percent']:+.2f}% | "
                           f"{'PASS' if case['passed'] else 'FAIL'}"
                           + (f": {case['error']}" if "error" in case else "") + " |")
    (output / "summary.md").write_text("\n".join(summary) + "\n")
    return int(failed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--policy", type=Path, default=POLICY)
    parser.add_argument("--print-baseline", action="store_true", help="validate policy and print the pinned source SHA")
    parser.add_argument("--baseline-binary", type=Path)
    parser.add_argument("--candidate-binary", type=Path)
    parser.add_argument("--output", type=Path, default=ROOT / "target/performance-gate")
    args = parser.parse_args()
    try:
        policy = load_policy(args.policy)
        if args.print_baseline:
            print(policy["baseline_revision"])
            return 0
        if args.baseline_binary is None or args.candidate_binary is None:
            parser.error("--baseline-binary and --candidate-binary are required")
        return compare(args, policy)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"performance gate: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
