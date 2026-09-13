# SPDX-License-Identifier: GPL-3.0-or-later
"""Netplay process supervision without requiring RetroArch or an X server."""

import importlib.util
from pathlib import Path
import subprocess
import sys
import unittest


spec = importlib.util.spec_from_file_location(
    "libretro_netplay", Path(__file__).resolve().parents[1] / "check-libretro-netplay.py"
)
netplay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(netplay)


class PeerSupervisionTests(unittest.IsolatedAsyncioTestCase):
    def process(self, source):
        process = subprocess.Popen(
            [sys.executable, "-c", source], stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )

        def cleanup():
            if process.poll() is None:
                process.kill()
            process.wait()
            process.stdin.close()

        self.addCleanup(cleanup)
        return process

    def waiting_process(self, exit_code=0):
        return self.process(f"import sys; sys.stdin.readline(); sys.exit({exit_code})")

    async def test_client_completion_leaves_host_for_cleanup(self):
        host = self.waiting_process()
        client = self.process("import time; time.sleep(0.1)")
        input_updates = []
        await netplay.wait_for_client(host, client, input_updates.append, 5)
        self.assertTrue(input_updates)
        self.assertEqual(client.returncode, 0)
        self.assertIsNone(host.poll())

    async def test_client_failure_is_not_success(self):
        host = self.waiting_process()
        client = self.process("raise SystemExit(7)")
        client.wait()
        with self.assertRaisesRegex(RuntimeError, "client exited unsuccessfully: 7"):
            await netplay.wait_for_client(host, client, lambda _: None, 5)

    async def test_early_host_exit_is_not_success(self):
        host = self.process("pass")
        host.wait()
        client = self.waiting_process()
        with self.assertRaisesRegex(RuntimeError, "host exited before the client completed"):
            await netplay.wait_for_client(host, client, lambda _: None, 5)

    async def test_workload_timeout_still_fails(self):
        host, client = self.waiting_process(), self.waiting_process()
        with self.assertRaisesRegex(RuntimeError, "netplay test timed out"):
            await netplay.wait_for_client(host, client, lambda _: None, 0)

    async def test_both_peers_exiting_is_not_success(self):
        host, client = self.process("pass"), self.process("pass")
        host.wait()
        client.wait()
        with self.assertRaisesRegex(RuntimeError, "host exited before the client completed"):
            await netplay.wait_for_client(host, client, lambda _: None, 5)


if __name__ == "__main__":
    unittest.main()
