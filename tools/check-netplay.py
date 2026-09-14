#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Run two local netplay peers, optionally with late-joining spectators, and
compare their confirmed headless captures."""

import argparse
import hashlib
import math
import os
from pathlib import Path
import re
import secrets
import socket
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/copperline"))
    parser.add_argument("--seconds", type=float, default=20.0)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--internet", action="store_true", help="use Internet invitations and public relays")
    parser.add_argument("--relay-only", action="store_true", help="disable direct IP paths (requires --internet)")
    parser.add_argument("--spectators", type=int, default=0, help="spectators that join the host (0..8)")
    parser.add_argument("--spectate-after", type=float, default=1.0,
                        help="wall-clock seconds after the players connect before spectators start")
    parser.add_argument("machine_args", nargs=argparse.REMAINDER,
                        help="machine arguments after --, e.g. --config game.toml")
    args = parser.parse_args()
    if not math.isfinite(args.seconds) or args.seconds <= 0:
        parser.error("--seconds must be finite and positive")
    if args.relay_only and not args.internet:
        parser.error("--relay-only requires --internet")
    if not 0 <= args.spectators <= 8:
        parser.error("--spectators must be 0..8")
    if not math.isfinite(args.spectate_after) or args.spectate_after < 0:
        parser.error("--spectate-after must be finite and non-negative")
    extra = args.machine_args
    if extra[:1] == ["--"]:
        extra = extra[1:]
    if any(arg.startswith("--netplay-") for arg in extra):
        parser.error("this check supplies its own netplay settings")
    output = (args.output_dir or Path(tempfile.mkdtemp(prefix="copperline-netplay-"))).resolve()
    output.mkdir(parents=True, exist_ok=True)
    ports = []
    if not args.internet:
        # Reserve distinct ports together, then release them immediately before launch.
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as first, \
                socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as second:
            first.bind(("127.0.0.1", 0))
            second.bind(("127.0.0.1", 0))
            ports = [first.getsockname()[1], second.getsockname()[1]]
    invitation = output / "invitation.txt"
    spectator_invitation = output / "spectator-invitation.txt"
    if args.internet and (invitation.exists() or spectator_invitation.exists()):
        parser.error("the output directory already contains an invitation; use a fresh directory")
    session = secrets.token_hex(16)
    processes = []
    logs = []
    names = ["player1", "player2"] + [f"spectator{n + 1}" for n in range(args.spectators)]

    def wait_for_code(path, deadline):
        while not path.exists():
            if processes[0].poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError(f"host did not create {path.name}; see {output}")
            time.sleep(0.05)
        # File creation precedes writing; wait for the completed code.
        while not (code := path.read_text()):
            if processes[0].poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError(f"host {path.name} remained empty; see {output}")
            time.sleep(0.05)
        return code

    def launch(name, role):
        log = (output / f"{name}.log").open("w")
        logs.append(log)
        command = [str(args.binary.resolve()), "--factory", "--model", "A500",
                   "--serial", "off", "--port1", "joystick", "--port2", "joystick"]
        # Only the host has game assets/configuration. Guests and spectators
        # prove setup transfer by starting with their bare local defaults.
        if role == 0:
            command += extra
        if args.internet:
            if role == 0:
                command += ["--netplay-host", str(invitation)]
                if args.spectators:
                    command += ["--netplay-spectators", str(args.spectators),
                                "--netplay-spectator-invite", str(spectator_invitation)]
            elif role == 1:
                command += ["--netplay-join", wait_for_code(invitation, time.monotonic() + 30)]
            else:
                command += ["--netplay-watch", wait_for_code(spectator_invitation, time.monotonic() + 30)]
            if args.relay_only:
                command += ["--netplay-relay-only"]
        elif role < 2:
            command += ["--netplay-bind", f"127.0.0.1:{ports[role]}",
                        "--netplay-peer", f"127.0.0.1:{ports[1 - role]}",
                        "--netplay-player", str(role + 1), "--netplay-session", session]
            if role == 0 and args.spectators:
                command += ["--netplay-spectators", str(args.spectators)]
        else:
            command += ["--netplay-watch", f"127.0.0.1:{ports[0]}", "--netplay-session", session,
                        "--netplay-bind", "127.0.0.1:0"]
        command += ["--noaudio"]
        if role < 2:
            command += ["--joy-after", str(args.seconds / 2), "red", "100", str(role + 1)]
        command += ["--screenshot-after", str(args.seconds), str(output / f"{name}.png")]
        processes.append(subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                          env={**os.environ, "RUST_LOG": "warn,copperline::netplay=info"}))

    try:
        for role in range(2):
            launch(names[role], role)
        if args.spectators:
            # Spectators join a game in progress and replay its history:
            # wait for the players to connect, then a little longer. Headless
            # players run unthrottled, so this is well into the game.
            host_log = output / f"{names[0]}.log"
            deadline = time.monotonic() + 60
            while "netplay: connected" not in host_log.read_text():
                if any(process.poll() is not None for process in processes) or time.monotonic() >= deadline:
                    raise RuntimeError(f"the players did not connect before the spectators joined; see {output}")
                time.sleep(0.05)
            time.sleep(args.spectate_after)
            if any(process.poll() is not None for process in processes):
                raise RuntimeError(f"a player exited before the spectators joined; use a longer --seconds; see {output}")
            for n in range(args.spectators):
                launch(names[2 + n], 2 + n)
        deadline = time.monotonic() + max(90, args.seconds * 10)
        for name, process in zip(names, processes):
            result = process.wait(timeout=max(0.1, deadline - time.monotonic()))
            if result:
                raise RuntimeError(f"{name} exited with {result}; see {output}")
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        for log in logs:
            log.close()
    hashes = []
    for role, name in enumerate(names):
        text = (output / f"{name}.log").read_text()
        if args.relay_only and "Internet route is relay" not in text:
            raise RuntimeError(f"{name} did not select a relay; see {output}")
        if role < 2:
            status = re.search(r"netplay: finished frames=(\d+) confirmed=(\d+) checked=(\d+)", text)
            if not status or status[1] != status[2] or (args.seconds >= 2 and int(status[3]) == 0):
                raise RuntimeError(f"{name} did not finish with confirmed input/checksums; see {output}")
            print(f"{name}: {status[1]} frames, checked through {status[3]}")
        else:
            status = re.search(r"netplay: spectating finished frames=(\d+) checked=(\d+) swaps=(\d+)", text)
            if not status or (args.seconds >= 2 and int(status[2]) == 0):
                raise RuntimeError(f"{name} did not finish with checked frames; see {output}")
            print(f"{name}: {status[1]} frames, checked through {status[2]}")
        hashes.append(hashlib.sha256((output / f"{name}.png").read_bytes()).hexdigest())
    if any(digest != hashes[0] for digest in hashes):
        raise RuntimeError(f"captures differ; see {output}")
    print(f"Matching PNG SHA-256: {hashes[0]}\nCaptures and logs: {output}")


if __name__ == "__main__":
    main()
