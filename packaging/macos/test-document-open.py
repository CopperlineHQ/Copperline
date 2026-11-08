#!/usr/bin/env python3
"""Verify Finder document delivery on a logged-in macOS desktop using an app copy."""

import json
import os
from pathlib import Path
import plistlib
import shutil
import socket
import subprocess
import sys
import tempfile
import time

LSREGISTER = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"


def wait_for(predicate, description):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise RuntimeError(f"timed out waiting for {description}")


def main():
    source = Path(sys.argv[1]).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="copperline-document-test-") as temporary:
        root = Path(temporary)
        app = root / "Copperline Document Test.app"
        shutil.copytree(source, app)
        bundle_id = f"dev.copperline.document-test.{os.getpid()}"
        info = app / "Contents/Info.plist"
        metadata = plistlib.loads(info.read_bytes())
        assert metadata["CFBundleDocumentTypes"], "bundle advertises no file types"
        metadata["CFBundleIdentifier"] = bundle_id
        info.write_bytes(plistlib.dumps(metadata))
        (app / "Contents/MacOS/portable.txt").touch()
        subprocess.run(["codesign", "--force", "--deep", "--sign", "-", str(app)], check=True)
        subprocess.run([LSREGISTER, "-f", str(app)], check=True)
        try:
            # An intentional ROM-read failure proves the delivered TOML reached
            # the normal CLI startup path. The child then exits by itself.
            cold_marker = str(root / "cold-ROM-was-requested")
            warm_marker = str(root / "warm-ROM-was-requested")
            cold = root / "Cold café.toml"
            warm = root / "Warm config.toml"
            cold.write_text(f"rom = {json.dumps(cold_marker)}\n")
            warm.write_text(f"rom = {json.dumps(warm_marker)}\n")
            cold_log = root / "cold.log"
            subprocess.run([
                "open", "-n", "-g", "--env", "RUST_LOG=copperline=debug", "--stdout", str(cold_log),
                "--stderr", str(cold_log), "-a", str(app), str(cold),
            ], check=True, timeout=30)
            wait_for(lambda: cold_marker in cold_log.read_text(errors="replace"), "cold document startup")
            def cold_finished():
                query = ("ObjC.import('AppKit'); String($.NSRunningApplication."
                         f"runningApplicationsWithBundleIdentifier({json.dumps(bundle_id)}).count)")
                return subprocess.check_output(
                    ["osascript", "-l", "JavaScript", "-e", query], text=True,
                ).strip() == "0"
            wait_for(cold_finished, "cold bootstrap exit")


            warm_log = root / "warm.log"
            endpoint = root / "control.json"
            subprocess.run([
                "open", "-n", "-g", "--env", "RUST_LOG=copperline=debug", "--stdout", str(warm_log), "--stderr", str(warm_log),
                "-a", str(app), "--args", "--factory", "--noaudio",
                "--control-gui", ":0", "--control-info", str(endpoint),
            ], check=True)
            wait_for(endpoint.exists, "warm session control endpoint")
            details = json.loads(endpoint.read_text())
            host, port = details["listen"].rsplit(":", 1)
            with socket.create_connection((host, int(port)), timeout=10) as connection:
                stream = connection.makefile("rwb")
                request_id = 0

                def call(method, params):
                    nonlocal request_id
                    request_id += 1
                    request = dict(jsonrpc="2.0", id=request_id, method=method, params=params)
                    stream.write((json.dumps(request) + "\n").encode())
                    stream.flush()
                    while True:
                        reply = json.loads(stream.readline())
                        if reply.get("id") == request_id:
                            assert "error" not in reply, reply
                            return reply["result"]

                call("hello", {"token": details["token"]})
                call("status", {})
                subprocess.run(["open", "-g", "-a", str(app), str(warm)], check=True)
                wait_for(lambda: warm_marker in warm_log.read_text(errors="replace"), "warm document startup")
                call("status", {})  # Opening the document must preserve the existing guest.
                call("shutdown", {})
        except Exception:
            for log in root.glob("*.log"):
                contents = "\n".join(line for line in log.read_text(errors="replace").splitlines()
                                     if "token" not in line.lower())
                print(f"{log.name}:\n{contents}", file=sys.stderr)
            raise
        finally:
            # Address only this disposable bundle ID, never an installed app.
            cleanup = (
                "ObjC.import('AppKit');"
                f"var apps=$.NSRunningApplication.runningApplicationsWithBundleIdentifier({json.dumps(bundle_id)});"
                "for(var i=0;i<apps.count;i++){apps.objectAtIndex(i).forceTerminate;}"
            )
            subprocess.run(["osascript", "-l", "JavaScript", "-e", cleanup], check=True, capture_output=True)
            subprocess.run([LSREGISTER, "-u", str(app)], check=True)
    print("macOS cold and warm document opening passed; existing guest remained reachable")


if __name__ == "__main__":
    main()
