import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("cargo_report", Path(__file__).parents[1] / "cargo_report.py")
wrapper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(wrapper)


class CargoReportTests(unittest.TestCase):
    def run_child(self, stdout="", stderr="", status=0):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        code = f"import sys; sys.stdout.buffer.write({stdout.encode()!r}); sys.stderr.buffer.write({stderr.encode()!r}); sys.exit({status})"
        child = Path(temp.name) / "child.py"
        child.write_text(code, encoding="utf-8")
        return wrapper.execute([sys.executable, str(child)], Path(temp.name))

    def diagnostic(self, level="error", message="bad type", rendered=None):
        return json.dumps({"reason": "compiler-message", "message": {
            "level": level, "message": message, "code": {"code": "E0308"},
            "spans": [{"file_name": "src/cia.rs", "line_start": 12, "column_start": 3, "is_primary": True}],
            "rendered": rendered}}) + "\n"

    def test_success_is_one_line_with_warning_count_and_full_logs(self):
        stdout = self.diagnostic("warning") * 2 + "noisy output\n" * 2000
        stderr = "warning: build script warning\n" + "progress\n" * 2000
        status, text, folder = self.run_child(stdout, stderr)
        self.assertEqual(status, 0)
        self.assertEqual(len(text.splitlines()), 1)
        self.assertIn("warnings=2", text)
        self.assertEqual((folder / "stdout.log").read_text(), stdout)
        self.assertEqual((folder / "stderr.log").read_text(), stderr)

    def test_unique_compiler_errors_keep_locations_and_notes(self):
        diagnostic = self.diagnostic(rendered="error[E0308]: bad type\n --> src/cia.rs:12:3\n note: expected u16, found u8\n")
        status, text, folder = self.run_child(diagnostic * 2, "later noise\n" * 1000, 101)
        self.assertEqual(status, 101)
        self.assertEqual(text.count("error[E0308]"), 1)
        self.assertIn("src/cia.rs:12:3", text)
        self.assertIn("expected u16, found u8", text)
        self.assertEqual(json.loads((folder / "result.json").read_text())["exit_status"], 101)

    def test_failed_test_name_and_assertion_difference(self):
        output = "test cia::irq ... FAILED\nfailures:\n\n---- cia::irq stdout ----\nthread 'cia::irq' panicked at src/cia.rs:42:1:\nassertion `left == right` failed\n  left: 2\n right: 4\n\nfailures:\n    cia::irq\ntest result: FAILED\n"
        status, text, _ = self.run_child(output, "error: test failed, to rerun pass --lib\n", 101)
        self.assertEqual(status, 101)
        self.assertIn("test failure:", text)
        self.assertIn("FAILED cia::irq", text)
        self.assertIn("left: 2", text)
        self.assertIn("right: 4", text)

    def test_original_failure_is_not_lost_behind_progress(self):
        status, text, folder = self.run_child("progress\n" * 2000, "error: linker crashed\n" + "noise\n" * 2000, 7)
        self.assertEqual(status, 7)
        self.assertIn("unexpected failure:", text)
        self.assertIn("linker crashed", text)
        self.assertIn(str(folder), text)
        self.assertLessEqual(text.count("noise"), 2)

    def test_unknown_failure_uses_bounded_beginning_and_preserves_bytes(self):
        status, text, folder = self.run_child("first output\n" + "x" * 20000, "unrecognized trouble\n", 9)
        self.assertEqual(status, 9)
        self.assertIn("first output", text)
        self.assertLess(len(text), wrapper.LIMIT)
        self.assertEqual((folder / "stdout.log").stat().st_size, 20013)

    def test_report_is_bounded_for_many_errors(self):
        output = "".join(self.diagnostic(message=str(i), rendered=f"error: {i}\n" + "x" * 3000) for i in range(50))
        _, text, _ = self.run_child(output, status=101)
        self.assertLess(len(text), wrapper.LIMIT)
        self.assertIn("Full logs:", text)

    def test_json_option_precedes_test_arguments(self):
        self.assertEqual(wrapper.cargo_command(["--", "+stable", "test", "--locked", "--", "--exact", "cia::irq"]),
                         ["cargo", "+stable", "test", "--locked", "--message-format=json", "--", "--exact", "cia::irq"])
        self.assertEqual(wrapper.cargo_command(["fmt", "--", "--check"]), ["cargo", "fmt", "--", "--check"])
        with self.assertRaises(ValueError):
            wrapper.cargo_command(["build", "--message-format=human"])

    def test_missing_executable_is_compact_failure(self):
        with tempfile.TemporaryDirectory() as folder:
            status, text, logs = wrapper.execute([str(Path(folder) / "absent-cargo")], folder)
            self.assertEqual(status, 127)
            self.assertIn("unexpected failure", text)
            self.assertTrue((logs / "stderr.log").stat().st_size)


if __name__ == "__main__":
    unittest.main()
