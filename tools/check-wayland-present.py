#!/usr/bin/env python3
"""Exercise the real windowed Vulkan path on an isolated Wayland compositor."""

import argparse
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import time


def stop(process):
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def wait_for_file(path, process, timeout=45):
    deadline = time.monotonic() + timeout
    while not path.exists():
        if process.poll() is not None:
            raise RuntimeError(f"process exited with {process.returncode} before {path.name}")
        if time.monotonic() >= deadline:
            raise TimeoutError(f"waiting for {path.name}")
        time.sleep(0.1)


class Control:
    def __init__(self, endpoint):
        host, port = endpoint["listen"].rsplit(":", 1)
        self.connection = socket.create_connection((host, int(port)), timeout=15)
        self.stream = self.connection.makefile("rwb")
        self.ident = 0
        self.token = endpoint["token"]

    def close(self):
        self.stream.close()
        self.connection.close()

    def send(self, method, params=None):
        self.ident += 1
        request = {
            "jsonrpc": "2.0", "id": self.ident,
            "method": method, "params": params or {},
        }
        self.stream.write((json.dumps(request) + "\n").encode())
        self.stream.flush()
        return self.ident

    def rpc(self, method, params=None):
        wanted = self.send(method, params)
        while True:
            line = self.stream.readline()
            if not line:
                raise RuntimeError("control connection closed")
            reply = json.loads(line)
            if reply.get("id") == wanted:
                if "error" in reply:
                    raise RuntimeError(reply["error"])
                return reply.get("result")


def wait_for_control_info(path, process, timeout=45):
    deadline = time.monotonic() + timeout
    while True:
        try:
            return json.loads(path.read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            # The server creates the file before writing its JSON. Existence
            # alone is not readiness, particularly on a busy CI runner.
            pass
        if process.poll() is not None:
            raise RuntimeError(f"process exited with {process.returncode} before control info")
        if time.monotonic() >= deadline:
            raise TimeoutError(f"waiting for complete JSON in {path.name}")
        time.sleep(0.1)


def submitted_frames(log):
    return sum(
        "window frame:" in line and "submitted=true" in line
        for line in log.splitlines()
    )


def wait_for_frames(control, emulator, log_path, before_frame, before_submitted=0):
    deadline = time.monotonic() + 30
    while True:
        time.sleep(0.5)
        if emulator.poll() is not None:
            raise RuntimeError(f"emulator exited with {emulator.returncode}")
        after = control.rpc("status")
        log = log_path.read_text()
        submitted = submitted_frames(log)
        if after["frame"] >= before_frame + 5 and submitted >= before_submitted + 5:
            return submitted, log
        if time.monotonic() >= deadline:
            raise TimeoutError("Wayland window did not continue presenting")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    compositor = None
    emulator = None
    control = None
    with tempfile.TemporaryDirectory(prefix="wayland-", dir=output) as temporary:
        runtime = Path(temporary)
        runtime.chmod(0o700)
        info = runtime / "control.json"
        env = dict(os.environ)
        # This test has its own display and a factory machine configuration.
        for key in list(env):
            if key.startswith("COPPERLINE_"):
                del env[key]
        for key in ("DISPLAY", "WINIT_UNIX_BACKEND", "MESA_VK_WSI_PRESENT_MODE"):
            env.pop(key, None)
        env.update(
            XDG_RUNTIME_DIR=str(runtime),
            WAYLAND_DISPLAY="wayland-present-test",
            WAYLAND_DEBUG="client",
            WGPU_BACKEND="vulkan",
            RUST_LOG="info",
            COPPERLINE_PRESENT_PROFILE="1",
        )
        with (output / "weston.log").open("w") as weston_log, (
            output / "copperline.log"
        ).open("w") as emulator_log:
            try:
                compositor = subprocess.Popen(
                    [
                        "weston", "--backend=headless-backend.so", "--use-pixman",
                        "--socket=wayland-present-test", "--width=1280", "--height=720",
                        "--idle-time=0", "--no-config", "--shell=kiosk-shell.so",
                    ],
                    env=env, stdout=weston_log, stderr=subprocess.STDOUT,
                )
                wait_for_file(runtime / env["WAYLAND_DISPLAY"], compositor)
                emulator = subprocess.Popen(
                    [
                        str(args.binary.resolve()), "--factory", "--noaudio",
                        "--control-gui", "127.0.0.1:0", "--control-info", str(info),
                    ],
                    env=env, stdout=emulator_log, stderr=subprocess.STDOUT,
                )
                # Keep one connection: reconnecting repeatedly would keep
                # showing the attachment OSD and prevent a truly idle window.
                control = Control(wait_for_control_info(info, emulator))
                control.rpc("hello", {"token": control.token})
                before = control.rpc("status")
                log_path = output / "copperline.log"
                submitted, log = wait_for_frames(control, emulator, log_path, before["frame"])
                if "window_system=Wayland" not in log or "backend=Vulkan" not in log:
                    raise RuntimeError("test did not use native Wayland and Vulkan")
                if not re.search(r"wl_surface[@#]\d+\.frame\(", log):
                    raise RuntimeError("no compositor frame callback was requested")
                # Let the event loop idle, then prove redraws resume as well as
                # emulation. A status-only check would miss a frozen window.
                control.rpc("pause")
                time.sleep(4)  # Let the attachment OSD expire before resuming.
                paused = control.rpc("status")
                previous_submitted = submitted_frames(log_path.read_text())
                # continue replies at the next stop; status remains available
                # while it is pending on this same connection.
                control.send("continue")
                submitted, _ = wait_for_frames(
                    control, emulator, log_path, paused["frame"], previous_submitted,
                )
                screenshot = output / "wayland-present.png"
                control.rpc("capture.screenshot", {"path": str(screenshot)})
                if not screenshot.is_file() or screenshot.stat().st_size == 0:
                    raise RuntimeError("no screenshot produced")
                control.rpc("shutdown")
                emulator.wait(timeout=15)
                if emulator.returncode != 0:
                    raise RuntimeError(f"emulator exited with {emulator.returncode}")
                print(f"Wayland/Vulkan: {submitted} submitted frames, pause/resume, screenshot and shutdown OK")
            finally:
                stop(emulator)
                stop(compositor)
                if control is not None:
                    control.close()


if __name__ == "__main__":
    main()
