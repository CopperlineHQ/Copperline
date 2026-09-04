# Per-frame profiling

The `profile.start` method on the [control protocol](control.md) captures
per-frame performance data for external profilers, analysis tools, and scripts.
Only one profile capture can run at a time; call `profile.stop` on an active
session before starting a new one.

```text
profile.start {"path": "out/profile", "frames": 500, "slots": true,
               "memory": true, "screenshots": "last", "pc_samples": true,
               "trigger": {"busy_cck_over": 60000}}

# Precise instruction sampling (registers are optional):
profile.start {"path": "out/profile", "frames": 60, "samples": true,
               "registers": true,
               "unwind": {"base": "0x123400", "table": "<base64>"},
               "relocation_bases": ["0x123400", "0x20a000"],
               "code_ranges": [{"base": "0x123400", "size": "0x6000"}]}
profile.stop
profile.status
```

Each committed emulated frame appends a JSON object to `profile.jsonl` in the
output directory. When the capture completes or `profile.stop` is called, a
`profile.json` summary is written beside it. Streaming to `profile.jsonl`
ensures that recorded data is preserved even if emulation stops unexpectedly.
The `frames` parameter defaults to 500 (approx. 10 seconds in PAL) and is
capped at 100,000. When the frame budget is reached, capture halts
automatically (`profile.status` reports `done`).

An optional `trigger` keeps the profiler armed but does not write records until
an absolute emulated `frame` is reached or a completed frame's busy colour-clock
count exceeds `busy_cck_over`. Busy clocks are the frame length minus uaelib
idle markers when the guest supplies them; otherwise the traced frame length is
used. `profile.status` reports `triggered` and `triggered_at`.

Running a profile capture activates the Frame Analyzer's cheap owner tracing.
`"slots": true` promotes it to the full per-colour-clock record level, which
temporarily suspends run-ahead input latency reduction. Tracing is shared with
the Frame Analyzer UI pane: closing the UI pane does not interrupt an active
profile capture, and stopping a profile capture leaves tracing enabled if the
UI pane remains open.

## `profile.jsonl`

One JSON object per committed frame:

| Field | Meaning |
|---|---|
| `frame`, `seconds` | Emulated timeline position. |
| `idle_cck` | Colour clocks declared idle by guest code via [uaelib trap](../guide/run.md#uaelib-trap) markers (null if unused). |
| `retired` | 68k instructions retired during the frame. |
| `pc` | Program counter sample at the frame boundary (`pc_samples: true` only). |
| `traced` | True if chip-bus tracing covered the frame. |
| `rows`, `line_cck`, `cck_length` | Raster geometry: scanline count, clocks per line, and total clocks per frame. |
| `owner_cck` | Clocks granted per chip-bus owner (`refresh`, `bitplane`, `sprite`, `disk`, `audio`, `copper`, `blitter`, `cpu`, `idle`). |
| `blitter` | `busy_cck` (clocks blitter requested bus) and `starve_cck` (breakdown of owners that stalled it). |
| `blits` | List of blits started during the frame (max 64): control words, size, pointers, and start/end beam positions. |
| `cpu` | The CPU's side of the arbitration: `wait_cck` (colour clocks the CPU asked for the chip bus and was denied), `wait_by` (those clocks by denier: `refresh`, `bitplane`, `sprite`, `disk`, `audio`, `copper`, `blitter` with BLTPRI clear, `blitter_nasty` with BLTPRI set including its warm-up fence, and `port` for the 020+ chip port's own turnaround), `wait_by_kind` (by the pending access: `read`, which includes the 68000's opcode prefetches since the CPU core issues them as plain word reads; `fetch` for immediate and extension words read outside the prefetch queue; `write`; `custom` for custom-register accesses), `stall_pcs` (up to 16 `{"pc", "cck"}` entries, the instructions that waited longest, longest first), `stall_pcs_distinct` and `stall_pcs_other` (clocks pooled once 4096 distinct PCs are kept). Zero entries are omitted from the maps. |
| `partial` | True if tracing was enabled mid-frame. |
| `registers` | Frame-start snapshot: all 256 custom-register words, `chipset_flags`, and the AGA palette's 256 high and low nibbles. Present on every traced frame. |
| `slots` | When `"slots": true`: run-length encoded per-clock owner grid for each scanline (`"12R3B497."`). Codes match vAmiga DMA debugger (`R` refresh, `B` bitplane, `S` sprite, `D` disk, `A` audio, `C` copper, `L` blitter, `P` CPU, `.` idle). |
| `cpu_wait` | When `"slots": true`: the CPU wait grid per scanline in the same run-length encoding. Codes are the denier's owner letter (`R`, `B`, `S`, `D`, `A`, `C`, `L` for the blitter with BLTPRI clear), `N` for the blitter with BLTPRI set, `p` for the port turnaround, and `.` where the CPU was not waiting. |
| `slots_file`, `slots_record_bytes` | When `"slots": true`: raw full-record sidecar filename and its fixed 24-byte stride. |
| `screenshot`, `digest` | When `"screenshots": "every"`: frame screenshot PNG filename and FNV-1a64 hash digest. |
| `samples`, `samples_meta` | With `"samples": true`, the filenames of this frame's compact instruction stream and Copperline timing metadata. |
| `sample_count`, `samples_total`, `irq_cck` | Encoded samples in this frame, cumulative encoded samples, and interrupt-dispatch colour clocks in this frame. |

`stall_pcs` names the instruction that was executing when each wait began.
On the precise CPU loop that is the current instruction; under `[cpu] jit`
the PC is republished once per batch, so the attribution is per batch.

If timeline position moves backward (via state load or reverse step), a
`{"marker": "reposition", "frame": N}` marker is emitted to rebaseline
instruction counters.

## `profile.json`

Written when the profile stops: contains `version`, machine configuration,
capture options, the list of chip-bus owner names (`owners`) and CPU wait
classes (`cpu_wait_classes`), `started`/`ended` timeline points,
`frames_written`, and a snapshot of registered uaelib resources (matching
`debug.resources`) for address labeling. It also records `systemStackLower`,
`systemStackUpper`, `stackLower`, and `stackUpper`, read from ExecBase and
ThisTask when a valid AmigaOS task is running (otherwise null).

With `"memory": true`, `chip-ram.bin` and `slow-ram.bin` are written once,
at capture start. Together with the per-slot write records these let an offline
consumer reconstruct memory at any later colour clock without running the
emulator.

## Full chip-bus slot records

`"slots": true` writes one `slots-NNNNNN.bin` per recorded frame. Records are
in raster order (`VPOS * line_cck + HPOS`) and use this packed, little-endian
24-byte layout:

| Offset | Type | Field |
|---:|---|---|
| 0 | u16 | `reg`: custom-register offset, `0x1000` for CPU memory, or `0xffff` when not applicable |
| 2 | u8 | `kind`: refresh 1, CPU 2, Copper 3, audio 4, blitter 5, bitplane 6, sprite 7, disk 8, conflict 9 |
| 3 | u8 | `subtype`: CPU code/data; Copper move/wait/skip; audio 0-3; bitplane 1-8; sprite 0-7; blitter A-D plus fill bit `0x10` and line bit `0x20` |
| 4 | u8 | `size`: transferred bytes (2, 4, or 8; zero when no data transfer is attached) |
| 5 | u8 | `ipl`: CPU-visible interrupt level at this slot |
| 6 | u16 | `flags`: bit 0 write; bit 1 CIA access valid; bit 2 CIA-B; bits 8-11 CIA register; bits 12-14 E-clock phase |
| 8 | u32 | `addr`: chip/CPU address |
| 12 | u64 | `data`: transfer data, including 32/64-bit AGA FMODE fetch groups |
| 20 | u32 | `events`: hardware-edge bits listed below |

The same fields are available live from `frame.slots {"row": V}` and in the
Frame Analyzer's selected/hovered-slot readout.

| Bit | Event |
|---:|---|
| 0 | blitter interrupt request |
| 1 | blitter final D write |
| 2 | blitter start or finish (the streamed event name distinguishes the edge) |
| 3 | bitplane fetch/update |
| 4 | Copper WAIT wake |
| 5 | CPU interrupt recognition |
| 6 | INTREQ set |
| 7 | Copper wanted a fixed-DMA-owned slot |
| 8 | no requester received the slot |
| 9 | blitter denied by CPU |
| 10 | CPU denied by BLTPRI blitter |
| 11 | Copper SKIP taken |
| 12-14 | DDFSTRT, DDFSTOP, hard DDF stop |
| 15 | CIA register access |
| 16-26 | VB, VS, LOF, LOL, HBS, HBE, HDIWS, HDIWE, VDIW, HSS, HSE |
| 27-28 | CIA-A and CIA-B IRQ pin assertions |
| 29-30 | CPU STOP entered and STOP IPL wake |

VB, VS, and VDIW are signal-state markers on the first refresh slot of every
line; LOF and LOL similarly mark the second refresh slot. HBS/HBE, HSS/HSE,
HDIWS/HDIWE, and DDF events mark their programmable comparator edges.

`registers.chipset_flags` uses bit 0 for AGA, bit 1 for ECS Agnus or newer,
bit 2 for NTSC, bit 3 for interlace, and bit 4 for the LOF field.

When precise sampling is enabled, the summary also records
`cck_per_cpu_cycle`, `samples_total`, `irq_cck`, every loaded hunk base and
executable range, the unwind text base and size, and the sidecar layouts.
Samples use colour clocks (CCK), Copperline's native
chipset time unit; `cck_per_cpu_cycle` converts the configured CPU clock to
that unit.

## Precise CPU samples and unwinding

`"samples": true` moves a JIT-configured CPU temporarily onto the precise
per-instruction path. It does not change the emulated timeline. Every retired
instruction records its PC and colour-clock cost. `"registers": true` appends
D0-D7, A0-A7 and SR. Interrupt entry is a distinct `[IRQ]` sample; its metadata
contains the interrupt level and exception vector rather than inferring them
from the cost.

Each `samples-SSSSSS-frame-NNNNNN.bin` is a little-endian u32 stream compatible with
vscode-amiga-debug/WinUAE: leaf-to-root call-stack PCs, `0xffffffff - cck`,
then the 17 optional register words. PCs in the supplied text range are
relative to its base; Kickstart PCs in `$F80000..$FFFFFF` remain absolute.
Samples longer than 65535 CCK are split so their cost word cannot be mistaken
for a PC by existing parsers.

The optional live unwind table has one six-byte row for every two bytes of
text: `(cfa_register << 12) | cfa_offset`, saved-A5 offset, return-address
offset, all little-endian i16 words. CFA register 13 is A5 and 15 is A7. The
emulator keeps expanded offsets as i32, follows the return address at sample
time, and stops when it leaves the supplied text. `copperline-ctl --dap`
builds this table directly from the already-loaded DWARF call-frame
information; no objdump process is involved.

`samples-SSSSSS-frame-NNNNNN.meta` starts with `CLSM`, u32 version 1, and a u32 row count.
Each row is five little-endian u32 values: total CCK, instruction CCK,
chip-bus-wait CCK, IRQ level, and IRQ vector. The latter two are `0xffffffff`
for ordinary instructions. This parallel file lets Copperline reports expose
`[Bus wait]` below the responsible function while leaving the main stream
compatible with Bartman's reader.

`SSSSSS` is a monotonic capture sequence, so revisiting the same emulated
frame through reverse execution or state loading never overwrites an earlier
sidecar. `relocation_bases` is ordered by hunk and lets the offline converter
map absolute samples from every code hunk. `code_ranges` lets the live compact
unwinder retain a leaf or caller from a code hunk outside its hunk-0 table while
still stopping at external code. Older captures without relocation data fall
back to the unwind table's hunk-0 base.

## Converting to a CPU profile

Convert an offline capture with the same hunk executable and, when applicable,
its ELF debug sibling:

```sh
copperline-ctl profile-report out/profile --program hello \
  --elf hello.elf --out hello.cpuprofile
```

The default is one merged Chrome DevTools `.cpuprofile`. Add `--per-frame` for
one numbered file per captured frame, `--format bartman` for Bartman's `$amiga`
annotations, or repeat `--source-map FROM=TO` to rewrite recorded build paths.
Functions, source lines, and optimized inline frames come from Copperline's
native debug-info reader. VS Code opens `.cpuprofile` files directly; its CPU
profile flame-chart extension adds the graphical flame view.

## Storage overhead

Enabling `slots` keeps one 24-byte record per colour clock live (about 1.7 MiB
for a 313x227 PAL frame) and writes about the same amount per frame, plus the
roughly 2-20 KB run-length encoded grids. Setting `"memory": true` adds one
copy each of chip and slow RAM. Setting `"screenshots": "every"` produces 50 PNG images per emulated
second in PAL. Precise sampling is larger: without registers each sample is
the call stack plus one word; registers add 68 bytes per sample. All three
options are disabled by default. Captures up to 100,000 frames are accepted.
