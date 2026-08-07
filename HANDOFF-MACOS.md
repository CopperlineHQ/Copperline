# Handoff: review the Windows and Linux work, then finish macOS

**Delete this file before the branch is merged.** It is a working brief for a
session on a Mac, not documentation. It replaces `HANDOFF-LINUX.md`, which
replaced `HANDOFF-WINDOWS.md`; each went when its backend landed.

All three platform backends of the real-disk feature now exist on
`feature/native-hdd`: macOS (written first), Windows, and Linux. Two jobs
remain, in this order: **an objective review of everything the Windows and
Linux sessions added**, and then the macOS work those sessions could not do.

Read `AGENTS.md` (`CLAUDE.md` is a symlink to it) first: it governs how you
work here.

---

## 0. The review

### Scope

```sh
git fetch origin && git checkout feature/native-hdd
git diff 6b34a33^..HEAD -- src/ docs/ copperline.example.toml   # ~4400 lines
git log --oneline 6b34a33^..HEAD                                # 16 commits
```

`6b34a33` is where the platform seam was cut and the two briefs written;
everything after it is the Windows and Linux backends and the launcher and
bus changes they needed. The bulk is two files that share no code:
`src/blockdev/windows.rs` (1850 lines) and `src/blockdev/linux.rs` (1776).
Everything before `6b34a33` is the macOS backend and the platform-neutral
core, already reviewed when it landed.

### Why this needs a fresh reviewer

Each backend was written by a session running on that platform, which could
build and test its own file and **neither compile nor run the other two**.
That is the structural weakness of the whole feature and it is where the
review should push hardest:

- Nothing has ever type-checked all three backends together. `cargo clippy`
  on a Mac has never seen `windows.rs` or `linux.rs`.
- `src/blockdev/mod.rs`, `src/main.rs`, `src/bus.rs`, and
  `src/video/window.rs` are shared and were edited by all three sessions. A
  change made for one host that quietly breaks another would not have been
  caught by anybody so far.
- The per-host claims in the module docs are each attested by one session
  only, on one machine, and are what the next person will build on.

**Do not take this file's summaries on trust.** It was written by the session
that wrote the Linux backend, so it is the least reliable witness to that
backend's quality. Read the diff.

### Decisions that deserve challenge

These were judgement calls, not forced moves. Each is argued in the code, and
each could reasonably go the other way:

1. **`pkexec` over udisks2 on Linux** (`linux.rs` module docs). Both were
   measured, both cost an admin password. `pkexec` avoided a hand-rolled
   D-Bus client. Someone may reasonably prefer the desktop-native call and a
   crate, given `AGENTS.md` invites the argument for a D-Bus dependency.
2. **Reservation at the Mount button** — the disk is taken when Mount is
   pressed, not when the machine starts. Windows did this first, Linux copied
   it, macOS still does not (§3). It means a disk can be held with no machine
   running, and the launcher's Unmount is the only thing that frees it.
3. **Essential mounts beyond `/`** (`ESSENTIAL_MOUNTS` in `linux.rs`). A list
   of paths is a heuristic. It was added because a root on ZFS or a live USB
   is served by no block device, and reading only `/` would have offered the
   host its own medium. Is the list right? Is a list the right shape at all?
4. **Refusing everything when the root cannot be traced.** Safe, and it means
   one unrecognised layout disables the feature entirely with only a log line
   to explain it.
5. **`open_device` keeps a disk it took**, so a machine powered off and on
   again does not re-prompt — at the cost of the disk staying held while the
   machine is off. Both hosts do this; it is contestable.
6. **Enumeration cost.** `find_device` is a full enumeration and is called per
   disk, in `mod.rs::reserve_devices` and in the Linux broker. Judged not
   worth optimising for the handful of disks anybody attaches. Check that
   judgement against a machine with many mounts.

### Where the bodies are likely buried

Weighted by where review effort actually pays here:

- **The privileged halves.** Both hosts run a second copy of this binary as
  root/Administrator. `serve_broker_request` in each is reached by a command
  line and must decide for itself what it will open. The Linux one calls
  `super::refuse_if_unusable`; **the Windows one still hand-copies two of its
  three checks and is missing the block-size one** (§4).
- **Descriptor and handle lifetimes.** `O_EXCL` on Linux and the volume locks
  on Windows are both released by closing, and both are duplicated between a
  reservation and a running machine. Trace every error path for one that
  leaks or double-frees.
- **The shared files.** `mod.rs`'s cfg gates, `main.rs`'s per-platform broker
  argument parsing, `bus.rs`'s power-off/on handling, `window.rs`'s
  Mount/Unmount. This is where cross-platform damage would hide.
- **`docs/guide/host-disks.md`** makes claims about all three hosts. Only the
  macOS ones can be checked from a Mac — do check them.

### What the Linux session already found in review

A review of the Linux backend before it landed caught two bugs worth knowing
about, because both are the kind that a single-platform author cannot see and
both have analogues elsewhere:

- Deciding "is this a whole disk?" by whether the sysfs parent directory is
  named `block`. True for SCSI and ATA, false for NVMe namespaces, which hang
  straight off their controller — so every NVMe root traced to nothing, and
  since an untraceable root refuses every disk, the feature was dead on most
  modern machines. **The unit test missed it because it modelled NVMe as if it
  were SCSI.** Treat every fake-layout test in this diff with that in mind.
- A root on ZFS or a live-session overlay is served by no block device, which
  was being read as "no disk here is the host's" — offering the running
  system's own medium. See §3: the macOS equivalent looks unhandled.

Assume more of this kind remains. The Windows backend has had no equivalent
adversarial pass by anybody but its author.

---

## 1. What is done

`src/blockdev/mod.rs` is platform-neutral and owns the safety rules, the
512-byte sector translation, and the types. Each host has one file beside it,
supplying `list_devices` and `open_device`:

| | macOS | Windows | Linux |
|---|---|---|---|
| Enumeration | IOKit `IOMedia`, `Whole` only | `GUID_DEVINTERFACE_DISK`, opened with access 0 | `/sys/block`, no open at all |
| System disk found via | `getfsstat`, mount on `/` | volume → disk extents | `mountinfo` maj:min → sysfs, recursing `slaves/` |
| Privileged open | `authopen` (Apple's broker) | own broker, `runas` + `DuplicateHandle` | own broker, `pkexec` + `SCM_RIGHTS` |
| Unmount | `diskutil unmountDisk` | `FSCTL_LOCK_VOLUME`/`DISMOUNT` | `udisksctl unmount -b` |
| Exclusion lasts | the descriptor | the handle (must be held) | the open file description (`O_EXCL`) |
| `reserve_devices` | **no-op — see §3** | yes | yes |

The user-facing documentation the feature never had is now written:
`docs/guide/host-disks.md`, linked from `docs/index.md` and `docs/myst.yml`,
with `[[host_disk]]` documented in `docs/guide/configuration.md` and
`copperline.example.toml`.

---

## 2. What each brief got wrong

This is the part worth reading. Both previous briefs stated the privilege
model confidently and both were wrong about it in the same direction — they
assumed removable media would be easier than it is.

- **Windows' brief** said removable media grants the interactive user full
  access, so no elevation would be needed for the case the feature exists
  for. False: the permissive grant is on the *volume* object, never on the
  disk, so raw disk access needs Administrator whether the medium is
  removable or not.
- **Linux's brief** said to try the direct open first because "a udev rule
  granting the seat's user access to removable media" might already allow it.
  Measured on Ubuntu 24.04/GNOME 46, an active local session, ordinary user:
  `/dev/sdb` is `root:disk 0660` with **no ACL**, and the card reader's node
  is exactly as closed as the system disk's. `open()` returns `EACCES` for
  every mode. There is no seat-based grant for whole disks. The escalation is
  the ordinary path on a desktop, not a fallback.

The lesson for macOS: **measure the privilege model before designing around
it**, and write down what you measured, on what, and when. `windows.rs` and
`linux.rs` both do this at the top of the file; `macos.rs` does not yet, and
the one macOS claim currently in the tree is a known-broken one (§3).

Other claims, and how they held up:

- "`size` is always in 512-byte units" (Linux) — **true, and verified the
  hard way.** A `scsi_debug` disk with `sector_size=4096` reports
  `logical_block_size = 4096` and `size = 131072` for a 64 MB disk;
  131072 × 512 is the 64 MB it is. Multiplying by the logical block size
  would have called it 512 MB. A disk wrong by a factor of eight still
  enumerates, still opens, and fails only at the far end of the medium.
- "`O_EXCL` on a block device is not advisory" (Linux) — **true**, and
  measured: a second `O_EXCL` open gets `EBUSY`, and `mount` is refused while
  it is held. But note a plain non-exclusive `open()` still succeeds, so it
  excludes mounts and other exclusive users, not all readers.
- "Linux should not need `BlockDevice::holding`" — **true.** The claim lives
  as long as the open file description, so `dup` shares it and the last close
  drops it. Nothing has to hold volume handles the way Windows does.
- "udisks2 is the desktop-native answer" (Linux) — **true but not worth it.**
  `OpenDevice` exists in 2.10.1 and its `options` argument does take extra
  open flags, so `O_EXCL` is reachable; the polkit prompt does appear. But
  its action `org.freedesktop.udisks2.open-device` is `auth_admin_keep` even
  for an active local session, so it costs an administrator password exactly
  as `pkexec` does — and getting there would have meant hand-rolling a D-Bus
  client (SASL, marshalling, `NEGOTIATE_UNIX_FD`) or a new dependency, to
  arrive at the same prompt. `pkexec` was chosen. udisks2 is still used for
  the unmount, where `filesystem-mount` *is* `yes` for an active session and
  so costs no prompt at all.

---

## 3. What macOS still owes

### Check this first: does an APFS container hide the physical system disk?

The Linux backend's worst bug, found in review, was that `/` is usually *not*
served by a partition directly: LVM and LUKS put a synthesized device in
between, and identifying only that device leaves the physical disk underneath
looking like anybody's to take. The fix traces through every mapper layer to
the real disks.

**macOS has the same shape of problem and, reading the code, does not appear
to handle it.** `macos.rs` takes the mount on `/`, reduces its
`f_mntfromname` to a whole-disk name with `whole_disk_of`, and marks the
`IOMedia` with that BSD name `SystemDisk`. On a modern Mac `/` is
`/dev/disk3s1s1`, so that yields **`disk3` — the synthesized APFS container**,
not `disk0`, the physical drive the container is stored on. `disk0` is then
classified merely `Internal`, and `Safety::Internal.openable()` is `true`: it
is hidden from the launcher, but nothing refuses

```sh
copperline --host-disk disk0
```

or a `[[host_disk]] device = "disk0"` in a config file. `launcher.rs`'s
`sample_host_disks` comment already names this risk as the reason internal
disks are hidden — hiding is the mitigation, but it is not the refusal that
rule 1 of the module asks for.

Confirm it before fixing it, in two commands:

```sh
diskutil info -plist / | plutil -p - | grep -i devicenode   # the container slice
diskutil list                                              # disk3 "(synthesized)"
copperline --list-disks                                    # is disk0 marked?
```

If `disk0` comes back without `system disk` on it, that is the bug. The
physical stores of a container are reachable from IOKit by walking the media
object's parents, or out of `diskutil info -plist disk3` as
`APFSPhysicalStores` — and the answer is a *set*, exactly as on Linux, because
a Fusion Drive container spans two physical disks and both of them carry the
running system. The Linux equivalent is `whole_disks_of`, and the tests worth
copying in spirit are `a_root_on_lvm_still_names_its_physical_disk` and
`every_disk_under_a_stacked_root_is_the_system_disk`.

While you are there, two more layouts the Linux review turned up that macOS
should be asked about: a root on a *disk image* (`/` on a synthesized
`diskN` backed by a file) and a machine booted from an external installer
volume, where the medium the system is running from is removable and would
otherwise be offered as an ordinary choice.

### The prompt is attributed to `authopen`, not to Copperline

`src/blockdev/macos.rs` `authorize_open` builds the authorization here so the
dialog is raised by, and named after, Copperline, and hands the external form
to `authopen -extauth`. Its doc comment records that `AuthorizationCreate`
currently **fails from this binary**, so the fallback runs and the user sees a
dialog titled `authopen`, which tells them nothing about who is asking or why.

The suspicion in the comment is that it wants a signed application bundle.
That is the thing to establish: build or fake up a signed bundle, run from
inside it, and see whether `AuthorizationCreate` succeeds. Either fix it or
replace the comment with what you measured, because at the moment the code
carries a workaround for a cause nobody has confirmed.

### `reserve_devices` / `release_device` are still no-ops on macOS

Both Windows and Linux now take the disk **at the launcher's Mount button**,
so the permission dialog belongs to the button somebody just pressed rather
than turning up minutes later behind a machine starting. macOS was
deliberately left alone rather than changed on a guess — it is the one host
whose author is available to decide.

The reasons the other two did it:

- One dialog covers however many disks were ticked, instead of one per disk.
- The disk is already in hand when Run is pressed, so a machine that stops and
  starts again is not a second prompt.
- On Linux it also settles a problem the exclusive open creates: the
  reservation is itself an `O_EXCL` claim, so a machine that opened the node
  afresh would meet it and fail with `EBUSY`. It has to be *lent* — see
  `lend_reserved`, and the test
  `a_reserved_disk_is_lent_to_the_machine_rather_than_reopened`.

macOS has no equivalent forcing function, since `authopen` hands back a
descriptor with no lasting claim on the media. So this is a UX decision, not a
correctness one: is a dialog at Mount better than a dialog at Run? If yes, the
shape to copy is in `linux.rs` (`RESERVED`, `reserve_devices`,
`release_device`, `lend_reserved`) — it is the simpler of the two, because
there is only a descriptor to keep and not a bag of volume handles.

The three `#[cfg(any(windows, target_os = "linux"))]` gates in
`blockdev/mod.rs` are where it plugs in. If macOS adopts it, those gates all
become unconditional and the `#[cfg]`s disappear, which is the tidier end
state and the reason they were written as an `any(...)` list rather than
two separate cfg blocks.

### The module docs do not say what was measured

`windows.rs` and `linux.rs` both open with a "what was measured, and where"
section carrying real command output and a date. `macos.rs` explains the
design well but asserts its privilege model without evidence. Since you will
be measuring `AuthorizationCreate` anyway, write that section while you are
there.

---

## 4. Owed: Windows and Linux say the same things twice

A review of the Linux backend found real duplication between it and
`windows.rs`, and it was deliberately **not** fixed in that session, because
the fix means editing `windows.rs` and nothing on a Linux machine can compile,
let alone run, that file. Doing it blind to save a few lines was the worse
trade. It is genuinely worth doing from a machine that can build both, or with
a Windows box to hand:

- **`broker_argument` / `parse_broker_argument`** (`linux.rs`, `windows.rs`)
  are byte-identical, as is the `ok` / `error <message>` preamble of
  `parse_answer`. That is the shared half of one protocol, described twice.
  Both backends carry their own unit test for it, so a Linux CI run proves
  nothing about the Windows copy. Note while you are there that the parse uses
  `rsplit_once(':')`, so an identifier containing a colon would split wrongly
  on both.
- **The whole reservation state machine** — the `RESERVED` static, the
  poison-recovering accessor, the stale-entry eviction, the
  already-held-on-the-same-terms skip, the release-then-retake on changed
  terms, and `release_device` — is the same ninety lines in both files,
  comments included. Only *taking* the disk genuinely differs. The natural
  shape is a reservation table in `mod.rs` holding a backend-supplied handle,
  with each backend contributing only "take" and "lend". If macOS adopts
  reservation (§3), do this first and get three users out of one
  implementation rather than a third copy.

One duplication that was fixed, and is worth knowing about because the same
trap exists on Windows: the Linux privileged half had grown its own hand-copied
likeness of the safety rules, and had silently lost `refuse_if_unusable`'s
block-size check in the copying — so a 520-byte-block disk was refused for an
ordinary user and accepted by the half running as root. It now calls
`super::refuse_if_unusable`. **`windows.rs`'s `broker_open` still has the
hand-copied pair** (`windows.rs` ~line 1359) and has the same gap.

## 5. What is not verified anywhere

Be honest about these rather than assuming they work.

- **The GUI power-off / power-on cycle on Linux.**
  `Bus::release_host_disks` and `Bus::attach_host_disks` are only reachable
  from a windowed session (`src/video/window.rs`), so a headless run cannot
  exercise them. The Linux-specific mechanic underneath — that dropping the
  `BlockDevice` releases the `O_EXCL` claim, and that a reservation survives
  the machine dropping its copy — *is* covered by
  `a_reserved_disk_is_lent_to_the_machine_rather_than_reopened`. The
  window-level path is not. Same gap on macOS.
- **Any multi-disk attach**, on any host. Every test so far has used one disk.
  The "one prompt for several disks" path (`open_through_broker` with more
  than one entry, and `parse_answer` returning several) has only unit-test
  coverage of the wire format, not a real two-card run.
- **SCSI attachment of a real disk.** Everything tested went on IDE
  (`ide-master`). `attach = "scsi0"` is wired but unexercised against real
  media.
- **A write to a real Amiga card on Linux.** The read path is proven against
  the maintainer's card (see §6); the write path is proven only against a
  disposable `scsi_debug` disk, because writing to somebody's only copy of an
  Amiga system disk needs their say-so.

---

## 6. How the Linux backend was verified, so you can do the same

The read-only attach really is the most informative single test, exactly as
the Windows brief claimed. Against a real PFS3 card in a USB reader:

```sh
./target/release/copperline --model A1200 --fast 8M KICK31.ROM \
  --host-disk-read-only sdb --noaudio --screenshot-after 40 /tmp/ro.png
```

The screenshot showed the guest's own `PFS-III Error Requester: Write 2 Error
173 on block 1026146`, and the log showed `blockdev` refusing a write to
sector 1026146 — the same block, which is what ties the two halves together.
That one picture proves enumeration, the open, the sector translation, and the
RDB parsing at once.

For writes without risking real media, Linux's disposable disk is
`scsi_debug`, not a loop device — loop devices are deliberately excluded from
enumeration, so one cannot be reached by name. The recipe is on the doc
comment of `blockdev::tests::device_round_trip`, alongside the macOS
`hdiutil` and Windows VHD ones. `sector_size=4096` is the useful knob: it is
the only way most machines have of exercising the read-modify-write path for
media whose blocks are larger than a guest sector, and it is worth running on
macOS too.

---

## 7. When you are done

Push to the fork (`origin`, `hobbo91/Copperline`). **Never open a pull
request** — that is the maintainer's step, always. Delete this file.

Report the review as findings, not as a rewrite: the two backends work on
their own hardware and each was verified there, so a change made from a Mac to
code that cannot be built or run from a Mac needs a stronger reason than
taste. Where a finding cannot be fixed safely from this host, say so and leave
it written down — that is how the last two sessions handed on the things they
could not close, and it is why §4 and §5 exist.
