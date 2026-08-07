# bsdsocktest conformance status

Tracks the current pass/fail/known-gap state of the external
[bsdsocktest](https://github.com/tbdye/bsdsocktest) conformance suite
against this project's `bsdsocket.library`, per PROPOSAL.md's Phase 4
"bsdsocktest run and gap triage" — both the default loopback tier and the
`NETWORK=1` tier (see `tests/bsdsocktest/`). Updated as fixes land; not
auto-generated.

**Last updated:** 2026-08-07. **Status:** suite runs to completion, both
tiers, and the new `net = "host"` backend (see its own section below).

```
# smoltcp backend (net = "loopback" / "nat" / "bridge"):
#   Loopback tier (default):   125 passed,  1 failed, 0 known, 16 skipped (142 total)
#   NETWORK=1 (both tiers):    135 passed,  3 failed, 0 known,  4 skipped (142 total)
# host backend (net = "host"), after fixing a connect() livelock, five
# real gaps (shutdown, MSG_PEEK, sendmsg/recvmsg, FIONREAD,
# SO_EVENTMASK/GetSocketEvents), and one real half-gap (send/recv(MSG_OOB),
# see the table below for why only half of it panned out):
#   Loopback tier only:        126 passed,  2 failed, 0 known, 14 skipped (142 total)
#   NETWORK=1 (127.0.0.1, no NAT indirection needed):
#                               135 passed,  5 failed, 0 known,  2 skipped (142 total)
#   -- test 41 (inbound accept from a remote host) passes here, unlike
#      the smoltcp/NAT backend, where it's a permanent structural skip.
#   -- test 27 (recv(MSG_OOB): urgent data delivery) also passes here,
#      unlike the smoltcp backend, where it's a permanent structural skip.
#   -- the failures are the pre-existing ICMP knock-on effect (132, and
#      133-135 under NETWORK=1) plus test 64 (WaitSelect exceptfds), which
#      turned out to be a *different*, unfixable gap than test 27 despite
#      looking related -- see the MSG_OOB row in the table below.
```

("0 known" is expected, not a red flag — bsdsocktest's own known-failure
annotations in `known_failures.c` are keyed by version string against a
handful of *real* stacks (Roadshow, Amiberry/UAE, WinUAE); this library
reports its own version string, so none of those entries match
automatically. The [Compatibility](#compatibility) section below is the
manual equivalent of that lookup for this implementation.)

## How to reproduce

```sh
cd tests/bsdsocktest
COPPERLINE=/path/to/copperline \
KICK=/path/to/kickstart-3.1.rom \
BSDSOCKTEST_DIR=/path/to/bsdsocktest-checkout \
BENCH=1800 \
./run.sh
```

See `tests/bsdsocktest/run.sh` and `machine.toml`'s own header comments
for prerequisites and design notes.

## Hangs fixed to reach completion

Each of these blocked the suite indefinitely (no `# Results:` line ever
appeared, regardless of `BENCH`) until fixed. Listed in the order they
were found running the suite forward for the first time.

| # | Symptom | Root cause | Fix |
|---|---|---|---|
| 1 | Hung forever on test 11 (`connect(): TCP to loopback listener`) | `bsdsocktest` connects to literal `127.0.0.1`, but the interface only recognized its own configured address | Added `127.0.0.1/8` to the interface's own address list (`crates/hostsocket-plugin/src/lib.rs::init`) |
| 2 | Hung on test 12 (`connect(): ECONNREFUSED to closed port`) | `do_connect`'s retry check used `!socket.is_open()` to mean "first attempt" — also true for a *closed-via-RST* socket, so the guest's retry silently restarted a fresh connect() attempt whose new pending state was never re-registered for a wakeup | Track `connect_started` explicitly on the fd instead of inferring it from socket state |
| 3 | Every errno-checking test failing (not a hang, but the mechanism bsdsocktest actually uses for errno) | `SocketBaseTagList`/`SocketBaseTags` was a total stub — `open_bsdsocket()`'s errno-pointer registration silently did nothing | Implemented `SBTM_SETVAL(SBTC_ERRNOLONGPTR)` |
| 4 | Hung on test 25 (8192-byte transfer) | `do_send` did one `send_slice()` and returned a short count even in blocking mode, instead of real BSD blocking-`send()` semantics (block until everything's queued) | Made blocking `send()` retry via the same `RES_PENDING` + wait-registration shape as `connect()`/`recv()`, tracking progress across retries (`send_progress`) |
| 4b | Same test, after 4 | `SOCKET_BUF_LEN` (4096) was smaller than bsdsocktest's largest single-shot transfer (8192), and this project has no real multitasking of guest tasks — a same-task send-then-recv sequence deadlocks if the buffer can't hold the whole transfer | Raised `SOCKET_BUF_LEN` to 16384 |
| 5 | Hung on test 26 (`recv(MSG_PEEK)`) | `flags` was staged by the guest trampoline but never read on the Rust side — every `recv()` consumed data regardless of `MSG_PEEK`, so the test's second (real) `recv()` found nothing left and blocked forever | Wired `flags` through; `MSG_PEEK` now uses smoltcp's `peek_slice` instead of `recv_slice` |
| 6 | Hung on test 35 (`send(): error after peer closes connection`) | `do_close` called `tcp::Socket::close()` (which only flips internal state) then immediately `sockets.remove()` in the same call, before any `iface.poll()` ever ran — the FIN was never actually transmitted, so the peer's socket just sat in `Established` forever | Defer removal: orphaned-but-not-yet-`Closed` sockets go on `closing_sockets` and get reaped in `tick()` once their close handshake genuinely finishes |
| 7 | Hung on test 52 (`SO_ERROR: pending error after failed connect`) | `scan_select`'s write-readiness check was just `can_send()`, which is false both while still connecting *and* after a failed connect — real `select()`/`WaitSelect()` write-readiness means "connect() has concluded, successfully or not" | Write-ready now also fires once `!may_send()` outside of `SynSent`/`SynReceived` |
| 8 | Hung on test 61 (`WaitSelect(): timeout fires when idle`) | `do_wait_select` recomputed a new deadline (`now + timeout`) from scratch on every retry instead of persisting the one from the first call — each near-instant recheck reset the clock before ever reporting a real timeout | Deadline is now computed once and persisted per-task (`wait_select_deadline`), reused on retries |
| 9 | Hung on test 62 (`WaitSelect(): NULL timeout blocks until activity`) | `scan_select`'s read-readiness check used ordinary TCP data-availability semantics (`can_recv()`/`may_recv()`) for every fd, including listeners — but a listener's own "ready" signal is "a connection arrived" (`!is_listening()`), which `do_accept` already knew and `scan_select` didn't | Read-readiness on a listener fd now checks `!is_listening()` instead of data availability |
| 10 | Hung on test 66 (`WaitSelect(): Amiga signal interruption`) | The `signals` (6th) `WaitSelect` argument — a real Amiga signal mask — was staged by the guest trampoline but never read on the Rust side at all, so a self-signal sent before `WaitSelect()` was silently ignored and the call just blocked on fd readiness that would never come | Read the calling task's own `tc_SigRecvd` directly out of its `struct Task` in Amiga memory (`task_sig_recvd`, offset verified via the real NDK toolchain's `offsetof`) and check it against the requested mask at call time. **Known remaining gap:** only checked at call time / on retry, not while already parked in the guest's own private `Wait()` — a signal arriving strictly *after* blocking has started won't wake it early. Not exercised by bsdsocktest's own loopback-tier tests, so left as documented rather than architected around. |
| 11 | Hung (really: silent wasm panic) on test 139 (`Throughput: UDP loopback`) | `scan_select` always called `sockets.get::<tcp::Socket>(...)`, regardless of the fd's real kind — smoltcp's `SocketSet::get::<T>` panics on a type mismatch, so the *first* `WaitSelect()` call naming a UDP fd trapped the whole wasm module. No `REG_RESULT` is ever written after a trap, so the guest just sits in `Wait()` forever — indistinguishable from an ordinary hang from the TAP log alone. | `scan_select` now branches on `slot.kind`, using `udp::Socket::can_recv()`/`can_send()` for UDP fds instead of unconditionally treating every fd as TCP |
| 12 | Hung on test 11 again, but *only* under `NETWORK=1` (`net = "nat"`) — the exact same test that hang #1 already fixed, passing reliably under the default `net = "loopback"` for the entire rest of this table | This interface has no dedicated loopback device separate from its one real one — 127.0.0.1 is just an extra address on the same Ethernet-medium interface `INTERFACE_ADDR` lives on. Under `net = "loopback"` that's invisible (Copperline's Loopback backend echoes back *everything* transmitted, any destination, so 127.0.0.1 "worked" as a side effect). Under `net = "nat"`, Copperline's real NAT engine only answers ARP for its own gateway/DNS addresses — a self-connect to 127.0.0.1 needs to ARP-resolve 127.0.0.1's own hardware address first (ordinary Ethernet on-link behavior smoltcp applies uniformly), that request goes unanswered forever, and smoltcp never even attempts to send the real TCP SYN: stuck on ARP resolution alone, invisible in the loopback tier because nothing there ever exercises a real backend. | Gave `HostDevice` a real internal loopback path, the way an actual OS kernel's loopback interface works: `TxTok::consume` intercepts (via `loopback_response`) both IPv4 traffic addressed to 127.0.0.0/8 (queues the same frame back) *and* ARP requests targeting that range (synthesizes a real ARP reply from the interface's own `INTERFACE_MAC`), instead of ever handing either to the real backend. `receive()` drains that queue before ever calling `net_recv`. |

## NETWORK tier (`NETWORK=1`)

```sh
NETWORK=1 \
COPPERLINE=/path/to/copperline \
KICK=/path/to/kickstart-3.1.rom \
BSDSOCKTEST_DIR=/path/to/bsdsocktest-checkout \
./run.sh
```

Starts a real Python host helper (`BSDSOCKTEST_DIR/host/bsdsocktest_helper.py`)
and swaps the plugin's `net = "loopback"` for `net = "nat"` via a temporary
manifest copy (`run.sh` never edits the real one), giving the guest real
outbound access through Copperline's own userspace NAT engine. `HOST
10.0.2.2` (that engine's own gateway address, mapped onto the host's
127.0.0.1) is baked into the generated startup-sequence, dropping the
tier filter entirely so both tiers run in one pass. `run.sh`'s own
`NETWORK=1` comment has the full design rationale, matching
`../copperline/run.sh`'s established pattern for the same kind of
opt-in-only real-networking test.

**Two tests cannot pass under this setup, permanently** — both a
Copperline-level NAT engine design decision, not something fixable from
this project's plugin:

- Test 41 (`accept(): incoming connection from remote host`) needs the
  helper to connect *into* the Amiga, and Copperline's NAT engine has no
  inbound path at all yet (`src/net/nat/mod.rs`'s own doc comment: "no
  host port forwards yet"). It fails cleanly after a real 5-second
  `WaitSelect` timeout rather than hanging.
- Test 136 (`ICMP echo: timeout on unreachable host`, RFC 5737 TEST-NET-1,
  192.0.2.1) expects silence/timeout for a genuinely unroutable address.
  Copperline's NAT engine answers *every* ICMP echo request locally and
  unconditionally as a liveness check (`src/net/nat/frames.rs`'s own
  `icmp_echo_reply`, confirmed by reading that source directly: "The
  gateway has no raw-socket path to really ping for the guest, so like
  classic slirp a reply only proves the NAT is alive" — no destination
  check at all, an existing Copperline unit test
  (`icmp_echo_to_an_external_address_is_answered_locally`) codifies
  exactly this), so a reserved/unroutable target gets an instant reply
  instead of silence. This is invisible under the default `net =
  "loopback"` backend (test 132 catches self-ping there, and a bounced-
  back *request* — not a synthesized reply — correctly fails the
  test's own `type == ECHO_REPLY` check), and only surfaced once this
  project's own ICMP support (tests 132–135, see the table below) made
  `socket(AF_INET, SOCK_RAW, IPPROTO_ICMP)` work for real.

**Real network round trips now genuinely pass**, not just skip: 64KB TCP
integrity over NAT (test 39), UDP over NAT (40), NAT-routed throughput
benchmarks (138, 140, 142) — all real data through the host helper's TCP
echo/UDP echo/sink/source services.

Running the full test list with a host helper attached for the first
time (rather than 16 of 142 tests silently skipping) surfaced three more
real bugs — none of them actually NETWORK-tier-specific once found; all
three reproduce under the plain loopback tier too and are already fixed
(see the table below): a `WaitSelect` `nfds` off-by-one (69), a stale
`SO_ERROR` after a *successful* non-blocking connect (71), and the
`gethostbyaddr`/`getservby*`/`getprotoby*`/`getnetby*` NULL-vs-`-1`
stub-return bug (92, 93, 94, 95, 96, 97, 101, 102, 104 — nine tests at
once from one small guest-side fix).

## Host backend (`net = "host"`)

A second, structurally different backend: instead of terminating TCP/IP on
this plugin's own embedded smoltcp stack, each socket operation delegates
straight to a real host OS socket (the Amiberry/WinUAE approach). Run
against the loopback
tier the same way as the table above, same rig (`net = "host"` swapped in
for `net = "loopback"` in `machine.toml`'s `[hostsocket]` section, nothing
else changed).

**First run: 111 passed, 15 failed, 16 skipped, then a real livelock.**
The very first run never got past test 11 — the guest hung forever in
`Wait()` on test 12 (`connect(): ECONNREFUSED to closed port`), the exact
same symptom (and the exact same test) as the smoltcp path's own
long-since-fixed hang #2 above. Root cause here was different but
adjacent: `process_waiters`'s `WaitKind::Connect` readiness check for a
host-backed fd only treated `sock_poll`'s `WRITABLE`/`ERROR` bits as
"go recheck" — but a refused loopback connect's own `poll(2)` result
turned out to be platform-dependent (macOS reports only `POLLHUP` for
this exact case, which this module classifies as `READABLE`, not
`WRITABLE`/`ERROR`), so the waiter was never woken and the guest sat in
`Wait()` forever. Fixed by treating *any* readiness bit as "go recheck" —
safe because `do_connect_host`'s own retry re-issues `sock_connect` and
trusts nothing from `sock_poll` itself; that call is the only thing that
determines the real outcome. Once fixed, the suite runs to completion.

**After the livelock fix: 111 passed, 15 failed, 16 skipped.** Triage,
and four of these turned out to be real, fixable gaps rather than
deliberate scope decisions:

| Tests | What | Verdict |
|-----:|------|---------|
| 16, 17, 18 | `shutdown(SHUT_RD/SHUT_WR/SHUT_RDWR)` | **Real gap, fixed.** `do_shutdown` had no host-backed branch at all (fell through to plain `ENOTSOCK`) — never exercised by this project's own hand-written tests, which never called `Shutdown()`. Added `sock_shutdown` (a real `socket2::Socket::shutdown()`) and `do_shutdown_host`. Verified with a native test that checks the *peer* observes a real EOF, not just that the call returns success (`hostsocket_plugin_host_backend_shutdown_write_reaches_the_real_peer`). |
| 26 | `recv(MSG_PEEK)` | **Real gap, fixed.** Added `sock_peek` (a real, non-consuming `socket2::Socket::peek()`) and wired it into `do_recv_host`/`do_recvmsg_host`. Verified by reading the *same* bytes twice — once via `MSG_PEEK`, then via a plain `recv()` right after — which only works if the peek genuinely left the data in the kernel's own receive buffer (`hostsocket_plugin_host_backend_recv_msg_peek_does_not_consume`). |
| 31, 32 | `sendmsg()`/`recvmsg()` | **Real gap, fixed.** `do_sendmsg`/`do_recvmsg` never got a host-backed branch. `do_send_host` was refactored into a shared `send_host_stream` helper (the host-backend counterpart of the smoltcp path's own `send_tcp_stream`) so `do_sendmsg_host` could reuse its blocking/partial-progress logic directly; `do_recvmsg_host` scatters a single `sock_recv`/`sock_peek` call's result across the caller's iovecs, same shape the smoltcp version already uses. Verified with a real one-iovec-each round trip (`hostsocket_plugin_host_backend_sendmsg_recvmsg_round_trip`). |
| 56 | `IoctlSocket(FIONREAD)` | **Real gap, fixed.** Added `sock_nread` (a real `ioctl(fd, FIONREAD, &n)`, unix only) and wired it into `do_ioctl_socket`'s host branch. Verified against the real pending byte count after a peer send, not a placeholder (`hostsocket_plugin_host_backend_ioctl_fionread_reports_pending_bytes`). |
| 132 | ICMP echo: loopback | **Not a host-backend bug** — confirmed by running `bsdsocktest CATEGORY icmp` in isolation under `net = "host"`, which passes cleanly (`RTT=0.002ms`). ICMP was never routed to the host backend at all (`do_socket_host` only ever creates `Tcp`/`Udp`; a raw ICMP socket always goes through the same smoltcp path `net = "loopback"` uses, unmodified) — this is a knock-on effect from state one of the other failing tests leaves behind over the course of the full 142-test run, not a direct regression. Reproduced identically both before and after the four fixes above, so it isn't one of those four either. Not chased further: isolating which specific remaining failure causes it would take real additional investigation for a test that already passes on its own. |
| 79, 80, 82, 83, 84, 85 | `SO_EVENTMASK`/`GetSocketEvents()` family | **Real gap, fixed.** `do_get_socket_events` itself (`GetSocketEvents()`) turned out to already be fully fd-agnostic -- it only ever reads the calling task's own `event_queues` entry, never touches `fds`/`host_fds` directly -- so the only work was getting `process_socket_events` to populate that queue for host fds too. Added `sample_event_level_host` (the host-backend counterpart of `sample_event_level`, built on a real `sock_poll` instead of smoltcp socket state), `HostFdSlot::is_listener` (set by `do_listen_host`, needed to tell a pending-accept listener apart from an ordinary readable fd the same way `sample_event_level` already does for smoltcp), and `HostSockOpts::eventmask`/`ev_prev` (mirroring `SockOpts`'s own fields) plus matching `SO_EVENTMASK` handling in `do_setsockopt_host`/`do_getsockopt_host`. The one real gap `sock_poll` had for this: no way to tell "there is real data" apart from "the peer hung up, a `recv()` here would just return EOF" without actually consuming anything -- needed for the `FD_CLOSE` edge (`may_recv` going false). Fixed by giving `sock_poll` a fourth bit, `SOCK_HUP` (set alongside `SOCK_READABLE` specifically on a real `POLLHUP`, or a zero-length `peek()` on the non-unix fallback), rather than folding hangup into plain readability the way it was before. `process_socket_events` itself gained a parallel `host_fds` arm (same edge-detection arithmetic, different sampling function and `opts` root) rather than being rewritten -- keeps the smoltcp arm's own already-verified logic untouched, consistent with every other `do_*_host` sibling function this backend has added. Verified against a real bsdsocktest run, not just native tests: all six tests pass. |
| 87 | `WaitSelect` + signals stress test | Same class of gap as the smoltcp path's own documented signal-interruption limitation (hang #10 above): a signal delivered strictly *after* the guest has already parked in `Wait()` isn't observed until the next retry. Not specific to the host backend. |
| 27 vs. 64 | `send`/`recv(MSG_OOB)` vs. `WaitSelect(exceptfds)` | **Half real gap, fixed; half tried and abandoned.** These look like the same feature (both gated behind bsdsocktest's own "MSG_OOB not supported" skip), but turned out to need two structurally different mechanisms. `send`/`recv(MSG_OOB)` (test 27): a real gap, fixed -- added `sock_send_oob`/`sock_recv_oob` (`socket2::Socket::send_out_of_band`/`recv_out_of_band`, real `send(2)`/`recv(2)` with `MSG_OOB`) and wired them into `do_send_host`/`do_recv_host`, blocking via a new `WaitKind::RecvOob` when nothing's pending yet. Genuinely works: a real host TCP socket supports this even though smoltcp's own `socket::tcp` has no urgent-pointer support to build it on at all (the same permanent gap `do_send`'s own `MSG_OOB` rejection already documented) -- one more case, like test 41, of the host backend doing something the smoltcp one structurally cannot. `WaitSelect(exceptfds)` (test 64): tried, found unreliable, reverted. The natural approach -- give `sock_poll` a `POLLPRI` bit so `scan_select` could report except-readiness non-consumingly -- was implemented, passed a hand-written native test... and then failed the real bsdsocktest run. Root-caused with a standalone `libc::poll` repro outside the whole plugin stack: on this project's own macOS dev host, `poll(2)` *never* reports `POLLPRI` for a genuine `MSG_OOB` send, in either direction (client→server or server→client), blocking or non-blocking -- but *did* report it once, spuriously, exactly coincident with an unrelated `POLLHUP` (the peer socket closing at the same instant), which is what made the native test pass: it was catching a connection-teardown artifact, not real urgent-data detection. Backed the `POLLPRI`/`SOCK_OOB` plumbing out entirely rather than ship a signal proven to fire on the wrong condition; `WaitSelect()`'s `exceptfds` on this backend now always reports empty, matching the smoltcp path's own permanent gap here. `WaitKind::RecvOob`'s own wakeup (needed for a *blocking* `recv(MSG_OOB)`, unaffected by this) was changed to not depend on `POLLPRI` either -- any readiness bit is treated as "go recheck" (same reasoning as the connect() livelock fix above), which in practice means "recheck every tick" since a connected socket's own writable bit is normally always set. |

**After all five fixes plus the `MSG_OOB` half-fix: 126 passed, 2 failed,
0 known, 14 skipped (142 total)** in the loopback tier -- two better than
the smoltcp backend's own loopback tally (test 27 now a genuine pass
instead of a skip), with one new genuine failure (64, an honest "doesn't
work" rather than a skip) alongside the pre-existing ICMP knock-on (132).
The signals stress test (87) also now passes (unclear whether that's
causally related to the `SO_EVENTMASK` fix or just timing-sensitive, same
caveat this doc's own test-81-before-the-fix note above already flags for
this class of test -- not chased further, since it's a pass either way).

### `NETWORK=1` (real outbound *and inbound* access)

Run against the Python host helper the same way as the smoltcp path's own
`NETWORK=1` tier, but simpler to set up: no NAT indirection to swap in at
all (`net = "host"` gives the guest the host's own real network identity
directly), so `bsdsocktest HOST 127.0.0.1 NOPAGE` just works —
`127.0.0.1` *is* the same machine the helper is listening on.

**Result (re-run after all five fixes plus the `MSG_OOB` half-fix above):
135 passed, 5 failed, 0 known, 2 skipped (142 total).**

**Test 41 (`accept(): incoming connection from remote host`) passes.**
This is the headline result: under the smoltcp/NAT backend this test is a
*permanent* skip (documented in the "NETWORK tier" section above --
Copperline's NAT engine has no inbound path at all, "no host port
forwards yet"). Under `net = "host"` that limitation doesn't exist at
all: a `bind()`/`listen()`/`accept()` on a host-backed fd is a real host
port, so the helper's own "connect back to whatever address the Amiga's
control connection came from" mechanism reaches it directly. A case
where this backend does something the smoltcp one structurally cannot.

**Test 27 (`recv(MSG_OOB)`) also passes here**, another case the smoltcp
backend can't match -- see the `MSG_OOB` row above for the full story,
including why its sibling test 64 (`exceptfds`) does *not* similarly pass
despite looking like the same feature.

The remaining 2 skips are backend-independent structural gaps already
known: `FIOASYNC` (57, redundant with `SO_EVENTMASK`) and the fd>31
WaitSelect wire-protocol ceiling (70) -- neither specific to this
backend, both already covered in the [Compatibility](#compatibility)
section's own account of the smoltcp path's identical gaps.

The 5 failures are the loopback-only run's own ICMP failure (132) and
`exceptfds` failure (64) plus 3 newly-attempted network-tier ICMP tests
(133, 134, 135 -- previously honest `SKIP`s with no helper connected, now
real attempts). All four ICMP tests fail together in both runs
(loopback-only *and* this one), while `bsdsocktest CATEGORY icmp` in
isolation passes cleanly under `net = "host"` either way -- consistent
with the loopback-only run's own finding that this is cumulative state
from one of the other failing tests, not a direct ICMP-over-host-backend
bug. Still not chased further without isolating the actual source.

## Fixed after the initial triage

Real (non-hanging) bugs from the first `not ok` triage, fixed and
verified against a fresh run each time.

| Test(s) | Root cause | Fix |
|---|---|---|
| 61, 63 (`WaitSelect` timeout duration) | Two separate bugs. First, `timeout` was parsed as a single raw 4-byte microsecond count instead of a real 8-byte `struct timeval` (`tv_secs`/`tv_micro`) — fixed by reading both fields and computing `secs * 1_000_000 + micro`. Second, once that was fixed, timeouts still fired ~3.5x too fast: `self.micros` (the clock `do_wait_select`'s deadline math runs on) accumulates raw PAL colour-clock ticks from `tick()`'s `cck` argument (~3.546895 MHz, Copperline's own `config::COLOR_CLOCK_HZ`), not real microseconds, despite the field's name — fine for smoltcp's own internal timers (only relative progress matters there), wrong once a *real* wall-clock duration from the caller needs comparing against it. | Real `struct timeval` parsing, plus converting the requested duration through `CCK_HZ` before adding it to `self.micros` (`do_wait_select`'s own comment has the exact numbers: a 1-second timeout was elapsing in ~280ms real time before this, `1,000,000 / 3,546,895 ≈` that same ratio). Test 61 now measures exactly 1000ms for a requested 1-second timeout. |
| 88 (`gethostbyname("localhost")`) | `do_gethostbyname` always issued a real DNS query, with no hosts-file or `"localhost"`-specific handling at all — but `"localhost"` resolves locally on every real system (glibc, BSD libc, and AmigaOS stacks alike special-case it at the resolver level, independent of any actual hosts file), so a query for literally `"localhost"` was never going to succeed against a real DNS server anyway. | `do_gethostbyname` now special-cases `"localhost"` (case-insensitive) to resolve directly to 127.0.0.1 via the existing `write_hostent`, without ever touching the DNS query path. |
| 122 (`SetErrnoPtr`, 1-byte) | `Board::set_errno`'s size handling only special-cased `size == 2`; a 1-byte request fell into the 4-byte write path, writing errno's first byte (usually 0) into the 1-byte target and overrunning 3 bytes past it. | Added a real `size == 1` branch that writes a single byte. |
| 4, 5, 120 (`socket()` validation) | `do_socket` never read `domain` at all and treated any `type` other than `SOCK_DGRAM` as TCP, accepting literally anything (`socket(-1, -1, -1)` succeeded). Test 120 shares this root cause: it calls `socket(-1,-1,-1)` expecting failure, so its own assertion never got past `fd < 0`. | `do_socket` now validates `domain == AF_INET` and `type` ∈ {`SOCK_STREAM`, `SOCK_DGRAM`, `SOCK_RAW`}, rejecting anything else with `EINVAL`. |
| 6 (`bind()` port 0 → ephemeral) | `do_bind` just recorded whatever port was requested, including a literal 0, instead of resolving it to a real port the way `alloc_local_port()` (already used by `connect()`/`listen()`'s own auto-bind) does. | `do_bind` now resolves a requested port of 0 through `alloc_local_port()` before recording it. |
| 8 (`bind()` EADDRINUSE) | `do_bind` never checked whether another TCP fd already had the requested port bound. | Added a scan over `self.fds` for a conflicting `bind_port` before accepting a new bind. |
| 21 (`getsockname()` returns bound address) | `do_getsockname` always reported the interface's own fixed address for an unconnected TCP socket, regardless of what `bind()` actually recorded — a specific (non-wildcard) `bind()` and a wildcard one were indistinguishable afterwards, since only the *port* was ever stored. | Added `FdSlot::bind_addr`, recorded by `do_bind` whenever a non-`INADDR_ANY` address is given, and consulted by `do_getsockname` before falling back to the interface's own address. |
| 139 (UDP loopback throughput, 96% loss even though the test itself already passed) | UDP has no flow control, so a real burst of datagrams (bsdsocktest's own test fires 200 `sendto()`s in a tight loop before ever calling `recv()`, same as real UDP senders routinely do) all lands in the receive queue at once — 8 metadata slots and a 16KB byte buffer (shared with TCP's `SOCKET_BUF_LEN`) meant only 8 of 200 survived. | Split UDP onto its own `UDP_BUF_LEN` (256KB) and raised `UDP_META_SLOTS` to 256, sized for that one test's burst rather than any specific real-world requirement. Loss dropped from 96% to 0%. |
| 56 (`IoctlSocket(FIONREAD)`) | Unimplemented — `do_ioctl_socket` only handled `FIONBIO`. | Added a real `FIONREAD` branch reporting `recv_queue()` (TCP and UDP both expose this on smoltcp's own socket types). |
| 43, 45, 47–54 (`setsockopt`/`getsockopt`, all 12 tests) | `do_setsockopt` never even read `optname` — every option silently "succeeded" while doing nothing, so nothing survived a set/get roundtrip. `SO_ERROR` (52) needed a second, genuinely different fix: `getsockopt(SO_ERROR)` read `task.last_errno`, which a non-blocking `connect()` sets to `EINPROGRESS` — nothing ever updated it to the real `ECONNREFUSED` once the connect failed asynchronously and was only observed via `WaitSelect()`'s write-readiness rather than a `connect()` retry (`do_connect`'s own errno handling only runs on that retry path). | Added `FdSlot::opts` (`SockOpts`) with real per-fd roundtrip storage for `SO_REUSEADDR`/`SO_KEEPALIVE`/`SO_LINGER`/`SO_RCVTIMEO`/`SO_SNDTIMEO`/`SO_RCVBUF`/`SO_SNDBUF`/`TCP_NODELAY`/`SO_TYPE` — none of these change smoltcp's actual behavior, matching bsdsocktest's own stated scope for several ("Set/get roundtrip only... enforcement cannot be safely tested," its own `SO_RCVTIMEO` test comment). For `SO_ERROR`: added `record_connect_completion_errors`, called wherever `do_wait_select` reports write-readiness, which records `ECONNREFUSED` for any write-ready TCP fd found `Closed` after a `connect()` was issued on it. |
| 69 (`WaitSelect` `nfds` boundary) | `scan_select`'s fd loop was `1..=nfds` (inclusive) — real select()/WaitSelect() semantics define `nfds` as "highest fd + 1", so the valid scan range is fds strictly *below* it. A caller deliberately passing a too-low `nfds` to exclude a specific fd (bsdsocktest's own test does exactly this) still got that fd checked anyway. | Changed the loop bound to `1..nfds.min(MAX_FDS as u32 + 1)` (exclusive of `nfds`) in both `scan_select` and `record_connect_completion_errors`, which had the identical bug. |
| 71 (`WaitSelect` non-blocking connect completion, `SO_ERROR` after *success*) | `record_connect_completion_errors` (added for test 52's fix above) only ever recorded `ECONNREFUSED` on failure — a *successful* non-blocking connect left the earlier `EINPROGRESS` sitting in `last_errno` forever, so `getsockopt(SO_ERROR)` read back "still connecting" instead of "no error" for a connection that had already fully completed. | Also clears the error to 0 when a connecting socket's state reaches `Established`, not just recording one on `Closed`. |
| 92, 93, 94, 95, 96, 97, 101, 102, 104 (`gethostbyaddr`/`getservby*`/`getprotoby*`/`getnetby*`, nine tests at once) | All backed by the generic `_hs_stub` guest trampoline, which returns `-1` in `d0` — correct for the plain-`LONG`-returning stubs sharing it, but every one of these actually returns a *pointer* (`struct hostent`/`servent`/`protoent`/`netent` *), where real BSD "not found" means `NULL` (`d0 = 0`), not `-1`. The caller treated `-1` as a garbage non-null pointer, so tests checking specific field values saw real-looking-but-wrong data instead of a clean "not found" (test 93's own `tap_ok(s == NULL, ...)` failed outright on this) — found running the full suite with a host helper attached for the first time, since the loopback-tier's own use of these functions never checks field values, only "did it crash" (which `-1`-as-a-pointer never did either). | Added a second guest trampoline, `_hs_stub_null` (`d0 = 0`), and rewired these seven LVOs to it. `_hs_gethostbyname`'s own existing NULL-on-failure path was already doing the right thing — this just brings the rest of the pointer-returning stubs in line with it. |
| 79, 80, 82, 83, 84, 85, 87 (`SO_EVENTMASK`/`GetSocketEvents`, seven tests at once) | Neither piece of AmiTCP's asynchronous event-notification mechanism existed: `SO_EVENTMASK` (a `setsockopt()` option selecting which `FD_ACCEPT`/`FD_CONNECT`/`FD_READ`/`FD_WRITE`/`FD_CLOSE` conditions to report) wasn't recognized at all, `SBTC_SIGEVENTMASK` (which Amiga signal to deliver events on, via `SocketBaseTags`) was silently ignored, and `GetSocketEvents()` (drains one pending `(fd, event mask)` pair, or -1) was `_hs_stub`. | Added real per-fd `SO_EVENTMASK` storage (`SockOpts::eventmask`) and per-task `SBTC_SIGEVENTMASK` storage (`TaskState::sig_event_mask`), plus a new `process_socket_events` pass (called every `tick()`, right before `process_waiters`) that edge-detects `FD_ACCEPT`/`FD_CONNECT`/`FD_READ`/`FD_WRITE`/`FD_CLOSE` transitions per fd — reusing the same readiness rules `scan_select` already uses for `WaitSelect()`, sampled once per tick and diffed against the previous tick's readiness (`SockOpts::ev_prev`, seeded at `setsockopt()` time so an already-ready socket doesn't fire a spurious event the moment its mask is set — bsdsocktest's own `eventmask_no_spurious` test, 81, checks exactly this and already passed by coincidence before this fix, for the opposite reason: nothing ever fired). Matching events land in a per-task FIFO queue (`Board::event_queues`, coalesced by fd) and wake the registered signal through the existing `wake_queue` mechanism; `GetSocketEvents()` (`CALL_GETSOCKETEVENTS`, a new RPC — never blocks, matching the real "poll, don't wait" API) just dequeues one entry. `FD_OOB`/`FD_ERROR` are intentionally never generated (no urgent-data modeling, same gap as test 64's `exceptfds`; no separate "async error" source beyond what `SO_ERROR` already reports). |
| 74, 75, 76, 77, 78, 128 (`SocketBaseTagList`'s remaining tags, six tests at once) | `SBTM_GETREF` (GET) wasn't implemented for *any* tag, and `SBTC_BREAKMASK`/`SBTC_DTABLESIZE` were silently ignored even for SET. `getdtablesize()` (128) shares this root cause: it's specifically checking that a prior `SBTC_DTABLESIZE` SET is reflected. | Added real GET(REF) *and* SET for `SBTC_BREAKMASK` (`TaskState::break_mask`, pure roundtrip storage — nothing delivers a real Ctrl-C break signal today) and `SBTC_DTABLESIZE` (`Board::reported_dtablesize`, monotonic — only ever grows on a SET, matching bsdsocktest's own "Restore (may not reduce)" expectation for it; `do_getdtablesize` now reports this instead of a hardcoded `MAX_FDS`, though the real `fds` array stays fixed-size). Added GET for `SBTC_SIGEVENTMASK` (reads back `TaskState::sig_event_mask`, the storage this session's `SO_EVENTMASK` work above already added). `SBTC_ERRNOLONGPTR`/`SBTC_HERRNOLONGPTR`'s GET needed a different kind of fix: real guest RAM the library owns to point at, since a GETREF caller expects a real address it can dereference directly without ever calling `Errno()` again — added two 4-byte scratch LONGs to the guest ROM's own library base (`LIB_ERRNO_SLOT`/`LIB_HERRNO_SLOT`, entry.s), passed down on every `SocketBaseTagList()` call so the plugin can auto-register the errno one (`do_set_errno_ptr`) if a task hasn't already supplied its own via an earlier SET. (amitcp/socketbasetags.h's own comment calls `SBTC_ERRNOLONGPTR` "SETTING (only)", and this project's Compatibility doc used to cite Roadshow sharing that same GET gap — bsdsocktest's own test only checks the GETREF result is non-null, though, so answering it for real seemed better than leaving a real stack's own limitation un-chased forever.) |
| 98 (`gethostname()`) | `_hs_stub` — not implemented at all. | Added a real, configurable hostname (`[config] hostname` in the manifest, defaulting to `"amiga"` — same `config_get_string` pattern `dns_server` already uses), written into the caller's buffer and truncated to fit if needed. `gethostid()` (test 100, already passing by coincidence — `_hs_stub`'s `-1` return happens to satisfy "non-zero") got a real implementation too while touching this LVO family: returns `INTERFACE_ADDR`, matching real BSD systems' historical convention of using the primary interface's own address as the host ID. |
| 89 (`gethostbyname()` sets h_errno on failure) | h_errno was never wired up at all — `SBTC_HERRNOLONGPTR` only had a GET half (added alongside the `SocketBaseTagList` fix above), but bsdsocktest's own `open_bsdsocket()` registers its h_errno storage via **SET** (`SocketBaseTags(SBTM_SETVAL(SBTC_HERRNOLONGPTR), &bsd_h_errno, ...)`, testutil.c), which was a silent no-op. | Added `TaskState::herrno_ptr` and `set_herrno` (mirroring `errno_ptr`/`set_errno` exactly, minus `SetErrnoPtr`'s size variants -- h_errno is always a plain 4-byte LONG). `do_gethostbyname` now calls it with `HOST_NOT_FOUND` on a failed/timed-out DNS query, `TRY_AGAIN` if smoltcp couldn't even start one, and `0` on every success path (including the `"localhost"` special case) -- netdb.h's real values, not guessed. |
| 64 (`WaitSelect(exceptfds)` detects OOB data) | `send(..., MSG_OOB)` silently treated the out-of-band byte as ordinary in-band data (the dispatcher dropped `send()`'s own `flags` argument entirely, so no LVO ever even saw `MSG_OOB` was set) -- masking the real gap (smoltcp's TCP socket has no urgent-pointer/URG support at all to build real `exceptfds` detection on) behind a `send()` that looked like it "worked". | `do_send` now checks `flags & MSG_OOB` up front and returns `-1`/`EOPNOTSUPP` -- the same outcome bsdsocktest's own test explicitly anticipates and treats as a legitimate `SKIP` ("MSG_OOB not supported"), not a failure. Test 27 (a *different* MSG_OOB test, `sendmsg`/`recvmsg`'s sibling in test_sendrecv.c) has the identical skip path and flips from a coincidental pass (the old silent-accept behavior happened to round-trip the same byte) to the same honest skip -- a net wash in the total pass count, but both are now doing the right thing for the right reason. |
| 31, 32 (`sendmsg()`/`recvmsg()`, scatter/gather I/O) | Both `_hs_stub` -- never implemented at all. | Added real TCP-only `sendmsg`/`recvmsg` (`CALL_SENDMSG`/`CALL_RECVMSG`, new RPCs): `do_sendmsg` gathers every `struct msghdr`'s `msg_iov`/`msg_iovlen` array into one flat buffer, then reuses `do_send`'s own blocking/partial-progress logic (extracted into a shared `send_tcp_stream`, parameterized over where the next chunk of bytes comes from -- a guest `buf_addr` for plain `send()`, the pre-gathered buffer for `sendmsg()`). `do_recvmsg` mirrors `do_recv` (single-shot, short reads legitimate, `MSG_PEEK` supported) but scatters the received bytes across the iovecs in order, filling each to its own `iov_len` before the next -- real `readv()`/`recvmsg()` semantics. `msg_name`/`msg_control` are ignored (no test sets them; ancillary-data support is a distinct, much bigger feature nothing here needs) and UDP is rejected with `EOPNOTSUPP` (nothing exercises it). `struct msghdr`'s actual layout isn't in this project's local NDK checkout at all (only referenced, never defined) -- confirmed against the real guest toolchain container's own `clib2/include/sys/socket.h` instead of guessing. |
| 132, 133, 134, 135 (ICMP echo, four tests at once) | `SOCK_RAW` was never backed by a real socket kind -- `do_socket` accepted it but silently stored it as a plain `SockKind::Tcp`, so `sendto()`/`recv()` on the "raw" socket just hit ordinary (and nonsensical) TCP code paths. | Added a real `SockKind::Icmp` backed by smoltcp's own `icmp::Socket` (`socket-icmp` + `auto-icmp-echo-reply` features, previously unused) for `socket(AF_INET, SOCK_RAW, IPPROTO_ICMP)` specifically (`do_socket` now reads the `protocol` argument the dispatcher was silently dropping, same class of bug as `send()`'s own `flags` above). `do_icmp_sendto`/`do_icmp_recv` bridge this to real BSD raw-socket semantics, which are asymmetric: writes are header-less (the caller supplies just the ICMP message, matching bsdsocktest's own `icmp_ping()`), but reads deliver the *full* IP packet -- smoltcp's own `icmp::Socket::recv_slice` hands back payload only, so `do_icmp_recv` synthesizes a minimal, correctly-checksummed 20-byte IPv4 header (`synth_ipv4_header`) and prepends it. Binds the underlying socket lazily by the identifier embedded in the first echo request sent (smoltcp's own `Endpoint::Ident`, the "ping socket" demux model), not connection-oriented. `auto-icmp-echo-reply` makes the *interface itself* answer incoming echo requests automatically -- without it, self-pinging 127.0.0.1 (which loops back to the same interface, see `HostDevice`'s own loopback comment) would never get a reply, since nothing else in this plugin answers on the stack's behalf. Required touching every other `match slot.kind`/`if kind == ...` site in the file (11 exhaustive matches, the Rust compiler's own checklist, plus several non-exhaustive `if` checks that would otherwise panic on an ICMP handle) to add a third arm rather than silently falling into TCP- or UDP-shaped code. |
| 90, 91, 104 (`gethostbyaddr()`, reverse/PTR DNS) | `_hs_stub_null` -- always returned `NULL`. Not actually a scored fix: all three of bsdsocktest's own gethostbyaddr tests `tap_ok(1, ...)` in *both* their success and failure branches, so this cost nothing in the pass count either before or after -- implemented anyway, on request, for genuine `gethostbyaddr()` correctness rather than just bsdsocktest's own leniency. | smoltcp's `dns::Socket` can't do this at all: no PTR `DnsQueryType` variant, and `get_query_result()` is hard-typed to return `Vec<IpAddress>`, with no way to get a domain-name answer back out even if a PTR query were accepted. So `do_gethostbyaddr` speaks DNS wire format directly over a new plain UDP socket (`ptr_socket`) instead of reusing that type: builds a real query packet by hand (`wire::dns`'s own public `Repr`/`Question` primitives, the same ones `dns::Socket` itself is built on, just used one layer down), and `parse_ptr_response` walks the raw response looking for a PTR answer, using smoltcp's own `Packet::parse_name` to correctly resolve DNS name compression (a PTR answer's target name is very commonly a compression pointer back into the question) -- confirmed against a hand-built response with a compressed name in a native unit test, not just trusted to work. Verified against a *real* external reverse lookup under `NETWORK=1` (test 104: resolved a helper's IP to `gateway.local` via Copperline's own NAT DNS forwarder) -- not just a clean-failure smoke test. |
| 116, 118, 119 (`Dup2Socket` to a specific slot, `ObtainSocket`/`ReleaseSocket`/`ReleaseCopyOfSocket`) | The remaining Phase 4 scope decisions that cost nothing to leave undone (bsdsocktest's own tests accepted the fallback outcome as equally valid) -- implemented anyway, on request, for real AmiTCP SDK 4.0 shared-socket-pool semantics rather than leaving the fallback in place. | `Dup2Socket(fd, newfd)`'s specific-target form now really places the duplicate at `newfd` (closing whatever was already there first, via `do_close`'s own refcount-aware logic, with `newfd == fd` handled as a real no-op rather than risking `do_close` dropping the very socket being duplicated). `ObtainSocket`/`ReleaseSocket`/`ReleaseCopyOfSocket` (LVOs -144/-150/-156) got a real `Board::socket_pool: HashMap<i32, FdSlot>` keyed by caller-chosen (or library-assigned, for `UNIQUE_ID`) `id`: `ReleaseSocket` moves the fd's `FdSlot` into the pool (the fd becomes invalid immediately, like `CloseSocket`); `ReleaseCopyOfSocket` does the same but leaves the original fd valid, via the same `Rc`-refcount aliasing `Dup2Socket` already uses; `ObtainSocket` looks the `id` up, checks `domain`/`type`/`protocol` compatibility (`protocol == 0` matches any), and moves it into a fresh fd, removing it from the pool (can only be obtained once). Full protocol confirmed against `bsdsocktest`'s own `docs/AMITCP_API.md`, not guessed. |
| 92, 93, 94, 95, 96, 97, 101, 102 (`getservbyname`/`getservbyport`/`getprotobyname`/`getprotobynumber`/`getnetbyname`/`getnetbyaddr`, eight tests at once) | All backed by `_hs_stub_null` -- always returned `NULL`, honestly skipped rather than failed (see the "getservby\*/getprotoby\*/getnetby\*" note above), but genuinely unimplemented. Revisited once it became clear this project is used by general Amiga software, not just as a CI testing tool (see README.md's own framing) -- real network-aware software commonly resolves well-known ports/protocols by name, and this doesn't need a parsed `/etc/services`-style config file to answer that: several minimal AmiTCP-compatible stacks ship a small compiled-in table instead, which is all `getservbyname("http", "tcp")` (test 92) or `getprotobynumber(6)` (test 97) actually need answered. | Added `SERVICES`/`PROTOCOLS`/`NETWORKS`, small static `(name, ...)` tables covering the common well-known entries (`crates/hostsocket-plugin/src/lib.rs`), six new RPCs (`CALL_GETSERVBYNAME`/`CALL_GETSERVBYPORT`/`CALL_GETPROTOBYNAME`/`CALL_GETPROTOBYNUMBER`/`CALL_GETNETBYNAME`/`CALL_GETNETBYADDR`, replacing `_hs_stub_null` -- now fully unused and removed) and matching guest trampolines (register conventions confirmed against the real NDK's `inline/bsdsocket.h`, not guessed), and `write_servent`/`write_protoent`/`write_netent` marshaling helpers mirroring `write_hostent`'s own established `LIB_*BUF` scratch-buffer pattern. None of these need a network round trip (pure local table lookups), so the guest side uses the plain, non-blocking doorbell. Caught one real bug while wiring `getservbyport()` up: an early draft applied an unnecessary `u16::from_be` to the incoming port, reasoning (wrongly) that a swap still needed undoing -- network byte order *is* m68k's own big-endian order, so nothing here ever needs converting, and the extra swap silently broke every real lookup. A native unit test (`getservbyport_finds_a_known_port_regardless_of_proto_filter`) caught it immediately, before it ever reached a real emulator run. Verified end to end against a real Copperline boot: all eight tests pass for real, including the name-string lookups (92, 95, 96, 101) that native tests can't reach at all (`dma_read` is a no-op stub outside the wasm target) -- confirms the marshaling path, not just the lookup logic, works correctly. |

## Compatibility

Known issues, organized by category, in the same spirit as bsdsocktest's
own [COMPATIBILITY.md](https://github.com/tbdye/bsdsocktest/blob/main/docs/COMPATIBILITY.md)
for real stacks (Roadshow, Amiberry/UAE, WinUAE). "Matches a real stack"
below means the *same test* is a documented `KNOWN_FAILURE`/`KNOWN_CRASH`
for at least one real implementation in bsdsocktest's own
`known_failures.c` — not necessarily the same root cause, just evidence
this isn't a uniquely-broken corner.

Version string: `hostsocket.library 4.0 (2026-08-03)`.

### Failures (1)

#### Send-after-close (1, matches real stacks)

| Test | Description | Detail |
|-----:|-------------|--------|
| 35 | send(): error after peer closes connection | **Investigated in depth and confirmed structurally unfixable in this project's model, not just left alone.** A real fix needs two things to both hold: (a) the orphaned server socket must actually produce an RST once abandoned, and (b) the test's own execution must leave enough real elapsed time for that to happen before it gives up. (a) is real and implemented: `do_close`'s graceful `.close()` can't finish its own 4-way handshake when the peer (this test's own `client`) never sends its own FIN either — exactly this test's scenario — so the orphaned socket now sits on `Board::closing_sockets` with a 2-real-second deadline, past which `tick()` calls `tcp::Socket::abort()` (a real RST, confirmed via smoltcp's own `dispatch()` — `State::Closed => repr.control = TcpControl::Rst`) and reaps it. Verified working end-to-end with a native test that drives a real connect/accept/close/tick sequence and confirms the peer genuinely reaches `ECONNRESET` (`orphaned_closing_socket_gets_aborted_and_the_peer_sees_a_reset`, `crates/hostsocket-plugin/src/lib.rs`) — the mechanism is not hypothetical. But (b) doesn't hold for *this specific test*: `SO_RCVTIMEO` is roundtrip-only in this project (stored, never enforced — see the `setsockopt`/`getsockopt` fix above), so the test's own `set_recv_timeout(client, 1)` has no effect; `do_recv` instead returns synchronously the moment `may_recv()` goes false (`CloseWait`, reached within a couple of `tick()`s of the server's own FIN, not the RST) with a plain `rc=0` EOF -- so all 5 attempts of the test's own retry loop complete near-instantly, with no real blocking wait for a 2-second deadline to ever have a chance to elapse against. Enforcing `SO_RCVTIMEO` for real would fix this, but is a much bigger change (every blocking wait path, not just this one) for a single test. **Matches all three real stacks in `known_failures.c`** (Roadshow: "loopback does not generate RST for closed peer"; Amiberry 7.1.1 and WinUAE: "send after peer close returns wrong errno") — independent confirmation this is a structural gap real AmigaOS TCP/IP stacks hit too, not unique to this project. |

`sendmsg`/`recvmsg` (tests 31, 32), `SocketBaseTagList`'s own gaps (tests
74–78, 128), `WaitSelect(exceptfds)` OOB detection (test 64),
`gethostbyname()`'s h_errno-on-failure (test 89), ICMP echo (tests 132–135),
and `gethostbyaddr()`/reverse DNS (tests 90, 91, 104) — all previously
their own Compatibility entries here — are now implemented and passing;
see the "Fixed after the initial triage" table above. Test 64's own fix
is a `SKIP`, not a real pass (`send(..., MSG_OOB)` now honestly reports
"not supported" rather than silently mis-delivering the byte in-band) —
same as test 27, its sibling in test_sendrecv.c.

`getservby*`/`getprotoby*`/`getnetby*` are now implemented for real
(small static well-known-name tables, see the "Fixed after the initial
triage" table above) — all eight tests (92, 93, 94, 95, 96, 97, 101, 102)
pass genuinely rather than honestly skipping. `gethostname`/`gethostid`
(test 98, 100) are now implemented and passing too — see the same table.

### Investigated, not a bug: test 137's `ms=0 KB/s=0`

Test 137 (TCP loopback send/recv, 512KB) reports `ms=0 KB/s=0` despite a
real, correct transfer. This is **not** a `bsdsocket.library` gap —
`timer_now()`/`timer_elapsed_ms()` (testutil.c) measure elapsed time via
`GetSysTime()`, a real timer.device call reading Copperline's own
emulated CIA hardware timer. Nothing in this project's own RPC layer is
involved in that measurement at all. Under `warp_speed = "max"`
(`machine.toml`), the CPU races through instructions far faster than
real-time playback, so a loopback transfer with no real network delay —
where `WaitSelect()` correctly returns immediately every iteration
because the data really is ready — can genuinely complete within a
single tick of whatever resolution `GetSysTime()` has, rounding to 0ms.
Test 60 (`WaitSelect(): tv={0,0} immediate poll`) already legitimately
reports `elapsed: 0ms` for the same reason, and test 141 (1MB, *the same*
loop structure as 137, just double the data) measures a real `40ms` —
consistent with a fast-but-nonzero-real-duration transfer that crossed a
tick boundary, not a dead or broken timer. Same underlying *class* of
issue as `Board::micros`'s own colour-clock-vs-real-time gotcha (see the
"Fixed after the initial triage" table above), just in a completely
separate subsystem this project doesn't implement or influence at all —
not something to chase further here.

## Next steps

1. Only one failure remains in the loopback tier (35, send-after-close), and it's now confirmed genuinely structural, not a deliberate scope decision or an under-investigated gap — see the Compatibility section above for the full root-cause chain. All of this project's own deliberate Phase 4 scope decisions (`Dup2Socket` to a specific slot, `ObtainSocket`/`ReleaseSocket`/`ReleaseCopyOfSocket`) are now implemented for real (tests 116, 118, 119). The `NETWORK=1` tier adds two *permanent* Copperline-level limitations (test 41's inbound-NAT gap, test 136's NAT engine unconditionally faking ICMP replies — see the "NETWORK tier" section above for both) — neither fixable from this plugin.
2. Test 35 is closed out: a real FIN_WAIT_2-style abort/RST mechanism (`Board::closing_sockets`) was built and verified working end-to-end natively, but the test's own near-instant execution profile (no `SO_RCVTIMEO` enforcement anywhere in this project means nothing in its retry loop actually blocks long enough for a timeout-based fix to matter) makes it structurally unreachable without enforcing real per-fd receive timeouts everywhere — a much larger change for one test that three real AmigaOS stacks already fail the same way. Not worth pursuing further absent a real product need for enforced `SO_RCVTIMEO`.
3. `getservby*`/`getprotoby*`/`getnetby*` (tests 92-97, 101, 102) turned out to be the one remaining easy win after all — see the "Fixed after the initial triage" table above. What's left of the 11-skip floor (57, 70, plus MSG_OOB's 27/64) breaks down the same way tests 41/136 do: `IoctlSocket(FIOASYNC)` (57) is a narrow, largely-redundant alternate to the `SO_EVENTMASK`/`GetSocketEvents` mechanism this project already implements for real async notification — revisit only if real software specifically needs it. Test 70 (`WaitSelect()` on a descriptor above the wire protocol's 31-fd ceiling) needs two coordinated changes together (a bigger `fds` array *and* a wider `WaitSelect` wire format) to mean anything beyond a coincidentally-correct skip — a guest+plugin ABI change, not a plugin-only fix. MSG_OOB (27, 64) is a hard `smoltcp` dependency wall (no TCP urgent-pointer support at all), matching tests 41/136's own "not fixable from this plugin" character even though the blocker here is a library, not Copperline.
