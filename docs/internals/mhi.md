# The MHI decoder board: mailbox register protocol

This page is the **contract**, not an implementation note: it specifies the
register-level protocol of Copperline's virtual MHI (Music Hardware
Interface) MPEG audio decoder board, precisely enough that the host board
(`src/mhi.rs`) and the guest library (`guest/mhi/`) can be implemented
independently against it and interoperate. Where this document and either
implementation disagree, this document wins -- fix the implementation, not
the spec, unless the spec itself is being deliberately revised (see
[Versioning](#versioning)).

The protocol is deliberately **bus-agnostic**: every offset below is
relative to the start of the board's autoconfigured window, and nothing in
the register semantics depends on being read by Copperline specifically. See
[Porting to another emulator](#porting-to-another-emulator) at the end.

MHI itself -- `MHIAllocDecoder`/`MHIQueueBuffer`/`MHIGetStatus`/etc., the
Amiga-side API AmigaAMP and other players call -- is not this board's wire
format. MHI is a **library API**, implemented by `mhi_copperline.library`
(the guest side of this project); this board never sees an `MHIP_*` or
`MHIQ_*` constant, a decoder handle, or a signal mask. See
[The MHI-API/board split](#the-mhi-api-board-split) for exactly what stays
guest-side and why.

## Zorro identity

- Zorro II slave, one 64 KiB register window, no autoboot ROM (`romtype`
  not present -- the board never appears in the Exec free-memory list and
  never autoboots).
- Manufacturer **5192** / `0x1448` (the Copperline manufacturer ID; see
  [](../zorro)'s [manufacturer ID table](../zorro.md#the-copperline-manufacturer-id)),
  product **7** -- the next free product number after HostSocket (6).
- The board is **not** a bus master at the guest-visible level: nothing in
  this protocol lets the guest program a live DMA pointer the board reads
  asynchronously. The guest only ever hands the board a static
  address+length pair per descriptor (see
  [Descriptor queue and doorbell](#descriptor-queue-and-doorbell)); when the board consumes it,
  it is Copperline's own host-side implementation detail that the copy
  happens through `DeviceHost`'s DMA accessors, exactly as the A2091
  SCSI controller's data phase does. Another emulator could just as
  validly implement "consume the descriptor" by memcpy'ing from its own
  guest RAM model directly -- the protocol only promises *what* gets read
  (a byte range of 24-bit Amiga address space) and *when* (in emulated
  time, at the decoded-audio rate -- see [Determinism and timing](#determinism-and-timing)),
  never *how*.

## Register map

All registers are 16-bit and word-aligned; addresses are offsets within the
64 KiB window. `RO` = guest may only read; `WO` = guest may only write
(reads of a `WO` register return 0); `RW` = both. Offsets not listed are
**reserved**: they read as `0x0000` and silently discard writes, in every
protocol version, so a guest built against a newer spec than the board
implements degrades safely on old fields rather than reading garbage. See
[Access size and alignment](#access-size-and-alignment) for the rules
`move.b`/`move.w` accesses follow.

| Offset | Name | Width | Access | Reset value | Group |
|---|---|---|---|---|---|
| `0x00` | `VERSION` | word | RO | `0x0001` | Capability/version |
| `0x02` | `CAPS` | word | RO | board-fixed | Capability/version |
| `0x04` | `STATUS` | word | RO | `0x0000` (STOPPED) | Status/control |
| `0x06` | `CONTROL` | word | WO | -- | Status/control |
| `0x08` | `INTREQ` | word | RW | `0x0000` | Interrupts |
| `0x0A` | `INTENA` | word | RW | `0x0000` | Interrupts |
| `0x0C` | `QUEUE_DEPTH` | word | RO | board-fixed (`16`) | Descriptor queue |
| `0x0E` | `QUEUE_COUNT` | word | RO | `0x0000` | Descriptor queue |
| `0x10` | `DESC_ADDR_HI` | word | WO | -- | Descriptor queue |
| `0x12` | `DESC_ADDR_LO` | word | WO | -- | Descriptor queue |
| `0x14` | `DESC_LEN_HI` | word | WO | -- | Descriptor queue |
| `0x16` | `DESC_LEN_LO` | word | WO | -- | Descriptor queue |
| `0x18` | `DOORBELL` | word | WO | -- | Descriptor queue |
| `0x1A` | `COMPLETED_COUNT` | word | RO | `0x0000` | Completion/reclaim |
| `0x1C` | `PARAM_SELECT` | word | RW | `0x0000` | Param latches |
| `0x1E` | `PARAM_VALUE` | word | RW | per-param default | Param latches |
| `0x20`-`0xFFFF` | *reserved* | -- | -- | `0x0000` | -- |

The window is 64 KiB (the smallest legal Zorro II size) even though only
32 bytes of it currently decode to anything; the rest is headroom for
future protocol versions (M3's seek support, a wider param space, etc.)
without moving the board to a bigger window, which would change its
autoconfig identity.

### Capability/version registers

- **`VERSION`** (`0x00`, RO) -- the register-protocol version this board
  implements, currently `1`. A guest library reads this once at
  `FindConfigDev` time and refuses to drive a board whose major protocol
  it does not understand (see [Versioning](#versioning)).
- **`CAPS`** (`0x02`, RO) -- a bitmask of the MPEG formats and bitrate
  modes this board's decoder accepts. Bit layout:

  | Bit | Meaning |
  |---|---|
  | 0 | MPEG-1 supported |
  | 1 | MPEG-2 supported |
  | 2 | MPEG-2.5 supported |
  | 3 | Layer III supported |
  | 4 | CBR (constant bitrate) supported |
  | 5 | VBR accepted as input (decodes correctly; no seek support) |
  | 6-15 | reserved, read as 0 |

  M1-M2 sets bits 0-5 (MPEG-1/2/2.5, Layer III, CBR, VBR-no-seek) and no
  others -- Layer I/II are not implemented, so bits for them do not exist
  in this version; a future version that adds them would need a `CAPS`
  bit for each, which is exactly the kind of change that bumps
  `VERSION`. This register is what the guest library's `MHIQuery` handler
  consults for `MHIQ_MPEG1`/`MHIQ_MPEG2`/`MHIQ_MPEG25`/`MHIQ_LAYER3`/
  `MHIQ_VARIABLE_BITRATE` -- it is genuine hardware-reported capability,
  unlike decoder identity (below).

  Decoder identity strings (`MHIQ_DECODER_NAME`, `MHIQ_DECODER_VERSION`,
  `MHIQ_AUTHOR`, `MHIQ_CAPABILITIES`'s MIME-type string,
  `MHIQ_IS_HARDWARE`/`MHIQ_IS_68K`/`MHIQ_IS_PPC`) have **no register** --
  they identify the *guest library*, not the board, and are answered
  entirely from constants compiled into `mhi_copperline.library`. See
  [The MHI-API/board split](#the-mhi-api-board-split).

### Status and control

- **`STATUS`** (`0x04`, RO) -- the board's current transport state:

  | Value | State |
  |---|---|
  | `0` | `STOPPED` |
  | `1` | `PLAYING` |
  | `2` | `PAUSED` |
  | `3` | `OUT_OF_DATA` |

  These are the board's own state codes, not `MHIF_*` values (the official
  `libraries/mhi.h` defines `MHIF_PLAYING`=0, `MHIF_STOPPED`=1,
  `MHIF_OUT_OF_DATA`=2, `MHIF_PAUSED`=3 -- a different assignment and a
  different order from this register's `STOPPED`=0/`PLAYING`=1/`PAUSED`=2/
  `OUT_OF_DATA`=3, deliberately, so a guest that forgets to translate
  fails loudly instead of silently reporting the wrong status half the
  time). `mhi_copperline.library`'s `MHIGetStatus` maps this register's
  value to the matching `MHIF_*` constant. Reading `STATUS` has no side
  effect and may be polled freely at any time, from any context --
  `MHIGetStatus` is documented as callable at will, so nothing here may
  depend on how recently it was last read.

- **`CONTROL`** (`0x06`, WO) -- a one-shot command register; each write is
  interpreted as a command and takes effect immediately (there is no
  latency or acknowledgement -- by the time the `move.w` retires, `STATUS`
  already reflects the new state):

  | Value | Command | Effect |
  |---|---|---|
  | `0` | (no-op) | Ignored; reserved so an accidental zero write is inert |
  | `1` | `PLAY` | `STOPPED`/`PAUSED` &rarr; `PLAYING`. From `STOPPED`, playback (and bitstream consumption) starts at the head of the queue if non-empty, or the board immediately reports `OUT_OF_DATA` if the queue is empty. From `PAUSED`, resumes exactly where it left off. No-op from `PLAYING`/`OUT_OF_DATA`. |
  | `2` | `PAUSE` | `PLAYING` &rarr; `PAUSED`. Halts bitstream consumption and audio output; the queue is untouched and the decoder's cross-frame state is preserved, so `PLAY` resumes mid-stream with no audible gap or restart. No-op from any other state (in particular, `PAUSE` from `OUT_OF_DATA` or `STOPPED` does nothing -- MHI's own `MHIPause` is only meaningful while playing). |
  | `3` | `STOP` | Any state &rarr; `STOPPED`. **Discards every queued descriptor**, completed or not yet started (`QUEUE_COUNT` &rarr; 0), and resets `COMPLETED_COUNT` to 0. This matches `MHIStop`'s documented semantics exactly ("stop all decoding... all buffers in the queue are flushed") -- the guest library performs no separate flush step. |

  Values above `3` are reserved and behave as the no-op.

### Interrupts

INT2, level-sensitive: the line is asserted whenever `(INTREQ & INTENA) !=
0`. Both registers share one bit layout:

| Bit | Meaning | Raised when |
|---|---|---|
| 0 | `BUFFER_DONE` | A descriptor finished playing out and was reclaimed (`COMPLETED_COUNT` advanced) |
| 1 | `OUT_OF_DATA` | `STATUS` transitioned into `OUT_OF_DATA` |
| 2 | `QUEUE_OVERFLOW` | A `DOORBELL` write was dropped because the queue was full (diagnostic; not part of MHI's own semantics, but useful to a guest that mis-tracks `QUEUE_COUNT`) |
| 3-15 | reserved | never set in this version |

- **`INTREQ`** (`0x08`, RW) -- the pending-interrupt bits. **Ack protocol
  is write-1-to-clear**: writing a word to `INTREQ` clears exactly the
  bits that are `1` in the value written and leaves the rest untouched;
  writing `0` to a bit never sets it (only the board itself sets bits, on
  the events above). This is a deliberate departure from Toccata's
  read-acknowledges-on-status-read pattern: Toccata's status register
  exists only on the interrupt-service path, so conflating "read status"
  with "ack" is harmless there. This board's `STATUS` is polled from
  contexts that have nothing to do with servicing INT2 (`MHIGetStatus`
  can be called at any time, per its own docs), so acknowledging
  interrupts as a side effect of an unrelated status read would risk
  losing a completion notification a client never meant to consume yet.
  Write-1-to-clear keeps the INT2 server's job mechanical and exactly
  right: read `INTREQ`, act on the set bits, write back the same value to
  ack precisely the bits handled, nothing else.
- **`INTENA`** (`0x0A`, RW) -- enable mask, same bit layout, reset to
  `0x0000` (fully masked) like every other Zorro board's interrupt enable
  on power-on/reset. The guest library must set the bits it wants before
  it can expect INT2 to fire.

### Descriptor queue and doorbell

The board holds a FIFO queue of up to `QUEUE_DEPTH` descriptors, each an
(Amiga address, length) pair identifying a buffer of encoded MPEG
bitstream in Amiga memory that the guest handed over via `MHIQueueBuffer`.
**16 descriptors deep** (`QUEUE_DEPTH` reads back the constant `16`) --
generous enough that AmigaAMP-style double/triple buffering never
back-pressures on the board even with small buffers, without pinning an
unreasonable amount of encoded audio in flight (at typical 32 KiB MP3
buffers, 16 deep is 512 KiB of staged bitstream, well within a stock
machine's chip+fast memory).

To enqueue a descriptor:

1. Write the Amiga source address, high word then low word (either order
   is accepted; both are independent latches -- see
   [Access size and alignment](#access-size-and-alignment)), to
   `DESC_ADDR_HI`/`DESC_ADDR_LO`.
2. Write the buffer length, high word then low word, to
   `DESC_LEN_HI`/`DESC_LEN_LO`. Length is a full 32-bit byte count (Zorro
   II's 24-bit address space bounds it further in practice, but the
   register pair itself does not truncate).
3. Write any value to `DOORBELL`. This is what actually commits the
   staged address+length as a new descriptor at the tail of the queue --
   steps 1-2 only load latches that `DOORBELL` reads at the moment it is
   written; nothing is queued until the doorbell write happens.

If the queue is full (`QUEUE_COUNT == QUEUE_DEPTH`) when `DOORBELL` is
written, the descriptor is **dropped** (the staged address/length are
left as-is, so the guest may simply retry once space frees) and
`INTREQ.QUEUE_OVERFLOW` is set. This mirrors `MHIQueueBuffer`'s own
contract: it returns `FALSE` when a buffer cannot be queued, and the
guest library is expected to poll room before calling it via
`QUEUE_COUNT < QUEUE_DEPTH`, exactly as `MHIGetEmpty` polls for reclaimed
buffers. `QUEUE_OVERFLOW` exists as a diagnostic for a guest bug (racing
the check), not as a code path production drivers should hit.

- **`QUEUE_DEPTH`** (`0x0C`, RO) -- the constant `16`. Read once; it never
  changes at runtime, but is a register (not baked into the spec as a
  bare number) so a future board revision could legally offer a deeper
  queue and have a conforming guest library adapt without a rebuild.
- **`QUEUE_COUNT`** (`0x0E`, RO) -- the number of descriptors currently
  outstanding: enqueued but not yet fully consumed. Incremented by a
  successful `DOORBELL` write, decremented when a descriptor finishes
  playing out (the same instant `COMPLETED_COUNT` advances and
  `INTREQ.BUFFER_DONE` is set). Reset to `0` by `CONTROL=STOP` or a
  hardware reset.

### Completion and reclaim

The board does not hand back buffer pointers or a per-descriptor
ring index -- descriptors complete strictly in FIFO order (the guest
library already knows, in its own client-side queue mirroring
`MHIQueueBuffer`'s call order, which buffer is next), so a single
monotonic counter is sufficient and simpler than a ring of completion
records:

- **`COMPLETED_COUNT`** (`0x1A`, RO) -- a free-running counter,
  incremented by one each time a descriptor finishes playing out (see
  [Determinism and timing](#determinism-and-timing)), wrapping modulo
  65536. Reading it has no side effect. The guest library keeps its own
  local copy of the last-observed value and, on each `BUFFER_DONE`
  interrupt (or when polling `MHIGetEmpty` directly), computes the delta
  with wraparound-safe 16-bit subtraction (`(u16)(now - last)`) to learn
  how many buffers to pop from its own client-side queue and return via
  `MHIGetEmpty` -- the same idiom other in-tree boards use for
  free-running hardware counters. `CONTROL=STOP` resets it to `0`
  alongside `QUEUE_COUNT`; a guest observing a `STOP` (its own or another
  client's, if the board is ever shared -- it isn't in M1-M2's single
  `MHIAllocDecoder` model) must resynchronize its local counter to `0`
  rather than compute a delta across the reset.

### Param latches

MHI's tone/volume/panning controls (`MHISetParam`) are modeled as a
two-register **select/value mailbox** rather than one register per
parameter -- MHI defines a long tail of them (`MHIP_BAND1`..`MHIP_BAND10`
for a 10-band EQ, on top of volume/panning/bass/mid/treble/crossmixing/
prefactor), and a fixed one-register-per-param layout would either waste
window space up front or need a protocol bump the day a client asks for
one more band. The mailbox pattern keeps the register count fixed
regardless of how many parameters the guest library ends up exposing.

- **`PARAM_SELECT`** (`0x1C`, RW) -- the board-defined parameter index to
  address. This is **not** an `MHIP_*` value (see
  [The MHI-API/board split](#the-mhi-api-board-split)); the guest library
  translates `MHISetParam`'s `MHIP_*` constant to the board's own index:

  | Index | Parameter | Range | Default |
  |---|---|---|---|
  | `0` | Volume | 0-100 | 100 |
  | `1` | Panning | 0-100 (50 = centre) | 50 |
  | `2` | Bass | 0-100 (50 = flat) | 50 |
  | `3` | Mid | 0-100 (50 = flat) | 50 |
  | `4` | Treble | 0-100 (50 = flat) | 50 |
  | `5` | Crossmixing | 0-100 (0 = none) | 0 |
  | `6` | Prefactor | 0-100 (50 = unity) | 50 |

  Indices `7`-`65535` are reserved (unimplemented in this version; a
  future version adding the 5/10-band EQ params would assign them here
  under a `VERSION` bump). Selecting a reserved index and then reading or
  writing `PARAM_VALUE` is well-defined but inert: reads return `0`,
  writes are latched but never consulted by anything.
- **`PARAM_VALUE`** (`0x1E`, RW) -- write: latches the given value against
  whichever index `PARAM_SELECT` currently holds (out-of-range values for
  a 0-100 parameter are clamped by the board, not rejected). Read:
  returns the currently latched value for that index, so the guest
  library can implement a param readback path (MHI itself has no
  `MHIGetParam`, but a latch that cannot be read back would make the
  guest library's own bookkeeping the only source of truth, which this
  avoids).

  **M1-M2 scope**: writes latch the value and it reads back correctly;
  nothing about decoded audio changes yet. **M4** connects these latches
  to the actual PCM path (volume/pan as post-decode gain and stereo
  placement, tone controls as a simple filter bank) -- this is why the
  register shape is fixed now: M4 changes what a latch *does*, never
  where it lives or how it's addressed, so no guest library rebuild is
  needed when M4 lands.

## Access size and alignment

The window behaves like a 16-bit peripheral: every register above is a
plain word (16-bit) register at an even offset, and the board only ever
decodes even addresses.

- **Word access** (`move.w`) is the primary and recommended access size,
  and is what the guest library uses exclusively.
- **Byte access** (`move.b`) is honored for compatibility: the high byte
  of a register is at its listed offset, the low byte at offset+1
  (big-endian, matching 68k byte order), and a byte write only changes
  that half of the register -- there is no special latch-on-low-byte
  behaviour the way the 32-bit descriptor fields latch on a separate
  register (see below). A `WO` register's byte-write value is simply
  discarded like any other write to it.
- **Longword access** (`move.l`) is **not supported** and its behaviour
  is undefined at the protocol level -- the bus is 16 bits wide, so a
  32-bit access does not atomically span two registers the way it would
  on a genuinely 32-bit-decoded peripheral. The guest library must never
  issue one; two `move.w`s are always used instead, including for the
  32-bit `DESC_ADDR_*`/`DESC_LEN_*` pairs (see
  [Descriptor queue and doorbell](#descriptor-queue-and-doorbell)), which are
  deliberately specified as two independent word registers rather than
  one 32-bit register precisely so this never comes up.
- Reads of a **`WO`** register return `0`; writes to an **`RO`** register
  are silently discarded. Neither is an error condition -- there is no
  fault or diagnostic bit for it, matching the rest of Copperline's
  Zorro-board conventions (e.g. Toccata's undecoded-port behaviour, see
  [](toccata)).
- **Reserved offsets** (`0x20` and above) read as `0x0000` and discard
  writes, in every protocol version -- see the note at the top of
  [Register map](#register-map).
- No register access has a read side effect in this protocol (contrast
  Toccata, where reading its status register acknowledges pending
  interrupt bits): every ack is the explicit `INTREQ` write-1-to-clear
  described above, and every other register is freely, repeatedly
  pollable.

## Out-of-data semantics

`STATUS` transitions to `OUT_OF_DATA` (`3`) exactly when playback drains
the queue: the board is in `PLAYING`, the last outstanding descriptor
finishes playing out, and `QUEUE_COUNT` reaches `0` with nothing new
enqueued in the same instant. That transition raises **both**
`INTREQ.BUFFER_DONE` (for the descriptor that just completed) and
`INTREQ.OUT_OF_DATA` together -- a guest servicing only `BUFFER_DONE` and
checking `STATUS` afterward, or one servicing both bits, both see a
consistent picture.

While `OUT_OF_DATA`, the board is not stopped -- it matches MHI's own
description of the state ("run out of data but still waiting for more"):
decoder cross-frame state is preserved exactly as `PAUSE` preserves it,
and audio output is silence in the meantime (no repeat-last-sample
holdover the way Toccata's FIFO underrun behaves -- MPEG frames are not
individually meaningful to hold on to). The moment a `DOORBELL` write
successfully enqueues a new descriptor while `STATUS == OUT_OF_DATA`
(`QUEUE_COUNT` goes from `0` to `1`), the board resumes playback
automatically and `STATUS` returns to `PLAYING` with no `CONTROL=PLAY`
required -- a guest need not notice `OUT_OF_DATA` at all if it always
keeps the queue fed; it exists for the client that briefly runs dry (the
common case an MHI-aware application like AmigaAMP polls for, e.g. to
know a track has finished once no more data is coming).

`CONTROL=STOP` from `OUT_OF_DATA` behaves exactly as from any other
state: transition to `STOPPED` (a no-op on the already-empty queue).

## Determinism and timing

The board consumes a descriptor's bitstream at the **decoded audio's own
emulated-time rate**, the same principle as every other in-tree audio
device (see [](audio)'s determinism section and [](toccata)'s "mixer
cadence" for the worked example): a decoded MPEG frame (1152 PCM samples
at the stream's sample rate) is not considered "played out" -- and its
bytes are not considered consumed from the descriptor, and
`COMPLETED_COUNT`/`INTREQ` do not advance -- until that many emulated
sample-clock ticks have elapsed, exactly as if the samples were being
produced for playback in real time. A descriptor's completion event and
any INT2 assertion it causes are therefore also emulated-time events, not
host-wall-clock ones: they fire the tick a frame's worth of emulated
sample time has elapsed, identically whether the host machine runs in
real time, is throttled, or is warped as fast as the host CPU allows.
This is what makes a scripted scenario against this board reproducible
byte-for-byte and makes `--audio-wav`/stem captures of its output
deterministic across runs, the same guarantee every other Copperline
audio path already gives.

## The MHI-API/board split

This board's registers are deliberately **innocent of MHI's own
numbering** -- no `MHIF_*`, `MHIP_*`, or `MHIQ_*` constant appears in this
protocol, and the split is intentional, not an oversight:

| Concern | Lives in |
|---|---|
| Decoder identity strings (`MHIQ_DECODER_NAME`/`_VERSION`, `MHIQ_AUTHOR`, the `MHIQ_CAPABILITIES` MIME-type string) | Guest library (`guest/mhi/`) -- compile-time constants; they describe the library, not the board |
| `MHIQ_IS_HARDWARE`/`_IS_68K`/`_IS_PPC` | Guest library -- static answers (this is a real register-mailbox device the library talks to over the Zorro bus, so `MHIQ_IS_HARDWARE` answers true; it runs no 68k/PPC code of its own, so both processor queries answer false) |
| MPEG version/layer/bitrate-mode support (`MHIQ_MPEG1`/`_MPEG2`/`_MPEG25`, `MHIQ_LAYER3`, `MHIQ_VARIABLE_BITRATE`) | `CAPS` register (`0x02`) -- genuinely board-reported, since a future board revision's decoder could differ |
| `MHIQ_JOINT_STEREO` | Guest library -- fixed `MHIF_SUPPORTED`; decoding joint-stereo Layer III is inherent to any conforming decoder, not a distinct board capability worth its own `CAPS` bit |
| Tone/volume/output query flags (`MHIQ_VOLUME_CONTROL`, `MHIQ_PANNING_CONTROL`, `MHIQ_BASS_CONTROL`, `MHIQ_TREBLE_CONTROL`, `MHIQ_MID_CONTROL`, `MHIQ_PREFACTOR_CONTROL`, `MHIQ_CROSSMIXING`, `MHIQ_5_BAND_EQ`, `MHIQ_10_BAND_EQ`) | Guest library -- fixed `MHIF_SUPPORTED` for the params this board's [param latch](#param-latches) table defines (indices `0`-`6`: volume, panning, bass, mid, treble, crossmixing, prefactor), `MHIF_UNSUPPORTED` for ones it does not yet (the 5/10-band EQ, until a later `VERSION` adds `MHIP_MIDBASS`/`MHIP_MIDHIGH`/`MHIP_BAND1`-`MHIP_BAND10` equivalents at reserved indices `7`+) |
| Decoder handle, client task pointer, signal mask (`MHIAllocDecoder`/`MHIFreeDecoder`) | Guest library only -- entirely a host-side (Amiga-side) bookkeeping concept; the board has no notion of "a handle" and, in this M1-M2 scope, serves exactly one client at a time |
| Transport (`MHIPlay`/`MHIStop`/`MHIPause`), status (`MHIGetStatus`), queueing (`MHIQueueBuffer`/`MHIGetEmpty`), params (`MHISetParam`) | Guest library translates 1:1 to/from this board's `CONTROL`/`STATUS`/descriptor-queue/`PARAM_*` registers |

Keeping MHI's own vocabulary entirely out of the wire protocol is what
lets this spec describe a board that could serve *any* MHI-shaped guest
front-end (or, in principle, a non-MHI player that just wants a hardware
MPEG decoder) without the register file encoding one particular API
version's constants -- and it is what makes the split in
[Porting to another emulator](#porting-to-another-emulator) below
possible without also porting MHI-specific glue.

## Versioning

`VERSION` (`0x00`) is the register-protocol's own version number, starting
at **1** for the M1-M2 surface this document describes. This document is
the contract: a change to any offset, width, access rule, bit meaning, or
semantic described above is a protocol change and must bump `VERSION`,
whether or not it happens to be backward compatible. A guest library
should treat an unrecognized (higher-than-known) `VERSION` as "features
beyond what I implement may exist at reserved offsets," not as an error,
since new fields land at previously-reserved offsets (which any
conforming board -- old or new -- already defines as inert) rather than by
repurposing an existing offset's meaning. Repurposing an offset's meaning
between versions is not permitted by this spec's own conventions; a
protocol revision that needs to do that must move to a new offset and
leave the old one reading its previous semantics (or a fixed sentinel) for
compatibility, exactly like any other stable ABI.

## Porting to another emulator

Everything above is expressed purely in terms of the autoconfigured
window's own offsets and the Amiga's 24-bit address space -- nothing
references Copperline's internal types, its `ZorroDevice`/`DeviceHost`
Rust traits, or its savestate format. An unrelated emulator wanting to
support the same guest library and the same MHI test assets needs only
to:

1. Autoconfig a Zorro II board at manufacturer `0x1448`, product `7`,
   64 KiB, no autoboot ROM.
2. Implement the register map above over its own bus/register dispatch.
3. On a successful `DOORBELL` write, copy `DESC_LEN_HI:DESC_LEN_LO` bytes
   from `DESC_ADDR_HI:DESC_ADDR_LO` in the emulated Amiga's address space
   into wherever its own MPEG decoder wants the bytes -- by whatever
   internal mechanism that emulator already uses to read guest memory
   from a device model (a literal DMA engine, a direct memory-array
   read, anything at all; this spec does not constrain it).
4. Pace descriptor consumption and `COMPLETED_COUNT`/`INTREQ` updates to
   the decoded audio's own emulated-time rate, per
   [Determinism and timing](#determinism-and-timing), so that scripted
   scenarios and captures built against one implementation reproduce on
   the other.

Copperline's own implementation notes -- the `minimp3`-based decoder
choice, how `push_source("mhi", ...)` joins the mixer, `BoardDevice`
wiring, and savestate serialization of in-flight decoder/queue state --
are Copperline-internal and out of scope for this document; they belong
in `src/mhi.rs`'s own doc comments and this page's future host-board
implementation notes once WP3 lands, not in the protocol spec itself.
