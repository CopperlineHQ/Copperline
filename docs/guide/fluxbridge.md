# Physical floppy drives (FluxBridge)

Copperline can use a real 3.5" floppy drive connected to the host through a
[Greaseweazle](https://github.com/keirf/greaseweazle) USB controller in place
of a disk image. The pure-Rust
[FluxBridge](https://github.com/CopperlineHQ/FluxBridge) library, compiled
into Copperline, talks to the hardware.

The bridge supplies the MFM data passing under the real drive's head, and the
emulated machine is otherwise unchanged: Paula's disk DMA,
`trackdisk.device`, and custom loaders work as they would on hardware.

## Requirements

- A [Greaseweazle](https://github.com/keirf/greaseweazle) board running firmware 0.27 or newer.
- A 3.5" PC or Shugart floppy drive and an appropriate data/power cable.
- Standard double-density (DD) or high-density (HD) floppy disks.

FluxBridge is compiled into Copperline by default. To build without physical
drive support, leave out the `fluxbridge` feature (see
[Cargo features](getting-started.md#cargo-features)):

```sh
cargo build --release --no-default-features \
  --features "midi,frontend,wasm-boards,control,ctl-bin,import-uae-bin,net-nat,net-bridge,mt32,coppersynth,cpu-jit,profile-stats,game-library,mhi,cd-mp3,cd32-fmv,gdb,dap,netplay-internet,host-serial"
```

## Configuration

In the launcher, open the **Floppy** tab and tick the **Physical drive** box
for the desired bay (`DF0:` through `DF3:`), then press **Configure**.

In `copperline.toml`:

```toml
[floppy.df0]
bridge = "greaseweazle"      # "greaseweazle" (or "gw") or "off"
write_protected = true       # emulator-level write protection (default: true)
# bridge_port = "/dev/ttyACM0"   # serial port path (omit for auto-detection)
# bridge_cable = "a"             # "a"/"b" (PC cable) or "0".."3" (Shugart)
# bridge_density = "auto"        # "auto", "dd", or "hd"
# bridge_mode = "normal"         # "normal", "compatible", or "stalling"
# replay_speed = "fast"          # "fast" (default) or "normal"
```

From the command line:

```sh
copperline --model A500 --floppy-bridge df0 greaseweazle kickstart.rom
```

| Command-line flag | Configuration key | Description |
|---|---|---|
| `--floppy-bridge DFN NAME` | `bridge` | Interface driving the bay (`greaseweazle` or `off`) |
| `--floppy-bridge-port DFN PORT` | `bridge_port` | Serial device path (default: auto-detect) |
| `--floppy-bridge-cable DFN SEL` | `bridge_cable` | Cable drive select (`a`, `b`, `0`..`3`) |
| `--floppy-bridge-mode DFN MODE` | `bridge_mode` | Read mode (`normal`, `compatible`, `stalling`) |
| `--floppy-bridge-density DFN D` | `bridge_density` | Track density (`auto`, `dd`, `hd`) |
| `--floppy-replay-speed DFN SPEED` | `replay_speed` | Replay rate (`fast`, `normal`) |
| `--floppy-bridge-writable DFN` | `write_protected = false` | Allow disk writes |

### Serial port detection

By default, Copperline finds a connected Greaseweazle by itself. If more than
one interface is plugged in, set `bridge_port` explicitly (for example
`/dev/ttyACM0` on Linux or `COM3` on Windows).

### Cable conventions and drive select

`bridge_cable` sets the drive select signal:
- `a` or `b` for standard IBM PC twisted floppy cables (drive A or B).
- `0` through `3` for straight Shugart cables.

Make sure this matches your cabling and the drive's jumpers. Disk changes are
detected directly on PC cables; on Shugart cabling, a swapped disk is noticed
on the next read.

### Density detection

`bridge_density` defaults to `auto`, which senses the density from the disk.
Force `dd` or `hd` for non-standard disks, such as high-density media
formatted as double-density.

### Read modes

- **`normal` (default):** Capture starts as soon as the head settles, without
  waiting for the index pulse. A revolution captured off-index is joined where
  the recording repeats, and FluxBridge checks the join and the AmigaDOS track
  checksums before the track is kept in memory; an unverified capture is used
  for one pass only and then read again. (`fast`, the upstream name for this
  mode, is also accepted.)
- **`compatible`:** Captures from one index pulse to the next. Waiting for the
  index costs a little time, but non-standard and copy-protected tracks are
  reproduced exactly as recorded.
- **`stalling`:** Index-aligned like `compatible`, but the emulated machine
  waits until the track has been read. Use it only for timing-sensitive custom
  loaders that fail with the normal read latency.

`bridge_mode = "turbo"` is refused: that mode answers AmigaDOS calls instead of
reading the disk.

### Replay speed

Once a track is verified and kept in memory, `replay_speed` controls how fast
later reads of that track are served:

- `fast` (default): Kept tracks are replayed at double speed. The first read
  always happens at the platter's own rate.
- `normal`: Kept tracks are replayed at the normal rotational speed.

## Write protection and disk writes

Writing to a physical disk needs both of these:

1. The disk's own write-protect tab must be set to writable.
2. The configuration must set `write_protected = false` (or pass
   `--floppy-bridge-writable`).

Writes go straight to the physical disk as it turns, without the emulated
machine waiting for them. A write of a full revolution can start at any
rotational position, but a partial-track write that does not begin at the
index pulse is refused, because the interface cannot place it accurately.

## Operational differences from disk images

- **Physical disk swapping:** Insert and eject disks in the drive itself.
  The status bar shows the drive's status and write-protection state.
- **Drive sound effects:** Copperline's floppy drive sounds are off for bridged
  drives, since the real drive makes its own noise.
- **Emulation pacing:** A machine with a physical drive always runs at real
  speed, including headless capture runs, and refuses warp, because the real
  platter cannot be hurried.
- **Determinism:** Save states cannot capture the physical medium, and input
  recordings do not replay identically against a real drive.

## Troubleshooting

- **Device permissions (Linux):** Your user account must belong to the `dialout`
  (or `uucp` / `plugdev`) group to open `/dev/ttyACM*`.
- **Greaseweazle firmware:** Copperline refuses firmware older than 0.27; update
  it with the official `gw` utility.
- **Detailed diagnostic logging:** Set `COPPERLINE_DIAG_FLUXBRIDGE=1` in the
  environment to log head stepping, track capture and decoding, and seek
  timings.
