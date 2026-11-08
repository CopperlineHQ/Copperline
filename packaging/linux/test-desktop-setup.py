#!/usr/bin/env python3
"""Exercise AppImage desktop installation, Exec expansion and removal on Linux."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def main():
    appimage = Path(sys.argv[1]).resolve(strict=True)
    helper = Path(__file__).with_name("copperline-desktop-setup")
    app_id = "dev.copperline.Copperline"
    with tempfile.TemporaryDirectory(prefix="copperline-desktop-test-") as temporary:
        root = Path(temporary)
        data = root / "data"
        config = root / "config"
        config.mkdir()
        sentinel = config / "mimeapps.list"
        sentinel.write_text("[Default Applications]\napplication/zip=existing-archive-handler.desktop;\n")
        expected_defaults = sentinel.read_bytes()
        log = root / "opened.json"
        # The real AppImage supplies its packaged integration files. The small
        # wrapper records gio's actual Exec expansion without opening a window.
        wrapped = root / 'Copperline café $cash `tick` "quote" 100% \\disk.AppImage'
        wrapped.write_text(
            "#!/usr/bin/env python3\nimport json, os, pathlib, sys\n"
            f"real = {str(appimage)!r}\n"
            "if sys.argv[1:2] == ['--appimage-extract']:\n"
            "    os.execv(real, [real, *sys.argv[1:]])\n"
            f"pathlib.Path({str(log)!r}).write_text(json.dumps(sys.argv[1:]))\n"
        )
        wrapped.chmod(0o755)
        env = dict(os.environ, XDG_DATA_HOME=str(data), XDG_CONFIG_HOME=str(config))

        def run(*args):
            completed = subprocess.run(args, env=env, text=True, capture_output=True)
            if completed.returncode:
                raise RuntimeError(f"{args}: {completed.stdout}\n{completed.stderr}")
            return completed

        for _ in range(2):  # Reinstall must be idempotent and retain defaults.
            run(sys.executable, str(helper), "install", str(wrapped))
            desktop = data / f"applications/{app_id}.desktop"
            run("desktop-file-validate", str(desktop))
            files = [str(root / "Disk 1.adf"), str(root / "Disk café.adf")]
            log.unlink(missing_ok=True)
            run("gio", "launch", str(desktop), *files)
            deadline = time.monotonic() + 10
            while not log.exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            assert json.loads(log.read_text()) == ["--", *files]
            for suffix, expected in [
                ("adz", "application/x-copperline-floppy-image"),
                ("adf.gz", "application/x-copperline-floppy-image"),
                ("slave", "application/x-amiga-whdload-slave"),
                ("rom", "application/x-amiga-kickstart-rom"),
            ]:
                media = root / f"test.{suffix}"
                media.write_bytes(b"\x00\x01fixture")
                detected = run("gio", "info", "-a", "standard::content-type", str(media)).stdout
                assert f"standard::content-type: {expected}" in detected, (suffix, detected)
            assert sentinel.read_bytes() == expected_defaults
        run(sys.executable, str(helper), "uninstall")
        run(sys.executable, str(helper), "uninstall")
        assert not desktop.exists()
        assert not (data / f"mime/packages/{app_id}.xml").exists()
        assert not (data / f"icons/hicolor/256x256/apps/{app_id}.png").exists()
        assert sentinel.read_bytes() == expected_defaults
    print("AppImage desktop install, file arguments, MIME detection and uninstall passed")


if __name__ == "__main__":
    main()
