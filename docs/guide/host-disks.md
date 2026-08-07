# Real hard disks

Copperline can give the emulated machine a real disk of this computer's
instead of a hard-drive image -- any disk it can see: a card in a reader, a
USB drive, a drive on a SATA port. The Amiga sees the medium as it is, with
its own Rigid Disk Block, its own partitions, its own filesystem. Nothing is
copied or converted, so a card taken out of an A1200 boots the same Workbench
it booted five minutes earlier.

This writes to whole physical media rather than to a file, where a mistake
costs somebody their only copy of an Amiga's system disk. The rules below are
not configurable.

## What you can attach

```sh
copperline --list-disks
```

names every disk, how big it is, and anything you need to know before
choosing one:

```text
Host disks (name one to --host-disk, or as [[host_disk]] device):
  sdb        Generic MassStorageClass (31.9 GB)
  sdc        ATA Samsung SSD 870 (500.1 GB) [internal]
  sda        ATA Ubuntu Linux-0 S (68.7 GB) [system disk, internal, mounted: /]  -- cannot be used
```

Listing reads nothing from any medium and needs no privileges, so it is safe
to run at any time.

Every disk is offered except **the one this computer is running from**, which
is refused by name however it is asked for -- launcher, config file, or
command line. It is still listed, marked `cannot be used`, because a disk
that silently vanished would just look like a bug. Which disk that is gets
worked out afresh on every run from your machine's own layout, following the
running system through whatever sits under it: LVM and LUKS on Linux, an APFS
container down to the drive it is stored on for macOS. If a layout cannot be
traced, nothing is offered at all.

Disks on an internal bus are labelled `internal` and sorted last, but they
are yours to use. And no RDB is ever invented over a disk that has none: an
Amiga disk carries its own partition table, and one that does not is a disk
Copperline will not pretend to understand.

## Attaching one

From the launcher: **Storage → Host Disk**, tick the disk, choose where the
machine should see it, and press **Mount**. Permission is asked for there, at
the button you pressed, rather than later behind a machine that is starting.

From the command line:

```sh
copperline --model A1200 --fast 8M KICK31.ROM --host-disk sdb
copperline --model A1200 --fast 8M KICK31.ROM --host-disk-read-only sdb
```

Or in a configuration file:

```toml
[[host_disk]]
device = "sdb"            # "sdb" on Linux, "disk4" on macOS, "PhysicalDrive1" on Windows
attach = "ide-master"     # ide-master (default), ide-slave, or scsi0..scsi6
read_only = true          # absent means writable
```

`device` is the host's stable name for the hardware, exactly as
`--list-disks` prints it -- not a node path, which belongs to this boot
rather than to the disk. A disk named here that is not plugged in leaves that
drive slot empty and the machine starts anyway, as a real Amiga does with an
absent drive.

## Read-only first

Attach a disk read-only the first time. It costs one boot and tells you more
than anything else:

```sh
copperline --model A1200 --fast 8M KICK31.ROM --host-disk-read-only sdb
```

An Amiga filesystem writes when it mounts -- PFS marks the volume in use --
so a *correct* read-only attach boots far enough to raise the guest's own
write error, and the log names the same block:

```text
blockdev: sdb is attached read-only, so the guest's write to sector 1026146
          was refused; tick R/W (or drop `read_only`) to let it write
```

That proves the read path end to end. Attach it read-write and it should
reach Workbench with no requester at all.

## While the machine has it

The disk is taken from the host completely. Volumes mounted from it are
unmounted first -- the host writing its own metadata under a guest that
cannot account for it changing is a hazard either way -- and the medium stays
the machine's until you give it back. If something else still has a file open
on the disk, the attach fails and says so rather than taking it out from
underneath.

It is taken once, at **Mount** (or when a configuration-driven run first
starts), and stays taken for the rest of the session. Powering the emulated
machine off and on again does not ask for permission a second time.
**Unmount** -- on the Host Disk page, or beside the drive on the Storage page
-- hands the disk back, and so does quitting.

Writes are flushed to the medium rather than left in the host's cache,
because a card can be pulled out of a reader at any moment.

## Permission

Raw access to a whole disk is privileged everywhere, and each system grants
it differently:

| Host | |
|---|---|
| **Linux** | `pkexec` raises the polkit prompt. A direct open is tried first, which is enough if you are in the `disk` group or running as root. |
| **macOS** | `/usr/libexec/authopen`, Apple's own tool for this, shows the standard authorization prompt. |
| **Windows** | Raw disk access is for Administrators only, so Windows asks for consent. |

You are asked by the system's own prompt, once per session, and what comes
back is a handle to *one named disk* -- it cannot be turned on anything else.
Copperline itself never runs elevated.

On Windows and Linux the Host Disk page warns you before you tick anything,
so the prompt after **Mount** is not a surprise, and several disks ticked at
once cost one prompt between them. Running elevated skips both. macOS shows
no warning because elevation is not what gates the disk there -- root meets
the same prompt.
