# WinUAE state import

Copperline can import a supported WinUAE `.uss` AmigaStateFile and profile
its resumed frames. Supply the exact Kickstart used to create it:

```sh
copperline --factory kickstart.rom --load-uss scene.uss --noaudio \
  --screenshot-after 0.1 scene.png
copperline-ctl profile scene.uss --rom kickstart.rom \
  --frames 2 --out out/scene
```

The importer derives the CPU model, chipset and RAM sizes and restores the
saved register values. CPU clocks use Copperline's model defaults; WinUAE
speed/JIT preferences are not carried over. A ROM CRC mismatch reports the required identity
through Copperline's ROM database. It does not load host paths or ROM images
embedded in the USS; use your own matching ROM. A 256 KiB ROM may be mirrored
in Copperline's 512 KiB ROM window.

One reconstructed video frame is discarded automatically. Scheduled times
then use Copperline's new emulated timeline, which already includes that
frame; they do not retain the source emulator's timestamps. `--load-uss`
cannot combine with `--load-state`, `--run` or `--whdload`. An explicit
configuration can supply matching disk images and host preferences, but the
USS hardware configuration takes precedence.

`copperline-ctl profile` accepts 1-100 frames and writes a native capture
with screenshots, DMA slots and instruction/register samples. Add
`--format bartman --out scene.profile` for the fixed PAL binary used by
Bartman's `ProfileFile` reader. See [profiling](../debugger/profiling.md).

## Coverage

This is an approximate interchange path for resident code that owns the
chipset. It does not establish frame profiling of every title. A skipped
warm-up frame cannot reconstruct missing expansion hardware, media state,
or an unfinished operation.

| USS state | Import behavior |
|---|---|
| ASF framing, plain/zlib chunks | Length and aggregate limits checked; truncated data and duplicate mandatory chunks rejected. |
| CPU | 68000-68060 registers, SR, USP/ISP/MSP, VBR and cache controls; 68EC020 address width retained. Active MMU, FPU chunks and halted CPUs rejected. Cache contents/prefetch are reconstructed. |
| CHIP | Compact and full register layouts; custom latches, Copper lists, bitplane pointers, blitter configuration, interrupt/DMA controls and disk pointer/sync. Active blits and disk DMA rejected. |
| AGAC, SPR0-7, AUD0-3 | 24-bit palette, sprite pointers/position/data/arming, audio pointers/lengths/period/volume and current sample state. Shift pipelines are approximate. |
| CIAA/B | Ports, timer counters/latches, control registers, interrupt state and TOD/alarm/latch; supports older 30-byte and extended chunks. |
| CRAM, BRAM | Chip RAM and slow RAM. |
| FRAM, ZRAM, EXPA | Fixed Zorro II/III RAM mappings from the saved expansion bases. |
| A3K1, A3K2 | Motherboard RAM below and CPU-slot RAM above `$08000000`, within Copperline's supported limits. |
| ROM | One 256/512 KiB Kickstart, verified by size/CRC. Extension/board ROM states rejected. |
| DSK/DSD, input, RTC | Drive position/backing media, keyboard/input and real-time-clock internals are not restored; omitted chunks are reported. |
| CPUX/CPUT, CYCS, CHPX/CHPD/CHSL, BPLX, BLIT/BLTX | Emulator-specific pipelines/event timing are reconstructed rather than replayed. Programmable beam timing is not restored. |
| FSYS/FSYC/FSYP, BORO, PRAM/ZCRM, DMAC, CD32/CDTV, P96 | Unsupported mounted filesystems, board/RTG RAM, storage and CD hardware states are rejected. |

A short resumed capture may succeed even when the next disk access would
fail. Configure matching disks for continued use, and validate the actual
scene before relying on its profile. The importer neither follows saved
host filesystem paths nor reconstructs filesystem handles.

## Compatibility assessment

The initial assessment used the 58 USS fixtures distributed with
[Bartman's extension](https://github.com/BartmanAbyss/vscode-amiga-debug/tree/master/src/test/suite/data/uss).
With matching local ROMs for 53 fixtures, 45 produced two-frame native
captures; eight were rejected for active disk DMA (three), active blits
(two), unsupported RAM ranges (two), or board ROM state (one). Five required
a different local ROM and were not run. These are capture-completion
counts, not 45 verified compatible titles.

Visual checks included the OCS `desertdream-dots` scene, ECS Workbench 2.0,
and AGA `roots a1200 linetunnel`. The Gods fixture resumed to a mostly blank
frame, so it is not validated as compatible. No comparison emulator was run
and this assessment does not claim pixel or cycle equivalence to WinUAE.
The first useful target is self-contained demo frame analysis; general
arbitrary-title coverage also needs drive state, expansion devices and
in-flight hardware operations.

The wire format and field ordering follow
[WinUAE's savestate implementation](https://github.com/BartmanAbyss/WinUAE/blob/master/savestate.cpp)
and its CPU/custom/CIA/audio chunk writers. See
[import internals](../internals/savestate.md#winuae-interchange).
