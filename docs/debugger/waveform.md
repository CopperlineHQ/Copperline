# Waveform export (VCD logic analyser)

Copperline can record its internal chipset signals -- beam counters, chip-bus
owner, CPU bus accesses, Copper and blitter state, custom-register writes,
interrupt levels, DMA activity -- into a [VCD](https://en.wikipedia.org/wiki/Value_change_dump)
file that [GTKWave](https://gtkwave.sourceforge.net/) (or any VCD viewer)
displays as a logic-analyser trace. Because the emulator arbitrates the chip
bus per colour clock, the trace shows exactly which device owned every bus
slot and how the Copper, blitter, CPU, and DMA interleave -- the view a logic
analyser probing a real Amiga's bus would give, plus internal state no probe
could reach.

A capture is *trigger-based* and *bounded*: it arms, waits for a trigger (a
CPU PC, a beam position, a register write, an emulated time, or immediately),
records for a fixed window, then finishes the file. This keeps files small
and puts the interesting event at the start of the trace.

## Starting a capture

From the command line (works headless, e.g. together with
`--screenshot-after`):

```sh
copperline --config game.toml --noaudio \
  --waveform out.vcd \
  --wave-trigger pc=0x00C033C2 \
  --wave-duration 20000cck \
  --wave-signals cpu,bus,copper,blitter \
  --screenshot-after 30 /tmp/shot.png
```

From the debugger console (`Cmd/Alt+K`), with the same order-free arguments:

```
WAVE START out.vcd pc=C033C2 20000cck cpu,bus,copper,blitter
WAVE            (status)
WAVE STOP       (finish early)
```

From a live [CCP control session](control.md), using the same spec strings:

```text
waveform.start {"path":"out.vcd","trigger":"pc=0x00C033C2","duration":"20000cck","signals":"cpu,bus,copper,blitter"}
waveform.status
waveform.stop
```

From the debugger window (`Cmd/Alt+B`): the **Wave** tab has Arm and Stop
buttons; type the same order-free spec into the entry box (empty means all
defaults) and click Arm.

Everything is optional: the default trigger is `now`, the default duration
one video frame, the default signal set `all`, and an omitted path becomes
`copperline-wave-<timestamp>.vcd` in the working directory.

## Triggers

| Spec | Fires when |
|---|---|
| `now` | immediately on arming (default) |
| `pc=ADDR` | the CPU retires the instruction at hex ADDR |
| `beam=VPOS` / `beam=VPOS:HPOS` | the beam crosses the (decimal) position |
| `reg=OFF` | a custom register is written (hex word offset, e.g. `reg=180` for COLOR00) |
| `time=SECS` | emulated time reaches SECS (fractional ok) |

## Durations

`20000cck` (colour clocks; a bare number means cck), `2f` / `2frames`,
`50ms`, `1.5s`. The default is one frame. A safety cap bounds any capture at
10 emulated seconds, and an emergency stop finishes the file if it passes
512 MB.

## Signal groups

Select with `--wave-signals` (comma list) or the same list as a console/GUI
token. Default: `all`.

| Group | VCD variables |
|---|---|
| `beam` | `vpos[15:0]`, `hpos[7:0]`, `frame[31:0]` |
| `bus` | `owner[3:0]`, `owner_name` (string), `dmacon[15:0]`, `data[15:0]` (the shared chip data bus latch) |
| `cpu` | `addr[23:0]`, `kind` (fetch/read/write/custom), `rw`, `wait_cck[15:0]` -- one record per granted CPU **chip-bus** slot (fast-RAM traffic never touches the chip bus) |
| `copper` | `pc[23:0]`, `state` (run/wait/skip/jump/stop) |
| `blitter` | `busy`, `slot` (the pipeline cycle label: A/B/C/D, line-mode L1..L4, fill FI, ...), `apt/bpt/cpt/dpt[23:0]` |
| `regs` | `off[8:0]`, `value[15:0]`, `source` (cpu/copper), `strobe` (toggles per write so identical back-to-back writes stay visible) |
| `irq` | `ipl[2:0]` (the level presented to the CPU, after INTEN masking), `intreq[15:0]`, `intena[15:0]` |
| `audio` | `channel[1:0]` + `strobe` per audio DMA grant |

`owner` uses the bus-accounting indices (0 refresh, 1 bitplane, 2 sprite,
3 disk, 4 audio, 5 copper, 6 blitter, 7 cpu, 8 idle); `owner_name` carries
the same information as text, which GTKWave shows directly on the wave.

## Time base

One VCD time unit is one colour clock, with timestamps relative to the
trigger. The file declares `$timescale 1 us` -- the closest legal VCD unit --
so cursor deltas in GTKWave read directly as colour clocks (a PAL line is
227, a PAL frame 71,364). Signals are sampled at the chip-bus arbitration
point, so a value change lands on the exact colour clock it took effect.

## Viewing

```sh
gtkwave out.vcd            # macOS: brew install gtkwave
vcd2fst out.vcd out.fst    # optional: GTKWave's compact FST format
```

In GTKWave, append the signals you care about from the `copperline` scope
tree, set `owner_name`/`state`/`slot` to ASCII display, and use marker
deltas to measure in colour clocks.

## Notes

- A capture observes the machine without disturbing it: no timing changes,
  and the hot path costs a single branch while nothing is armed.
- Arming replaces (and finishes) any previous capture; captures do not
  survive save-state loads.
- The `pc=` trigger matches retired instructions, so it works with every
  CPU model including prefetch/cache effects.
