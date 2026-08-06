# Real hard disks

Copperline can give the emulated machine a *real* disk of the host's -- the
CF card, SD card, or IDE drive a real Amiga boots from -- instead of a hard
drive image. The Amiga sees the medium exactly as it is: its own Rigid Disk
Block, its own partitions, its own filesystem. Nothing is copied, converted,
or synthesised, so a card taken out of an A1200 and put in a reader boots the
same Workbench it booted five minutes earlier.

This is the most dangerous thing the emulator can be asked to do. Everything
else it writes to is a file; this writes to whole physical media, where a
mistake is not a corrupted image but somebody's only copy of their Amiga's
system disk. The safety rules below are not configurable, and are the reason
the feature is shaped the way it is.

## What is offered, and what is not

```sh
copperline --list-disks
```

names every disk the host can see, what it is, how big it is, and anything
you need to know before choosing it:

```text
Host disks (name one to --host-disk, or as [[host_disk]] device):
  sdb        Generic MassStorageClass (31.9 GB)
  sda        ATA Ubuntu Linux-0 S (68.7 GB) [system disk, mounted: /]  -- cannot be used
```

Listing opens nothing. It cannot spin up a sleeping drive, cannot disturb a
disk, and needs no privileges -- so it is safe to run at any time, and if
listing ever asks you for a password, something is wrong.

Three rules decide what you may have:

- **The disk the host is running from is never offered and never opened.** Not
  a warning and not a confirmation: it is refused, by name, however it is
  asked for -- from the launcher, from a config file, from `--host-disk`, and
  by the privileged half of the opener that runs as root. It is still *shown*
  by `--list-disks`, marked `cannot be used`, because a disk that silently
  vanished from the list would just look like a bug.
- **Internal fixed disks are hidden, not refused.** An Amiga disk reaches a
  modern computer through a card reader or a USB adapter, so internal storage
  is almost never what you want; it is left out of the launcher's list but can
  still be named deliberately.
- **Nothing is invented over unfamiliar bytes.** No RDB is synthesised over a
  real disk. An Amiga disk carries its own partition table, and a disk that
  does not have one is a disk Copperline will not pretend to understand.

On Linux the "which disk is the host's" question is harder than it looks,
because `/` is often served by LVM or LUKS rather than by a partition
directly. Copperline traces the root filesystem through however many
device-mapper and MD layers sit under it, and a volume group spanning several
disks marks *every* one of them as the system's. If a layout ever cannot be
traced, no disk is offered at all and the log says so -- offering nothing is
recoverable, and offering the wrong disk is not.

## Attaching one

From the launcher: **Storage → Host Disk**, tick the disk, choose where the
machine should see it, and press **Mount**. Permission is asked for there --
at the button you just pressed -- rather than later, behind a machine that is
starting.

From the command line:

```sh
copperline --model A1200 --fast 8M KICK31.ROM --host-disk sdb
copperline --model A1200 --fast 8M KICK31.ROM --host-disk-read-only sdb
```

Or in a configuration file, naming the disk as `--list-disks` prints it:

```toml
[[host_disk]]
device = "sdb"            # "sdb" on Linux, "disk4" on macOS, "PhysicalDrive1" on Windows
attach = "ide-master"     # ide-master (default), ide-slave, or scsi0..scsi6
read_only = true          # absent means writable
```

The identifier is the *stable* one the host uses for the hardware, not the
node path, because a node path is a property of this boot rather than of the
disk. A configuration naming a disk that is not plugged in is a warning, not
an error: that drive slot is left empty and the machine starts anyway, which
is what a real Amiga does with an absent drive.

## Read-only first

Attach a disk read-only the first time, always. It costs one boot and it is
the single most informative test there is:

```sh
copperline --model A1200 --fast 8M KICK31.ROM \
  --host-disk-read-only sdb --noaudio --screenshot-after 40 /tmp/ro.png
```

An Amiga filesystem writes when it mounts -- PFS marks the volume in use --
so a *correct* read-only attach boots far enough to raise the guest's own
write error on screen, and the log shows the refusal coming from `blockdev`
naming the same block:

```text
blockdev: sdb is attached read-only, so the guest's write to sector 1026146
          was refused; tick R/W (or drop `read_only`) to let it write
```

That proves the whole read path at once: enumeration, the open, the sector
translation, and the RDB parsing. Then the same run read-write should reach
Workbench with no requester at all.

## Exclusive use, and giving the disk back

A disk given to the machine is taken from the host completely. Any volumes
the host had mounted from it are unmounted first, and the medium is claimed
exclusively for as long as the machine has it -- on every attach, read-only
included. The host writing its own filesystem metadata to a medium underneath
a guest that cannot account for it changing is a hazard whichever way the
emulator opened it.

On Linux the unmount goes through `udisksctl`, the same machinery your file
manager's eject button uses, so unmounting your own removable disk normally
costs no prompt at all. If something else still has a file open on the disk,
the attach fails and says so rather than taking it out from underneath.

Powering the emulated machine off hands the disk back to the host, and
powering it on takes it again -- a drive is powered by the machine, so with
the Amiga off the disk belongs to the host, exactly as a physical floppy
drive is released. A disk taken from the launcher's **Mount** button stays
taken across a machine being stopped and started, so you are not asked for
permission again; **Unmount** is what gives it back for good.

Writes are flushed to the medium rather than left in the host's cache,
because a card can be pulled out of a reader at any moment.

## Permission

Raw access to a whole disk is privileged on every supported host, and each one
grants it differently.

| Host | What happens |
|---|---|
| **Linux** | `/dev/sdX` is `root:disk 0660`, so a desktop user cannot open it. Copperline tries a direct open first -- which is enough if you are in the `disk` group, have a udev rule, or are running as root -- and otherwise asks through `pkexec`, which raises the polkit prompt and runs a small privileged half of Copperline that opens the disk and passes the descriptor back. |
| **macOS** | Raw media is gated behind a privacy check as well as file permissions, and being root does not satisfy it. Copperline asks through `/usr/libexec/authopen`, Apple's own tool for exactly this. |
| **Windows** | Raw access to a whole disk is granted to Administrators only, whether the disk is removable or not. Copperline runs itself once with consent, and that privileged half copies the open handles back into the running process. |

In every case the same shape holds: you are asked once, by the system's own
prompt, and what comes back is an already-open handle to *one named device* --
a capability that cannot be turned on anything else. The emulator itself never
runs with elevated privileges, and the privileged half re-checks the safety
rules for itself rather than trusting what asked it, so the disk the host runs
from is refused there too.

If several disks are ticked, they are taken together, because the prompt is a
dialog somebody has to read and one is enough for all of them.
