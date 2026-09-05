# Physical floppy drives (FluxBridge)

Copperline can interface directly with real 3.5" floppy drives connected to the
host via a [Greaseweazle](https://github.com/keirf/greaseweazle) USB controller.
Hardware communication is handled by the [FluxBridge](https://github.com/CopperlineHQ/FluxBridge)
library.

When using a physical drive, the bridge streams MFM data to Paula and disk DMA,
allowing `trackdisk.device` and custom loaders to operate as they would on hardware.

## Requirements

- A [Greaseweazle](https://github.com/keirf/greaseweazle) board running firmware 0.27 or newer.
- A 3.5" PC or Shugart floppy drive and an appropriate data/power cable.
- Standard double-density (DD) or high-density (HD) floppy disks.

FluxBridge is compiled into Copperline by default. To build without physical
drive support:

```sh
cargo build --release --no-default-features \
  --features "midi,frontend,wasm-boards,control,ctl-bin,import-uae-bin,net-nat,net-bridge,mt32,coppersynth,cpu-jit,profile-stats,game-library,mhi,cd-mp3,cd32-fmv,gdb,dap"
```

## Configuration

In the launcher, navigate to the **Floppy** tab and enable the **Physical drive**
checkbox for the desired bay (`DF0:` through `DF3:`), then select **Configure**.

In `copperline.toml`:

```toml
[floppy.df0]
bridge = "greaseweazle"      # "greaseweazle" or "off"
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
| `--floppy-bridge DFN NAME` | `bridge` | Enable bridge device (`greaseweazle` or `off`) |
| `--floppy-bridge-port DFN PORT` | `bridge_port` | Serial device path (default: auto-detect) |
| `--floppy-bridge-cable DFN SEL` | `bridge_cable` | Cable drive select (`a`, `b`, `0`..`3`) |
| `--floppy-bridge-mode DFN MODE` | `bridge_mode` | Read mode (`normal`, `compatible`, `stalling`) |
| `--floppy-bridge-density DFN D` | `bridge_density` | Track density (`auto`, `dd`, `hd`) |
| `--floppy-replay-speed DFN SPEED` | `replay_speed` | Replay rate (`fast`, `normal`) |
| `--floppy-bridge-writable DFN` | `write_protected = false` | Allow disk writes |

### Serial port detection

By default, Copperline automatically scans for connected Greaseweazle devices.
If multiple serial devices are attached, set `bridge_port` explicitly (e.g.,
`/dev/ttyACM0` on Linux or `COM3` on Windows).

### Cable conventions and drive select

`bridge_cable` sets the drive select signal:
- `a` or `b` for standard IBM PC twisted floppy cables (drive A or B).
- `0` through `3` for straight Shugart cables.

Ensure this matches your physical cabling and drive jumper configuration.
Disk change detection is supported on PC cables; on Shugart configurations,
disk swaps are detected during subsequent read operations.

### Density detection

`bridge_density` defaults to `auto`, detecting bit timings directly from flux
transitions. You can explicitly force `dd` or `hd` if reading non-standard disks
(such as high-density media formatted as double-density).

### Read modes

- **`normal` (Default):** Captures begin immediately when the drive head settles
  without waiting for an index pulse. Data is decoded and supplied to the guest
  pipelined in real time. Track revolutions that start off-index are reconstructed
  and verified against AmigaDOS track checksums. Verified tracks are cached in memory.
- **`compatible`:** Captures strictly from index pulse to index pulse. This mode
  incurs a slight delay waiting for the index hole, but preserves non-standard
  and copy-protected track structures exactly as recorded on the physical disk.
- **`stalling`:** Index-aligned capture that stalls guest CPU execution until track
  reading completes. This is intended only for timing-sensitive custom loaders
  that fail under normal read latency.

### Replay speed

Once a track is verified and cached in memory, `replay_speed` controls how fast
subsequent reads of that track are served:

- `fast` (Default): Cached tracks are replayed at double speed. The initial read
  always occurs at the physical platter rate.
- `normal`: Cached tracks are replayed at standard 1x rotational speed.

## Write protection and disk writes

To write to a physical disk, two requirements must be met:

1. The physical write-protect tab on the 3.5" disk must be set to writable.
2. `write_protected = false` (or `--floppy-bridge-writable`) must be specified in the configuration.

Writes are verified and committed directly to the physical medium. Full-track
revolution writes can start at any rotational position. However, partial track
writes that do not begin at the index pulse are refused because the hardware interface
cannot accurately position an offset partial write.

## Operational differences from disk images

- **Physical disk swapping:** Insert and eject disks directly using the physical drive.
  The status bar displays drive status and write-protection state.
- **Drive sound effects:** Virtual floppy drive sound synthesis is disabled for bridged
  drives since the physical drive produces acoustic feedback.
- **Emulation pacing:** Sessions using physical drives run at 1x wall-clock speed
  to maintain synchronization with the mechanical drive spindle.
- **Save states:** Save states cannot capture the state of physical magnetic media.

## Troubleshooting

- **Device permissions (Linux):** Ensure your host user account belongs to the `dialout`
  (or `uucp` / `plugdev`) group so it has permission to access `/dev/ttyACM*`.
- **Greaseweazle firmware:** Ensure your Greaseweazle board is running firmware 0.27
  or newer using the official `gw` utility.
- **Detailed diagnostic logging:** Set the environment variable `COPPERLINE_DIAG_FLUXBRIDGE=1`
  to view detailed head stepping, track decoding metrics, and seek timings in the terminal.
