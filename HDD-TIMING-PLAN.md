# Period hard-drive timing: project plan

Status: IMPLEMENTED on `feat/hdd-timing` (written 2026-10-24 after a
research session, then built phase by phase). This document is meant to be
picked up cold by any
developer or LLM agent: the drive figures in "Drive profiles" come from
the manufacturer manuals and datasheets named next to them, the current
code paths were read from the tree at `3c44ad3fc`, and every design choice
says why it was made, so it can be revisited without redoing the research.

## Goal

An optional, per-drive model of what a real hard drive of the period does
between a command arriving and its data being ready -- command overhead,
seek, rotational latency, media transfer rate, track/cylinder skew, the
drive's buffer and read-ahead, write caching, and (opt-in) spin-up -- so
that a hard-disk Amiga in Copperline loads, boots and benchmarks at the
speed the real machine did with the drive it shipped with:

- A600 / A1200: 2.5" ATA drive on Gayle.
- A4000: 3.5" ATA drive on the motherboard IDE port.
- A3000: 3.5" SCSI drive on the Super DMAC + WD33C93A.
- A2091 / A590 (Zorro II / side-car SCSI): 3.5" SCSI drive, the "hardcard"
  drive mounted on the board.
- A4091 and lide boards: same model, period-appropriate defaults.

Requirements:

1. **Off by default.** Today every controller completes a transfer within
   the access that starts it; that stays the default ("instant") and stays
   byte-identical to current behaviour, so no existing config, golden, test
   or save state moves.
2. **Hardware behaviour, not titles.** The model is a drive mechanism
   (spindle, actuator, buffer) parameterised from datasheets; nothing keys
   on software.
3. **Deterministic.** Every delay is a pure function of emulated time and
   the request stream: headless runs stay reproducible byte-for-byte, a
   resumed save state is identical to an uninterrupted run, and host disk
   speed never leaks into emulated time.
4. **Shared, not per-controller.** One mechanics model drives every
   controller front-end (ATA task file, WD33C93, 53C710); each front-end
   only decides how "not ready yet" looks on its own bus (BSY, a deferred
   REQ, a deferred interrupt).

### Non-goals (initially)

- Changing what the drive *reports*. IDENTIFY strings/CHS, INQUIRY and mode
  pages stay as they are: the default 16-head x 32-sector translation is
  what existing RDBs were partitioned against (see `IdeDrive::from_disk`),
  and the timing model only needs the *physical* geometry internally.
- CD-ROM timing (ATAPI, SCSI CD, CDTV, CD32). Akiko and the CDTV already
  pace their own sector delivery; an ATAPI/SCSI CD seek/speed model could
  reuse the mechanics core later.
- `copperhf.device`, the SF2000 SD board, the host filesystem (`filesys`),
  and PCMCIA CompactFlash. copperhf is explicitly an emulator-only
  zero-cost board; the rest are flash or host-side, with no spindle to
  model.
- SCSI disconnect/reselect during seeks. A target that is not granted
  disconnect privilege holds the bus through the seek, which is exactly
  what a single-drive chain observes anyway; multi-target overlap is
  future work.
- Arbitrating the A2091/SDMAC bus-master cycles against the CPU (the
  existing TODO in `a2091.rs`). The drive model makes DMA happen *over
  time*; it does not make it steal CPU bus cycles.

## How the code works today

All read at `3c44ad3fc`.

- **ATA** (`src/ata.rs`): `AtaBus` is one cable (task file + command
  engine) shared by Gayle (`gayle.rs`, A600/A1200), the A4000 port
  (`ide_a4000.rs`) and the lide boards (`ide_zorro.rs`). The module doc
  says it plainly: "Transfers complete within the access that triggers
  them: a command reads or writes its sectors immediately and BSY is never
  observable." `ST_BSY` exists but is `#[allow(dead_code)]`. READ
  (`0x20/0x21/0xC4`) fills the first DRQ block in the command write and
  raises INTRQ; each drained block refills the next one inside the data
  port read that drained it (`pio_in_block_consumed`); WRITE commits each
  block inside the data-port write that filled it
  (`pio_out_block_filled`). Power commands (IDLE/STANDBY/CHECK POWER MODE,
  `0xE0-0xE5`, `0x94-0x98`) and SEEK (`0x70`) are unimplemented and abort.
  The only time-driven part is `tick_atapi`, which advances ATAPI CD
  drives.
- **Gayle interrupt latch**: `Gayle::latch_ide_irq` moves `AtaBus`'s
  INTRQ edge into the `$DA9000` change register; today it is only reached
  from register accesses, because INTRQ is only ever raised inside one.
- **WD33C93A** (`src/scsi.rs`), shared by the A2091/A590 (`a2091.rs`) and
  the A3000 SDMAC (`sdmac.rs`): the target executes the CDB synchronously
  (`ScsiDisk::execute`), the data phase is offered to the DMAC at once
  (`start_data_phase`), and the DMAC front-ends move every byte within the
  handshake (`pump_dma`). Completion interrupts already go through a
  delay queue (`pending: VecDeque<(csr, delay_cck)>`, `CMD_DELAY_CCK = 64`)
  counted down in `Wd33c93::tick` -- the natural hook for drive time.
- **A4091** (`src/a4091.rs`): the 53C710 SCRIPTS processor runs to its
  next interrupt synchronously from the DSP write (`run_scripts`); `tick`
  only advances CD targets.
- **Shared sector store** (`src/harddrive.rs`): `HardDriveImage` is the
  HDF/HDZ/CHD/VHD/directory/host-disk backend under every IDE and SCSI
  drive. It is pure storage; it knows nothing about commands or time.
- **Device time**: controllers get `tick(cck)` deltas from
  `Bus::tick_timed_devices`; CPU accesses to these devices go through
  `cpu_slow_external_access`, which flushes deferred device clocks first,
  so a device sees time up to the instant of the access.
- **Idle fast-forward** (`Emulator::idle_fast_forward_chunk`): while the
  CPU is in STOP the emulator naps up to the next known event (CIA
  underflow, IRQ pipe, floppy, Copper, blitter, serial, keyboard, frame).
  No storage controller reports a next event today; with 64-cck delays
  that does not matter, with millisecond drive delays it does (see
  "Determinism").
- **Host-side interface cost is already modelled**: Gayle/A4000 register
  accesses are billed as slow external accesses (two colour clocks per
  word), so the CPU-driven PIO loop already costs realistic bus time. The
  drive model adds only the *drive* side.

## The drive model

A request is serviced in these steps; every quantity is converted once
from datasheet microseconds into colour clocks (PAL 3.546895 MHz, NTSC
3.579545 MHz -- take the machine's actual clock from the bus) and kept as
integers.

1. **Command overhead** `t_cmd`: decoding, cache lookup, firmware. Conner
   quotes 1.0 ms, WD 0.3 ms, Seagate's ST1xxN "typically less than 3 ms";
   where a manual is silent, use 1.0 ms.
2. **Buffer check.** If the requested sectors are in, or are about to be
   in, the read-ahead stream (step 7), skip seek and latency: each sector
   is ready at its stream time (possibly already in the past). A profile
   with `partial_hits = false` only takes this path when *every*
   requested sector is already buffered; otherwise the request goes to
   the media like a miss. No manual found documents this behaviour
   either way. It is a hypothesis drawn from the period A4000
   measurements (see "Validation"), to be confirmed in phase 2.
3. **Map LBA to a physical position.** An image no larger than the drive
   lies on its outer cylinders, as a partition at the start of the real
   disk would: the drive's own zones, tracks and cylinders, used from the
   outside in. A larger image is shared between the zones in proportion to
   their native capacity; each zone keeps its sectors per track and head
   count but needs more cylinders than the drive has, and seek distances
   scale onto the real stroke -- so a 2 GB HDF on a "64 MB drive" profile
   still seeks within the datasheet full-stroke time and spins like that
   drive. (As implemented. The first version stretched every image across
   the full stroke; the guest benchmark caught that a small image then puts
   adjacent cylinders several physical cylinders apart, so every cylinder
   crossing costs more than the cylinder skew covers and loses a
   revolution.)
4. **Seek** from the current cylinder: zero for the same cylinder, a
   head-switch time for a different head on the same cylinder, otherwise
   a curve through the three datasheet numbers. For a seek of `n >= 1`
   cylinders on a drive of `C` cylinders, with `d = (n - 1) / (C - 2)` (so
   a one-cylinder seek has `d = 0` and the full stroke, `C - 1`
   cylinders, has `d = 1`),

       seek(n) = t_t2t + (t_max - t_t2t) * d^p

   For distances between uniformly random cylinders, `d` has density
   `2(1 - d)`, so the expected value of `d^p` is `2 / ((p + 1)(p + 2))`;
   choosing

       k = (t_avg - t_t2t) / (t_max - t_t2t)
       p = (sqrt(1 + 8 / k) - 3) / 2

   makes the curve give exactly the track-to-track time for one cylinder,
   the full-stroke time for the full stroke, and the datasheet *average*
   over random seeks (to within `1/C`). `k < 1` always gives `p > 0`.
   Every voice-coil profile below has `k > 1/3` and so a concave
   (`p < 1`, 0.56-0.79) curve -- the square-root-then-linear shape
   Ruemmler & Wilkes measured on period drives ("An Introduction to Disk
   Drive Modeling", IEEE Computer, March 1994). The band-stepper Seagate
   ST1xxN comes out at 0.89-1.04, i.e. close to linear, which is what a
   stepper actuator does. Every manual quoted here defines "average" as
   the mean over random seeks, matching this derivation. The exponent for
   each built-in profile is listed in its table. A profile may carry a
   second triple for writes (the ST9655AG and the Hawk quote separate
   write seeks, 1-4 ms longer for the tighter settle).
5. **Rotational latency**, deterministically: the platter angle is
   `(now - spin_origin) mod t_rev`, so the wait for the target sector is
   the angular distance from where the head lands to where the sector
   starts. Sector `s` of track `n` sits at angle
   `((s + skew(n)) mod spt) / spt`, with
   `skew(n) = n_headswitches * head_skew + n_cylinders * cyl_skew` and the
   skews equal to the head-switch and track-to-track times rounded up to
   whole sectors -- the way the drive's low-level format lays them out.
   Where a manual publishes the skews, they are used as given (ProDrive
   40S/80S: 7 sectors per track, 15 per cylinder; IBM DPRS: 11 and 15).
   That one rule makes sequential transfers across track boundaries cost
   what they cost on the real drive with no special case. Worked example,
   Conner CP2064 (3486 rpm, 38 sectors, 4 heads): one revolution is
   17.21 ms and one sector 0.453 ms; the default 3.0 ms head switch gives a
   7-sector head skew and the 5 ms track-to-track a 12-sector cylinder
   skew, so a cylinder of 152 sectors costs 152 + 3 x 7 + 12 sectors of
   time and sustained sequential reading runs at 929 KB/s against the
   1130 KB/s media rate.
6. **Media transfer**: `t_rev / spt_zone` per sector.
7. **Read-ahead.** After a read the drive keeps reading the following
   sectors into its buffer at media rate until the buffer (minus the
   segment the host is draining) is full or another command arrives. The
   stream is stored as an origin (LBA, time) so every sector's arrival
   time is computable without simulating it. A write invalidates
   overlapping buffered sectors.
8. **Writes.** Without write cache, the command's last sector is committed
   at `t_cmd + seek + latency + n * t_sector` and the drive is busy until
   then. With write cache (a per-profile default; SCSI mode page 8 /
   ATA SET FEATURES could toggle it later), the host transfer completes at
   interface speed and the drive stays busy flushing in the background, so
   the *next* command starts at `max(now, flush_done)`.
9. **Interface.** ATA PIO is host-paced: the drive only decides when each
   DRQ block becomes available. SCSI data phases are additionally capped
   at the profile's bus rate (asynchronous SCSI-1 typically 1.5-3 MB/s on
   these drives): byte `i` cannot move before its sector is in the buffer,
   nor faster than the bus.
10. **Spin-up (opt-in).** From power-on (and after STANDBY, see phase 5)
    the drive is not ready for `t_spinup`: ATA keeps BSY set; SCSI answers
    TEST UNIT READY with NOT READY / "becoming ready" (ASC 04h/01h). Off by
    default even with timing on, because it adds seconds of emulated boot
    time -- and because it is authentic enough to fail: the 1996 A1200 FAQ
    records slow-starting drives missing the Kickstart 3.0 `scsi.device`
    IDE timeout (3.1 waits longer), and the Conner CP2044's 10 s typical /
    20 s maximum start is in that range.

## Drive profiles

Source grades:

- **A**: rpm, physical geometry, seeks and buffer all come from the
  manufacturer's own manual (cited by key; see "References").
- **B**: manufacturer's manual, with named gaps filled by the rules below.
- **C**: no manufacturer's manual found. Built from compilations plus
  estimates:
  - the printed The Ref 4.30, which the maintainer confirms and which
    agrees with the manuals wherever both exist;
  - Disk/Trend tables (an industry compilation, near-primary);
  - stason.org's web TheRef (secondary; differs from the printed edition
    in places).

  Replace the profile when its OEM manual turns up.

Gap rules, applied only where a row says "gap" or "est.":

- No head-switch time: 3.0 ms. Of the manuals found, only the ProDrive
  40S/80S quotes one alongside its skews; the Hawk's 1 ms is a later
  generation. No Conner or Seagate 2.5" manual gives a figure.
- No command overhead: 1.0 ms (Conner's figure).
- No full-stroke seek: 1.8 x average (ProDrive 40S/80S ratio, 35 / 19).
- Zoned, no zone table: zones are spaced evenly across the stroke. Each
  published rate maps to sectors per track at 0.8 format efficiency (the
  512-byte payload against sync, ID, ECC and gaps):
  - With a published rate *range*, the outermost and innermost zones come
    from its two ends.
  - With only a maximum, the outer zone comes from it. Then:
    - if the physical cylinder count is known, the inner zones step down
      evenly to the average the capacity implies (LPS 52S);
    - if not, the innermost zone carries three quarters of the outer's
      sectors (ST3144A).

  Either way, any unknown cylinder count is then chosen so the total
  matches the capacity.

"Media rate" is user data, derived as `spt * 512 * rpm / 60`. It is lower
than the raw bit rates the manuals headline, which include the format
overhead (Conner quotes 1.5 MB/s "to/from media" for 1.13 MB/s of user
data).

### 2.5" ATA (A600, A1200)

| id | Drive | rpm | Physical geometry | Seek ms t2t / avg / full (p) | Media rate | Buffer, cache | Overhead | Spin-up | Grade |
|---|---|---|---|---|---|---|---|---|---|
| `conner-cp2024` | Conner CP2024, 21 MB, 1990 | 3433 | 653 cyl x 2 hd x 32 spt | 8 / 23 / 40 (0.63) | 937 KB/s | 8 KB, look-ahead | gap | 7 s typ, 15 max | A [C24] |
| `conner-cp2044` | Conner CP2044, 43 MB, 1990 | 3486 | 548 x 4 x 38 | 5 / 19 / 40 (0.79) | 1130 KB/s | 32 KB, look-ahead on (SET FEATURES AAh/55h) | 1.0 ms | 10 s typ, 20 max | A [C44] |
| `conner-cp2064` | Conner CP2064, 64 MB, 1990 | 3486 | 823 x 4 x 38 | 5 / 19 / 40 (0.79) | 1130 KB/s | 32 KB, look-ahead on | 1.0 ms | 10 s typ, 20 max | A [C64] |
| `conner-cp2084` | Conner CP2084, 85 MB, 1991 | 3486 | 1096 x 4 x 38 | 5 / 19 / 40 (0.79) | 1130 KB/s | 32 KB, look-ahead on | 1.0 ms | 10 s typ, 20 max | A [C84] |
| `seagate-st9080a`, `seagate-st9145a`, `seagate-st9235a` | Seagate ST9080A, 64 MB, 1992 (ST9145A 128 MB / ST9235A 210 MB: same mechanics, 4 / 6 heads, 64 KB) | 3449 | 2 hd, zoned; no zone table, 16 Mbit/s max | 5 / 16 / 27 typ (0.56) | est. | 32 KB, look-ahead on, no write cache | gap | 4 s; 2 s from standby | B [S235] |
| `seagate-st9655ag` | Seagate ST9655AG, 524 MB, 1994 | 3980 | 8 hd, zoned; no zone table, 28.26 Mbit/s max | read 6 / 16 / 26 (0.56); write 7 / 20 / 28 | est. | 120 KB, look-ahead **and write cache on** by default | gap | 7 s; 3 s from standby | B [S655] |
| `ibm-dpra-21215` | IBM DPRA-21215, 1.2 GB, 1995 | 4900 | 3 disks / 6 hd, 8 zones, 36.3-57.0 Mbit/s; skews 11 / 15 | read 4 / 12 / 23 (0.74); write 4 / 13 / 24 | est. | 64 KB segmented, write cache off | 1.0 ms | 3.8 s typ, 10 max | B [IBM] |

The CP2064 and the ST9080A are drop-in twins: the same 125,096 sectors
and the same 823/4/38 default translation [C64][S235]. The IBM spec is
the SCSI DPRS. The ATA DPRA is reported to be the same mechanism, but its
cache defaults may differ.

### 3.5" ATA (A4000, lide)

| id | Drive | rpm | Physical geometry | Seek ms t2t / avg / full (p) | Media rate | Buffer, cache | Overhead | Spin-up | Grade |
|---|---|---|---|---|---|---|---|---|---|
| `seagate-st3144a` | Seagate ST3144A, 130 MB, 1991 | 3211 | 2 platters, 3 hd, MZR, 1791 TPI; est. 2 zones of ~1134 cyl at 43 / 32 spt (see note) | 3 / 16 / 29 est. (0.56) | 1178 / 877 KB/s est. | 32 KB; `partial_hits = false` (see "Validation") | gap | -- | C, maintainer-confirmed [REF43] |
| `conner-cp30104`, `conner-cp30084` | Conner CP30104, 120 MB, 1991 (CP30084, 84 MB: 1053 cyl) | 3400 | 1524 x 4 x 39 user (40 physical) | 8 / 19 / 35 (0.77) | 1132 KB/s | 64 KB, look-ahead on | 1.0 ms | 15 s typ, 20 max | A [CON] |
| `wd-ac2120` | WD Caviar AC2120, 120 MB, 1992 | 3605 | 4 hd; zone 1: 970 cyl x 48 spt, zone 2: 379 cyl x 40 | 6 / 15 / 28 (0.77) | 1477 / 1231 KB/s | 32 KB "CacheFlow", 4 segments, adaptive read-ahead, writes not cached | 0.3 ms | 5 s typ, 15 max | A [WD] |
| `quantum-fireball-1080at` | Quantum Fireball 1080AT, 1.08 GB, 1995 | 5400 | 4 hd x 3835 cyl, 88-177 spt (zone count unpublished) | 3 / 12 / 22 (0.61) | 4.06-8.16 MB/s | 128 KB | gap | -- | B [QFB][DT95Q] |

ST3144A note: the profile follows the printed The Ref 4.30 (05/1993) row,
which the maintainer has confirmed as accurate for this drive:

- 2 platters, 3 heads, MZR (multi-zone recording), 1791 TPI
- logical 1001/15/17 (130.6 MB formatted)
- 16 ms average seek, 3.0 ms track-to-track
- 12.0 Mbit/s transfer rate, 32 KB cache
- "MZR, 3,211 RPM"

The Ref lists the same 3211 rpm for the sibling ST3051A, ST3096A and
ST3120A. The 1992 Disk/Trend table disagrees on two points:

- It gives 3528 rpm with an 8.5 ms latency, consistent with each other
  but not with The Ref.
- Its "1001 tracks per surface" is the logical cylinder count: with 3
  surfaces it would put 85 sectors on every track, which the 12 Mbit/s
  rate cannot carry.

The physical zone layout is unpublished, so it is estimated:

- The sibling ST3120A is listed as "MZR: 2-ZONE", so 2 zones.
- The outer zone carries 43 sectors per track: 12 Mbit/s at 3211 rpm is
  28,029 raw bytes per revolution, at the 0.8 format efficiency.
- The inner zone is assumed to carry three quarters of that (32 sectors).
- The 255,255-sector capacity then gives about 1134 cylinders per zone,
  2269 in all.

Two figures remain estimates: the zone split and the full-stroke seek.
Seagate's own manual would settle both, but Seagate's legacy archive has
no copy and bitsavers has only board photos of the sibling ST3120A.

### 3.5" SCSI (A3000, A2091/A590, A4091)

| id | Drive | rpm | Physical geometry | Seek ms t2t / avg / full (p) | Media rate | Buffer, cache | Overhead | SCSI (async) | Spin-up | Grade |
|---|---|---|---|---|---|---|---|---|---|---|
| `seagate-st125n`, `seagate-st157n` | Seagate ST125N, 21 MB, 1987 (ST157N, 49 MB: 615 cyl x 6 hd) | 3600 | 407 x 4 x 26 (1 spare per cylinder) | 8 / 38 / 90 (0.89), MLC-0 firmware; MLC-1: 8 / 28 / 70 (1.04). Band stepper | 799 KB/s | none documented: no read-ahead | < 3 ms (use 2) | 1.5 MB/s | 25 s | A [S157] |
| `quantum-prodrive-40s`, `quantum-prodrive-80s` | Quantum ProDrive 40S / 80S, 42 / 84 MB, 1988 | 3662 | 834 cyl x 3 / 6 hd; zones: ~570 outer cyl x 35 spt, ~264 inner x 28 (split derived from the 82,029-sector capacity) | 6 / 19 / 35 (0.67); head switch 3.0; skews 7 / 15 | 1094 / 875 KB/s | 64 KB DisCache: 4 x 16 KB segments, prefetch on, write-through | gap | 2.0 MB/s (4.0 sync) | 13 s typ, 15 max | A [Q88] |
| `quantum-lps-52s`, `quantum-lps-105s` | Quantum ProDrive LPS 52S, 52 MB, 1990 (LPS 105S: 2 platters / 4 hd, otherwise the same) | 3660 | 1219 cyl x 2 hd, MZR 3 zones, 16 Mbit/s; est. ~406 cyl each at 51 / 42 / 33 spt | 5 / 17 / 31 est. (0.64) | 1594 / 1312 / 1031 KB/s est. | 64 KB; policy as ProDrive DisCache | gap | 4.0 MB/s sync quoted; use ProDrive 2.0 async | gap: ProDrive 13 s | C [REF43][DT90Q][DT92Q] |
| `seagate-st31200n` | Seagate Hawk 1LP ST31200N, 1.05 GB, 1995 | 5411 | 2700 cyl x 9 hd, zoned, 85 spt average | read 1.2 / 9.3 / 19.4 (0.68); write 1.7 / 10.5 / 20.4; head switch 1.0, cylinder switch < 3 | ~3.9 MB/s avg (26.8-47.2 Mbit/s) | 256 KB (240 usable), 3 segments, prefetch on, write cache off | ~0.9 ms | 3.1 avg / 5.2 burst (10 sync) | 15 s typ, 20 max | A [HAWK] |
| `quantum-fireball-1080s` | Quantum Fireball 1080S, 1.08 GB, 1995 | as 1080AT | as 1080AT | 3 / 12 / 22 (0.61) | 4.06-8.16 MB/s | 128 KB | gap | 10 MB/s sync | -- | B [QFB][DT95Q] |

The Hawk's own "including controller overhead" seek figures, and its
7.32 ms from CDB to first data on a zero-stroke read, are ready-made
assertions for the front-end tests.

The LPS 52S zones are estimated as follows:

- The Ref 4.30 gives 3 zones and 16 Mbit/s.
- The outer zone gets 51 sectors per track: 16 Mbit/s at 3660 rpm and
  the 0.8 format efficiency.
- The other two step down evenly to the 42-sector average implied by the
  capacity (the LPS 52AT's 751/8/17 translation over 2 heads x 1219
  cylinders).

The Ref's own rows for the ProDrive 40S, ST125N, ST9080A and WD AC2120
agree with those drives' manufacturer manuals (cylinders, heads, zones,
seeks, rpm), which is the reason to trust it where no manual exists. One
inconsistency to note: The Ref gives the LPS 52S 5.0 ms track-to-track in
its ProDrive LPS row but 6.0 ms in its "Impulse 52S/LP" row of the same
drive. The profile takes 5.0.

## Per-machine defaults

`timing = "period"` resolves by controller and machine. Commodore never
documented which drive models it fitted, so the evidence is owner
reports, reviews and price lists; each row says how strong it is.

| Machine / controller | `period` resolves to | Evidence |
|---|---|---|
| A600 (Gayle) | `conner-cp2024` | The A600HD launched with 20 MB [BBoAH-11]. Amiga Shopper (3/1993) confirms the Conner CP2034/2064/2084 in the A600 [Duplain], and one owner reports a 40 MB Conner as the original A600HD drive. Low confidence on the exact model; `conner-cp2044` covers the 40 MB A600HD. |
| A1200 (Gayle) | `conner-cp2064` | "Conner 60 meg" in friends' basic A1200s (1994 review) and an A1200 Conner reset-bug readme. Seagate also fitted them ("the Seagate drives included with many A1200s", 1996 FAQ); `seagate-st9080a` is the twin with identical geometry. Low-medium. |
| A4000 (motherboard IDE) | `seagate-st3144a` | 120 MB IDE was standard on the A4000/040 [4000pr]; an ST3144A was in a November 1992 retail unit, and an ST3096A in an A4000/030. High for early units, but the profile is grade C; `conner-cp30104` is the fully documented 120 MB alternative. |
| A3000 (SDMAC) | `quantum-lps-52s` | Owners describe "the original (C= issued) 52MB Quantum"; a wiki names the LPS 52S/105S. Medium; `quantum-lps-105s` for the 100 MB option. |
| A2091 in an A2000 | `quantum-lps-52s` | A2000HD: "usually ... Quantum (52MB)" [BBoAH-18]. Medium. Early boards and GVP Series I hardcards carried the ProDrive 40S/80S [BBoAH-1195], available as `quantum-prodrive-80s`. |
| A2091 controller on an A500 / A500+ (the A590) | `seagate-st125n` | The A590 shipped a 20 MB Seagate SCSI or a 20 MB WD XT drive [BBoAH-534]. The ST125N is Seagate's 20 MB SCSI drive of the period; the model is inferred from capacity (low confidence). The XT interface is not emulated. |
| A4091 | `seagate-st31200n` | No bundled drive. The A4000T shipped a "1 GB SCSI Seagate" with no model named; the Hawk 1LP is that drive class. Low confidence on model, grade A profile. |
| lide | `wd-ac2120` | A modern board with no factory drive; the best-documented period 3.5" ATA drive. |

## Integration

### Mechanics core (new: `src/harddrive/mechanics.rs`)

Pure arithmetic, no I/O, fully unit-testable:

```rust
pub struct DriveProfile {         // owned: built-ins and [[drive_profile]] both
    pub id: String,               // "conner-cp2064"
    pub label: String,
    pub rpm: u32,
    pub heads: u8,                // physical
    pub cylinders: u32,           // physical
    pub zones: Vec<Zone>,         // (first_cylinder, sectors_per_track); one entry if not zoned
    pub track_to_track_us: u32,
    pub average_seek_us: u32,
    pub full_stroke_us: u32,
    pub write_seek_us: Option<[u32; 3]>, // track-to-track / average / full stroke, if quoted
    pub head_switch_us: u32,
    pub skew_sectors: Option<(u32, u32)>, // (track, cylinder) when the manual publishes them
    pub command_overhead_us: u32,
    pub buffer_kib: u32,
    pub read_ahead: bool,
    pub partial_hits: bool,
    pub write_cache: bool,
    pub spin_up_ms: u32,
    pub scsi_async_kb_per_s: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct DriveMechanics {
    timing: ResolvedTiming,       // the profile in colour clocks, plus the seek exponent
    now: u64,                     // colour clocks since attach (accumulated ticks)
    cylinder: u32,
    head: u8,
    busy_until: u64,              // write-cache flush / previous command
    stream: Option<ReadAhead>,    // origin LBA + origin time + buffer bound
    ready_at: u64,                // spin-up end (0 when spin-up is off)
}

impl DriveMechanics {
    pub fn advance(&mut self, cck: u32);
    /// Start a read; the cursor yields each sector's ready time lazily
    /// (a 64 MiB SCSI READ should not allocate 131072 timestamps).
    pub fn read(&mut self, lba: u64, count: u32, total: u64) -> SectorCursor;
    /// Start a write whose data the host finishes handing over at
    /// `accepted`; returns when the drive reports it committed.
    pub fn write(&mut self, lba: u64, count: u32, total: u64, accepted: u64) -> u64;
    /// Non-transfer commands: overhead only (or a seek, for RECALIBRATE/SEEK).
    pub fn overhead(&mut self) -> u64;
    pub fn seek_to(&mut self, lba: u64, total: u64) -> u64;
}
```

`IdeDrive` and `ScsiDisk` each gain `mechanics: Option<DriveMechanics>`
(`None` = instant, today's behaviour, and the only path existing tests
exercise). Storing the resolved profile in the state, not just its id,
means a save state replays identically even if a later build retunes a
built-in profile.

A diagnostic knob, `COPPERLINE_DIAG_HDTIMING` (through `envcfg`, never a
live `std::env::var`), logs every request's breakdown -- overhead, seek
distance and time, latency, media time, cache hit -- which is what the
validation below needs.

### ATA front-end (`ata.rs`; Gayle, A4000, lide)

- `AtaBus` gains `pending: Option<Pending>` (a due time plus what happens
  then: present the next DRQ block, finish a write block, complete a
  non-data command). While pending, status reads `BSY|DRDY` (DSC clear
  during a seek); DRQ is clear. Per ATA, the other bits are not
  meaningful while BSY is set, so drivers that honour BSY -- all real ones
  -- wait.
- `tick_atapi` becomes `tick`: it advances both drives' mechanics and
  fires `pending` when due. Gayle's tick then calls `latch_ide_irq`, so
  an INTRQ raised from `tick` reaches `$DA9000` exactly as one raised from
  a register access does; the A4000's INT2 is level-fed already; lide
  polls and has no interrupt.
- READ: the command write sets BSY and schedules the first block at the
  ready time of its last sector. Draining a block presents the next one
  at once if its sectors are already buffered (read-ahead keeps up with a
  slow host) or goes BSY until they are. The image read itself still
  happens when the block is presented, so the task-file advance stays
  consistent with what the host has seen.
- WRITE: the first block's DRQ is immediate (the drive's buffer takes it,
  as today); after each block, BSY until the drive commits it (or at once
  with write cache and buffer room), then the next DRQ or the completion
  INTRQ.
- RECALIBRATE seeks to cylinder 0; IDENTIFY, SET MULTIPLE and INITIALIZE
  DEVICE PARAMETERS cost command overhead. Phase 5 adds SEEK and the
  power commands, which today abort.

### WD33C93 front-end (`scsi.rs`; A2091/A590, A3000 SDMAC)

- `ScsiDisk::execute` stays the pure CDB engine; the transaction records
  the mechanics' plan alongside it (data-in sector cursor, or data-out
  commit time).
- The data phase starts when its first sector is ready: until then the
  target simply has not asserted REQ, so the chip stays busy (ASR
  BSY|CIP) and the DMAC sees no request.
- `dma_remaining()` is capped by bytes the target has buffered *now* and
  by the SCSI bus budget accrued in `tick`, so the DMAC front-ends'
  existing `pump_dma` calls from `tick` spread the transfer over time with
  no change of their own. (Verify during implementation that no front-end
  path assumes a data phase drains in one pump -- the A2091 `ST_DMA`
  strobe pumps within the access.)
- `finish_to_status` posts the completion pair no earlier than the
  commit/last-byte time: the existing delay queue just takes a larger
  delay. The manual (Select + Transfer Info) path gets the same treatment.
- `tick_targets` advances disk targets' mechanics as well as CD-ROMs.

### 53C710 front-end (`a4091.rs`)

The SCRIPTS processor has to learn to stall: a block move in DATA IN /
DATA OUT whose bytes are not ready rewinds DSP to that instruction, records
`stalled_until`, and returns without an interrupt; `tick` resumes
`run_scripts` when due. That is what the real chip does -- it waits for the
target's REQ. Biggest change of the three, so it is its own phase.

### Idle fast-forward

Add `Bus::next_storage_event_cck()` -- the minimum over Gayle/A4000
`pending`, the SDMAC, and every Zorro device through a new defaulted
`ZorroDevice::next_event_cck()` (A2091, A4091, lide) -- and cap
`idle_fast_forward_chunk` with it. Without the cap, a guest waiting in STOP
for the disk interrupt sleeps past it by up to a whole nap (8192
instructions), adding milliseconds of fake latency per request. The cap
also tightens today's 64-cck WD33C93 completion delays.

### HDD LED

With timing on, `take_activity` should report the drive busy for the whole
seek/transfer, not just on register traffic, so the LED flickers and
stays lit the way the real one does under a seek-heavy load.

## Configuration surface

```toml
[ide]
timing = "period"            # "instant" (default), "period", or a profile id
spin_up = false              # opt-in power-on spin-up delay
master = "System.hdf"
slave = { path = "Work.hdf", timing = "seagate-st9080a" }  # per-drive override

[scsi]
controller = "a3000"
timing = "period"
unit0 = "System.hdf"
unit1 = { path = "Work.hdf", timing = "instant" }

[lide]
timing = "period"
drive0 = "System.hdf"

[[drive_profile]]            # user-defined mechanisms, referenced by id
id = "my-drive"
rpm = 3600
# ... every DriveProfile field; validation rejects inconsistent values
```

- `period` resolves per controller and machine (table above); a profile id
  pins a mechanism; `instant` is today's behaviour.
- `timing` on a CD-image unit is ignored with a warning. `RawDrive` is the
  shared drive-entry type, so `[copperhf]` and `[sf2000sd]` units would
  parse a `timing` key: validation must reject it there explicitly.
  `[pcmcia]` has its own struct, whose `deny_unknown_fields` already
  rejects it.
- A `[[host_disk]]` attached to an IDE/SCSI/lide slot takes the section's
  `timing` like an image does: the real disk's own speed is invisible to
  emulated time (host I/O is synchronous), so the model applies unchanged.
- CLI: `--hd-timing VALUE` sets the section-level default on every
  controller (explicit per-drive TOML entries still win), and
  `--list-drive-profiles` prints the built-in table (id, label, key specs)
  and exits.
- Launcher: a "Hard drive timing" choice (Instant / Period / every
  profile) and "Drive spin-up", writing the section defaults. As
  implemented they sit on the A/V & Emu -> Emulation page: the Storage
  page's twelve rows are full, and a ninth Storage nav chip wraps onto a
  third row that pushes the last Storage row out of the panel.
- Raw parsing (`src/config/raw.rs`): `RawDrive`'s table form (its
  hand-written visitor and its field list in the `unknown_field` error)
  and the `RawIde`/`RawScsi`/`RawLide` sections gain `timing`; the three
  sections gain `spin_up`. `RawDrive` must still serialise back to the bare
  path string when `timing` is absent, so existing configs round-trip
  unchanged.
- Docs, in the same change: `docs/guide/configuration.md` (`[ide]`,
  `[scsi]`, `[lide]`, a new "Drive timing" subsection with the profile
  table), `docs/internals/peripherals.md` (the mechanics model, replacing
  the "transfers complete within the access" sentences for the timed
  path), `copperline.example.toml`, and `docs/guide/headless.md` for the
  CLI flags.

## Determinism and save states

- All times derive from accumulated device ticks; the platter angle at
  power-on is zero. Nothing reads the host clock, and host I/O stays
  synchronous at the point the data becomes visible, exactly as now.
- New serialized fields (`IdeDrive::mechanics`, `ScsiDisk::mechanics`,
  `AtaBus::pending`, the WD33C93 transaction's plan and bus budget, the
  53C710 stall) are `Option`/`#[serde(default)]`, so existing states load
  (as instant drives) without a chunk version bump.
- A save state carries the drive model it was saved with; loading it under
  a config that asks for different timing keeps the state's (the state is
  the machine). Log a note when they differ.
- Timing does not interact with warp or "fastest" speed modes: those
  change host pacing, not emulated time.

## Validation

1. **Mechanics unit tests**: the seek curve reproduces each built-in
   profile's track-to-track, average (Monte Carlo over random cylinder
   pairs, within 1%) and full-stroke figures; mean rotational latency over
   random requests is half a revolution; sequential throughput equals
   `spt * 512 * rpm / 60` derated by the skew; a read-ahead hit costs only
   overhead; write cache delays the following command; zone mapping covers
   the whole LBA range; identical request streams give identical times.
2. **Front-end unit tests**: BSY observable after an ATA READ until the
   due colour clock, DRQ/INTRQ exactly then, multi-block pacing, write
   commit, Gayle `$DA9000` latching from `tick`; WD33C93 completion
   delayed by the drive, DMA paced by buffer and bus; every existing test
   unchanged with timing off.
3. **Guest benchmark** (`guest/hdbench/`, `tests/hd_timing.rs`): run with
   `--run ... --exit-on-return` under the bundled AROS ROM (A2091, no
   assets) and real Kickstarts (A1200, A4000 IDE), timing sequential reads
   at 512 B to 256 KiB, random 4 KiB reads and a write through the device
   with the E-clock, reporting through a result file. The results are
   gated as an `#[ignore]` integration test with generous windows (the
   CPU-side cost of each driver differs).
4. **Determinism**: two headless runs of the benchmark are byte-identical;
   a save state taken mid-transfer resumes identically.
5. **Real hardware (optional)**: the same `hdbench` binary on a real
   A1200/A3000 with a period drive gives a direct cross-check.

### Period measurements to calibrate against

| Machine, drive, tool | Result | Source |
|---|---|---|
| Stock A1200 (no Fast RAM), Conner 60 MB, SysInfo, 1994 | 600 KB/s | [M-Caviar] |
| Stock A1200, Maxtor 85 MB 2.5", 1993 | "about 500KB/s" | [M-A1200] |
| Stock A1200, Quantum 2.5", 1994 | 900 KB/s | [M-Caviar] |
| A4000/040, ST3144A, DiskSpeed 4.2, 256 KB Fast RAM buffer | read 774,809 B/s; write 459,955; create 264,110 | [M-A4000] |
| A4000, "Seagate 124 MB" (ST3144A), ScsiSpeed 4.2, Fast RAM | 145 / 198 / 804 / 1030 KB/s at 512 B / 4 KB / 32 KB / 256 KB; seek + read 13.5 ms | [M-AC31200F] |
| A4000/030, ST3144A / ST3096A, SysInfo | about 1.0 / 1.06 MB/s | [M-Samsung] |
| A3000/25, stock 52 MB Quantum, DiskSpeed 3.4 | about a third of an ST3655N's 1.7 MB/s | [M-ST3655N] |
| A3000, Quantum Q210S, DiskSpeed 3.1 (1991, Randell Jesup) | 700-900 KB/s | [M-Jesup] |
| A500+ with A590, Commodore 6.6 ROM | "1.4 MB/sec at it's best" (drive unnamed) | [M-A590] |

What these say about the model:

- **Stock A1200**: its Chip RAM PIO ceiling is about 1.2-1.4 MB/s, and
  period 2.5" drives deliver 0.94-1.13 MB/s off the media. The two
  limits overlap, so the drive model matters most here.
- **A4000 ST3144A, ScsiSpeed rows**: the sharpest target. Converted to
  time per request and set against the confirmed 3211 rpm profile (one
  revolution 18.7 ms, one outer-zone sector 0.435 ms):

  | Request | Measured | Model, buffer hit | Model, buffer miss |
  |---|---|---|---|
  | 512 B | 3.5 ms | overhead only: fits | ~20 ms |
  | 4 KB | 20.7 ms | ~6 ms | ~21.7 ms: fits within 5% |
  | 32 KB | 40.8 ms | ~32 ms | ~49 ms |
  | 256 KB | 254.5 ms | 258.7 ms (1013 KB/s): fits within 2% | ~277 ms |

  A miss means the request waits for most of a revolution while its
  first sector comes round again. The single-sector hits next to the
  4 KB misses suggest firmware that serves only *complete* hits from its
  buffer. That is why the profile starts with `partial_hits = false`.

  The two large sizes cost less than a full lost revolution, though. One
  possible cause is that the host's own time per request overlaps the
  rotation wait; another is a look-ahead bounded by a segment or a
  track. So no single flag fits all four rows yet.

  Measured with `hdbench` under Kickstart 3.1 on an emulated A4000/040
  (16 MiB image, so the outer zone), as implemented:

  | Request | Period ScsiSpeed | `partial_hits = false` | `partial_hits = true` |
  |---|---|---|---|
  | 512 B | 145 KB/s | 352 KB/s | 352 KB/s |
  | 4 KB | 198 KB/s | 181 KB/s | 1003 KB/s |
  | 32 KB | 804 KB/s | 645 KB/s | 1001 KB/s |
  | 256 KB | 1030 KB/s | 891 KB/s | 1002 KB/s |

  Complete-hit-only firmware is the only one of the two that reproduces the
  4 KB row (within 9%; partial hits put it five times too fast), and it is
  within 20% on the large ones, so the profile keeps `partial_hits =
  false`. The 512 B row runs faster than the period figure under both:
  it is all buffer hits, so it measures the driver and benchmark per
  request rather than the drive, and ScsiSpeed's own per-request cost is
  not modelled.

  The other profiles measured the same way: the A1200 with
  `conner-cp2064` reads 909 KB/s sequentially (929 KB/s is the profile's
  skewed media rate) and averages 26.5 ms per random 4 KB read over the
  outer 16 MiB; the A2091 (AROS, 68000) with `quantum-prodrive-80s` reads
  877 KB/s at 30.8 ms per random read, and with the bufferless
  `seagate-st125n` 599 KB/s and 29 KB/s on single sectors -- a revolution
  per request, as a drive with no read-ahead must.
- **Period benchmark tools**: DiskSpeed 4.2, ScsiSpeed and SysInfo are
  freely distributed (Aminet), so the same binaries can run headless under
  Copperline with scripted input and a screenshot of the result. Treat
  them as local assets, like ROMs, and never commit them. Running ScsiSpeed
  itself would close the remaining gap at 32 KB and 256 KB.

## Phases

All five are implemented on `feat/hdd-timing`, one commit each, plus the
launcher choice and the guest benchmark in phase 5.

1. **Mechanics core + built-in profiles + unit tests.** No wiring, no
   config. Lands safely on its own.
2. **ATA wiring** (Gayle, A4000, lide) + `[ide]`/`[lide]` `timing` +
   `--hd-timing` + the idle-fast-forward cap + save-state fields + LED +
   docs. This alone covers the A600, A1200 and A4000.
3. **WD33C93 wiring** (A2091/A590, A3000) + `[scsi]` `timing`.
4. **53C710 SCRIPTS stall** (A4091).
5. **Extras**:
   - opt-in spin-up;
   - SEEK and the ATA power commands (IDLE/STANDBY/CHECK POWER MODE), with
     the standby timer spinning the drive down and the next access paying
     spin-up. Period spin-down tools then work instead of getting
     command-aborted;
   - the Seagate 2.5" idle mode [S235][S655]: after 5 s without a command
     the heads park off the data zone while the spindle keeps turning, so
     the next access pays an unpark-and-seek, not a spin-up. The GoDrive
     GLS "autoidle" is similar, but whether its spindle stops is
     unconfirmed;
   - `[[drive_profile]]` (with `base` to change one built-in figure), the
     launcher choice and the `hdbench` integration test.
     `--list-drive-profiles` landed with phase 2, since its docs and error
     messages point at it;
   - SCSI START STOP UNIT spinning a timed drive down and up, and NOT
     READY / becoming ready while it spins up.

This is a new feature with new config surface, so it is based on `main`,
not `v1` (`RELEASE.md`, "Branches").

## Open questions for the maintainer

- Should `period` ever become a machine-profile default (say, the A600HD /
  A1200HD launcher presets)? This plan keeps everything instant unless
  asked.
- Spin-up: opt-in as planned, or on whenever timing is on?
- `period` per controller and machine (as planned) vs one global default?
- The factory models are unconfirmed for the A600 (CP2024 is chosen for
  the 20 MB launch A600HD), the A1200 (Conner vs Seagate) and the A590.
  Does anyone have a teardown photo or a dealer document naming them?
- Manuals still missing: the Seagate ST3144A (for its zone split and full
  stroke) and the Quantum LPS 52S/105S (for zones, full stroke, overhead
  and spin-up).

## References

Manufacturer manuals and specifications:

- [C24] Conner CP2024 product manual, Rev I, 10/1990:
  https://files.retropc.se/unpacked/drivers_1/hd/sekalais/conner.zip/scp2024.asc
- [C44] Conner CP2044PK product manual, Rev I.4, 3/1991:
  https://bitsavers.org/pdf/conner/disk/00501-007_Conner_CP2044PK_Product_Manual_199103.pdf
- [C64] Conner CP2034/CP2064 product manual, 3/1991:
  https://bitsavers.org/pdf/conner/disk/00501-006_Conner_CP2034_CP2064_Product_Manual_199103.pdf
- [C84] Conner CP2084 product manual:
  https://files.retropc.se/unpacked/drivers_1/hd/sekalais/conner.zip/scp2084.asc
- [S235] Seagate ST9080A/ST9145A/ST9235A product manual 36206-001 Rev B,
  4/1993: https://www.seagate.com/support/disc/manuals/ata/9235pmb.pdf
- [S655] Seagate ST9655AG product manual 36270-001, 9/1994:
  https://www.seagate.com/support/disc/manuals/ata/9655pma.pdf
- [IBM] IBM DPRS-20810/21215 specification S39H-4500-02, 11/1995:
  https://web-docs.gsi.de/~kraemer/COLLECTION/IBM/dprs_spw.pdf
- [CON] Conner CP30064/CP30084/CP30104 product manual, 7/1991:
  https://bitsavers.org/pdf/conner/disk/00501-045_Conner_CP30064_CP30084_CP30104_Product_Manual_199107.pdf
- [WD] Western Digital Caviar AC160/AC2120 technical reference, 2/1992:
  https://bitsavers.org/pdf/westernDigital/disk/Western_Digital_Caviar_AC160_AC2120_Technical_Ref_Feb92.pdf
- [S157] Seagate ST125N/ST138N/ST157N product manual 36045-001 Rev C,
  3/1988:
  https://bitsavers.org/pdf/seagate/scsi/36045-001C_ST125N_ST138N_ST157N_Product_Manual_Mar88.pdf
- [Q88] Quantum ProDrive 40S/80S product manual, 6/1988:
  https://bitsavers.org/pdf/quantum/ProDrive/Quantum_ProDrive_40S_80S_Product_Manual_Jun88.pdf
- [HAWK] Seagate Hawk 1LP product family manual 77767452 Rev D, 3/1995:
  https://bitsavers.org/pdf/seagate/scsi/hawk/77767452D_Hawk_1LP_Product_Family_Manual_199503.pdf
- [QFB] Quantum Fireball and ProDrive LPS spec sheets (Quantum, 1995),
  mirrored at https://files.retropc.se/drivers_1/HD2/quantum/
- Quantum ProDrive LPS 210AT preliminary manual, 3/1994 (later LPS
  generation: 4.5 ms head switch, 16 zones):
  https://bitsavers.org/pdf/quantum/ProDrive/81-106898-01_ProDrive_LPS_210AT_Product_Manual_Preliminary_199403.pdf

Compilations:

- [REF43] The Ref, Version 4.30 (05/1993), printed hard disk drive
  directory (maintainer's scan). It is the source for the ST3144A and
  LPS 52S profiles, and its rows agree with the manufacturer manuals
  wherever both exist.
- Disk/Trend tables, per vendor and year, on bitsavers: [DT90Q] [DT92Q]
  [DT95Q] `pdf/quantum/19xx_Quantum_Products.pdf`, [DT92S]
  `pdf/seagate/1992_Seagate_Products.pdf`, [DT92M]
  `pdf/maxtor/1992_Maxtor_Products.pdf`, and the Conner and Seagate
  volumes cited inline above.
- [ST*] stason.org TheRef web edition (secondary; differs from the printed
  4.30 in places, e.g. the ST3144A's internal rate):
  https://stason.org/TULARC/pc/hard-drives-hdd/

Factory-fit evidence:

- [BBoAH-11] [BBoAH-18] [BBoAH-534] [BBoAH-1195] Big Book of Amiga
  Hardware product pages (A600, A2000HD/A2091, A590, GVP Impact Series I):
  https://bigbookofamigahardware.com/bboah/product.aspx?id=N
- [Duplain] Andy Duplain's A1200/A600 2.5" drive compatibility list,
  4/1993: https://groups.google.com/g/comp.sys.amiga.hardware/c/EzEhdheLa7E
- A1200 hard drive FAQ (1996), Seagate and Conner fits and the KS 3.0 IDE
  timeout, on "Amiga Latest & Greatest Volume 230" (discmaster.textfiles.com)
- [4000pr] A4000 announcement: https://ftp.fau.de/mirrors/aminet/docs/anno/4000pr.txt
- A3000 "C= issued 52MB Quantum":
  https://www.math.uh.edu/~barrett/data/all/SyQuestSQ3270S

Measurements:

- [M-Caviar] https://www.math.uh.edu/~barrett/data/all/Caviar
- [M-A1200] https://www.math.uh.edu/~barrett/data/all/Amiga1200_2
- [M-A4000] https://www.math.uh.edu/~barrett/data/all/Amiga4000_3
- [M-AC31200F] https://www.math.uh.edu/~barrett/data/all/CaviarAC31200F
- [M-Samsung] https://www.math.uh.edu/~barrett/data/all/SamsungSHD-3212A
- [M-ST3655N] https://ftp.fau.de/mirrors/aminet/docs/rview/SeagateST3655N.txt
- [M-Jesup] https://www.tuhs.org/Usenet/comp.sys.sgi/1991-May/003689.html
- [M-A590] https://ftp.fau.de/mirrors/aminet/docs/help/a590faq.txt

Modelling:

- C. Ruemmler and J. Wilkes, "An Introduction to Disk Drive Modeling",
  IEEE Computer 27(3), March 1994.
