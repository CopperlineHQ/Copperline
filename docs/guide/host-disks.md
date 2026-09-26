# Physical host disks

Copperline can give the emulated machine a real disk of the host's -- a
CompactFlash or SD card in a reader, a USB flash drive, or a hard drive --
instead of an image. The Amiga uses the medium as it is, with its own Rigid
Disk Block (RDB), partitions, and filesystems.

## Listing host disks

To list the disks the host can see:

```sh
copperline --list-disks
```

Example output:

```text
Host disks (name one to --host-disk, or as [[host_disk]] device):
  sdb        Generic MassStorageClass (31.9 GB)
  sdc        ATA Samsung SSD 870 (500.1 GB) [internal]
  sda        ATA Ubuntu Linux-0 S (68.7 GB) [system disk, internal, mounted: /]  -- cannot be used
```

Listing opens no device and needs no privileges. Disks on an internal bus are
labelled `internal` but still offered.

Safety protections:
- **System disk protection:** The disk the host is running from is detected and
  can never be opened (`cannot be used`); the launcher does not offer it.
- **Raw medium access:** The physical medium is presented to the guest as-is.
  Copperline does not synthesize an RDB for a disk that has none.

## Attaching a disk

### In the launcher

1. Go to **Storage -> Host Disk**.
2. Tick the disk's **Enable** box and click its **Attach** cell to choose the
   attachment point. **R/W** is ticked by default; untick it to keep the guest
   from writing to the disk.
3. Press **Mount**, and approve the operating system's permission prompt if
   one appears.

### From the command line

`--host-disk DEVICE [ATTACH]` attaches a disk read-write for this run, and
`--host-disk-read-only DEVICE [ATTACH]` attaches it write-protected. `ATTACH`
defaults to `ide-master`.

```sh
# Read-only mount (recommended for a first test):
copperline --model A1200 --fast 8M KICK31.ROM --host-disk-read-only sdb

# Read-write mount:
copperline --model A1200 --fast 8M KICK31.ROM --host-disk sdb
```

### In `copperline.toml`

```toml
[[host_disk]]
device = "sdb"                 # the name --list-disks prints
fingerprint = "v1-..."         # opaque identity written by the launcher
attach = "ide-master"          # attachment point (see table below)
read_only = true               # the default; false allows guest writes
```

The `[[host_disk]]` section of the [configuration reference](configuration.md)
gives the full rules.

### Attachment options

| Value | Controller and channel |
|---|---|
| `ide-master` | Built-in Gayle / A4000 IDE Master (Default) |
| `ide-slave` | Built-in Gayle / A4000 IDE Slave |
| `lide0-master` | `[lide]` expansion board Channel 0 Master (RIPPLE, RIDE, AT-Bus 2008) |
| `lide0-slave` | `[lide]` expansion board Channel 0 Slave |
| `lide1-master` | `[lide]` expansion board Channel 1 Master (RIPPLE only) |
| `lide1-slave` | `[lide]` expansion board Channel 1 Slave |
| `scsi0` .. `scsi6` | SCSI Controller Unit 0 through 6 |
| `pcmcia` | The A600/A1200 PCMCIA slot, as a CompactFlash card |

### A CompactFlash card from a real Amiga

A CF card that lives in an A600's or A1200's PCMCIA slot carries the
partitions the Amiga's own CF driver wrote (`compactflash.device`,
`cfd.device`, or a patched `scsi.device`), and those drivers expect the
card in the slot, not on the IDE cable. Put the reader's device on the slot
and the same driver in the guest finds the same card:

```sh
copperline --model A1200 KICK31.ROM --host-disk-read-only disk4 pcmcia
```

```toml
[machine]
profile = "A1200"

[[host_disk]]
device = "disk4"
attach = "pcmcia"
read_only = true
```

The card is presented through Gayle's slot windows with the CF register
layout a real card has (memory-mapped, contiguous I/O, or PC-style I/O,
whichever the driver configures), so the guest sees a card, not an IDE
drive; Kickstart's `card.resource` reports the insertion but does not mount
it by itself. The slot holds one card, so `attach = "pcmcia"` cannot be
combined with a `[pcmcia]` image, and more than 4M of Zorro II fast RAM
disables the slot (see [`[pcmcia]`](configuration.md#pcmcia-config)).
A card reader is removable media, so read-write access must be selected
afresh each session, as for any other removable disk below.

## Device fingerprints and renaming

Operating systems may change device names (such as `/dev/sdb` or `disk4`) when
devices are connected in a different order. To avoid mounting the wrong disk,
the launcher saves a `fingerprint` derived from the drive's serial number,
model, capacity, and sector size.

When a `fingerprint` is present in the configuration, Copperline matches the
device against current host drives. If the match is ambiguous or the device
cannot be found, the drive slot stays empty rather than mounting the wrong
device, and the machine starts anyway.

Read-write access to removable media (such as a card reader, which often
reports its own serial number rather than the card's) is not kept across
restarts, and neither is read-write access to a fixed disk without a credible
serial number: select and mount the disk in the launcher before each writing
session.

## Host operating system permissions

Reading or writing a whole disk is privileged on every supported host. The
system's own prompt asks for permission, and what comes back is an open handle
to that one device, not general elevated rights:

- **Linux:** A user in the `disk` group (or root, or one covered by a site udev
  rule) opens the device directly. Otherwise Copperline asks through Polkit
  (`pkexec`), which usually costs an administrator password.
- **macOS:** Authorization is requested through the system's `authopen` prompt.
- **Windows:** Copperline runs itself once more with administrator rights,
  which raises the UAC consent dialog, opens the disk there, and hands the
  handle back to the running session.

On Linux and Windows, several disks attached together share one prompt.

## Safe usage recommendations

1. **Test with read-only first:** Boot the disk with `read_only = true` (or
   `--host-disk-read-only`) to check that its filesystems mount correctly.
2. **Host volumes:** Copperline unmounts the host's volumes on the disk while
   the machine has it; macOS and Linux mount them again when the machine
   releases the disk. If a volume is busy and cannot be unmounted, the disk is
   not attached; close whatever is using it and try again. Linux unmounts
   through `udisksctl`; on a system without udisks2, unmount the volumes by
   hand first.
3. **Flushing writes:** Each IDE or SCSI write command is flushed to the medium
   before the guest sees it complete, so a finished write is on the disk. A USB
   bridge with no flush command is noted once in the log.
