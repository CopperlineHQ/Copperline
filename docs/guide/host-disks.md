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
More disks than the table shows scroll with the arrows in its top and bottom
right corners, each greyed at its end of the list; held, they work up through
five speeds, a second at each.

From the command line:

```sh
copperline --model A1200 --fast 8M KICK31.ROM --host-disk sdb
copperline --model A1200 --fast 8M KICK31.ROM --host-disk-read-only sdb
```

Or in a configuration file:

```toml
[[host_disk]]
device = "sdb"                 # last name shown by --list-disks
fingerprint = "v1-..."         # opaque identity written by the launcher
attach = "ide-master"          # ide-master (default), ide-slave, or scsi0..scsi6
read_only = true               # the default; false explicitly allows writes
```

`device` is the host's current enumeration name, exactly as `--list-disks`
prints it: `sdb` on Linux, `disk4` on macOS, or `PhysicalDrive1` on Windows.
Those names can change when disks are attached in a different order. The
launcher therefore saves `fingerprint`, an opaque identity made from the
disk's reported serial/model and geometry. On the next run the fingerprint is
authoritative: Copperline follows the same disk to a changed name only when
exactly one attached disk matches it, and refuses a missing or ambiguous
match rather than guessing. Keep the value the launcher wrote; do not invent
or copy one between disks.

Older and hand-written entries may omit `fingerprint`; they use an exact
`device` lookup and cannot follow a renamed disk. To add one, select and mount
the disk afresh in the launcher, then save or launch; merely opening and
saving the old entry deliberately preserves the missing fingerprint. A disk
that cannot be resolved leaves that drive slot empty and the machine starts
anyway, as a real Amiga does with an absent drive.

Persisted permission to write is deliberately narrower than fingerprint
matching. Removable media is always treated as weak, even if its USB bridge
reports a serial: that value often identifies the reader rather than the card
inside it. The same is true of any fixed disk without a credible serial or
WWN. A fingerprint can still resolve either kind for read-only use, but select
and mount the physical disk afresh in the launcher before every writable
session. Only a fixed, non-removable disk with a credible serial/WWN may reopen
writable from persisted configuration.

Omitting `read_only` is safe: it now means read-only. This intentionally makes
older configs that omitted the key read-only too; set `read_only = false`
only after checking the selected disk. The command-line flags describe a
choice made for this run, so `--host-disk` is explicitly read-write and
`--host-disk-read-only` is protected.

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
          was refused; tick R/W (or set `read_only = false`) to let it write
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

Save states carry the same fingerprint. Loading one re-enumerates the host and
reattaches only one unambiguous match; a changed `sdb`/`disk4`/
`PhysicalDrive1` ordinal alone never authorises a writable reopen. The same
weak-identity rule applies: removable media and disks without a credible
serial/WWN may return read-only on one unambiguous fingerprint match, but are
refused writable until freshly selected.

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
