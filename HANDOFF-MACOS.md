# Handoff: the host-disk feature's open ends

**Delete this file before the branch is merged.** It is a working ledger for
`feature/native-hdd`, not documentation. Earlier versions were briefs for
work now done; this one keeps the measurements and judgement calls worth
preserving, and the list of what is still open. Where it disagrees with the
code or the commit messages, they are the record, not this.

Read `AGENTS.md` (`CLAUDE.md` is a symlink to it) first: it governs how you
work here.

---

## 1. Where things stand

`src/blockdev/mod.rs` is platform-neutral and owns the safety rules, the
512-byte sector translation, the reservation table (disks are taken at the
launcher's Mount button or on a config-driven run's first machine, lent to
each machine, held until Unmount or exit), and the broker wire format. Each
host's file supplies `list_devices`, `take_disks`, and `lend`:

| | macOS | Windows | Linux |
|---|---|---|---|
| Enumeration | IOKit `IOMedia`, `Whole` only | `GUID_DEVINTERFACE_DISK`, opened with access 0 | `/sys/block`, no open at all |
| System disk found via | mount on `/`, traced to APFS physical stores, closed both directions | volume → disk extents | `mountinfo` maj:min → sysfs, recursing `slaves/` |
| Privileged open | `authopen` (Apple's broker) | own broker, `runas` + `DuplicateHandle` | own broker, `pkexec` + `SCM_RIGHTS` |
| Unmount | `diskutil unmountDisk` | `FSCTL_LOCK_VOLUME`/`DISMOUNT` | `udisksctl unmount -b` |
| Exclusion lasts | the descriptor (writable attaches) | the handle (held in the reservation and the machine) | the open file description (`O_EXCL`) |

The review the previous version of this file asked for has happened, from a
machine that type-checks and clippy-checks all three targets (rustup
keg-only beside the Homebrew toolchain: `cargo clippy --lib
--no-default-features --target x86_64-unknown-linux-gnu` and
`x86_64-pc-windows-msvc`). What it found and changed is in the history from
`76320d2` onward -- the duplicated reservation/broker code unified, the
Windows privileged half's missing block-size check restored, the APFS
system-disk hole closed, macOS on the same Mount-time prompt as the others,
flushing through the disk ioctl, and the Gayle/ATA fixes that let AROS's
`ata.device` see IDE drives.

User-facing documentation: `docs/guide/host-disks.md`, `[[host_disk]]` in
`docs/guide/configuration.md` and `copperline.example.toml`.

---

## 2. Measurements worth keeping

Both early platform briefs stated the privilege model confidently and were
wrong in the same direction -- assuming removable media would be easier than
it is. The measurements that replaced the guesses:

- **Windows**: the permissive grant for removable media is on the *volume*
  object, never the disk, so raw disk access needs Administrator whether the
  medium is removable or not (Windows 11 build 26200; the security
  descriptor is quoted in `windows.rs`).
- **Linux** (Ubuntu 24.04/GNOME 46, active local session, ordinary user):
  `/dev/sdb` is `root:disk 0660` with no ACL; a card reader's node is as
  closed as the system disk's; `open()` is `EACCES` in every mode. There is
  no seat-based grant for whole disks -- escalation is the ordinary desktop
  path, not a fallback.
- **Linux**: sysfs `size` is always 512-byte units whatever
  `logical_block_size` says (verified with `scsi_debug sector_size=4096`);
  `O_EXCL` on a block device really is mandatory against mounts, though a
  plain non-exclusive `open()` still succeeds alongside it.
- **udisks2**: `Block.OpenDevice` works and takes extra open flags, but its
  polkit action is `auth_admin_keep` even for an active session -- the same
  password `pkexec` costs, for the price of a hand-rolled D-Bus client.
  `pkexec` was chosen; udisks2 still does the unmount, where the shipped
  policy is `yes` for an active session and so costs no prompt.
- **macOS** (Darwin 25.6.0): the raw node is `EPERM` for an ordinary user in
  both modes, root does not pass the privacy gate either, `authopen -o 2`
  hands back a working descriptor, `fsync` on `/dev/rdiskN` is `ENOTTY`
  (`DKIOCSYNCHRONIZECACHE` is the call that works), and
  `AuthorizationCreate` fails (-60008) from an unbundled binary. The full
  section is at the top of `macos.rs`.

## 3. Judgement calls a reviewer may still challenge

1. **`pkexec` over udisks2 on Linux** -- both cost an admin password;
   `pkexec` avoided a D-Bus dependency. Someone may reasonably prefer the
   desktop-native call and a crate.
2. **Reservation at the Mount button, kept for the session** -- one prompt
   ever, at the cost of a disk staying held while no machine runs. All three
   hosts now do this; Unmount (or quitting) is what frees the disk.
3. **`ESSENTIAL_MOUNTS` in `linux.rs`** -- a path list is a heuristic; it
   exists because a root on ZFS or a live USB is served by no block device.
   Is the list right? Is a list the right shape?
4. **Refusing everything when the root cannot be traced** -- safe, and one
   unrecognised layout turns the feature off with only a log line to say so.
5. **Enumeration cost** -- `find_device` is a full enumeration per disk.
   Judged fine for the handful anybody attaches; check against a machine
   with very many mounts.

---

## 4. Still open

- **The macOS prompt is attributed to `authopen`, not to Copperline.**
  `authorize_open` pre-authorizes in Copperline's name (one dialog covering
  every disk in the batch) and hands `authopen` the external form, but
  `AuthorizationCreate` fails from an unbundled binary, so the fallback runs
  and the dialog says `authopen`, once per disk. The suspicion is that a
  signed application bundle fixes it. Establish that when the `.app`
  packaging happens, and update `authorize_open`'s known-limitation note
  with what was measured.

## 5. Not verified anywhere

Be honest about these rather than assuming they work.

- **The GUI power-off / power-on cycle against a real card on Linux and
  macOS.** The mechanic underneath -- a reservation outliving the machine's
  copy -- is covered by
  `a_reserved_disk_is_lent_to_the_machine_rather_than_reopened`; the
  window-level path has been exercised against real media on Windows only.
- **Any multi-disk attach, on any host.** The one-prompt-for-several path
  has wire-format unit coverage, not a real two-card run.
- **SCSI attachment of a real disk.** `attach = "scsi0"` is wired and
  config-tested; no real medium has been on the emulated SCSI bus.
- **A write to a real Amiga card on Linux and macOS.** Reads are proven
  against the maintainer's card; writes only against disposable
  `scsi_debug`/attached-image disks. Writing to somebody's only copy of an
  Amiga system disk needs their say-so.
- **AROS booting a real card's installed OS.** AROS now lists an IDE drive
  and parses its RDB (proven with an image); the maintainer's OS3.x/PFS3
  card is untested under it -- a `.img` of the card is promised for exactly
  this.

## 6. How to verify against real media

The read-only attach is the most informative single test. Against a real
PFS3 card in a reader:

```sh
./target/release/copperline --model A1200 --fast 8M KICK31.ROM \
  --host-disk-read-only sdb --noaudio --screenshot-after 40 /tmp/ro.png
```

A correct run shows the guest's own write-error requester on screen (PFS
marks the volume in use at mount) and the log shows `blockdev` refusing the
same block -- one picture proving enumeration, the open, the sector
translation, and the RDB parsing at once. Then the same run read-write
should reach Workbench with no requester.

For writes without risking real media, the disposable disks are `scsi_debug`
(Linux; loop devices are deliberately not enumerated), an `hdiutil`-attached
image (macOS), and an attached VHD (Windows) -- recipes on the doc comment
of `blockdev::tests::device_round_trip`. `sector_size=4096` is the only way
most machines have of exercising the read-modify-write path.

---

## 7. When you are done

Push to the fork (`origin`, `hobbo91/Copperline`). **Never open a pull
request** -- that is the maintainer's step, always. Delete this file when
the branch merges.
