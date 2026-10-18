# Compact Cargo reporting

Run Cargo through the Python 3.9+ wrapper so full logs never reach the agent's
tool response:

```sh
python tools/cargo_report.py -- build --release --locked
python tools/cargo_report.py -- clippy --all-targets --all-features --locked -- -D warnings
python tools/cargo_report.py -- test --locked
python tools/cargo_report.py -- test --locked cia::
python tools/cargo_report.py -- fmt -- --check
```

The wrapper runs from the caller's working directory, preserves the child exit
status, and captures stdout and stderr directly into separate files. Each run
gets a unique directory under `target/cargo-reports/`, with `stdout.log`,
`stderr.log`, and `result.json` recording the command, duration and status.
Use `--log-dir PATH` or `--cargo PATH` before `--` to override these defaults.
Toolchain selection works: `-- +stable check --locked`.

Success prints one line: exit status, elapsed seconds and unique warning count.
Warnings are deduplicated by compiler code/message/primary locations; plain
build-script warnings are counted by distinct text. This includes diagnostics
Cargo replays from cached artifacts, so it is not a count of newly introduced
warnings. Failure reports are bounded to approximately 6,000 characters:

- Compilation: unique compiler errors, locations, source excerpts and notes.
- Tests: failed test names and captured panic/assertion differences.
- Other failures: early lines containing failure markers, or a bounded beginning
  when there is no marker, followed by full-log paths. No arbitrary log tail.

Cargo's JSON format covers compiler messages, not arbitrary process output.
The wrapper inserts `--message-format=json` before the test/rustc argument
separator and parses standard stable libtest text separately. Custom test
harnesses and build-script failures without structured errors use the bounded
fallback. Raw logs remain intact. This follows the
[Cargo Book's external tools interface](https://doc.rust-lang.org/cargo/reference/external-tools.html#json-messages).

Read the compact report first. Retrieve only the indicated diagnostic or test
block if further evidence is needed; do not print complete logs or ask a model
to summarize them. After successful runs, discover logs under the default
directory if needed; success deliberately does not print file paths.

Verify the wrapper without rebuilding Copperline:

```sh
python -m unittest discover -s tools/tests -p test_cargo_report.py -q
```
