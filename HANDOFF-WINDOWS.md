# Handoff: write the Windows host-disk backend

**Delete this file before the branch is merged.** It is a working brief for a
session on a Windows machine, not documentation.

You are picking up work on `feature/native-hdd` in Copperline, a cycle-driven
Amiga emulator in Rust. The feature lets the emulated Amiga use a *real* disk
of the host's — the SD or CF card that a real Amiga boots from — instead of a
disk image. Everything above the platform layer is written and working on
macOS. Your job is the Windows half.

Read `AGENTS.md` (`CLAUDE.md` is a symlink to it) first: it is the project's
standing instruction file and it governs how you work here.

---

## 1. Get oriented

```bash
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

```bash
.\target\release\copperline.exe --list-disks
```

Today it prints an empty list on Windows. When you are done it should name
every disk the machine has.

---

## 2. What already exists

### The seam you are filling

`src/blockdev/mod.rs` is platform-neutral and owns the safety rules, the
512-byte sector translation, and the types. Each host has one file beside it:

- `src/blockdev/macos.rs` — **written and working. Read this first.** It is the
  worked example for everything below.
- `src/blockdev/windows.rs` — a stub. **This is your file.** Its module docs
  already carry the API research; check each fact as you use it rather than
  trusting it.
- `src/blockdev/linux.rs` — a stub, for a later session.

A backend supplies exactly two functions:

```rust
pub fn list_devices() -> anyhow::Result<Vec<HostDevice>>;
pub fn open_device(device: &HostDevice, write: bool) -> anyhow::Result<BlockDevice>;
```

Nothing else. `mod.rs` handles sorting, the system-disk refusal, the
write-protect refusal, and the block-size check before your opener is reached.

### What you must fill in

```rust
pub struct HostDevice {
    pub id: String,          // "PhysicalDrive1" — stable, is what a config file names
    pub path: PathBuf,       // r"\\.\PhysicalDrive1" — what you open
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

`BlockDevice` already implements positioned reads and writes for Windows
(`seek_read`/`seek_write`) and already translates between the guest's 512-byte
sectors and larger media blocks, including the read-modify-write for a partial
block. You do not touch any of that. You hand it a `std::fs::File` — get one
from your `HANDLE` with `std::os::windows::io::FromRawHandle::from_raw_handle`.

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
   correctly. WinUAE gets this wrong (it guesses from the presence of an NTFS
   volume); do better.
2. **Internal fixed disks are hidden**, not refused — `Safety::Internal`. An
   Amiga disk arrives through a card reader or a USB bridge.
3. **Enumeration opens nothing for I/O.** Listing must not spin up a sleeping
   drive, must not need privilege, and must not disturb anything. Opening a
   device with `dwDesiredAccess = 0` for descriptive IOCTLs is fine and is the
   intended technique; opening for `GENERIC_READ` is not.
4. **A missing disk is a warning, not a crash.** A config naming a disk that is
   not plugged in must leave that drive slot empty and carry on.

---

## 4. Windows specifics

The module docs in `src/blockdev/windows.rs` spell out the API calls. The
decisions behind them:

- **No broker.** Windows has nothing like macOS's `authopen` that hands back an
  opened handle, so there is no equivalent of that dance.
- **Removable media does not need Administrator.** A disk with `RemovableMedia`
  set grants the interactive user full access — which covers the entire reason
  this feature exists. A fixed disk needs Administrator.
- **Do not relaunch elevated.** A process cannot elevate itself, and restarting
  the emulator would throw away the session. On `ERROR_ACCESS_DENIED`, return
  an error that says plainly this disk needs Copperline started as
  Administrator. Let the user decide.
- **Lock and dismount before writing.** Since Vista, a write to a sector owned
  by a mounted volume is refused. `FSCTL_LOCK_VOLUME` then
  `FSCTL_DISMOUNT_VOLUME`, and **keep the volume handles open for the session**
  — the lock dies with the handle. For an Amiga RDB disk this is usually a
  no-op, because Windows recognises nothing on it and mounts nothing.
- **Windows 10 and 11 both matter.** Nothing above is version-specific, but say
  so if you find something that is.

### FFI style

The project hand-rolls its FFI rather than pulling in binding crates — see
`src/blockdev/macos.rs` for IOKit and `src/net/bridge/windows.rs` for existing
Win32 declarations, which may already have what you need. Follow that. If you
believe a crate is genuinely warranted, say so and explain why rather than
adding it quietly.

---

## 5. House style

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

## 6. How to know it works

There is no substitute for a real card. Put an Amiga-formatted SD or CF card in
a reader, then:

```powershell
.\target\release\copperline.exe --list-disks
```

It must name the card, must mark the Windows system disk as unusable, and must
not offer it.

```powershell
.\target\release\copperline.exe --model A1200 --fast 8M KICK31.ROM --host-disk-read-only PhysicalDriveN
```

Read-only first, always. It should boot the card's Workbench.

Then the launcher: run with no arguments, Storage → Host Disk. The card should
be listed; ticking it and choosing Mount should attach it, and Run should boot.

**Ask the maintainer before any test that writes to a real disk.** Their card
is the only copy of an Amiga's system disk. There is an ignored round-trip test
(`blockdev::tests::device_round_trip`) that writes, and the doc comment on it
explains how to point it at a throwaway disk image instead of hardware — use
that.

Add unit tests for anything that is pure logic — the partition-to-disk name
mapping, the size formatting, the safety classification. `src/blockdev/macos.rs`
has examples at the bottom, including one that asserts enumeration works with
nothing plugged in and that exactly one disk is identified as the system's.

---

## 7. When you are done

Push the branch to the fork (`origin`, `hobbo91/Copperline`). Do not open a PR.

Then write the equivalent of this file for the **Linux** backend, as
`HANDOFF-LINUX.md`, so the next session can do that half on a real Linux host.
Base it on this one, on `src/blockdev/linux.rs`'s module docs, and on whatever
you learn writing the Windows side about which parts of the brief actually
helped. Linux's shape is different in one important way — the privilege
escalation there *can* hand back a descriptor (udisks2 over D-Bus, or `pkexec`
running a small privileged mode of this same binary), so it is closer to macOS
than to Windows. `src/blockdev/linux.rs` has the detail.
