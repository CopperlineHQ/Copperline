#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise patched GDB attach, profile, restart and exception contracts.

Build a C hunk executable with an ELF sibling containing main, then supply
Bartman's bundled GDB and (optionally) its generated compact unwind table.
Artifacts remain under --out. No ROM or executable is copied into the repo.
"""
import argparse
import math
import os
from pathlib import Path
import socket
import subprocess
import time


def quoted(value):
    return '"' + str(value).replace('\\', '\\\\').replace('"', '\\"') + '"'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--emulator', type=Path, default=Path('target/release/copperline'))
    parser.add_argument('--gdb', type=Path, required=True)
    parser.add_argument('--program', type=Path, required=True)
    parser.add_argument('--elf', type=Path, required=True)
    parser.add_argument('--unwind', type=Path)
    parser.add_argument('--rom', type=Path)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--gui', action='store_true')
    parser.add_argument('--attach-delay', type=float, default=0,
                        help='seconds to wait after the listener starts before launching GDB')
    parser.add_argument('--trace', action='store_true', help='log RSP packets')
    args = parser.parse_args()
    if not math.isfinite(args.attach_delay) or args.attach_delay < 0:
        parser.error('--attach-delay must be a finite non-negative number')
    args.out.mkdir(parents=True, exist_ok=True)
    capture = args.out.resolve() / 'capture.profile'
    if capture.exists():
        parser.error('choose a fresh --out directory (capture.profile already exists)')
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        port = probe.getsockname()[1]
    command = [str(args.emulator.resolve()), '--factory', '--model', 'A500',
               '--chip', '512K', '--slow', '512K', '--noaudio',
               '--run', str(args.program.resolve()), '--gdb-dialect', 'bartman',
               '--gdb-gui' if args.gui else '--gdb', f':{port}']
    if args.rom:
        command.append(str(args.rom.resolve()))
    with (args.out / 'emulator.log').open('w') as log:
        emulator = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                    env={**os.environ, 'RUST_LOG': 'info'})
        try:
            # Connecting a probe would itself consume a GDB session. Observe
            # the listener log instead, leaving the first attach to real GDB.
            deadline = time.monotonic() + 20
            while 'listening' not in (args.out / 'emulator.log').read_text():
                if emulator.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError('emulator did not start; see emulator.log')
                time.sleep(0.05)
            time.sleep(args.attach_delay)
            commands = (['set debug remote on'] if args.trace else []) + [
                'set confirm off', 'set pagination off', 'set remotetimeout 60',
                f'file {quoted(args.elf.resolve())}', f'target remote :{port}',
                'set $entry = $pc', 'tbreak main', 'continue',
                'printf "MAIN_PC=%u\\n", $pc == main',
                'monitor profile 2 ' + quoted(args.unwind.resolve() if args.unwind else '')
                + ' ' + quoted(capture),
                'monitor reset', 'maintenance flush register-cache',
                'printf "RESTART_PC=%u\\n", $pc == $entry',
                'tbreak *0xffffffff', 'continue',
                'printf "OUTSIDE_ROM=%u\\n", (unsigned long)$pc < 0xf80000 || (unsigned long)$pc > 0xffffff',
                'delete breakpoints', 'monitor reset',
                # Modify only this temporary emulator's RAM; reset restores
                # the entry snapshot between the two exception probes.
                'set {unsigned short}&main = 0x4afc', 'set $pc = main', 'continue',
                'monitor reset', 'set $pc = (unsigned long)&main + 1', 'continue',
                'monitor reset', 'detach',
            ]
            with (args.out / 'gdb.log').open('w') as output:
                gdb = subprocess.run([str(args.gdb.resolve()), '--batch',
                    *[item for command in commands for item in ('-ex', command)]],
                    stdout=output, stderr=subprocess.STDOUT, timeout=100)
            output = (args.out / 'gdb.log').read_text()
            assert gdb.returncode == 0, output
            assert 'MAIN_PC=1' in output, output
            assert 'OUTSIDE_ROM=1' in output, output
            assert 'SIGILL' in output and 'SIGBUS' in output, output
            assert 'RESTART_PC=1' in output, output
            assert 'PRF: 1/2' in output and 'PRF: 2/2' in output, output
            assert capture.is_file(), output
            assert 'Remote failure' not in output and 'Invalid hex' not in output, output
            print(f'Patched GDB attach, breakpoints, profile, restart and exceptions OK: {args.out}')
        finally:
            if emulator.poll() is None:
                emulator.terminate()
                try:
                    emulator.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    emulator.kill()
                    emulator.wait()


if __name__ == '__main__':
    main()
