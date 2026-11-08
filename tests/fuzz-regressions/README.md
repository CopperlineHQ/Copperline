# Retained parser inputs

`cargo test --test fuzz_regressions` replays each target's files through
the same `fuzz/harnesses/<target>.rs` function used by libFuzzer. The
control and debug-info cases run when their `control` and `dap` features
are enabled (both are defaults). The other cases also run in core builds.

The initial inputs are owned, synthetic boundary cases, plus the ELF
compiled from `guest/dap-test/reloc/`; they are not claimed fuzz findings.
`fuzz/tools/generate_parser_seeds.py` regenerates those files and the
initial corpus without deleting later retained crashes. The archive cases
include highly compressible data and forged uncompressed lengths.

When a campaign finds a failure:

1. Download its `fuzz-<target>-<run>-<attempt>` artifact (or the PR's
   `fuzz-smoke` artifact). Preserve the log and original crashing bytes.
2. Reproduce and minimize with `cargo +nightly fuzz run <target> <file>`
   and `cargo +nightly fuzz tmin <target> <file>` from `fuzz/`.
3. Fix the parser and copy the minimized input under the matching target
   directory here. Name it for the malformed structure and describe the
   failure in the fix/test or PR, without third-party media.
4. Run `cargo test --test fuzz_regressions <target> -- --exact` and the
   focused semantic regression test for the defect. Commit both the fix
   and retained input. The stable replay detects panics; explicit tests
   should also pin size errors, rejected framing, or other intended results.

All nine target directories are replayed by the native test jobs. Logs,
fresh campaign corpora and untriaged artifacts are not committed.
