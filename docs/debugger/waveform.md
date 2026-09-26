# Waveform export (VCD logic analyzer)

Copperline can record internal chipset signals -- beam counters, chip-bus owner,
CPU bus accesses, Copper and blitter state, custom register writes, interrupt
levels, and audio DMA -- into a standard [VCD](https://en.wikipedia.org/wiki/Value_change_dump)
file. The trace can be viewed in [GTKWave](https://gtkwave.sourceforge.net/)
or any other VCD viewer.

Copperline arbitrates the chip bus per colour clock, so the traces show the
exact cycle-by-cycle interleaving of the CPU, Copper, blitter, and DMA channels.

Captures are bounded and triggered: the recorder arms, waits for its trigger,
records for the requested duration, and closes the file. Only one capture runs
at a time: arming a new one finishes the previous file. Run-ahead is suspended
while a capture is armed.

## Capturing waveforms

### Command line

```sh
copperline --config game.toml --noaudio \
  --waveform out.vcd \
  --wave-trigger pc=0x00C033C2 \
  --wave-duration 20000cck \
  --wave-signals cpu,bus,copper,blitter \
  --screenshot-after 30 /tmp/shot.png
```

### Debugger console (`Cmd+K` / `Alt+K`)

```text
WAVE START out.vcd pc=C033C2 20000cck cpu,bus,copper,blitter
WAVE                # Display capture status
WAVE STOP           # Finish capture immediately
```

The `WAVE START` arguments can come in any order: a token containing `=` (or
`now`) is the trigger, one starting with a digit is the duration, a list of
signal group names is the signal selection, and any other token is the output
path.

### Control protocol (CCP)

```text
waveform.start {"path":"out.vcd","trigger":"pc=0x00C033C2","duration":"20000cck","signals":"cpu,bus,copper,blitter"}
waveform.status
waveform.stop
```

### Debugger window (`Cmd+B` / `Alt+B`)

On the **Wave** tab, type the same order-free arguments as `WAVE START` into the
address/command field (or leave it empty for the defaults), then click **Arm**.
**Stop** ends the capture early.

Every interface uses the same defaults:
- Trigger: `now` (immediately upon arming)
- Duration: `1f` (one video frame)
- Signals: `all`
- Output file (console, debugger window, and control protocol):
  `copperline-wave-<timestamp>.vcd` in the
  [traces folder](../guide/ui.md#where-files-go)

## Trigger specifications

| Trigger spec | Fires when |
|---|---|
| `now` | Immediately when armed (default) |
| `pc=ADDR` | The CPU retires the instruction at hex `ADDR` |
| `beam=VPOS` or `beam=VPOS:HPOS` | The beam crosses the decimal raster position |
| `reg=OFFSET` | A custom register is written (even hex word offset below `200`, e.g. `reg=180` for `COLOR00`) |
| `time=SECS` | Emulated time reaches `SECS` |

## Duration formats

- `20000cck` (or a bare integer): colour clocks
- `2f` or `2frames`: video frames
- `50ms` (whole milliseconds) or `1.5s`: emulated time

A capture is capped at 10 emulated seconds, and the file is closed early if it
grows past 512 MiB.

## Signal groups

Select groups with `--wave-signals`, the console's signal list, or the control
protocol's `signals` field (comma-separated, default `all`):

| Group | Recorded variables |
|---|---|
| `beam` | `vpos[15:0]`, `hpos[7:0]`, `frame[31:0]` |
| `bus` | `owner[3:0]`, `owner_name`, `dmacon[15:0]`, `data[15:0]` |
| `cpu` | `addr[23:0]`, `kind` (fetch/read/write/custom), `rw`, `wait_cck[15:0]` |
| `copper` | `pc[23:0]`, `state` (run/wait/skip/jump/stop) |
| `blitter` | `busy`, `slot` (pipeline phase A/B/C/D, line mode, fill), `apt/bpt/cpt/dpt[23:0]` |
| `regs` | `off[8:0]`, `value[15:0]`, `source` (cpu/copper), `strobe` |
| `irq` | `ipl[2:0]`, `intreq[15:0]`, `intena[15:0]` |
| `audio` | `channel[1:0]`, `strobe` |

## Viewing in GTKWave

```sh
gtkwave out.vcd
# Optional: convert to GTKWave fast binary format:
vcd2fst out.vcd out.fst
```

Timestamps count colour clocks from the trigger. VCD only allows standard time
units, so the file declares `$timescale 1 us`: read every "microsecond" in the
viewer as one colour clock.

In GTKWave:
1. Expand the `copperline` scope in the signal tree; each signal group is a
   scope inside it.
2. Add the signals you want to the display.
3. Set `owner_name`, `state`, and `slot` display format to ASCII.
4. Use markers to measure timing intervals in colour clocks.
