# Hard-drive timing benchmark

`hdbench` measures a raw disk device the way a period benchmark would:
1 MiB of sequential reads at 512 B, 4 KiB, 32 KiB and 256 KiB per request, 32
random 4 KiB reads over the first 16 MiB, and a 256 KiB write in 32 KiB
requests at 8 MiB, each timed with `timer.device`'s E-clock. The host test
names the device in `hdbench-config` (`scsi.device 0`) and reads
`hdbench-result` back from the program's directory.

```sh
make -C guest/hdbench
cargo test --release --test hd_timing -- --ignored --nocapture
```

`hdbench-result` holds ten big-endian 32-bit words: magic `0x4844424E`
(`HDBN`), a completed-step mask (`127` when everything ran), the E-clock
frequency, the span the random reads cover, then the E-clock ticks taken by
the four sequential passes, the random reads and the write. Return code 0
means every step completed; 20 means one failed.
