# Device-level fault injection (`disk.fault.*`)

Copperline can inject deterministic, seeded faults into any image-backed
block device: dropped, torn, and reordered writes; ignored flushes;
stale and corrupted reads; media errors; delays; and simulated
power-cuts. This models the storage medium and bus misbehaving, not the
guest -- a test exercises a filesystem's or driver's actual recovery
code against the same byte-for-byte reproducible fault sequence on every
run, from a `copperline-ctl` script or an MCP-driving agent.

This is a separate namespace from `fault.inject` (the CPU-bus fault
family): `disk.fault.*` is medium/device behavior, never a guest-visible
bus error.

## Targets

A target names one device/unit:

```json
{"kind": "copperhf", "unit": 0}
{"kind": "pcmcia"}
{"kind": "ide", "unit": 0}
{"kind": "scsi", "unit": 0}
{"kind": "floppy", "drive": 0}
{"kind": "cd"}
```

`unit`/`drive` default to 0. `copperhf`/`pcmcia`/`ide`/`scsi` resolve to
whichever controller the machine profile actually has configured (Gayle,
A4000 IDE, a Zorro IDE board such as LIDE, A2091/A3000-internal/A4091
SCSI); `cd` resolves to the machine's CD drive, whichever it is (SCSI,
ATAPI, CD32's Akiko, or CDTV).

`floppy` only works on a plain (non-extended) ADF: extended ADF and raw
MFM images have no logical sector grid for a `lba` to name, and
`disk.fault.set` rejects them with a specific error rather than silently
installing a policy the read/write hooks never consult.

`cd` is read-only medium, so only `corrupt`/`error`/`delay`/`cut` ever
actually fire on it; a rule with `"op": "write"` is schema-valid there
but never matches.

## Policies and rules

A policy is a named, ordered list of rules:

```json
{
  "on_clear": "release",
  "rules": [
    {
      "op": "write",
      "lba": {"from": 100, "to": 200},
      "probability": 0.01,
      "after": {"writes": 50},
      "times": 10,
      "continue": false,
      "action": "drop"
    }
  ]
}
```

Several policies may be active on one target at once; they are
evaluated in installation order, and within a policy, rules are
evaluated in list order. For a given request, the first matching rule
whose `probability` test passes applies its action and stops
evaluation, unless it sets `"continue": true` (rule fields below), in
which case later rules may also match and apply on top of it. A write's
fate, once decided, wins over anything a later continuing rule tries to
do to the data: `"drop"` is final -- a later `"corrupt"`/`"tear"` cannot
make a dropped write land anyway, and a later `"drop"` discards whatever
an earlier `"reorder"` already queued -- and `"reorder"` defers the
decision -- a later `"corrupt"` mutates the queued copy in place rather
than also landing an independent write, and a later `"error"` removes
the queued copy before failing, since a write the guest sees fail never
left anything in a real drive's cache for a later flush to resurrect.

Rule fields (all but `action` optional):

- `op`: `"read"` / `"write"` / `"flush"` / `"trim"` / `"any"` (default).
  `trim` is accepted for forward compatibility but currently inert --
  nothing in the emulated IDE/SCSI/copperhf controllers issues a TRIM/
  UNMAP request yet, so a `trim` rule never actually fires.
- `lba`: `{"from": N, "to": M}`, inclusive. Matches if any sector of the
  request overlaps the range. Omitted matches every sector.
- `probability`: `0.0..=1.0` (default `1.0`). Drawn from a generator
  keyed on the policy's seed and the target's request sequence number --
  never wall-clock time or host state, so the same seed and guest input
  schedule fault identically every run.
- `after`: `{"requests"|"reads"|"writes"|"flushes": N}`. The rule stays
  dormant until `N` matching requests have been seen *since this rule
  was installed* (not since the target was first attached, and not
  reset by `disk.fault.reset_stats` once the gate has fired).
- `times`: a request budget; the rule stops matching once it has fired
  this many times.
- `continue` (default `false`): let evaluation continue to later rules
  after this one matches, instead of stopping here.
- `action`: see below.

### Actions

- `"drop"`: acknowledge the write; nothing reaches the image.
- `"tear"`: commit only part of the write, modelling a power cut
  mid-request. `"at": "sector"` (default) with `"n"`: keep the first `n`
  whole sectors (IDE WRITE MULTIPLE, SCSI WRITE(10)/(12)/(16), and a
  copperhf multi-sector job all commit a whole multi-sector request as
  one `disk.fault` request, so `n` can span more than one sector of it);
  `"at": "bytes"` with `"n"`: keep the first `n` bytes of the whole
  request. The untouched tail -- whatever is left after `n` -- is
  spliced from what the medium already held, not left as whatever the
  guest's buffer happened to contain.
- `"reorder"`: hold the write in a bounded queue (`"window"`, `1..=64`)
  instead of committing it immediately; a read of a held sector still
  sees it, as a write cache would. A flush (or `disk.fault.clear`)
  releases the whole queue, in a seeded random order, not FIFO -- if the
  same sector was held more than once, only the newest write for that
  sector survives the release. A window overflow evicts one held write
  at random and commits it for real immediately, rather than discarding
  it.
- `"ignore"` (flush only): acknowledge `CMD_UPDATE`/SYNCHRONIZE CACHE
  without actually releasing any held reorder queue. `"then": "hold"`
  (default) keeps the queues for a later flush to release; `"then":
  "drop"` discards them now.
- `"stale"` (write only): after this write, the next `"count"` (default
  1) reads of the same sector see its *previous* content, then revert to
  the real, already-committed data.
- `"corrupt"`: flips `"bits"` (default 1, capped at 65536) random bits in
  the sector.
- `"error"`: fail the request with `"code"`: `"media"` / `"timeout"` /
  `"not_ready"` / `"write_protect"` / `"aborted"`. Each controller maps
  the code to its own native representation -- IDE's error register
  (`ERR_UNC` for `media`, `ERR_ABRT` for everything else, since real ATA
  has no dedicated "not ready"/write-protect bit), SCSI's sense key
  (MEDIUM ERROR / ABORTED COMMAND / NOT READY / DATA PROTECT), copperhf's
  `io_Error` (`TDERR_WriteProt` for `write_protect`, a generic failure
  otherwise).
- `"delay"`: accumulates into `disk.fault.stats`' `delayed_ms`. On a
  `scsi` target, this also adds the equivalent extra colour-clock delay
  to the command's completion -- on every other target it currently
  only shows up in the stats, with no added latency yet.
- `"cut"`: simulate a power cut. Every later request on this target
  fails `not_ready` until an explicit `disk.fault.clear`, even across a
  `machine.reset` -- a reset must not quietly un-cut the drive. Discards
  every policy's held reorder writes and armed `stale` entries on this
  target (modelling the volatile write cache being lost), so a later
  clear cannot replay data that should have been lost in the cut.

`on_clear` (policy-level, default `"release"`): what happens to a held
reorder queue when this policy is cleared, or replaced by a `set` under
the same id -- `"release"` replays the held writes onto the image in
order (same effect as a flush), `"drop"` discards them.

## Control-protocol methods

- `disk.fault.set {"target": {...}, "policy": {...}, "seed": N, "id": "..."}`:
  install a policy, returning its id (auto-assigned if omitted). With
  `id` given and already installed, replaces that policy in place
  (releasing its own held writes first, per `on_clear`).
- `disk.fault.clear {"id": "...", "target": {...}}`: clear by id (target
  required -- ids are unique only within a target, so clearing by id
  alone across every target would clear unrelated policies that happen
  to share that id), every policy on a target, or every policy on every
  target with neither given.
- `disk.fault.list {"target": {...}}`: installed policies (optionally
  filtered to one target), each with its rules' `hits` counts.
- `disk.fault.stats {"target": {...}}` / `disk.fault.reset_stats
  {"target": {...}}`: cumulative per-target counts (requests, by op,
  dropped/torn/reordered/flush_ignored/stale_served/corrupted/errored,
  total injected delay, `cut` state) and resetting them. `reset_stats`
  does not affect installed policies, their rule hit counts, or an
  `after` gate that has already fired.

See the [control protocol reference](control.md) for the exact
parameter/reply shapes and error cases.

## Determinism

Every random choice (probability tests, reorder release order, corrupt
bit positions, auto-assigned ids) is drawn from a generator keyed on the
policy's own seed and the target's request sequence number, never on
wall-clock time or host state. Two runs with the same image, the same
guest input schedule, and the same seed fault identically, so a flaky
guest-side recovery bug reproduces exactly, every time.
