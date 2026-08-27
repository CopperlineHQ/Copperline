# Waveform export (VCD logic analyzer)

Copperline can record internal chipset signals -- beam counters, chip-bus owner,
CPU bus accesses, Copper and blitter state, custom register writes, interrupt
levels, and DMA activity -- into a standard [VCD](https://en.wikipedia.org/wiki/Value_change_dump)
file. The resulting trace can be viewed in [GTKWave](https://gtkwave.sourceforge.net/)
or any compatible logic analyzer trace viewer.

Because bus arbitration in Copperline is evaluated per colour clock, exported
traces show exact cycle interleaving between CPU, Copper, Blitter, and DMA channels.

Traces are bounded and trigger-based: the recorder arms, waits for a trigger condition,
captures for a specified duration, and writes the output file.

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

### Control protocol (CCP)

```text
waveform.start {"path":"out.vcd","trigger":"pc=0x00C033C2","duration":"20000cck","signals":"cpu,bus,copper,blitter"}
waveform.status
waveform.stop
```

### Debugger window (`Cmd+B` / `Alt+B`)

In the **Wave** tab, enter trigger and duration parameters, then click **Arm**.

Default values:
- Trigger: `now` (immediately upon arming)
- Duration: `1f` (one video frame; accepts `f` or `frames` suffix)
- Signals: `all`
- Default output filename: `copperline-wave-<timestamp>.vcd`

## Trigger specifications

| Trigger spec | Fires when |
|---|---|
| `now` | Immediately when armed (Default) |
| `pc=ADDR` | CPU retires instruction at hex `ADDR` |
| `beam=VPOS` or `beam=VPOS:HPOS` | Beam crosses decimal raster position |
| `reg=OFFSET` | Custom register written (hex word offset, e.g. `reg=180` for `COLOR00`) |
| `time=SECS` | Emulated time reaches `SECS` |

## Duration formats

- `20000cck` (or bare integer): duration in colour-clock cycles
- `2f` or `2frames`: duration in video frames
- `50ms`, `1.5s`: duration in emulated time

A 10-second safety cap limits maximum capture length, and files are automatically
closed if size exceeds 512 MB.

## Signal groups

Select groups with `--wave-signals` (comma-separated list, default `all`):

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

In GTKWave:
1. Expand the `copperline` scope in the signal tree.
2. Add desired signals to the display.
3. Set `owner_name`, `state`, and `slot` display format to ASCII.
4. Use markers to measure timing intervals in colour clocks.
