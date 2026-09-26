# Netplay architecture

Copperline's rollback netplay is built in five layers:

1. A deterministic session boundary. Before any peer executes a guest
   instruction, it validates the machine's host dependencies, adopts floppy
   contents into memory, normalizes disk path metadata and compares
   fingerprints.
2. A transport-independent input timeline. It samples local input with a
   fixed delay, predicts missing remote input, retains bounded snapshots and
   replays from the first incorrect prediction. Tests check it against
   uninterrupted execution.
3. Input exchange over bounded UDP datagrams, with cumulative
   acknowledgements, retransmission, a handshake, prediction backpressure,
   confirmed-state checksums and finite timeouts. Tests exercise loss,
   duplication and reordering between peers.
4. Integration with both native frontend loops. All machine input goes
   through the timeline, unilateral machine changes are blocked, and stale
   rendered frames are discarded after a rollback.
5. The same timeline and wire protocol in WASM. Packet transport is separate
   from the session, bounded queues bridge to WebRTC, and the page provides
   browser Host/Join setup.

The code lives in `src/netplay/`, `Emulator::step_netplay_frame`,
`src/video/window/app_netplay.rs`, `crates/copperline-web/src/netplay.rs` and
`crates/copperline-web/www/netplay*.js`. It is native Rust and does not link
the GGPO SDK. A desktop session drives two to four controller ports, one per
player, with the host relaying between guests (see Players and topology
below). Spectators follow the host through a separate confirmed-only feed
(see [Spectators](#netplay-spectator-design)). Public matchmaking,
reconnection and persistent host filesystem writes are not implemented. The
libretro core does not use this module: RetroArch's own netplay drives it
through save states (see [RetroArch netplay](../guide/libretro.md#retroarch-netplay)).

## Native Internet transport

`Session` coordinates setup and media around a
`Connection<Control<LinkTransport>>`. A guest's `LinkTransport` wraps its own
`NativeTransport`, which selects direct UDP or `InternetTransport`; the
host's wraps the `PeerTransport` its listener hands out for each admitted
guest. `Control` multiplexes reliable setup/media messages with the shared
input datagrams. The default `netplay-internet` feature adds iroh to native
builds. The browser build has its own transport boundary (see Browser
transport and lifecycle below).

The host sends a bounded JSON hardware manifest (at most 64 KiB) and
checksummed media: at most 25 files and 512 MiB in total, with 2 MiB per
ROM, 16 MiB per floppy and 256 MiB per hard drive. The machine it describes
may have at most 64 MiB of RAM, including expansion and video RAM. The host
keeps the media buffers for the whole session and queues them as shared
`Arc` parts, so one bundle serves every guest and spectator and the control
channel copies only packet-sized chunks. The receiver grows its buffer with
incoming data up to the validated message length.
GUI and CLI guests retain local output, display and host input preferences on
their placeholder machine while ignoring remembered machine and game settings.
The manifest contains no host file paths, display/output preferences, plugin
modules or host-device authority. Only generated filenames inside a private
temporary directory reach the guest's machine builder. The host and every
guest build the machine from the same bundle, compare initial fingerprints,
then begin the input handshake. Guest launcher settings remain local. No
remote emulator checkpoint is accepted or deserialized, by spectators either:
a late joiner rebuilds the game from the same cold bundle and the confirmed
input history.

Desktop setup uses the versioned `CLFLOP01` media container. Standard ADF
payloads stay byte-for-byte intact. Track images retain metadata that UAE
extended ADF cannot carry, including IPF density profiles. The container
preserves all 168 possible track slots, raw MFM words, bit and stored
lengths, revolution counts, legacy sync words, cell times and density spans.
Counts are checked against the remaining payload before allocation; track
geometry and timing spans are validated before insertion.
Only the dedicated netplay media loader accepts this container, so its
signature cannot be confused with an ADF bootblock. The 16 MiB floppy limit
applies to its complete encoded size. It contains no controller state or
paths and is not part of the save-state format. Normal disk exports
remain standard or UAE extended ADF; the extended ADF reader also accepts 168
tracks, matching IPF and SCP exports.

Setup and disk changes use a 32-packet selective-repeat window with cumulative
and selective acknowledgements, sequence numbers and a 200 ms retransmission
timer. Packets fit within the 1160-byte input datagram bound and use the
same peer and socket in both connection modes. The emulation thread polls
bounded queues; file selection uses an asynchronous dialog. Setup expires
after 15 minutes and a disk change after three minutes.

WHDLoad, executable boot and other host-directory volumes are converted to
Amiga OFS disk images with their volume names and boot priorities. Available
motherboard IDE slots are used first, then SCSI slots. Configured IDE/SCSI/LIDE
images keep their controller. The host exports complete virtual sectors,
including any synthesized partition metadata, so the receiver never has to
interpret host directory paths. The asynchronous `copperhf` controller and CD
backends are excluded from netplay.

Synthesized RDB headers mark the final LUN without claiming to be the final
target, allowing the ROM boot driver to discover subsequent IDE/SCSI volumes.

Session hard drives share an immutable base by SHA-256 and serialize only a
sorted overlay of changed sectors. Local checkpoint deserialization resolves
the base through a process-local weak registry; serialization borrows the
overlay without cloning dirty sectors. Deserialization never opens a path.
The live disk keeps its base alive. Read-only volumes reject writes,
including writes to partition metadata. Persistent hardfiles and ordinary
in-memory volumes keep their usual storage behaviour. Hard-drive state
records which of these three backings a drive uses; the distinction entered
the save-state format at version 80.

For a floppy change, every player holds its current frame and reports it to
the host, which picks the furthest reported frame (at most 32 frames ahead);
all players then catch up to it while still exchanging inputs. At that fully
confirmed boundary the host checks every guest's state digest against its
own and sends the disk. Once each guest has validated it, all players apply
the insertion or ejection and compare digests again before resuming. The
host records the change, with both digests, for spectators. Retained
predictions cannot restore the previous disk. Only the host initiates
changes, with one transaction outstanding at a time.

Internet setup uses a host-generated Ed25519 endpoint key and an independent
random 128-bit invitation capability. The `CLNI1.` code (at most 4096
characters) contains the host's public endpoint ID, up to eight relay or IP
addresses, the capability, the player count and the delay/window settings.
The private key stays in the host's memory and never enters a code. Every
peer uses the dedicated QUIC ALPN `copperline/netplay/1`. A bounded reliable
stream checks the capability before the host admits a guest; unrelated or
incomplete handshakes do not claim a port. Invitation parsers reject
unsupported routes and invalid timeline settings.

A dedicated thread owns a Tokio runtime and iroh endpoint. It connects through
an HTTPS relay, attempts NAT traversal, and uses direct paths when available.
The relay-only option removes IP transports entirely. Public relay addresses
come from iroh's production relay map; a custom relay replaces that map. The
minimal endpoint preset avoids public address lookup/publishing and automatic
router port mapping. No browser signaling or TURN credentials are involved.

Inputs use unordered, unreliable QUIC datagrams. The shared protocol continues
to own retransmission, acknowledgement and rollback. Receive/send queues each
hold at most 64 packets, and QUIC datagram buffers are bounded too. Once a
second the worker logs whether the selected path is direct or relayed,
without any addresses. Socket discovery and all network timers remain outside
emulator and save-state data.

The desktop logger keeps iroh, its QUIC/network-watcher dependencies and bridged
tracing spans at warning level by default. `COPPERLINE_NETPLAY_DEBUG`, read
through the startup `envcfg` snapshot, raises them and Copperline's own
`copperline::netplay` logs to debug level. An explicit `RUST_LOG` overrides
the preset. Without either, the connection, route and final frame summaries
still appear at info level.

`Transport::ready` holds the cold machine at frame zero during setup. An Internet
setup deadline of 15 minutes is separate from the protocol's 60-second handshake
and 10-second connected-peer timeouts. Dropping the transport cancels the worker
and closes its endpoint without blocking the UI. Runtime errors return through
the ordinary netplay error path. Run after F11 builds a new cold machine.

## Players and topology

A session drives two to four controller ports, one per peer. Ports 3 and 4
are the passive parallel-port adapter's sockets, so a session with more than
two players needs that adapter fitted with a joystick in each socket it uses;
`validate_player_ports` checks this on the machine every peer actually runs,
and the adapter travels in the setup manifest so each peer rebuilds it.
The adapter is wiring with no host peripheral behind it, which is why netplay
accepts it where it refuses a printer or sampler.

Player 1 hosts. Guests connect only to the host, which relays every port's
input to every other player: guests never address each other, so a session
needs one reachable endpoint rather than a full mesh. `Connection` therefore
holds one `Link` per peer -- a guest has one, to the host -- and the host's
socket or endpoint stays a listener that admits each guest and spectator and
hands out a per-peer transport. Every player must be present before frame
zero, since a latecomer could not reproduce the frames already executed.

Direct UDP admits a guest by its first control packet. A host either lists
one distinct address per guest and admits only those, or lists none and
admits any source that presents the session ID, as it does for spectators.
Internet mode admits guests by the invitation's capability and seats each on
the next free port, which it names in the `Offer`. A direct-IP guest asks for
the port it was configured for, and the host confirms it or refuses the
guest.

A guest submits its input to the host alone, so only the host's
acknowledgement releases its local history. The host owes each port's inputs
to every other player, and keeps them until the last link has acknowledged
them (`relay_floor`), which is separate from the confirmed frontier.

## Frame ownership

Network frame zero begins at cold boot. Each network frame ends when Agnus next
increments the emulated video-frame counter, at the first CPU instruction or
STOP fast-forward boundary after the wrap. This uses precise CPU stepping and
cycle accounting, independent of the ordinary frontend's CPU-budget quantum.
The scheduler state and transport remain outside serialized guest state.

An input contains eleven digital controller bits, a 128-key held-state bitmap,
signed mouse X/Y deltas and three held mouse buttons.
Each peer owns one port. Key bitmaps from every player are ORed, and
transitions from the previous merged bitmap are enqueued in raw-key order at
the frame boundary. An adapter socket carries switch joysticks only, so the
six direction and button bits are all that reach ports 3 and 4. Controller
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
Every timeline starts with the negotiated number of neutral delay frames
(0..6, default 2). A frame can advance only while both remote input and
remote acknowledgements remain within the configured prediction window
(1..12 frames, default 8). This also bounds unacknowledged local history
when only one direction of the connection works.

## Restore and replay

Each unconfirmed frame records its pre-execution machine snapshot, the input
set it actually executed (remote predictions included), and the prior merged
keyboard bitmap. An arriving input that differs from the recorded prediction
marks the earliest dirty frame. The engine restores that frame's snapshot,
removes the abandoned history, and re-executes through the current frame
with corrected input and fresh snapshots.

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
target, so they keep retransmitting inputs still needed by the other peers.

Only frames below the contiguous remote-input frontier are confirmed. Every
60 frames (`HASH_INTERVAL`) a checkpoint hashes the machine state at that
frame boundary -- after frame N-1 has executed and before frame N -- once the
inputs of every earlier frame are confirmed. The engine drops confirmed
snapshots, keeps one previous remote input as its prediction seed, and
releases acknowledged local inputs no longer needed for replay or relay. It
retains eight recent checkpoint hashes. Snapshot storage has a 256 MiB cap;
the configured prediction window bounds the number of snapshots.

## Wire protocol

`wire.rs` defines protocol version 3. Packets carry, in order: `CLNP`, the
protocol version, the save-state schema fingerprint
(`savestate::SCHEMA_FINGERPRINT`: crate version, container version, and
every chunk's tag and version), a 16-byte session ID, a 32-byte
initial-machine fingerprint, one byte each for the sending player's index,
the handshake-ready flag, the input delay, the prediction window and the
session's player count, one cumulative input acknowledgement per port (four
eight-byte slots), the latest confirmed checkpoint's frame and digest, and a
count followed by up to 32 input records. Integers are little-endian.
Records name the port they belong to and contain an eight-byte frame number,
two-byte controller bitmap, sixteen-byte key bitmap, two signed two-byte
mouse deltas, and one byte containing the three mouse buttons. The header is
136 bytes, each record 32 bytes, and the maximum packet 1160 bytes. Records
are ordered by port and then frame, so a repeated or reordered record inside
a packet is rejected, and a port outside the session acknowledges nothing. A
host sends each link only the ports that link does not own, in up to four
packets per service call. Version 1 and 2 peers are rejected as
incompatible, as are peers whose schema fingerprint differs.

The initial fingerprint hashes Copperline's display build version and the entire
normalized initial machine snapshot, including ROM and in-memory floppy data.
It does not fingerprint uncommitted source modifications: development peers must
build the same source. This input protocol carries no executable, ROM, disk image
or serialized guest state. Desktop and browser setup transfer ROMs and disks
over a separate reliable channel before the input handshake. An ID separates
sessions; the packet format supplies neither cryptographic peer
authentication nor encryption. Browser WebRTC adds transport encryption;
native Internet mode adds QUIC encryption and invitation authorization.
Direct UDP needs a VPN for that protection.

Every datagram repeats the session fingerprint and settings. The host
announces readiness once it has heard from every guest, and a guest starts on
that announcement, so all of them begin within one round trip. A guest's
packets are accepted only for its own port; the host is the only authority
for the others. Input packets repeat every input the receiving link has not
yet acknowledged. Sampling and confirmation polls send immediately; handshake
retries use a 10 ms timer. The frontend sleeps between stalled polls. Each
service call reads at most 64 packets per link.

Each packet also carries the sender's latest checkpoint digest. Once the
receiver has hashed the same frame it compares the two, and a mismatch stops
the session with a desynchronization error naming the player and frame.
Malformed lengths, invalid controls, duplicate frame ordering inside a packet,
unrelated endpoints, and unrelated session IDs are discarded. Conflicting
input, impossible acknowledgements, data beyond the bounded future horizon,
mismatched settings or initial fingerprints, and a packet claiming a port its
link does not own stop the session. Errors stay latched so callers cannot
accidentally continue a failed session as local play.

## Browser transport and lifecycle

`Connection<T: Transport>` owns the handshake, rollback and timeout logic.
Same-process checkpoints include the CPU adapter's sampled interrupt level and
microcode poll hold. Restoring the chipset without these latches can recognize
an interrupt one instruction early after replay. They remain outside file save
states; the rollback prefix and initial fingerprint change with the build.
On the desktop, `Session` (`desktop.rs`) wraps the connection with setup,
media and spectator handling. `ConnectionOptions` selects direct UDP
`Options`, Internet invitation settings, or their spectator counterparts
`WatchOptions` and `internet::SpectatorOptions`. Transport-independent
`Settings` contains the player, the session's player count, session ID,
input delay and prediction limit. Browser sessions stay at two players: a
page cannot fit the parallel-port adapter. Timers use `timebase::Instant` on
both targets. Neither target serializes transport or wall-clock state.

The web wrapper owns `Connection<PacketQueue>`. Each direction holds at most
64 packets of at most 1160 bytes. Incoming bursts evict the oldest datagram,
relying on subsequent retransmissions; a full outgoing queue reports
backpressure. `netplay.js` also bounds its receive queue and stops draining
Rust's send queue when the channel's buffered amount reaches 64 maximum-size
packets.

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
`netplay-diagnostics.js` records allowlisted states, candidate types and numeric
counters before disposal. It never exports SDP, candidate addresses or tokens.
The diagnostic sample survives a closed peer connection.
The input [WebRTC data channel](https://www.w3.org/TR/webrtc/)
(`copperline-netplay-v1`) is unordered and uses zero SCTP retransmissions,
since the shared Copperline protocol already repeats unacknowledged inputs.
Browser and native transports have no interoperability adapter. Codes are
bounded and checked before passing SDP to WebRTC.

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
precedes resume, and the host records the change with both digests for its
spectators. Empty payloads perform an eject. Transaction IDs, message phases,
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
no input, and never enters the players' rollback timeline. The host serves
spectators on the same star topology it already relays players over, so
guests never see them and the `CLNP` input protocol does not carry them.
`Role::{Host, Guest, Spectator}` on `ConnectionOptions` and `Session` says
which of the three a participant is.

`Connection::enable_feed` (host only, at frame zero) turns on a
`ConfirmedLog` that the host's `Rollback` fills inside `confirm()`: for every
frame that becomes confirmed it stores every port's input (its own submitted
input and the received ones, which can no longer change) and every
checkpoint digest at insertion, before the prune that releases them. Each
step drains the log into a `spectate::Feed`: the complete confirmed history
(23 bytes per port per frame, with the port count in each batch), the
checkpoint digests, and every disk change as a `SwapRecord` with the drive,
write flag, image bytes and the full-state digests the host measured before
and after applying it. A desktop host enables the feed only when it admits
spectators. The feed is capped (256 MiB native, 64 MiB browser); past the
cap the host refuses new spectators while existing streams continue.

`Feed::next_message` walks a `FeedCursor` in replay order: a `Checkpoint`
due at the cursor's frame, then a `Swap` due there, then a `Frames` batch of
at most 1024 records that never crosses the next checkpoint boundary or disk
change. That order matches the host, which hashes checkpoint N before it
applies a change stopped at N. Messages use
`[kind u8][len u32 LE][payload]` framing with the datagram input layout;
`FeedDecoder` validates each header before buffering its payload, so a hostile
length never allocates, and reassembles across arbitrary chunk boundaries.
The codec also defines two spectator-to-host messages: `Verified` (the
spectator's initial fingerprint; desktop spectators send it as a JSON setup
message instead) and `Status` (its executed frame, once a second, as a
liveness report).

`Spectator` is the confirmed-only timeline: it executes one input per
controller port through the same `Machine` adapter as rollback, adopting the
host's port count from its first batch, with no prediction and no snapshot
history. At every multiple of 60 frames it waits for the host's checkpoint,
compares `digest(netplay_snapshot())`, and fails closed on a mismatch before
executing further. A due `SwapRecord` blocks the frame until the frontend
applies it with `spectate::apply_swap`, which checks the host's digests on
both sides of the change. Feed messages must be contiguous and in order; a
swap below the buffered frontier, a late checkpoint, or more than 1,048,576
(`MAX_PENDING_FRAMES`) buffered frames is rejected.

Desktop: `Session` holds either a player `Connection` or a `Watcher` (a
control link plus a `Spectator`). Every control link checks a
`(local, peer)` role pair, with role bytes 0 (host), 1 (guest) and 2
(spectator). A receiver holds at most four complete messages; further chunks
wait, selectively acknowledged, in its receive window until the session
drains them. Each control message starts with a kind byte: 1 for JSON setup
messages (`Hello`, `Offer`, `Watch`, `Verified`, `Start`, `Refused` and the
`Swap` disk-change phases), 2 for the machine bundle, 3 for replacement disk
bytes and 4 for feed bytes. A spectator's setup is the guest's: bundle in,
`machine_identity` compared by the host, then the feed streams from frame
zero.

Direct UDP demultiplexes guest and spectator control packets by source
address on the host's existing socket (`SlotTable`, one slot per admitted
link, freed when it drops, with a separate cap for each kind; a host admits
at most eight spectators). A Windows `WSAECONNRESET` from an earlier send is
ignored, so a departed spectator cannot fail the players' link on the shared
socket. Internet mode keeps the host's iroh endpoint accepting after the
last player and classifies each connection by its capability: the
invitation's session admits players while ports remain, a separate random
capability in the `CLNS1.` code admits spectators, and neither opens the
other role. Each connection is pumped by its own task, so a spectator's
failure lands only in its own queues. The host services every link after its
own step: at most four feed messages queued per link, a `Head` keepalive
after a second of silence, a 10-second silence timeout, and any error drops
that link alone. Nothing a spectator does can stall or fail the players.

The frontend runs a spectator paced until it falls more than 25 frames
behind the host's confirmed frontier. It then runs unpaced bursts of up to
64 frames per loop pass, with live audio muted like a warp gate, until it is
back within 5 frames. Headless captures keep one frame per pass so a burst
cannot overshoot a scheduled screenshot. Window input is swallowed,
`App::mouse_port` is `None`, disk controls are unavailable, and F11 leaves.
A headless host that is about to exit keeps serving connected spectators
for up to five seconds (`Session::flush_spectators`), so its scheduled
capture does not cut their replay short.

Browser: a room host creates a separate watch room next to its player room
(`POST /watch` with all eight places, its own 22-character capability,
`#watch=` links) and enables the confirmed-history feed at frame zero, so
spectators can arrive at any time. Manual-code sessions have no room service
and no spectators. The panel shows the spectator invitation with its own QR
beside the player one until player 2 connects, then alone.

Spectator signaling runs the other way round. Each spectator reserves a
place (`/join`, which also issues its TURN credentials), offers over
`copperline-watch-v1` (ordered, reliable) plus the existing setup channel,
and polls for its answer every 2.5 seconds, backing off on HTTP 429. The
host polls `/offers` every two seconds while its machine runs, which also
extends the room's expiry. It answers each offer up to the places it holds
and refuses the rest (`/refuse`), since the service counts only places still
in signaling; a fetched answer frees the place. An expired room closes the
hub, which withdraws the invitation from the panel while the spectators
already admitted keep watching.

`SpectatorHub` keeps one `RtcWatchPeer` per spectator, describes the host
media once, serves it with `MediaTransfer`, requires a `Verified`
fingerprint equal to `WebEmu::netplay_identity`, then opens a feed cursor
(`spectator_feed_open`) and pumps `spectator_feed_take` in 16 KiB messages
under 256 KiB of buffered backpressure; a peer whose buffer never drains for
30 seconds is dropped. The spectator page boots the received media with
`start_spectating`, feeds bytes to `spectate_receive`, and `run_spectate`
paces it like a player or, more than 25 frames behind, replays the backlog
in bounded bursts (64 frames or 12 ms per call) with its audio discarded.
Every mutation and input entry point checks `session_active`, which covers
spectating.

## Configuration screen

`LauncherState::netplay` holds a `NetplaySetup` beside the machine setup. It uses
normal launcher rows, editing, hit testing and keyboard/gamepad navigation, but
never enters `RawConfig` or the machine serializer. Enabling it applies the
required controller and execution settings to `MachineSetup`; the ordinary
configuration pages show those changes. Its rows choose direct IP or Internet
mode and the relay, the player count (2..4) and this peer's role: a port
number (Host or Join in Internet mode) or, as the row's last choice,
Spectator (Watch in Internet mode). A host also sets how many spectators it
admits and can copy a separate spectator code.

Run commits any focused field, parses the connection through the shared session
ID/options validators, applies the deterministic RTC default and builds a cold
machine. Existing App-level control/GDB endpoints block netplay startup because
they survive machine replacement. Static validation (`validate_config`)
runs before construction. Among other host dependencies, it rejects
parallel-port host peripherals other than the passive four-player adapter,
including the sampler the frontend attaches later; the Toccata board, whose
serialized resampler map is not canonical; and the SF2000 SD controller,
whose board and ROM the setup manifest cannot carry. The session creates the
native transport before replacing the live machine, so a validation or
immediate bind failure leaves that machine intact and reports the error in
the launcher. The successful
session then uses the same `attach_netplay` path as a CLI launch. Session
code generation uses host randomness for a fresh identifier; it does not add
peer authentication.

F11 drops the session/socket, pauses the machine and restores the Netplay page
with the last connection settings. Runtime failures take the same path with an
error message. Run after that builds a new machine and rebinds the socket; it
never resumes an abandoned network timeline. Headless errors still return to
the caller normally.

## Validation

The regression suite covers:

- Zero and nonzero input delay, held-state prediction with zero predicted
  mouse motion, batched late arrivals, duplicate inputs, bounded stalls,
  recovery, and once-only local sampling, including pending mouse movement
  accumulated across stalls.
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
- Four complete emulators, one per controller port, connected through the host:
  each drives a different direction, every machine must show all four ports'
  switches, and all four must end with identical machine states, before and
  after a disk change that waits for every guest at each stage. A session
  without the adapter fitted is refused before it starts.
- UDP demultiplexing of guest and spectator links by source address, the
  port and spectator-place caps, and a host that admits only the guest
  addresses it names (a host's address list must name every guest or none).
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
  players; a host that ends its run and flushes the feed to a direct-UDP
  spectator; control-link role pairs and receive-window backpressure; iroh
  admission by capability; the launcher's Watch role and spectator code; the
  CLI flags; and a GUI spectator that joins late through the launcher,
  catches up unpaced and re-paces.
- Browser packet queue bounds, signaling validation, data-channel options,
  cancellation and backpressure. The release WASM smoke runs paired A500/PAL and
  A1200/NTSC machines through 120 confirmed/checksummed frames under loss,
  duplication, reordering and asymmetric pauses. Different volume, overscan and
  painting cadence must preserve checkpoints and produce identical final pixels.

Run the focused tests with `cargo test --profile ci --locked netplay`; UDP tests
need permission to bind loopback sockets. No external ROM or disk assets are
required for the regression suite. With a release build,
`tools/check-netplay.py` runs real player processes (`--players 2..4`, plus
`--internet` and `--relay-only` for relayed sessions) and, with
`--spectators N`, late-joining spectator processes, and requires identical
PNG captures from all of them.

After building the release web bundle, run `node tools/check-web-netplay.mjs` and
`npm test --prefix crates/copperline-web/www`. The WASM check also runs two
spectators that join at frames 60 and 200, replay the host feed in random
slices, ignore local input and match the host's picture at frame 300. CI
also runs the native web wrapper unit tests, including failed startup, input
routing and audio gain checks. `node tools/check-web-netplay-swaps.mjs`
exercises repeated replacements and ejections on real release WASM with
packet loss, reordering and asymmetric pacing, then checks continued state
agreement after each change. It also keeps one spectator following every
change and has a late joiner replay them all from frame zero. The browser
publishing workflow also checks the optimized bundle before copying all
netplay modules and the vendored QR license alongside the other page
modules. For a served local page, the optional Playwright check
`node tools/check-web-netplay-browser.mjs http://127.0.0.1:8000/` exercises actual
Host/Join, cancellation, reconnect by cold boot, locked controls and mismatch
rejection in two Chromium contexts. It also verifies that a device-keyboard tap
appears as a held key and subsequent release in transmitted input frames, and
that two-mouse sessions route DOM movement and button press/release events on
both pages while leaving keyboard joystick mode off. `CHROME_PATH` selects
an installed Chrome; `PLAYWRIGHT_MODULE` can point to a local Playwright
module.

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
checkpoints and identical machine snapshots. A third test admits a spectator
at frame 90 through its `CLNS1.` code and requires it to match the host's
snapshot at frame 150. These tests are opt-in because they depend on an
external service. The ordinary suite exercises encrypted loopback traffic,
capability rejection, invitation validation and launcher/CLI controls.
