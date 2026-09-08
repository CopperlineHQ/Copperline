#!/usr/bin/env python3
"""Run two real RetroArch peers with delayed traffic and independent inputs.

Linux dependencies: retroarch, Xvfb, libXtst, Python 3. Uses only the standard
library and the project-owned probe disk. Each peer gets its own display,
content and save directories. No external netplay service is used.
"""
import argparse
import asyncio
import ctypes as c
import ctypes.util
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time


async def check(args, root):
    spec = importlib.util.spec_from_file_location("abi", Path(__file__).with_name("check-libretro.py"))
    abi = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(abi)
    suffix = args.library.suffix
    library = root / ("copperline_libretro" + suffix)
    shutil.copyfile(args.library, library)
    info = Path(__file__).resolve().parents[1] / "crates/copperline-libretro/copperline_libretro.info"
    shutil.copyfile(info, root / info.name)
    libraries = [ctypes.util.find_library(name) for name in ("X11", "Xtst")]
    assert all(libraries), "install libX11 and libXtst before running this test"
    x11, xtst = [c.CDLL(name) for name in libraries]
    x11.XOpenDisplay.argtypes = [c.c_char_p]
    x11.XOpenDisplay.restype = c.c_void_p
    x11.XKeysymToKeycode.argtypes = [c.c_void_p, c.c_ulong]
    x11.XKeysymToKeycode.restype = c.c_uint
    x11.XFlush.argtypes = [c.c_void_p]
    x11.XCloseDisplay.argtypes = [c.c_void_p]
    xtst.XTestFakeKeyEvent.argtypes = [c.c_void_p, c.c_uint, c.c_int, c.c_ulong]
    displays, servers, peers, files = [], [], [], []
    transferred = [0, 0]

    async def proxy(reader, writer):
        remote_reader, remote_writer = await asyncio.open_connection("127.0.0.1", args.port)

        async def relay(source, destination, direction):
            try:
                while data := await source.read(65536):
                    await asyncio.sleep(args.delay_ms / 1000)
                    transferred[direction] += len(data)
                    destination.write(data)
                    await destination.drain()
            except (ConnectionError, BrokenPipeError):
                pass
            finally:
                destination.close()
        await asyncio.gather(relay(reader, remote_writer, 0), relay(remote_reader, writer, 1))

    proxy_server = await asyncio.start_server(proxy, "127.0.0.1", args.port + 1)
    try:
        for index, role in enumerate(("host", "client")):
            directory = root / role
            directory.mkdir()
            if args.content:
                content = directory / args.content.name
                shutil.copyfile(args.content, content)
            else:
                content = directory / "probe.adf"
                content.write_bytes(abi.probe_adf())
            system = directory / "system"
            if args.system:
                shutil.copytree(args.system, system)
            else:
                system.mkdir()
            (directory / "options.cfg").write_text(f'copperline_netplay = "enabled"\ncopperline_model = "{args.model}"\ncopperline_rom = "{args.rom}"\n')
            display = f":{args.display + index}"
            assert not Path(f"/tmp/.X11-unix/X{args.display + index}").exists(), "test display already in use"
            xlog = (directory / "xvfb.log").open("w")
            files.append(xlog)
            servers.append(subprocess.Popen(["Xvfb", display, "-ac", "-screen", "0", "800x600x24"], stdout=xlog, stderr=xlog))
            for _ in range(100):
                pointer = x11.XOpenDisplay(display.encode())
                if pointer:
                    displays.append(pointer)
                    break
                await asyncio.sleep(0.1)
            else:
                raise RuntimeError("Xvfb did not start")
            config = directory / "retroarch.cfg"
            config.write_text(f'''video_driver = "sdl2"
input_driver = "x"
audio_driver = "null"
audio_enable = "false"
video_vsync = "false"
audio_sync = "false"
video_scale = "1"
suspend_screensaver_enable = "false"
pause_nonactive = "false"
config_save_on_exit = "false"
global_core_options = "true"
netplay_public_announce = "false"
netplay_use_mitm_server = "false"
netplay_nat_traversal = "false"
netplay_check_frames = "1"
libretro_directory = "{root}"
libretro_info_path = "{root}"
core_options_path = "{directory / 'options.cfg'}"
system_directory = "{system}"
savefile_directory = "{directory}"
savestate_directory = "{directory}"
''')
            log = (directory / "retroarch.log").open("w")
            files.append(log)
            extra = ["--host", "--port", str(args.port)] if index == 0 else ["--connect", "127.0.0.1", "--port", str(args.port + 1)]
            peers.append(subprocess.Popen([args.retroarch, "-v", "-c", str(config), "-L", str(library), "--nick", role, "--check-frames", "1", "--max-frames", str(args.frames), *extra, str(content)],
                env={**os.environ, "DISPLAY": display, "SDL_VIDEODRIVER": "x11", "SDL_RENDER_DRIVER": "software"}, stdout=log, stderr=log))
            if index == 0:
                # Wait for the host to create its listening socket.
                for _ in range(200):
                    if "You have joined as player 1" in (directory / "retroarch.log").read_text():
                        break
                    if peers[0].poll() is not None:
                        raise RuntimeError("host exited before listening")
                    await asyncio.sleep(0.05)
                else:
                    raise RuntimeError("host did not start listening")
        start = time.monotonic()
        events = 0
        previous = None
        while any(p.poll() is None for p in peers):
            if time.monotonic() - start > args.timeout:
                raise RuntimeError("netplay test timed out")
            # Distinct changing RetroPad inputs: z is B, x is A, arrow keys
            # are directions. These appear in the serialized input-port state.
            phase = int((time.monotonic() - start) * 4)
            if phase != previous:
                previous = phase
                for index, display in enumerate(displays):
                    for key in [ord("z"), ord("x"), 0xff51, 0xff53]:
                        code = x11.XKeysymToKeycode(display, key)
                        pressed = (phase + index) % 4 == [ord("z"), ord("x"), 0xff51, 0xff53].index(key)
                        xtst.XTestFakeKeyEvent(display, code, int(pressed), 0)
                        events += 1
                    x11.XFlush(display)
            await asyncio.sleep(0.02)
        logs = [(root / role / "retroarch.log").read_text() for role in ("host", "client")]
        assert all(p.returncode == 0 for p in peers), "RetroArch exited unsuccessfully"
        assert "client has joined as player 2" in logs[0], "host did not admit client"
        assert "You have joined as player 2" in logs[1], "client did not join"
        for log in logs:
            assert not re.search(r"CRCs mismatch|savestate loading failed|Copperline:|Failed to initialize netplay", log), "netplay error; see logs"
        assert min(transferred) > 1000, "insufficient peer traffic"
        print(json.dumps({"frames": args.frames, "delay_ms_each_way": args.delay_ms, "elapsed_seconds": round(time.monotonic() - start, 1), "input_events": events, "peer_bytes": transferred, "result": "passed"}))
    finally:
        proxy_server.close()
        await proxy_server.wait_closed()
        for display in displays:
            x11.XCloseDisplay(display)
        for p in peers + servers:
            if p.poll() is None:
                p.terminate()
        for p in peers + servers:
            try:
                p.wait(timeout=3)
            except subprocess.TimeoutExpired:
                p.kill()
                p.wait()
        for file in files:
            file.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library", type=Path)
    parser.add_argument("--retroarch", default="retroarch")
    parser.add_argument("--frames", type=int, default=1200)
    parser.add_argument("--delay-ms", type=int, default=20)
    # Per-frame state CRCs and replay are expensive on shared CI runners.
    parser.add_argument("--timeout", type=int, default=600)
    parser.add_argument("--port", type=int, default=55435)
    parser.add_argument("--display", type=int, default=110)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--content", type=Path, help="single-file content (ADF, ISO, CHD or WHDLoad archive)")
    parser.add_argument("--system", type=Path)
    parser.add_argument("--model", choices=["Auto", "A500", "A1200", "CD32"], default="Auto")
    parser.add_argument("--rom", choices=["AROS", "Kickstart"], default="AROS")
    args = parser.parse_args()
    if args.output:
        args.output.mkdir(parents=True, exist_ok=True)
        asyncio.run(check(args, args.output.resolve()))
    else:
        with tempfile.TemporaryDirectory(prefix="copperline-netplay-") as temporary:
            asyncio.run(check(args, Path(temporary)))


if __name__ == "__main__":
    main()
