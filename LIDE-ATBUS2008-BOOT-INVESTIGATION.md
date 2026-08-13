# Investigation: AT-Bus 2008 lide personality never loads its driver under real Kickstart

Status: **FIXED (2026-08-13)**, applied to `src/ide_zorro.rs` on branch
`lide-zorro-ide-board`, with a regression test. Found while testing the
three lide personalities on
`--cpu 68020` (working around the separate
[MOVEM discarded-read bug](LIDE-BOOT-NODE-INVESTIGATION.md) in the `m68k`
crate, upstream PR benletchford/m68k-rs#126). RIPPLE and RIDE both boot a
real Kickstart 3.1 + real Workbench 3.1 hard disk to the desktop on 68020.
AT-Bus 2008 does not, and this is a distinct bug, unrelated to MOVEM/68000
timing -- reproduces identically on 68020.

## Symptom

`board = "atbus2008"` (ROM `test-assets/lide/lide-atbus.rom`) under real
Kickstart 3.1 with a real Workbench 3.1 RDB hard disk image: the machine
never even attempts to probe the drive. `COPPERLINE_DIAG_GAYLE=1` (ATA
command trace) shows **zero** ATA commands issued in a 40-second run --
contrast RIPPLE/RIDE, which show IDENTIFY/SET MULTIPLE/READ MULTIPLE
within the first couple of emulated seconds. The screen ends up on a solid
plum/purple background, which turns out to be lide's own boot-ROM failure
indicator frozen in place (see below), not the usual "insert disk"
requester.

This is **not** the movem-discarded-read bug: reproduces identically with
`--cpu 68020`, where that quirk doesn't apply.

## Root cause

lide's boot ROM chainloader (`bootrom/bootldr.S`) locates its relocatable
driver payload at a fixed window offset, `DRIVEROFFSET = $2000`, adjusted
by `+1` for boards whose `er_InitDiagVec` marks an odd-lane ROM (AT-Bus
2008 clones: `cmp.w #1,d1 / beq .odd`). So on AT-Bus 2008 the chainloader's
`_relocate()` starts reading the driver's hunk header at window offset
`$2001`, one byte per CPU access (`BYTEWIDE` build, confirmed: both
`lide.rom` and `lide-atbus.rom` are built from the same `-DBYTEWIDE` rule
in `bootrom/Makefile`; nibble decoding is DiagArea-only).

Per `src/ide_zorro.rs`'s own design comment: *"AT-Bus 2008 has no latch and
no banking: its 32K image sits on the odd lane across the whole window,
always, alongside the even-lane registers."* -- i.e. on real hardware,
`BoardBase+$2001` (odd) is a ROM byte and `BoardBase+$2000` (even) is a
register byte, coexisting at the same window offset. That's also exactly
where `LidePersonality::AtBus2008`'s single ATA channel's **control
block** lives (`channel_ctrl_base` = `0x2000`, from the same constant
`DRIVEROFFSET` shares).

The bug is in `IdeZorro::read()` (`src/ide_zorro.rs:445`). The dispatch
checks `register_block(off)` first, and if it matches -- which it does for
the whole `0x2000..0x3000` range, on **both** lanes, since
`register_block()`/`channel_ctrl_base()` know nothing about odd/even lane
-- it commits to serving the read as a register:

```rust
let value = if let Some((ch, is_ctrl)) = self.register_block(off) {
    if self.ide_enabled {
        self.read_register_block(ch, is_ctrl, off, size)   // <-- always taken
    } else if self.rom_visible(off) {
        self.read_rom(off, size)                            // <-- dead code here
    } ...
```

The `else if !self.ide_enabled` fallback to ROM is meant to cover "ROM
still visible because the latch hasn't fired yet" -- correct for RIPPLE
and RIDE, which do have a latch. But `LidePersonality::has_latch()` is
`false` for AT-Bus 2008 specifically because it has no latch: registers
and ROM are *simultaneously* live, distinguished only by lane. That makes
`self.ide_enabled` permanently `true` for this personality
(`ide_enabled = !personality.has_latch() || flash.is_empty()`), so the
`else if !self.ide_enabled` branch can never run -- the "ROM co-exists on
the odd lane" behavior the module doc comment describes is simply
unreachable in `read()`'s current dispatch order for AT-Bus 2008.

Confirmed on the wire: `COPPERLINE_DIAG_LIDE=1` shows all four bytes of
the chainloader's first `RomFetch32` (the `HUNK_HDR` check) coming back as
float value `0xFF`:

```
lide lide-atbus2008 rd 0x2001/1 -> 0x00FF
lide lide-atbus2008 rd 0x2003/1 -> 0x00FF
lide lide-atbus2008 rd 0x2005/1 -> 0x00FF
lide lide-atbus2008 rd 0x2007/1 -> 0x00FF
```

`read_register_block()` resolves `idx = (off >> 9) & 7 = 0` for all four
addresses -- not index 6, the board's one real control register
(alt-status/device-control) -- so `ctrl_index_reg(0)` is `None` and every
read floats to `0xFF`, per `read_register_block`'s `None => 0xFF` arm.
`0xFFFFFFFF != $3F3` (`HUNK_HDR`), so `bootldr.S`'s chainloader takes
`.RelocateFail` immediately, without ever loading the driver: no
`InitResident`, no `lide.device`, no ATA probing, ever. The purple
background is that failure path's own "checkered purple failure screen"
loop (`move.w #$0f0c,$dff180` / `move.w #$0000,$dff180`, bounded, so it
finishes and just leaves the color on screen in a headless capture) --
confirmed by content, not just color: the RIPPLE/RIDE flash content at the
matching ROM-file byte offset (`0x1000`, i.e. `DRIVEROFFSET` in CPU-address
units halved) is `00 00 03 F3` -- `HUNK_HDR` -- identically present in
*both* `lide.rom` and `lide-atbus.rom`. The ROM image is correct; only the
emulated board's read dispatch fails to reach it on this personality.

## Fix (verified)

Check the ROM lane before committing to the register-block read, when the
personality shares its ROM and register address space by lane parity:

```rust
let value = if let Some((ch, is_ctrl)) = self.register_block(off) {
    if self.personality.rom_lane_odd() && off & 1 == 1 && self.rom_visible(off) {
        self.read_rom(off, size)
    } else if self.ide_enabled {
        self.read_register_block(ch, is_ctrl, off, size)
    } else if self.rom_visible(off) {
        self.read_rom(off, size)
    } else if size == 1 {
        0xFF
    } else {
        0xFFFF
    }
} else if self.personality.bank_readable()
```

`rom_lane_odd()` is already `true` only for `AtBus2008`, so this is a
no-op for RIPPLE/RIDE (their `rom_lane_odd()` is `false`).

Verified end-to-end: applied this patch, rebuilt, and reran
`test-assets/lide/wb31_atbus.toml`-equivalent config (real KS3.1, real
WB3.1 RDB hard disk, `board = "atbus2008"`, `--cpu 68020`) -- boots
cleanly to the Workbench 3.1 desktop, matching RIPPLE and RIDE. **Applied**
to `IdeZorro::read()` in `src/ide_zorro.rs`, with a regression assertion
added to the existing
`atbus_rom_lane_is_odd_and_registers_stay_live_from_reset` unit test
(reads `0x2000`/`0x2001` -- the exact address the chainloader's driver
fetch collides on -- confirming the even lane still floats as a register
while the odd lane now reaches ROM). Full workspace suite
(`cargo test --release --lib`: 2536 passed) and `cargo clippy` are clean;
RIPPLE and RIDE reconfirmed unaffected (`rom_lane_odd()` is `false` for
both, so the new branch is a no-op for them).

Two things worth checking alongside this fix before landing it:
1. `peek_word()` (same file, ~line 501) has the identical shape --
   `register_block` checked first, ROM fallback gated on `!ide_enabled`
   only. It's used for word-wide `movem`-style bulk fetches
   (`AtaBus`-adjacent tooling); confirm whether it needs the same lane
   check for consistency, even if nothing currently exercises AT-Bus 2008
   ROM reads through it.
2. `write()` does not need the equivalent fix: `write_register_block`
   already special-cases the odd lane per-register (`1 => return, // odd
   lane: no register there`), so odd-lane writes in the control-block
   range are already correctly no-ops for AT-Bus 2008 (writes to ROM have
   no effect anyway).

## What's confirmed ruled out

- **Not the MOVEM discarded-read bug**: reproduces on `--cpu 68020`, which
  has none of the 68000 MOVEM timing quirks.
- **Not a ROM image content problem**: `lide-atbus.rom` has byte-identical
  `HUNK_HDR` content to the working `lide.rom` at the offset the
  chainloader computes.
- **Not an odd-address/`InitDiagVec` computation problem**: the CPU-side
  offset math is correct -- the trace shows exactly the expected addresses
  (`$2001`, `$2003`, `$2005`, `$2007`), matching `DRIVEROFFSET + 1` stepped
  by the `BYTEWIDE` chainloader's stride-2 fetch loop.
- **Not AutoConfig/DiagArea recognition**: the board autoconfigs correctly
  (`zorro II board "lide AT-Bus 2008 IDE" autoconfigured at 0x00EA0000` in
  the log) and the DiagArea nibble-decode runs to completion before this
  point, same as RIPPLE/RIDE.

## Files/branches

- Branch: `lide-zorro-ide-board` (same branch as the board implementation
  and the sibling MOVEM investigation).
- Board implementation: `src/ide_zorro.rs`.
- Test configs used: ad hoc (`board = "atbus2008"`, `rom =
  "test-assets/lide/lide-atbus.rom"`, real KS3.1 ROM, real
  `workbench-311.hdf`, `--cpu 68020`) -- not yet committed as a
  `test-assets/lide/*.toml` fixture; `test-assets/lide/atbus.toml` exists
  but points at the blank `work.hdf`, which doesn't carry a real RDB so it
  can't distinguish this failure from an unrelated "no boot node" cause.
