# A2091 DMA regression probe

`dmatest` talks directly to the bundled ROM's `scsi.device`, unit 0. The host
tests provide a private 4 MiB disk filled with the byte pattern `i % 251`;
the probe writes test data starting at 1 MiB. Run it with that fixture.

```sh
make -C guest/a2091-test
cargo test --release --test a2091_dma -- --ignored --nocapture
COPPERLINE_A2091_TEST_ASSETS="$HOME/Amiga" \
  cargo test --release --test a2091_dma -- --ignored --nocapture
```

The asset-free AROS cases use odd buffers on a 68000. The Kickstart 1.3 and
3.1 cases use a 68020 and accelerator RAM above the DMAC's 24-bit limit,
with 512 KiB, 1 MiB, and 2 MiB of Zorro II Fast RAM, Chip RAM fallback, and a
memory-pressure case leaving only 1.5 KiB free in Chip RAM. The small-box
Kickstarts do not register CPU-slot RAM themselves, so the probe adds the
mapped test bank to Exec before allocating its destination.

The tests verify large reads and writes, guard bytes, unaligned copies,
SCSI-direct reads, short DMA replies with exact residuals, direct DMA into
Chip RAM, a failure after a successful read chunk, subsequent recovery, and
buffer release. The host also checks DMA addresses, alternating read buffers,
and the untouched sectors around the write. Successful fixtures are deleted;
a failing fixture retains its report, log, disk, and screenshot for diagnosis.

`a2091-result` contains eight big-endian 32-bit words: magic `0x41323039`,
a nine-bit pass mask (`511` on success), destination address, Chip/Fast free
bytes before I/O, partial-read actual count, and Chip/Fast free bytes after
I/O. Return code 0 means all checks passed; 20 means a check failed.
