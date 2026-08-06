# Handoff: write the Linux host-disk backend

**Delete this file before the branch is merged.** It is a working brief for a
session on a Linux machine, not documentation.

You are picking up work on `feature/native-hdd` in Copperline, a cycle-driven
Amiga emulator in Rust. The feature lets the emulated Amiga use a *real* disk
of the host's — the SD or CF card that a real Amiga boots from — instead of a
disk image. Everything above the platform layer is written and working, and
two of the three hosts are done: macOS and Windows. Yours is Linux.

Read `AGENTS.md` (`CLAUDE.md` is a symlink to it) first: it is the project's
standing instruction file and it governs how you work here.

---

## 1. Get oriented

```sh
git fetch origin
git checkout feature/native-hdd
cargo build --release
cargo test
cargo clippy --all-targets
cargo fmt --check
```

All four should be clean. One test, `cpu::tests::three_byte_operand_moves_exactly_three_bytes`,
fails on `main` too — it is not yours and not related.

Then look at the feature as a user:

```sh
./target/release/copperline --list-disks
```

Today it prints an empty list on Linux. When you are done it should name every
disk the machine has, and mark the one the system is running from as unusable.

---

## 2. What already exists

### The seam you are filling

`src/blockdev/mod.rs` is platform-neutral and owns the safety rules, the
512-byte sector translation, and the types. Each host has one file beside it:

- `src/blockdev/macos.rs` — **written and working.** IOKit for enumeration and
  a broker (`authopen`) that hands back an opened descriptor. This is the one
  Linux most resembles, because Linux can also get a descriptor back from a
  privileged opener.
- `src/blockdev/windows.rs` — **written and working.** Worth reading because
  Windows ships no broker at all and the backend therefore *is* one: it runs
  the same binary again with consent, and that privileged half copies the open
  handles back into the still-running process and exits. If udisks2 turns out
  to be awkward, that shape — a privileged mode of this same binary, launched
  once, handing a descriptor back — is the `pkexec` route with a worked
  example already in the tree.
- `src/blockdev/linux.rs` — a stub. **This is your file.** Its module docs
  carry the API research. Treat every line of it as a hypothesis (see §5).

A backend supplies exactly two functions:

```rust
pub fn list_devices() -> anyhow::Result<Vec<HostDevice>>;
pub fn open_device(device: &HostDevice, write: bool) -> anyhow::Result<BlockDevice>;
```

`mod.rs` handles sorting, the system-disk refusal, the write-protect refusal,
and the block-size check before your opener is reached.

### What you must fill in

```rust
pub struct HostDevice {
    pub id: String,          // "sdb" — stable, is what a config file names
    pub path: PathBuf,       // "/dev/sdb" — what you open
    pub model: Option<String>,
    pub size_bytes: u64,
    pub block_size: u32,     // the media's own logical sector size
    pub removable: bool,
    pub internal: bool,
    pub writable: bool,      // the *hardware* says so (a locked SD card does not)
    pub mounted: Vec<String>, // where the host has volumes from this disk
    pub safety: Safety,      // Offerable | Internal | SystemDisk
}
```

And build the return of `open_device` with:

```rust
super::BlockDevice::new(file, device.id.clone(), device.block_size, device.size_bytes, write)
```

`BlockDevice` already implements positioned reads and writes for unix
(`pread`/`pwrite`) and already translates between the guest's 512-byte sectors
and larger media blocks, including the read-modify-write for a partial block.
You do not touch any of that. You hand it a `std::fs::File` — get one from
your descriptor with `std::os::fd::FromRawFd::from_raw_fd`.

There is also a `BlockDevice::holding` builder, added for Windows, which keeps
handles alive for as long as the machine has the disk. **Linux should not need
it**: `O_EXCL` on a block device lives as long as the descriptor you already
hand over, so the exclusion comes for free with the thing you return. If you
find yourself reaching for it, that is a sign something has been misunderstood
— say so rather than using it quietly.

### Where the disk ends up

`src/harddrive.rs` `HardDriveImage::open_device` wraps it; `src/ata.rs` and
`src/scsi.rs` attach it as an IDE or SCSI drive. **No RDB is synthesized over a
real disk** — an Amiga disk carries its own partition table, and inventing one
over unfamiliar bytes is how real media gets destroyed. Do not change this.

---

## 3. Rules that are not yours to relax

These are the point of the module. `src/blockdev/mod.rs` has the long version
in its module docs; the short version:

1. **The disk the host is running from is never offered and never opened.** Not
   a warning, not a confirmation — it is not in the list, and `open_device` in
   `mod.rs` refuses it before your code runs. Your job is to *identify* it
   correctly, and on Linux that is the hardest part of the whole file: see §4.
2. **Internal fixed disks are hidden**, not refused — `Safety::Internal`. An
   Amiga disk arrives through a card reader or a USB bridge.
3. **Enumeration opens nothing for I/O.** Listing must not spin up a sleeping
   drive, must not need privilege, and must not disturb anything. Reading
   sysfs is free; opening `/dev/sdX` to ask it something is not.
4. **A missing disk is a warning, not a crash.** A config naming a disk that is
   not plugged in must leave that drive slot empty and carry on.

---

## 4. Linux specifics

`src/blockdev/linux.rs`'s module docs spell out the files and calls. The
decisions behind them, and the parts that will actually cost you time:

- **Enumeration is free.** It is all in sysfs — `/sys/block`, skipping what is
  not real media (`loop*`, `ram*`, `zram*`, `dm-*`, `md*`, and `sr*`). No
  privilege, no opening. This is the easiest of the three hosts by a distance.
- **`size` is always in 512-byte units**, whatever `queue/logical_block_size`
  says. Multiply by 512, never by the logical block size. Windows had the
  mirror image of this trap and it is the kind of mistake that reads as
  plausible: a disk that is wrong by a factor of eight still enumerates, still
  opens, and fails only at the end.
- **Identifying the running system's disk is the dangerous part.** On macOS and
  Windows a volume maps to its disk by one call. On Linux `/` is very often
  served by `/dev/mapper/...` — LVM, LUKS, or both — and the physical disk
  underneath is reached through `/sys/block/dm-N/slaves/`, recursively, because
  a mapper device can sit on another mapper device. **A root on LVM whose disk
  is not identified leaves that disk offerable**, which is the one outcome this
  module exists to prevent. Write the test for this before the code, and if you
  cannot identify the root's physical disk on some layout, classify
  conservatively rather than guessing.
- **Taking the disk.** `O_EXCL` on a block device is not advisory: the kernel
  refuses it while any filesystem on that disk is mounted, and refuses a later
  mount while it is held. That plus `flock(LOCK_EX)` is the whole interlock.
  A disk the host has mounted must still be unmounted first — prefer
  `udisksctl unmount -b /dev/sdb1` over `umount(8)`, because it gives the user
  the polkit prompt they already know from their file manager.
  Note the Windows backend takes the volumes on **every** attach, read-only
  included, because exclusive use is the point of attaching at all. Match that.
- **Privilege, and the escalation.** `/dev/sdX` is `root:disk 0660`. Try the
  direct open first regardless — a user in the `disk` group, a udev rule, or
  simply running as root all make it work, and prompting when nothing is in the
  way would be rude. On `EACCES`/`EPERM`, escalate to a privileged open that
  hands the descriptor back:
  - **udisks2** (`org.freedesktop.UDisks2.Block.OpenDevice`) returns a file
    descriptor over D-Bus behind a polkit check. Desktop-native, already
    running on every desktop distribution, nothing to install. It does mean
    speaking D-Bus, including `UNIX_FD` passing.
  - **`pkexec`** running this same binary in a small privileged mode, which
    opens the device and sends the descriptor back over a unix socket the
    parent named. No new dependency, and one prompt can cover both the unmount
    and the open.

  Either way the privileged side opens with `O_EXCL` and the receiving side
  sets `FD_CLOEXEC` immediately. `src/net/bridge/linux.rs` already does
  `SCM_RIGHTS` in both directions (receiving at line ~380, sending at ~516) and
  ships a `copperline-net-helper` privileged helper with a socket path and a
  systemd unit — that is the closest worked example in the tree for the
  `pkexec` route, and `src/blockdev/macos.rs`'s `receive_fd` is the closest for
  the receiving half. Darwin's and Linux's `cmsg` rules differ in ways that
  fail silently, so take the Linux one from the Linux file.

### FFI style

The project hand-rolls its FFI rather than pulling in binding crates — see
`src/blockdev/macos.rs` for IOKit and `src/blockdev/windows.rs` for Win32.
Follow that. `libc` is already a dependency and is the right level for this
file. If you believe a crate is genuinely warranted — a D-Bus client is the
one plausible case — say so and explain why rather than adding it quietly.

---

## 5. Verify the research before you design around it

The single most useful thing learned writing the Windows backend: **the API
notes in the stub's module docs are a starting point, not fact, and the
privilege model is the one to check first because everything else is shaped by
it.**

The Windows stub stated that removable media grants the interactive user full
access, so no elevation would be needed for the case the feature exists for.
That is false. Ten minutes of measuring — opening the device with each access
mask and reading the security descriptor back — showed the permissive grant is
on the *volume* object and never on the disk, so raw disk access needs
Administrator whether the disk is removable or not. Had that been taken on
trust, the backend would have been built around a privilege escalation that
was neither needed nor possible, and the real one would have been missing.

So, before writing the backend: as an ordinary user, with a card in a reader,
actually try `open("/dev/sdX", O_RDONLY)` and `O_RDWR | O_EXCL`, and check the
mode and group of the node. Find out whether your desktop's udev rules already
grant the seat's user access to removable media — some do — because if they do,
the escalation is a fallback rather than the main path, and that changes what
the code should try first. Write down what you measured in the module docs the
way `windows.rs` does; the next person deserves facts with a date on them, not
inherited assumptions.

---

## 6. House style

`AGENTS.md` governs. What catches people out here:

- **Comments say *why*, never *what*.** Look at any file in `src/` before
  writing your first one. A comment restating the code will be sent back.
- **Model the hardware, never the program.** Explain behaviour in terms of the
  hardware and the OS, never "so that X works".
- **Document in the same change.** Anything user-visible updates the matching
  chapter in `docs/guide/`.
- `cargo clippy` and `cargo fmt --check` must both be clean.
- **Never commit ROMs or disk images.** They are local assets.
- **Never open a pull request.** Prepare the branch, push it, and stop. Opening
  the PR is the maintainer's step, always.

---

## 7. How to know it works

There is no substitute for a real card. Put an Amiga-formatted SD or CF card in
a reader, then:

```sh
./target/release/copperline --list-disks
```

It must name the card, must mark the system disk as unusable, and must not
offer it. Run it as an ordinary user: if listing needs privilege, something is
being opened that should not be.

Then attach it. **Read-only first, always** — and this sequence turned out to
be the most informative test of the lot on Windows, so use it:

```sh
./target/release/copperline --model A1200 --fast 8M KICK31.ROM \
  --host-disk-read-only /dev/sdX --noaudio --screenshot-after 40 /tmp/ro.png
```

An Amiga filesystem writes when it mounts — PFS marks the volume in use — so a
correct read-only attach boots far enough to raise the *guest's own* write
error on screen, and the log shows the refusal coming from `blockdev`. That
single screenshot proves the whole read path: enumeration, the open, the sector
translation, and the RDB parsing. Then the same run read-write must boot to
Workbench with no requester at all, which proves the write path and the
unmount. Neither test needs you to write a byte yourself.

Then the launcher: run with no arguments, Storage → Host Disk. The card should
be listed; ticking it and choosing Mount should attach it, and Run should boot.

**Ask the maintainer before any test that writes to a real disk.** Their card
may be the only copy of an Amiga's system disk. There is an ignored round-trip
test (`blockdev::tests::device_round_trip`) that writes, and the doc comment on
it explains how to point it at a throwaway instead of hardware — on Linux a
loop device over a sparse file is the equivalent of the macOS attached image
and the Windows VHD already documented there. Add the `losetup` recipe to that
comment when you have used it.

Add unit tests for anything that is pure logic — the partition-to-disk name
mapping (`sdb1`→`sdb`, `nvme0n1p3`→`nvme0n1`, `mmcblk0p1`→`mmcblk0`, and the
mapper cases), the bus classification, the safety classification.
`src/blockdev/macos.rs` and `src/blockdev/windows.rs` both have examples at the
bottom, including one that asserts enumeration works with nothing plugged in
and that the system's own disk is identified.

---

## 8. When you are done

Push the branch to the fork (`origin`, `hobbo91/Copperline`). Do not open a PR.

That is the last of the three backends, so also: delete this file and
`HANDOFF-WINDOWS.md`, and make sure `docs/guide/` actually describes the
feature. At the time this was written the host-disk feature had no chapter of
its own — only a passing mention under save states — which is a debt the last
of the three sessions is best placed to clear.

**Write down what both this and the Windows backend turned out to need.** The
maintainer goes back to macOS after this to finish the rest, and that session
starts knowing none of it: which claims in these briefs held up and which did
not, what each host demanded that its brief did not predict, and what is still
owed. Record it wherever this session's notes persist as well as in the code —
the module docs in `windows.rs` are the model, in that they say what was
*measured*, on what, and when.

One Windows decision worth a look before you copy it: the launcher's Mount
button takes the disk there and then (`blockdev::reserve_device`), so the
consent dialog belongs to the button somebody just pressed instead of turning
up minutes later behind a machine starting. `release_device` hands it back.
Both are no-ops off Windows, deliberately — macOS was left alone rather than
changed on a guess. Decide whether polkit wants the same, and say which you
chose and why.
