# SPDX-License-Identifier: GPL-3.0-or-later
"""Netplay process supervision without requiring RetroArch or an X server."""

import asyncio
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


spec = importlib.util.spec_from_file_location(
    "libretro_netplay", Path(__file__).resolve().parents[1] / "check-libretro-netplay.py"
)
netplay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(netplay)

FINISHED = "[INFO] [Core]: Content ran for a total of: 00 hours, 00 minutes, 24 seconds.\n"


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

    def log(self, text=""):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / "retroarch.log"
        path.write_text(text)
        return path

    def test_the_marker_reports_the_time_the_peer_emulated(self):
        self.assertIsNone(netplay.completed_seconds("[INFO] [Netplay] Connected to: \"host\""))
        self.assertEqual(netplay.completed_seconds(FINISHED), 24)
        self.assertEqual(
            netplay.completed_seconds("Content ran for a total of: 01 hours, 02 minutes, 03 seconds."),
            3723,
        )

    async def test_a_wedged_client_still_completes_the_workload(self):
        # RetroArch 1.18 can stop responding in driver teardown once netplay
        # has disconnected: the run is over, the process just never leaves.
        host, client = self.waiting_process(), self.waiting_process()
        seconds = await netplay.wait_for_workload(
            host, client, self.log(FINISHED), lambda _: None, 5
        )
        self.assertEqual(seconds, 24)
        self.assertIsNone(client.poll())
        self.assertIsNone(host.poll())

    async def test_a_host_that_died_by_completion_is_not_success(self):
        # The marker can be there on the first read, so nothing in the wait
        # loop runs: the host still has to be alive to call this a pass.
        host = self.process("pass")
        host.wait()
        client = self.waiting_process()
        with self.assertRaisesRegex(RuntimeError, "host exited before the client completed"):
            await netplay.wait_for_workload(
                host, client, self.log(FINISHED), lambda _: None, 5
            )

    async def test_inputs_are_injected_until_the_workload_finishes(self):
        host, client = self.waiting_process(), self.waiting_process()
        log = self.log()
        updates = []

        async def finish():
            await asyncio.sleep(0.2)
            with log.open("a") as handle:
                handle.write(FINISHED)

        _, seconds = await asyncio.gather(
            finish(), netplay.wait_for_workload(host, client, log, updates.append, 5)
        )
        self.assertEqual(seconds, 24)
        self.assertTrue(updates)
        self.assertIsNone(host.poll())

    async def test_a_client_that_exits_having_finished_completes(self):
        host = self.waiting_process()
        client = self.process("pass")
        client.wait()
        seconds = await netplay.wait_for_workload(
            host, client, self.log(FINISHED), lambda _: None, 5
        )
        self.assertEqual(seconds, 24)
        self.assertIsNone(host.poll())

    async def test_a_client_that_exits_unfinished_is_not_success(self):
        host = self.waiting_process()
        client = self.process("raise SystemExit(7)")
        client.wait()
        with self.assertRaisesRegex(RuntimeError, "client exited before the workload finished: 7"):
            await netplay.wait_for_workload(host, client, self.log(), lambda _: None, 5)

    async def test_early_host_exit_is_not_success(self):
        host = self.process("pass")
        host.wait()
        client = self.waiting_process()
        with self.assertRaisesRegex(RuntimeError, "host exited before the client completed"):
            await netplay.wait_for_workload(host, client, self.log(), lambda _: None, 5)

    async def test_workload_timeout_still_fails(self):
        host, client = self.waiting_process(), self.waiting_process()
        with self.assertRaisesRegex(RuntimeError, "netplay test timed out"):
            await netplay.wait_for_workload(host, client, self.log(), lambda _: None, 0)

    async def test_both_peers_exiting_is_not_success(self):
        host, client = self.process("pass"), self.process("pass")
        host.wait()
        client.wait()
        with self.assertRaisesRegex(RuntimeError, "host exited before the client completed"):
            await netplay.wait_for_workload(host, client, self.log(), lambda _: None, 5)


if __name__ == "__main__":
    unittest.main()
