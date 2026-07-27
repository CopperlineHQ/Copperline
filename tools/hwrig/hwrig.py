#!/usr/bin/env python3
"""Host harness for the hardware reference rig (and for testing it under
Copperline first).

The probe server (timing-test/probesrv.asm) speaks the same line protocol
whether it is running on real silicon behind a UART or inside Copperline behind
`[serial] mode = "tcp"`, so this tool drives both through one code path:

    # against the emulator
    copperline --model A500 --chip 512K --slow 512K --noaudio --serial tcp \\
        --insert-disk-after 0 df0 timing-test/probesrv.adf --screenshot-after 3600 /tmp/x.png &
    tools/hwrig/hwrig.py --tcp 127.0.0.1:1234 run timing-test/test.bin

    # against the real machine
    tools/hwrig/hwrig.py --port /dev/tty.usbserial-A1 run timing-test/test.bin

Machine reset and keyboard injection go through the control MCU on a second
serial port (tools/hwrig/hwrig-mcu/), selected with --mcu.
"""
import argparse
import socket
import sys
import time


class Transport:
    """A byte pipe with a line reader. Subclassed per physical link."""

    def write(self, data: bytes) -> None:
        raise NotImplementedError

    def read(self, n: int, timeout: float) -> bytes:
        raise NotImplementedError

    def close(self) -> None:
        pass

    def read_line(self, timeout: float) -> str:
        """Read up to the next LF. Returns the line without CR/LF."""
        buf = bytearray()
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(
                    f"timed out waiting for a line (partial: {bytes(buf)!r})")
            chunk = self.read(1, remaining)
            if not chunk:
                continue
            if chunk == b"\n":
                return bytes(buf).decode("ascii", "replace")
            if chunk != b"\r":
                buf += chunk


class TcpTransport(Transport):
    def __init__(self, hostport: str):
        host, _, port = hostport.partition(":")
        self.sock = socket.create_connection((host or "127.0.0.1", int(port or 1234)))

    def write(self, data: bytes) -> None:
        self.sock.sendall(data)

    def read(self, n: int, timeout: float) -> bytes:
        self.sock.settimeout(timeout)
        try:
            return self.sock.recv(n)
        except socket.timeout:
            return b""

    def close(self) -> None:
        self.sock.close()


class SerialTransport(Transport):
    def __init__(self, port: str, baud: int):
        try:
            import serial  # pyserial
        except ImportError:
            sys.exit("error: the --port path needs pyserial (pip install pyserial)")
        self.ser = serial.Serial(port, baud, timeout=0.1)

    def write(self, data: bytes) -> None:
        self.ser.write(data)
        self.ser.flush()

    def read(self, n: int, timeout: float) -> bytes:
        self.ser.timeout = timeout
        return self.ser.read(n)

    def close(self) -> None:
        self.ser.close()


def crc16(data: bytes) -> int:
    """CRC-16/XMODEM: poly 0x1021, init 0, no reflection, no final xor.
    Mirrors crcbyte in probesrv.asm."""
    crc = 0
    for byte in data:
        crc ^= byte << 8
        for _ in range(8):
            crc = ((crc << 1) ^ 0x1021) & 0xFFFF if crc & 0x8000 else (crc << 1) & 0xFFFF
    return crc


AGNUS_IDS = {
    0x00: "OCS Agnus 8361/8367 (PAL/NTSC, 512K)",
    0x10: "OCS Agnus 8370/8371 (1M)",
    0x20: "ECS Agnus 8372A (1M)",
    0x21: "ECS Agnus 8375 (2M)",
    0x22: "AGA Alice 8374 (rev 2)",
    0x23: "AGA Alice 8374 (rev 3/4)",
}
DENISE_IDS = {
    0xFFFC: "ECS Denise 8373",
    0x00F8: "AGA Lisa 4203",
}


def decode_banner(line: str) -> str:
    """Turn the raw hex banner into named parts. Unknown IDs are reported as
    raw values rather than guessed at -- the server deliberately reports raw."""
    fields = {}
    for token in line.split():
        key, sep, value = token.partition("=")
        if sep:
            try:
                fields[key] = int(value, 16)
            except ValueError:
                pass
    out = []
    agnus = fields.get("agnus")
    if agnus is not None:
        out.append(f"agnus  {agnus:#04x}  {AGNUS_IDS.get(agnus, 'unknown ID')}")
    denise = fields.get("denise")
    if denise is not None:
        known = DENISE_IDS.get(denise, "no DENISEID register (OCS Denise 8362)")
        out.append(f"denise {denise:#06x}  {known}")
    cpu = fields.get("cpu")
    if cpu is not None:
        out.append(f"cpu    {'68010 or later' if cpu else '68000'}")
    chipkb = fields.get("chipkb")
    if chipkb is not None:
        out.append(f"chip   {chipkb} KB")
    lines = fields.get("lines")
    if lines is not None:
        video = "PAL" if lines > 300 else "NTSC"
        out.append(f"video  {video} ({lines} lines/frame)")
    serper = fields.get("serper")
    if serper is not None:
        pal = 3546895 // (serper + 1)
        out.append(f"serial SERPER {serper} -> {pal} baud on PAL")
    return "\n".join("  " + o for o in out)


class ProbeServer:
    def __init__(self, transport: Transport, verbose: bool = False):
        self.t = transport
        self.verbose = verbose

    def command(self, text: str) -> None:
        # Bare LF, never CRLF: the server's line reader stops on the first
        # terminator, and a trailing LF left in the receiver would be read as
        # the first raw byte of a following LOAD payload.
        if self.verbose:
            print(f"-> {text}", file=sys.stderr)
        self.t.write(text.encode("ascii") + b"\n")

    def expect(self, want: str, timeout: float = 5.0) -> str:
        """Read lines until one starts with `want`. Anything else is banner
        noise or a late probe line and is echoed, not silently dropped."""
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                # A chattering server can otherwise defeat the deadline: each
                # line resets the per-read budget and this loop never exits.
                raise TimeoutError(f"no {want!r} from the server within {timeout:g}s")
            line = self.t.read_line(remaining)
            if self.verbose:
                print(f"<- {line}", file=sys.stderr)
            if line.startswith(want):
                return line
            if line.startswith("ERR") or line.startswith("LOADERR"):
                raise RuntimeError(f"server rejected the command: {line}")
            if not self.verbose and line:
                print(f"   (unexpected: {line})", file=sys.stderr)

    def sync(self) -> str:
        """Get a known-good banner, flushing whatever was in flight."""
        self.command("ID")
        return self.expect("BANNER", timeout=10.0)

    def load(self, blob: bytes, addr: int) -> None:
        self.command(f"LOAD {addr:X} {len(blob):X} {crc16(blob):X}")
        self.expect("LOADRDY")
        # Chunked so a slow UART's flow control has somewhere to push back.
        for i in range(0, len(blob), 256):
            self.t.write(blob[i:i + 256])
        self.expect("LOADOK", timeout=30.0)

    def run(self, addr: int, timeout: float):
        """Start the probe and collect its serial output. Returns
        (lines, returned): `returned` is True if the probe RTSed back to the
        command loop, False if it took the machine over and we timed out --
        which every committed probe does, and is normal. A probe that did not
        return leaves the machine needing a reset before it can be run again."""
        self.command(f"RUN {addr:X}")
        self.expect("BEGIN")
        out = []
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return out, False
            try:
                line = self.t.read_line(remaining)
            except TimeoutError:
                return out, False
            if line == "READY":
                return out, True
            if line:
                out.append(line)

    def wait_for_banner(self, timeout: float = 60.0) -> str:
        """Poll until the machine has rebooted and the server answers again."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                self.command("ID")
                return self.expect("BANNER", timeout=5.0)
            except (TimeoutError, RuntimeError):
                continue
        raise TimeoutError("server did not come back after reset")


def report_distribution(runs: list) -> str:
    """Pool repeated runs into a per-row distribution.

    A wire-driven run is not bit-reproducible even on the deterministic
    emulator: the beam, E-clock and refresh phase at the moment RUN is issued
    depend on host scheduling during the upload. Quoting one run is therefore
    never sound -- see the reproducibility section of tools/hwrig/README.md.
    """
    width = max(len(r) for r in runs)
    out = [f"{len(runs)} runs, {width} rows", "",
           "row  mode      n  min       max       spread"]
    unstable = 0
    for i in range(width):
        vals = [r[i] for r in runs if i < len(r)]
        counts = {}
        for v in vals:
            counts[v] = counts.get(v, 0) + 1
        mode = max(counts, key=lambda v: (counts[v], v))
        try:
            nums = [int(v, 16) for v in vals]
            spread = max(nums) - min(nums)
            lo, hi = f"{min(nums):08X}", f"{max(nums):08X}"
        except ValueError:
            spread, lo, hi = 0, min(vals), max(vals)
        flag = "" if len(counts) == 1 else f"  <- {len(counts)} distinct"
        if len(counts) > 1:
            unstable += 1
        out.append(f"{i:3d}  {mode}  {counts[mode]:2d}  {lo}  {hi}  "
                   f"{spread:+d}{flag}")
    out.append("")
    out.append(f"{width - unstable}/{width} rows stable across all runs; "
               f"{unstable} varied.")
    if unstable:
        out.append("Varying rows are phase noise until a stable mode is shown "
                   "across cold boots.")
    return "\n".join(out)


class Mcu:
    """The control MCU (Arduino Uno) on its own USB serial port."""

    def __init__(self, port: str, baud: int = 19200):
        self.t = SerialTransport(port, baud)
        time.sleep(2.0)  # the Uno auto-resets when the port opens
        # Drain the firmware's reset banner ("OK hwrig-mcu ..."), otherwise
        # the first command's reply would be the banner and every later reply
        # would be off by one.
        try:
            while True:
                self.t.read_line(0.5)
        except TimeoutError:
            pass

    def command(self, text: str, timeout: float = 15.0) -> str:
        self.t.write(text.encode("ascii") + b"\n")
        return self.t.read_line(timeout)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tcp", metavar="HOST:PORT",
                    help="talk to Copperline's serial TCP bridge")
    ap.add_argument("--port", metavar="DEV", help="talk to a real machine's UART")
    ap.add_argument("--baud", type=int, default=19200,
                    help="UART rate; must match SERPER_V in probesrv.asm")
    ap.add_argument("--mcu", metavar="DEV", help="control MCU serial port")
    ap.add_argument("-v", "--verbose", action="store_true")
    sub = ap.add_subparsers(dest="cmd", required=True)

    sub.add_parser("id", help="print and decode the machine banner")
    sub.add_parser("ping", help="check the server is alive")
    p_run = sub.add_parser("run", help="upload a probe binary and run it")
    p_run.add_argument("binary")
    p_run.add_argument("--addr", default="30000",
                       help="load address in hex (default 30000)")
    p_run.add_argument("--timeout", type=float, default=30.0)
    p_run.add_argument("--repeat", type=int, default=1, metavar="N",
                       help="run N times and report the per-row distribution; "
                            "needs --mcu to reset between runs unless the probe "
                            "returns")
    p_reset = sub.add_parser("reset", help="reset the machine via the MCU")
    p_reset.add_argument("--cold", action="store_true",
                         help="power-cycle instead of asserting /RESET")
    p_key = sub.add_parser("key", help="send raw Amiga keycodes via the MCU")
    p_key.add_argument("codes", nargs="+", help="hex keycodes, e.g. 45 50")

    args = ap.parse_args()

    if args.cmd in ("reset", "key"):
        if not args.mcu:
            return _fail("this command needs --mcu")
        mcu = Mcu(args.mcu)
        if args.cmd == "reset":
            print(mcu.command("POWER" if args.cold else "RESET"))
        else:
            for code in args.codes:
                print(mcu.command(f"KEY {code}"))
        return 0

    if args.port:
        transport = SerialTransport(args.port, args.baud)
    else:
        transport = TcpTransport(args.tcp or "127.0.0.1:1234")

    srv = ProbeServer(transport, args.verbose)
    try:
        if args.cmd == "id":
            banner = srv.sync()
            print(banner)
            print(decode_banner(banner))
        elif args.cmd == "ping":
            srv.command("PING")
            print(srv.expect("READY"))
        elif args.cmd == "run":
            with open(args.binary, "rb") as f:
                blob = f.read()
            addr = int(args.addr, 16)
            mcu = Mcu(args.mcu) if args.mcu else None
            runs = []
            for i in range(args.repeat):
                if i:
                    if mcu:
                        mcu.command("RESET")
                        srv.wait_for_banner()
                    else:
                        srv.sync()
                srv.sync()
                if args.repeat > 1:
                    print(f"run {i + 1}/{args.repeat}", file=sys.stderr)
                else:
                    print(f"uploading {len(blob)} bytes to {addr:#07x} "
                          f"(crc {crc16(blob):#06x})", file=sys.stderr)
                srv.load(blob, addr)
                lines, returned = srv.run(addr, args.timeout)
                runs.append(lines)
                if not returned and i + 1 < args.repeat and not mcu:
                    return _fail(
                        "the probe took over the machine and did not return, so "
                        "it cannot be run again without a reset: pass --mcu")
            if args.repeat > 1:
                print(report_distribution(runs))
            else:
                for line in runs[0]:
                    print(line)
    except (TimeoutError, RuntimeError) as exc:
        return _fail(str(exc))
    finally:
        transport.close()
    return 0


def _fail(msg: str) -> int:
    print(f"error: {msg}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
