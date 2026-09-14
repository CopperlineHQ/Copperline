# Netplay architecture

The implementation follows five steps:

1. Establish a deterministic session boundary. Validate the machine's host
   dependencies, adopt floppy contents into memory, normalize disk path metadata,
   and compare fingerprints before either peer executes guest instructions.
2. Add a transport-independent input timeline. Sample local input with a fixed
   delay, predict missing remote input, retain bounded snapshots, and replay from
   the first incorrect prediction. Validate against uninterrupted execution.
3. Exchange inputs over bounded UDP datagrams. Add cumulative acknowledgements,
   retransmission, handshake, prediction backpressure, confirmed-state checksums,
   and finite timeouts. Exercise loss, duplication, and reordering between peers.
4. Integrate both native frontend loops. Route all machine input through the
   timeline, prevent unilateral machine changes, discard stale rendered frames
   after rollback, and document supported workflows and limitations.
5. Reuse that timeline and wire protocol in WASM. Separate packet transport from
   the session, bridge bounded queues to WebRTC, and add browser Host/Join setup.

These steps are implemented in `src/netplay/`, `Emulator::step_netplay_frame`,
`src/video/window/app_netplay.rs`, `crates/copperline-web/src/netplay.rs` and
`crates/copperline-web/www/netplay.js`. The feature uses native Rust and does not
link the GGPO SDK. Spectators follow the host through a separate confirmed-only
feed (see [Spectators](#netplay-spectator-design)). Public matchmaking, reconnect and
persistent host filesystem writes remain separate work.

## Native Internet transport

`Session` coordinates setup and media around a
`Connection<Control<NativeTransport>>`; the adapter selects direct UDP or
`InternetTransport`. `Control` multiplexes reliable setup/media messages with
the shared input datagrams. The default `netplay-internet` feature adds iroh to
native builds. The browser/core build keeps its existing transport boundary.

The host sends a bounded JSON hardware manifest and checksummed media bytes.
The control channel takes ownership of separate media buffers and copies only
packet-sized chunks. It releases each buffer as it is queued, and the receiver
grows its buffer with incoming data up to the validated message length.
GUI and CLI guests retain local output, display and host input preferences on
their placeholder machine while ignoring remembered machine and game settings.
The manifest contains no host file paths, display/output preferences, plugin
modules or host-device authority. Only generated filenames inside a private
temporary directory reach the guest's machine builder. Both peers rebuild the
same cold setup, validate their initial fingerprints, then begin the input
handshake. Guest launcher settings remain local. No remote emulator checkpoint
is accepted or deserialized, by spectators either: a late joiner rebuilds the
game from the same cold bundle and the confirmed input history.

Desktop setup uses the versioned `CLFLOP01` media container. Standard ADF
payloads stay byte-for-byte intact. Track images retain metadata that UAE
extended ADF cannot carry, including IPF density profiles. The container
preserves all 168 possible track slots, raw MFM
words, bit and stored lengths, revolution counts, legacy sync words, cell times
and density spans. Counts are checked against the remaining payload before
allocation; track geometry and timing spans are validated before insertion.
Only the dedicated netplay media loader accepts this container, so its
signature cannot be confused with an ADF bootblock. The existing 16 MiB
floppy limit applies to its complete encoded size. It contains no controller
state or paths and does not change the save-state format. Normal disk exports
remain standard or UAE extended ADF; the extended ADF reader also accepts 168
tracks, matching IPF and SCP exports.

Setup and disk changes use a 32-packet selective-repeat window with cumulative
and selective acknowledgements, sequence numbers and a 200 ms retransmission
timer. Packets fit within the existing datagram bound and use the same peer and
socket in both connection modes. The emulation thread polls bounded queues;
file selection uses an asynchronous dialog. Setup expires after 15 minutes and
a disk change after three minutes.

WHDLoad, executable boot and other host-directory volumes are converted to
Amiga OFS disk images with their volume names and boot priorities. Available
motherboard IDE slots are used first, then SCSI slots. Configured IDE/SCSI/LIDE
images keep their controller. The host exports complete virtual sectors,
including any synthesized partition metadata, so the receiver never has to
interpret host directory paths. The asynchronous `copperhf` controller and CD
backends remain excluded from rollback.

Synthesized RDB headers mark the final LUN without claiming to be the final
target, allowing the ROM boot driver to discover subsequent IDE/SCSI volumes.

Session hard drives share an immutable base by SHA-256 and serialize only a
sorted overlay of changed sectors. Local checkpoint deserialization resolves
the base through a process-local weak registry; serialization borrows the
overlay without cloning dirty sectors. Deserialization never opens a path. The live
disk keeps its base alive. Read-only volumes reject writes, including writes to
partition metadata. Persistent hardfiles and ordinary in-memory volumes retain
their existing storage behavior. This adds the session backing to hard-drive
state and bumps save-state format 79 to 80; old save states are rejected.

For a floppy change, the host and guest hold their current frames, agree on the
greater frame, and catch up while still exchanging inputs. At that fully
confirmed boundary they compare state digests, transfer and validate the disk,
apply insertion/ejection, compare again, then resume. Retained predictions
cannot restore the previous disk. Only the host initiates changes, with one
transaction outstanding at a time.

Internet setup uses a host-generated Ed25519 endpoint key and an independent
random 128-bit invitation capability. The bounded `CLNI1.` code contains the
host's public endpoint ID, relay addresses, capability and delay/window. The
private key stays in the launcher's in-memory setup. Host and guest use a dedicated
QUIC ALPN. A bounded reliable stream checks the capability before accepting the
second player; unrelated or incomplete handshakes do not claim the session.
Invitation parsers reject unsupported routes and invalid timeline settings.

A dedicated thread owns a Tokio runtime and iroh endpoint. It connects through
an HTTPS relay, attempts NAT traversal, and uses direct paths when available.
The relay-only option removes IP transports entirely. Public relay addresses
come from iroh's production relay map; a custom relay replaces that map. The
minimal endpoint preset avoids public address lookup/publishing and automatic
router port mapping. No browser signaling or TURN credentials are involved.

Inputs use unordered, unreliable QUIC datagrams. The shared protocol continues
to own retransmission, acknowledgement and rollback. Receive/send queues each
hold at most 64 packets, and QUIC datagram buffers are bounded too. The worker
checks selected routes for address-free diagnostics. Socket discovery and all
network timers remain outside emulator and save-state data.

The desktop logger keeps iroh, its QUIC/network-watcher dependencies and bridged
tracing spans at warning level by default. `COPPERLINE_NETPLAY_DEBUG` enables
debug-level transport logs through the startup `envcfg` snapshot. An explicit
`RUST_LOG` overrides the preset. Copperline's own connection, route and final
frame summaries remain at info level.

`Transport::ready` holds the cold machine at frame zero during setup. An Internet
setup deadline of 15 minutes is separate from the protocol's 60-second handshake
and 10-second connected-peer timeouts. Dropping the transport cancels the worker
and closes its endpoint without blocking the UI. Runtime errors return through
the ordinary netplay error path. Run after F11 builds a new cold machine.

## Frame ownership

Network frame zero begins at cold boot. Each network frame ends when Agnus next
increments the emulated video-frame counter, at the first CPU instruction or
STOP fast-forward boundary after the wrap. This uses precise CPU stepping and
cycle accounting, independent of the ordinary frontend's CPU-budget quantum.
The scheduler state and transport remain outside serialized guest state.

An input contains eleven digital controller bits, a 128-key held-state bitmap,
signed mouse X/Y deltas and three held mouse buttons.
Each peer owns one port. Key bitmaps are ORed, and transitions from the previous
merged bitmap are enqueued in raw-key order at the frame boundary. Controller
buttons and keyboard prediction repeat the most recent remote held state at or
before that frame. Predicted mouse motion is zero: relative movement belongs to
one frame and must not repeat while waiting for another packet. Out-of-order
future inputs never seed an earlier prediction.

A delayed local input is submitted only once, even when repeated polling stalls
on the same frame. Interactive frontends use `Connection::step_local`, which
consumes pending mouse motion only on that first submission. `LocalInput` keeps
32-bit pending counts separate from the 16-bit per-frame wire deltas. Handshake and
confirmation polls preserve it, as do subsequent polls of an already sampled
frame. Each sample takes at most 100 counts per axis, retaining the remainder
for later frames to avoid ambiguous 8-bit JOYDAT wraparound. Mouse ports receive
motion and mouse buttons; digital-controller updates cannot replace their device.
Both timelines start with the negotiated number of neutral delay frames. A frame can advance only while both remote input and the remote
acknowledgement remain within the configured prediction window. This also bounds
unacknowledged local history when only one direction of the connection works.

## Restore and replay

Each unconfirmed frame records its pre-execution machine snapshot, the remote
input actually used, and the prior merged keyboard bitmap. An arriving input
that differs from the recorded prediction marks the earliest dirty frame. The
engine restores that frame's snapshot, removes the abandoned history, and
re-executes through the current frame with corrected input and fresh snapshots.

Snapshots reuse the machine serializer with an internal prefix for the open-bus
value and display latches omitted from file save states. The netplay restore
preserves captured video buffers; the ordinary file loader deliberately discards
them, which is unsuitable for immediate rollback. This internal prefix changes
neither the file save-state format nor its version. Rendering during netplay is
presentation-only, including the synchronous fallback. Replay is unpaced and
suppresses live audio and speculative host output.
It does not increment committed-frame statistics. The desktop renderer's
generation is invalidated after a correction, so an asynchronous result from
the old timeline cannot replace the corrected image. Interactive desktop sessions
finish rendering the corrected frame before returning to the window loop.
Otherwise, a rollback on every iteration can invalidate each queued render result
before the main thread collects it, freezing presentation during continuous mouse
movement even while emulation advances. Scheduled headless captures
wait for confirmation and local-input acknowledgement before rendering their
target, so they keep retransmitting inputs still needed by the other peer.

Only frames below the contiguous remote-input frontier are confirmed. A
checkpoint hashes the snapshot *after* its frame, once all inputs affecting it
are known. The engine drops confirmed snapshots, keeps one previous remote input
as its prediction seed, and releases acknowledged local inputs no longer needed
for replay. It retains eight recent checkpoint hashes. Snapshot storage has a
256 MiB cap; the configured prediction window bounds the number of snapshots.

## Wire protocol

`wire.rs` defines protocol version 2. Packets carry `CLNP`, the protocol
version and the save-state schema fingerprint
(`savestate::SCHEMA_FINGERPRINT`: crate version, container version, and
every chunk's version), a 16-byte session ID, a 32-byte initial-machine fingerprint,
player index, handshake-ready flag, delay/window settings, cumulative input
acknowledgement, the latest confirmed checkpoint, and up to 32 input records.
Integers are little-endian. Records contain an eight-byte frame number, two-byte
controller bitmap, sixteen-byte key bitmap, two signed two-byte mouse deltas,
and one byte containing the three mouse buttons. Each record is 31 bytes and
the maximum packet is 1103 bytes. Version 1 peers are rejected as incompatible,
as are peers whose schema fingerprint differs.

The initial fingerprint hashes Copperline's display build version and the entire
normalized initial machine snapshot, including ROM and in-memory floppy data.
It does not fingerprint uncommitted source modifications: development peers must
build the same source. This input protocol carries no executable, ROM, disk image
or serialized guest state. Browser setup transfers ROMs and disks over a separate
reliable channel before the input handshake. An ID separates sessions; the packet format supplies
neither cryptographic peer authentication nor encryption. Browser WebRTC adds
transport encryption; native Internet mode adds QUIC encryption and invitation
authorization. Direct UDP needs a VPN for that protection.

Every datagram repeats the session fingerprint and settings. Peers announce
whether they have seen a matching peer; emulation starts after receiving that
acknowledgement. Input packets repeat all unacknowledged local inputs. Sampling
and confirmation polls send immediately; handshake retries use a 10 ms timer.
The frontend sleeps between stalled polls. Each service call reads at most
64 packets.
Malformed lengths, invalid controls, duplicate frame ordering inside a packet,
unrelated endpoints, and unrelated session IDs are discarded. Conflicting input,
impossible acknowledgements, and data beyond the bounded future horizon stop the
session. Errors stay latched so callers cannot accidentally continue a failed
session as local play.

## Browser transport and lifecycle

`Connection<T: Transport>` owns the handshake, rollback and timeout logic.
Same-process checkpoints include the CPU adapter's sampled interrupt level and
microcode poll hold. Restoring the chipset without these latches can recognize
an interrupt one instruction early after replay. They remain outside file save
states; the rollback prefix and initial fingerprint change with the build.
`Session` is the native alias for `Connection<NativeTransport>`;
`ConnectionOptions` selects direct UDP `Options` or Internet invitation settings. Transport-independent `Settings` contains the player,
session ID, input delay and prediction limit. Timers use `timebase::Instant` on
both targets. Neither target serializes transport or wall-clock state.

The web wrapper owns `Connection<PacketQueue>`. Each direction holds at most
64 packets of at most 1103 bytes. Incoming bursts evict the oldest datagram, relying
on subsequent retransmissions; a full outgoing queue reports backpressure.
`netplay.js` also bounds its receive queue and stops draining Rust's send queue
when the channel's buffered amount reaches 64 maximum-size packets.

The page exchanges a versioned offer and answer containing SDP and host-selected
settings. It waits up to 15 seconds for ICE gathering before creating either
code. At the deadline it uses collected candidates if any are available; a slow
server cannot invalidate routes gathered from other servers. An empty candidate
set still fails setup, and diagnostics record a gathering-deadline event.
`netplay-room.js` sends these codes through the configured service; the host polls
for an answer every 1.5 seconds until joined, cancelled or expired. Manual
copy/paste remains available under Advanced and needs no signaling service.
Neither path trickles candidates.

`services/netplay` implements the Cloudflare Worker and SQLite Durable Object
used for each invitation. Room IDs and separate owner/guest tokens each contain
128 random bits. A serialized guest reservation admits one guest; role checks
restrict offer publication, answer publication and answer retrieval. Records
expire after 15 minutes via an alarm. DELETE ends setup early; cancellation also
aborts pending browser requests. The service carries no emulator packets.
Requests and responses are bounded, origins are allowlisted, and per-IP rate
limits bound creation separately from polling. An origin header is a browser
boundary, not authentication against arbitrary clients.

TURN API keys stay in Worker secrets. The service issues temporary, 24-hour ICE
credentials independently for each player; retries reuse the guest credentials
within the room lifetime. Room creation fails when a production relay service
is unavailable. Local development explicitly disables TURN requests. WebRTC
selects direct or relay routes, or relay-only when requested in Advanced.
The service filters alternate port 53 URLs from Cloudflare's response because
browser port blocking can delay gathering even after usable candidates arrive.
If a browser rejects TURN transport queries with a syntax error, the client
retries with query-free UDP and TLS URLs. This preserves the ports, credentials
and relay policy; plain TCP entries are omitted on that compatibility path.
`netplay-diagnostics.js` records whitelisted states, candidate types and numeric
counters before disposal. It never exports SDP, candidate addresses or tokens.
The diagnostic sample survives a closed peer connection.
The input [WebRTC data channel](https://www.w3.org/TR/webrtc/) is unordered and uses
zero SCTP retransmissions, since the shared Copperline protocol already repeats
unacknowledged inputs. Browser and native transports have no interoperability
adapter. Codes are bounded and checked before passing SDP to WebRTC.

`netplay-media.js` adds an ordered, reliable `copperline-setup-v1` channel when
the offer's settings include `media: "host-v1"`. An 8 KiB bounded manifest
describes the build, supported browser machine settings, ROM/extended ROM and
DF0/DF1 images with SHA-256 digests. Each ROM is bounded to 2 MiB, each disk to
16 MiB, and the total to 36 MiB. Binary messages contain at most 16 KiB; the
sender waits for buffer drainage above 256 KiB. The receiver validates metadata
before allocating, verifies every digest and acknowledges completion before
the host boots. A 30-second inactivity timer and a three-minute overall deadline
bound setup. Cancellation rejects pending operations and discards partial data.
Media stays off the signaling service; a TURN relay carries encrypted peer traffic.

`netplay-swap.js` negotiates `swaps: "disk-v1"` and an ordered, reliable
`copperline-disks-v1` channel. Only the host initiates transactions. Each peer
first holds its current frame; they then advance to the greater of those two
frames, bounded to a 32-frame difference. `WebEmu::run_netplay` enforces this
ceiling while continuing input polling and reconciliation. Both peers wait for
the stop frame to be confirmed and acknowledged, so no prediction history can
restore media from before the change. Input delay and already sampled future
inputs are retained, and wall-clock pacing is reanchored around the pause.

The peers compare SHA-256 digests of the complete confirmed machine state,
transfer at most 16 MiB in 16 KiB chunks with 256 KiB send-buffer backpressure,
verify the image digest, and decode into a temporary controller before agreeing
to apply. The core decoder enforces the same 16 MiB bound on gzip/zip expansion
during validation and insertion; the ordinary gzip floppy loader has a 128 MiB
expanded-size cap for larger flux captures. Host controls wait for the separate
disk channel to open. Both live drives change at the stopped boundary with canonical
`netplay-dfN` metadata and memory backing. A second full-state digest comparison
precedes resume. Empty payloads perform an eject. Transaction IDs, message phases,
metadata and chunk sizes are checked; overlapping requests are rejected. Thirty
seconds without control/transfer progress or three minutes total ends the session.
Cancellation discards buffered bytes and frees the machine through the normal
disconnect path. No save-state or input-packet format changes are required.

Host/Join freezes the chosen cold-boot media and frees any local emulator.
After media verification, the wrapper builds a fresh machine, fixes the RTC seed,
disables serial and sets both selected controllers before fingerprinting it.
JS numeric settings enter Rust as `f64` and are checked for finiteness, integer
values and range before narrowing. Setup operations carry their connection
identity across awaits so cancellation cannot publish a late machine or code.
Disconnect and runtime failures stop all session loops and free the emulator;
the chosen pre-session media remains available for another cold boot. The guest
uses a separate received snapshot; it never overwrites `bootRom`, `lastDisks`
or IndexedDB. Disconnect restores its original control values and disk names.

Netplay calls use the shared precise frame stepper, with browser pacing capped
at eight frames per call. Zero-frame polls can reconcile and acknowledge input
while painting is suspended. Corrections invalidate field history and cropping
latches. Browser netplay renders through `render_display_only_with_content` so
different paint cadences cannot write collision bits into checkpoint state.
Paula's output gain is serialized in ordinary save states, so browser netplay
fixes it at 100% and applies the page's volume when draining the host audio buffer.
This avoids a save-state format change and keeps volume local across replay.

Browser startup keeps a local rollback checkpoint and the original serial sink
until connection construction succeeds. Failure restores both before returning
an error. Floppy sound settings remain serialized because they also control the
sound generator timeline; browser setters and UI controls lock them during a
session. The wire decoder reports an incompatible protocol version or state
schema for the recognized session immediately, while unrelated traffic remains
ignored.

(netplay-spectator-design)=
## Spectators

A spectator is a third kind of participant: it owns no controller port, sends
no input, and never enters the players' rollback timeline. Only the host serves
spectators (star topology), so the guest and the `CLNP` input protocol are
unchanged; `Role::{Host, Guest, Spectator}` on `ConnectionOptions` and
`Session` replaces the old "opposite player" assumptions, and `Settings` stays
the two-player structure the browser build shares.

The host's `Rollback` records a `ConfirmedLog` inside `confirm()`: for every
frame that becomes confirmed it stores both ports' inputs (its own submitted
input and the peer's received one, which can no longer change) and every
checkpoint digest at insertion, before the prune that releases them.
`Connection::enable_feed` (host only, at frame zero) drains that log into a
`spectate::Feed`: the complete confirmed history (46 bytes per frame), the
checkpoint digests, and every disk change as a `SwapRecord` with the drive,
write flag, image bytes and the full-state digests the host measured before
and after applying it. The feed is capped (256 MiB native, 64 MiB browser);
past the cap the host refuses new spectators while existing streams continue.

`Feed::next_message` walks a `FeedCursor` in replay order: a `Checkpoint`
due at the cursor's frame, then a `Swap` due there, then a `Frames` batch of
at most 1024 records that never crosses the next checkpoint boundary or disk
change. The host therefore hashes checkpoint N and only then applies a change
stopped at N, and a spectator sees the messages in that order. Messages use
`[kind u8][len u32 LE][payload]` framing with the datagram input layout;
`FeedDecoder` validates each header before buffering its payload, so a hostile
length never allocates, and reassembles across arbitrary chunk boundaries.
Reverse messages are `Verified` (the spectator's initial fingerprint) and
`Status` (its executed frame, once a second, as a liveness report).

`Spectator` is the confirmed-only timeline: it executes `[Input; 2]` per
frame through the same `Machine` adapter as rollback, with no prediction and
no snapshot history. At every multiple of 60 frames it waits for the host's
checkpoint, compares `digest(netplay_snapshot())`, and fails closed on a
mismatch before executing further. A due `SwapRecord` blocks the frame until
the frontend applies it with `spectate::apply_swap`, which checks the
host's digests on both sides of the change. Feed messages must be
contiguous and in order; a swap below the buffered frontier, a late
checkpoint, or more than a million buffered frames is rejected.

Desktop: `Session` holds either a player `Connection` or a `Watcher`
(`Control<NativeTransport>` plus a `Spectator`). A spectator's control link
uses role byte 2; `Control` now checks a `(local, peer)` role pair, holds
chunks in its receive window instead of failing once four messages are
queued, and shares `Arc` parts so one bundle serves the guest and every
spectator without copies. Kind byte 4 carries feed bytes beside the JSON
setup messages (`Watch`, `Verified`, `Start`, `Refused`) and the bundle. A
spectator's setup is the guest's: bundle in, `machine_identity` compared by
the host, then the feed streams from frame zero. Direct UDP demultiplexes
spectator control packets by source address on the host's existing socket
(`SlotTable`, one slot per admitted spectator, freed when its link drops);
Internet mode keeps the host's iroh endpoint accepting after the player and
classifies each connection by its capability: the invitation's session admits
the one player, a separate random capability in the `CLNS1.` code admits
spectators, and neither opens the other role. Each connection is pumped by
its own task, so a spectator's failure lands only in its own queues. The host
services every link after its own step: at most four feed messages in flight
per link, a `Head` keepalive after a second of silence, a 10-second silence
timeout, and any error drops that link alone. Nothing a spectator does can
stall or fail the players.

The frontend runs a spectator paced while it is within 25 frames of the
host's confirmed frontier and in unpaced bursts of up to 64 frames per loop
pass while further behind, with live audio muted like a warp gate; headless
captures keep one frame per pass so a burst cannot overshoot a scheduled
screenshot. Window input is swallowed, `App::mouse_port` is `None`, disk
controls are unavailable, and F11 leaves.

Browser: a room host creates a separate watch room next to its player room
(`POST /watch` with all eight places, its own 22-character capability,
`#watch=` links) and enables the confirmed-history feed at frame zero, so
spectators can arrive at any time; manual-code sessions have no room
service and no spectators. The panel shows the spectator invitation with its
own QR beside the player one until player 2 connects, then alone. Signaling
runs the other way round: each spectator reserves a place
(`/join`, which also issues its TURN credentials), offers over
`copperline-watch-v1` (ordered, reliable) plus the existing setup channel,
and polls for its answer with 429 backoff; the host polls `/offers` every two
seconds while its machine runs, which also extends the room's expiry, and
answers each offer up to the places it holds, refusing the rest (`/refuse`),
since the service counts only places still in signaling; a fetched answer
frees the place, and an expired room closes the hub, which withdraws the
invitation from the panel while the spectators already admitted keep
watching. `SpectatorHub` keeps one
`RtcWatchPeer` per spectator, describes the host media once, serves it with
`MediaTransfer`, requires a `Verified` fingerprint equal to
`WebEmu::netplay_identity`, then opens a feed cursor
(`spectator_feed_open`) and pumps `spectator_feed_take` in 16 KiB messages
under 256 KiB of buffered backpressure; a peer whose buffer never drains for
30 seconds is dropped. The spectator page boots the received media with
`start_spectating`, feeds bytes to `spectate_receive`, and `run_spectate`
paces it like a player or replays a backlog in bounded bursts (64 frames or
12 ms per call) with its audio discarded. Every mutation and input entry
point checks `session_active`, which covers spectating.

## Configuration screen

`LauncherState::netplay` holds a `NetplaySetup` beside the machine setup. It uses
normal launcher rows, editing, hit testing and keyboard/gamepad navigation, but
never enters `RawConfig` or the machine serializer. Enabling it applies the
required controller and execution settings to `MachineSetup`; the ordinary
configuration pages show those changes.

Run commits any focused field, parses the connection through the shared session
ID/options validators, applies the deterministic RTC default and builds a cold
machine. Existing App-level control/GDB endpoints block netplay startup because
they survive machine replacement. Static validation rejects parallel host
devices before construction, including the sampler attached later by the
frontend, and rejects Toccata's noncanonical serialized resampler map.
It creates the native transport before replacing the live machine, so a
validation or immediate bind failure leaves that machine intact and reports the error in
the launcher. The successful session then uses the same `attach_netplay` path as
a CLI launch. Session code generation uses host randomness for a fresh identifier;
it does not add peer authentication.

F11 drops the session/socket, pauses the machine and restores the Netplay page
with the last connection settings. Runtime failures take the same path with an
error message. Run after that builds a new machine and rebinds the socket; it
never resumes an abandoned network timeline. Headless errors still return to
the caller normally.

## Validation

The regression suite covers:

- Zero and nonzero input delay, held-state prediction with zero predicted mouse
  motion, batched late arrivals,
  duplicate inputs, bounded stalls, recovery, and once-only local sampling,
  including pending mouse movement accumulated across stalls.
- Byte-identical replay against an uninterrupted 68000 workload that reads both
  JOYDAT registers and CIA fire inputs, writes RAM, and drives a display colour,
  with two mice, two joysticks, two CD32 pads and mixed mouse/CD32 ports.
- Desktop presentation during sustained late mouse movement at the default input
  delay, including agreement between threaded and synchronous rendering without
  changing machine state.
- Two complete emulators connected through local UDP proxies with deterministic
  loss, delay, duplication, reordering, and asymmetric pauses, with zero, default,
  and maximum input delay; both must confirm the same checkpoint and end with
  identical machine-state digests.
- Packet truncation/size bounds, conflicting inputs, invalid acknowledgements,
  initial mismatch, desynchronization, and disconnect timeouts.
- CLI combinations, GUI field/edit/navigation coverage, and frontend input/mutation
  routing. Two GUI-configured peers must connect, confirm matching states, return
  to setup and successfully rebind for another cold boot.
- Spectators: the feed codec round-trips across arbitrary chunk boundaries and
  rejects bad headers, counts, drives and controller bits; the confirmed log of
  a rollback run with late, reordered and duplicate input drives a spectator to
  the baseline at every delay; three desktop sessions where a spectator joins
  after 130 frames and a disk change, replays the backlog, follows further
  changes, matches the host's snapshot and leaves without disturbing the
  players; UDP demultiplexing by source with a place cap; control-link role
  pairs and receive-window backpressure; iroh admission by capability; the
  launcher's Watch role and spectator code; the CLI flags; and a GUI spectator
  that joins late through the launcher, catches up unpaced and re-paces.
  `tools/check-netplay.py --spectators N` runs late-joining spectator
  processes against the two players and requires identical PNGs.
- Browser packet queue bounds, signaling validation, data-channel options,
  cancellation and backpressure. The release WASM smoke runs paired A500/PAL and
  A1200/NTSC machines through 120 confirmed/checksummed frames under loss,
  duplication, reordering and asymmetric pauses. Different volume, overscan and
  painting cadence must preserve checkpoints and produce identical final pixels.

Run the focused tests with `cargo test --profile ci --locked netplay`; UDP tests
need permission to bind loopback sockets. No external ROM or disk assets are
required for the regression suite.

After building the release web bundle, run `node tools/check-web-netplay.mjs` and
`npm test --prefix crates/copperline-web/www`. The WASM check also runs two
spectators that join at frames 60 and 200, replay the host feed in random
slices, ignore local input and match the host's picture at frame 300; the
swap check keeps a spectator following every change and replays them all
from frame zero on a late joiner. CI also runs the native web wrapper
unit tests, including failed startup, input routing and audio gain checks.
`node tools/check-web-netplay-swaps.mjs` exercises repeated replacements and
ejections on real release WASM with packet loss, reordering and asymmetric pacing,
then checks continued state agreement after each change. The
browser publishing workflow also checks the optimized bundle before copying all netplay modules and the vendored QR license alongside
the other page modules. For a served local page, the optional Playwright check
`node tools/check-web-netplay-browser.mjs http://127.0.0.1:8000/` exercises actual
Host/Join, cancellation, reconnect by cold boot, locked controls and mismatch
rejection in two Chromium contexts. It also verifies that a device-keyboard tap
appears as a held key and subsequent release in transmitted input frames, and
that two-mouse sessions route DOM movement and button press/release events on
both pages while leaving keyboard joystick mode off.
`CHROME_PATH` selects an installed Chrome;
`PLAYWRIGHT_MODULE` can point to a local Playwright module.

Run `npm ci && npm test` in `services/netplay` for real local Worker lifecycle,
role, quota and TURN-provider tests. With that service running on port 8787 and
the site on 8765, `node tools/check-web-netplay-rooms.mjs` checks the invitation
flow with the release WASM bundle. `NETPLAY_SERVICE` selects a deployed endpoint;
`NETPLAY_RELAY_ONLY=1` requires an actual selected relay candidate and checked
emulation frames. `NETPLAY_BROWSER=webkit` selects an installed Playwright WebKit.
Desktop WebKit and mobile viewport tests do not qualify iOS hardware or beta Safari.
The QR encoder is the unmodified `qrcode-generator` 2.0.4 ES module, distributed
with its MIT license; no external image or QR service receives invitations.

Explicit Internet qualification uses
`cargo test --profile ci --locked --lib internet_netplay_ -- --ignored --nocapture`.
It runs two local emulators through public relays, once with automatic routes
and once with IP transports disabled, and requires 180 confirmed frames, checked
checkpoints and identical machine snapshots. These tests are opt-in because they
depend on an external service. The ordinary suite exercises encrypted loopback
traffic, capability rejection, invitation validation and launcher/CLI controls.
