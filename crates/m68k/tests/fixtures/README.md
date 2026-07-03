# Test fixtures

Everything in `extra/` is committed (small test binaries built by
`extra/build_fixtures.sh`, see `extra/Makefile`). Two fixture sets are too
large to vendor and are gitignored; the suites that use them skip (or, for
the compiled-in ones, are excluded from CI) when they are absent:

## `m68000/` -- SingleStepTests (used by `singlestep_m68000_v1_tests`)

Per-instruction 68000 fixtures generated from MAME's microcoded core, in
the upstream binary distribution format (`v1/*.json.bin`, decoded by the
suite exactly like upstream `decode.py`). MIT licensed.

```sh
git clone --depth 1 https://github.com/SingleStepTests/m68000 \
    crates/m68k/tests/fixtures/m68000
cargo test --release --test singlestep_m68000_v1_tests
```

Alternatively set `M68K_SST_FIXTURES` to the clone's `v1` directory; the
CI `m68k-singlestep` job does this with a cached checkout. The `#[ignore]`d
`cycle_gap_report` test in the same suite measures cycle/bus-access
accuracy against the fixtures' MAME transaction logs.

## `Musashi/` -- Musashi reference binaries (used by `cross_cpu_tests`, `musashi_tests`)

Precompiled test programs whose sources live in the Musashi project;
`verify/` holds the container recipe used to rebuild and verify them.
These suites `include_bytes!` the binaries, so they do not compile without
them and stay excluded from CI.
