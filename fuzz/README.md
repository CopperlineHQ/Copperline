# Fuzzing Copperline's untrusted-media parsers

Everything a session parses before any guest code runs is attacker-
reachable input: disk images, CD images, hardfiles, archives, and save
states can arrive from downloads and shared collections. The targets here
feed those parsers raw bytes under libFuzzer.

## Running

```sh
cargo install cargo-fuzz
cd fuzz
cargo +nightly fuzz run dms            # one target...
cargo +nightly fuzz run floppy_image   # ADF/extADF/DMS/SCP/IPF/gzip/zip
cargo +nightly fuzz run cd_image       # CUE/BIN and bare ISO
cargo +nightly fuzz run hardfile_rdb   # HDF classification + RDB parse
cargo +nightly fuzz run savestate      # .clstate bincode machine images
```

Crashes land in `fuzz/artifacts/<target>/`; reproduce one by passing the
file back to the same command. Each target treats errors as expected and
panics, hangs, and over-allocation as findings.

The `ci.yml` fuzz job builds every target (`cargo fuzz build`) so the
harnesses cannot rot; it does not run long campaigns. Sustained runs are
a local or scheduled-job concern.
