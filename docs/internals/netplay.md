# Netplay implementation plan and architecture

The implementation follows four steps:

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

These steps are implemented in `src/netplay/`, `Emulator::step_netplay_frame`,
and `src/video/window/app_netplay.rs`. The feature uses native Rust and does not
link the GGPO SDK. Matchmaking, relay/NAT traversal, spectators, reconnect, shared
media operations, and transactional host filesystems remain separate work.

## Frame ownership

Network frame zero begins at cold boot. Each network frame ends when Agnus next
increments the emulated video-frame counter, at the first CPU instruction or
STOP fast-forward boundary after the wrap. This uses precise CPU stepping and
cycle accounting, independent of the ordinary frontend's CPU-budget quantum.
The scheduler state and transport remain outside serialized guest state; the
save-state version is unchanged.

An input contains eleven digital controller bits and a 128-key held-state bitmap.
Each peer owns one port. Key bitmaps are ORed, and transitions from the previous
merged bitmap are enqueued in raw-key order at the frame boundary. Controller
and keyboard prediction both repeat the most recent remote input at or before
that frame. Out-of-order future inputs never seed an earlier prediction.

A delayed local input is submitted only once, even when repeated polling stalls
on the same frame. Both timelines start with the negotiated number of neutral
delay frames. A frame can advance only while both remote input and the remote
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
the old timeline cannot replace the corrected image. Scheduled headless captures
wait for confirmation and local-input acknowledgement before rendering their
target, so they keep retransmitting inputs still needed by the other peer.

Only frames below the contiguous remote-input frontier are confirmed. A
checkpoint hashes the snapshot *after* its frame, once all inputs affecting it
are known. The engine drops confirmed snapshots, keeps one previous remote input
as its prediction seed, and releases acknowledged local inputs no longer needed
for replay. It retains eight recent checkpoint hashes. Snapshot storage has a
256 MiB cap; the configured prediction window bounds the number of snapshots.

## Wire protocol

`wire.rs` defines protocol version 1. Packets carry `CLNP`, protocol and
save-state versions, a 16-byte session ID, a 32-byte initial-machine fingerprint,
player index, handshake-ready flag, delay/window settings, cumulative input
acknowledgement, the latest confirmed checkpoint, and up to 32 input records.
Integers are little-endian. Records contain an eight-byte frame number, two-byte
controller bitmap, and sixteen-byte key bitmap. The maximum packet is 943 bytes.

The initial fingerprint hashes Copperline's display build version and the entire
normalized initial machine snapshot, including ROM and in-memory floppy data.
It does not fingerprint uncommitted source modifications: development peers must
build the same source. No executable, ROM, disk image, or serialized guest state
is received from a peer. An ID separates sessions; this protocol supplies neither
cryptographic peer authentication nor encryption.

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
It creates the UDP session before replacing the live machine, so a
validation or bind failure leaves that machine intact and reports the error in
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

- Zero and nonzero input delay, repeat-last prediction, batched late arrivals,
  duplicate inputs, bounded stalls, recovery, and once-only local sampling.
- Byte-identical replay against an uninterrupted 68000 workload that reads both
  JOYDAT registers and CIA fire inputs, writes RAM, and drives a display colour.
- Two complete emulators connected through local UDP proxies with deterministic
  loss, delay, duplication, reordering, and asymmetric pauses, with zero, default,
  and maximum input delay; both must confirm the same checkpoint and end with
  identical machine-state digests.
- Packet truncation/size bounds, conflicting inputs, invalid acknowledgements,
  initial mismatch, desynchronization, and disconnect timeouts.
- CLI combinations, GUI field/edit/navigation coverage, and frontend input/mutation
  routing. Two GUI-configured peers must connect, confirm matching states, return
  to setup and successfully rebind for another cold boot.

Run the focused tests with `cargo test --profile ci --locked netplay`; UDP tests
need permission to bind loopback sockets. No external ROM or disk assets are
required for the regression suite.
