#!/usr/bin/env python3
"""Run Cargo with file-backed logs and a bounded report (Python 3.9+)."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time


JSON_COMMANDS = {"build", "check", "clippy", "test", "bench", "doc", "rustc", "rustdoc"}
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
LIMIT = 6000


def cargo_command(arguments, executable="cargo"):
    args = list(arguments)
    if args and args[0] == "--":
        args.pop(0)
    offset = int(bool(args and args[0].startswith("+")))
    if len(args) <= offset or args[offset] not in JSON_COMMANDS | {"fmt"}:
        raise ValueError("use build/check/clippy/test/bench/doc/rustc/rustdoc/fmt")
    boundary = args.index("--") if "--" in args else len(args)
    cargo_args = args[:boundary]
    if any(a == "--message-format" or a.startswith("--message-format=") for a in cargo_args):
        raise ValueError("the wrapper owns --message-format; remove that option")
    if args[offset] in JSON_COMMANDS:
        args.insert(boundary, "--message-format=json")
    return [executable] + args


def bounded(text, limit):
    return text if len(text) <= limit else text[:limit] + "\n[truncated; see full logs]"


def log_lines(path):
    with path.open(encoding="utf-8", errors="replace") as stream:
        yield from stream


def inspect_logs(stdout, stderr):
    errors, warnings, failures, excerpts = {}, set(), {}, []
    for path in (stdout, stderr):
        current = None
        context_left = 0
        for raw in log_lines(path):
            line = ANSI.sub("", raw).rstrip()
            message = None
            if line.startswith("{"):
                try:
                    value = json.loads(line)
                    if isinstance(value, dict) and value.get("reason") == "compiler-message":
                        message = value.get("message")
                except ValueError:
                    pass
            if isinstance(message, dict):
                level = message.get("level")
                spans = message.get("spans", [])
                primary = [s for s in spans if s.get("is_primary")] or spans[:1]
                locations = [f"{s.get('file_name')}:{s.get('line_start')}:{s.get('column_start')}" for s in primary]
                code = message.get("code") or {}
                identity = (code.get("code"), message.get("message"), tuple(locations))
                if level == "warning":
                    warnings.add(identity)
                elif level == "error":
                    # Rendered diagnostics retain labels, source excerpts, notes and suggestions.
                    detail = message.get("rendered") or json.dumps(message, ensure_ascii=False)
                    errors[identity] = bounded(ANSI.sub("", detail), 1800)
                continue
            if re.match(r"\s*warning(?:\[.*?\])?:", line):
                warnings.add((None, line.strip(), ()))
            match = re.match(r"test (.+?) \.{3} FAILED$", line)
            if match:
                failures.setdefault(match[1], "")
            block = re.match(r"---- (.+?) (?:stdout|stderr) ----$", line)
            if block:
                current = block[1]
                failures.setdefault(current, "")
                continue
            if line in ("failures:", "successes:") or line.startswith("test result:"):
                current = None
            if current and len(failures[current]) < 1800:
                failures[current] += bounded(line, 1800) + "\n"
            # Keep original failure evidence even when progress output follows it.
            marker = re.search(r"error|failed|panic|fatal|caused by", line, re.I)
            if len(excerpts) < 12 and (marker or context_left):
                excerpts.append(bounded(line, 500))
                context_left = 2 if marker else context_left - 1
    return errors, len(warnings), failures, excerpts


def execute(command, log_root):
    log_root = Path(log_root).resolve()
    log_root.mkdir(parents=True, exist_ok=True)
    folder = Path(tempfile.mkdtemp(prefix=time.strftime("%Y%m%d-%H%M%S-"), dir=log_root))
    stdout, stderr = folder / "stdout.log", folder / "stderr.log"
    started = time.monotonic()
    launch_error = None
    env = dict(os.environ, CARGO_TERM_COLOR="never")
    with stdout.open("wb") as out, stderr.open("wb") as err:
        try:
            process = subprocess.Popen(command, stdout=out, stderr=err, env=env)
            try:
                status = process.wait()
            except KeyboardInterrupt:
                process.terminate()
                status = process.wait()
                launch_error = "wrapper interrupted; child terminated"
        except OSError as exc:
            status = 127
            launch_error = str(exc)
            err.write(launch_error.encode("utf-8", errors="replace"))
    duration = time.monotonic() - started
    errors, warning_count, failures, excerpts = inspect_logs(stdout, stderr)
    kind = ("success" if status == 0 else "compilation failure" if errors else
            "test failure" if failures else "unexpected failure")
    report = {"result": kind, "exit_status": status, "duration_seconds": round(duration, 3),
              "warning_count": warning_count, "command": command, "cwd": str(Path.cwd()),
              "stdout": str(stdout), "stderr": str(stderr), "launch_error": launch_error}
    (folder / "result.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    lines = [f"{kind}: exit={status} duration={duration:.2f}s warnings={warning_count}"]
    if status:
        if errors:
            lines.extend(list(errors.values())[:8])
            if len(errors) > 8:
                lines.append(f"{len(errors) - 8} additional unique errors in full logs")
        elif failures:
            for name, detail in list(failures.items())[:12]:
                lines.append(f"FAILED {name}\n{detail.strip()}")
            if len(failures) > 12:
                lines.append(f"{len(failures) - 12} additional failed tests in full logs")
        else:
            lines.extend(excerpts)
            if not excerpts:
                # No recognizable marker: show the beginning, never an arbitrary tail.
                for path in (stderr, stdout):
                    with path.open(encoding="utf-8", errors="replace") as stream:
                        prefix = stream.read(1000).strip()
                    if prefix:
                        lines.append(prefix)
        text = bounded("\n".join(lines), LIMIT - 800)
        text += f"\nFull logs: {folder}\nstdout: {stdout}\nstderr: {stderr}"
    else:
        text = lines[0]
    return status, text, folder


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log-dir", type=Path, default=Path(__file__).resolve().parents[1] / "target/cargo-reports")
    parser.add_argument("--cargo", default="cargo", help="Cargo executable path")
    parser.add_argument("arguments", nargs=argparse.REMAINDER, help="-- build --release, or -- test --locked")
    args = parser.parse_args(argv)
    try:
        command = cargo_command(args.arguments, args.cargo)
    except ValueError as exc:
        parser.error(str(exc))
    status, text, _ = execute(command, args.log_dir)
    print(text)
    return status


if __name__ == "__main__":
    raise SystemExit(main())
