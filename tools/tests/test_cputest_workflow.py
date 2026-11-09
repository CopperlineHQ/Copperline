"""Execute the conformance workflow's actual run blocks with failing tools.

Only literal run blocks are extracted; unsupported workflow formatting fails
the test instead of silently testing a different command. No YAML dependency
or CPU corpus is needed for these shell/exit-status regressions.
"""

import os
from pathlib import Path
import re
import subprocess
import tempfile
import textwrap
import unittest


WORKFLOW = Path(__file__).resolve().parents[2] / ".github/workflows/cputest.yml"
PR_STEP = "Curated 68020, 68030, 68040 and 68060 instruction suites"
FULL_STEP = "Run the conformance suite"


def run_block(name):
    source = WORKFLOW.read_text()
    # Explicit Bash selects GitHub's -eo pipefail invocation. Reject overrides
    # so the test's shell always corresponds to the workflow's configuration.
    if not re.search(r"(?m)^defaults:\n  run:\n    shell: bash$", source):
        raise AssertionError("conformance workflow needs explicit Bash defaults")
    if re.findall(r"(?m)^\s*shell: (\S+)\s*$", source) != ["bash"]:
        raise AssertionError("conformance shell overrides need test coverage")
    lines = source.splitlines()
    start = lines.index(f"      - name: {name}") + 1
    end = start
    while end < len(lines):
        line = lines[end]
        if line.strip() and len(line) - len(line.lstrip()) < 8:
            break
        end += 1
    step = lines[start:end]
    body = step[step.index("        run: |") + 1:]
    if not body or any(line.strip() and not line.startswith("          ") for line in body):
        raise AssertionError(f"{name}: expected a literal run block")
    return textwrap.dedent("\n".join(body))


class ConformanceWorkflowTests(unittest.TestCase):
    def execute(self, step, cpu="68000", fail_cpu="", fail_generator=""):
        with tempfile.TemporaryDirectory(prefix="cputest-workflow-") as directory:
            root = Path(directory)
            crate = root / "crates/cputest-runner"
            runner = crate / "target/release/cputest-runner"
            generator = crate / "tools/cputest-gen.sh"
            runner.parent.mkdir(parents=True)
            generator.parent.mkdir(parents=True)
            runner.write_text(
                '#!/bin/sh\n'
                'printf "%s\\n" "$3" >> "$CPUTEST_TEST_CALLS"\n'
                'printf "runner %s\\n" "$3" >&2\n'
                'if [ "$CPUTEST_TEST_FAIL_CPU" = all ] || '
                '[ "$3" = "$CPUTEST_TEST_FAIL_CPU" ]; then exit 7; fi\n'
            )
            generator.write_text(
                '#!/bin/sh\n'
                'if [ "$2" = "$CPUTEST_TEST_FAIL_GENERATOR" ]; then\n'
                '  echo "injected generator failure" >&2; exit 9\n'
                'fi\n'
                'printf "%s generated\\n" "$2"\n'
            )
            runner.chmod(0o755)
            generator.chmod(0o755)
            calls = root / "calls"
            calls.touch()
            temporary = root / "temporary"
            (temporary / "cputest-logs").mkdir(parents=True)
            env = os.environ | {
                "RUNNER_TEMP": str(temporary),
                "CPUTEST_TEST_CALLS": str(calls),
                "CPUTEST_TEST_FAIL_CPU": fail_cpu,
                "CPUTEST_TEST_FAIL_GENERATOR": fail_generator,
            }
            body = run_block(step).replace("${{ matrix.cpu }}", cpu)
            result = subprocess.run(
                ["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", body],
                cwd=root if step == PR_STEP else crate,
                env=env, capture_output=True, text=True, timeout=20,
            )
            logs = {path.name: path.read_text() for path in (temporary / "cputest-logs").iterdir()}
            return result, calls.read_text().splitlines(), logs

    def test_pr_job_keeps_all_cpu_results_and_rejects_any_runner_failure(self):
        for fail_cpu in ["", "68030", "all"]:
            with self.subTest(fail_cpu=fail_cpu):
                result, calls, logs = self.execute(PR_STEP, fail_cpu=fail_cpu)
                self.assertEqual(result.returncode, 1 if fail_cpu else 0, result.stdout + result.stderr)
                self.assertEqual(calls, ["68020", "68030", "68040", "68060"])
                for cpu in calls:
                    self.assertIn(f"runner {cpu}", logs[f"run-{cpu}.log"])

    def test_full_jobs_reject_runner_failure_through_tee(self):
        for cpu in ["68000", "68010"]:
            for fail_cpu in ["", cpu]:
                with self.subTest(cpu=cpu, fail_cpu=fail_cpu):
                    result, calls, logs = self.execute(FULL_STEP, cpu=cpu, fail_cpu=fail_cpu)
                    self.assertEqual(result.returncode, 7 if fail_cpu else 0, result.stdout + result.stderr)
                    self.assertEqual(calls, [cpu])
                    self.assertIn(f"runner {cpu}", logs["run.log"])

    def test_generator_failure_fails_before_running_that_cpu(self):
        result, calls, _ = self.execute(PR_STEP, fail_generator="68030")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(calls, ["68020"])
        self.assertIn("injected generator failure", result.stdout)


if __name__ == "__main__":
    unittest.main()
