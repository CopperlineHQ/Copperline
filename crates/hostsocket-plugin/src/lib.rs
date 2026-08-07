//! Copperline WASM board: `bsdsocket.library` backed by an embedded
//! `smoltcp` TCP/IP stack. Developed in a standalone repository whose
//! PROPOSAL.md carried the phased design plan; that document was not
//! imported with the crate, so the PROPOSAL.md citations below are
//! historical markers of real scope decisions, not live references. The
//! verification record (docs/bsdsocktest-status.md) *was* imported and is
//! the current statement of what this board does and does not implement.
//!
//! Phase 1 proved the RPC/DMA/smoltcp/NetBackend chain end to end with a
//! single hard-coded socket and a guest-side busy-spin standing in for real
//! blocking. Phase 2 added a real fd table, per-task errno state
//! (`SetErrnoPtr`/`Errno`), `IoctlSocket(FIONBIO)` + genuine non-blocking
//! mode, and real `Wait`/`Signal`-based blocking via `int2()` and a
//! guest-installed interrupt server (replacing the Phase 1 spin), plus
//! `WaitSelect` over the same fd table. This file now implements Phase 3:
//! the real server-side path (`bind`/`listen`/`accept` -- the Phase 1/2
//! internal hard-coded echo listener is gone, replaced by a real
//! guest-driven server), UDP (`sendto`/`recvfrom`, plus plain `send`/`recv`
//! against a peer recorded by `connect()` on a UDP fd), and the remaining
//! common LVOs (`shutdown`/`setsockopt`/`getsockopt`/`getsockname`/
//! `getpeername`). Phase 4 added `Dup2Socket`, the `Inet_*`/`inet_*`
//! address-conversion LVOs, and `getdtablesize()`. This file now also
//! implements `gethostbyname` (forward DNS, A records only, via smoltcp's
//! `socket-dns`) -- the one LVO in this project's own scope notes that
//! needs a real DNS server to test against, unlike everything else here,
//! which runs fully over the deterministic Loopback backend. `gethostbyaddr`
//! (reverse/PTR lookup) stays `_hs_stub`: smoltcp 0.13's `wire::dns::Type`
//! has no `Ptr` variant at all, so a real reverse lookup would need a
//! hand-rolled DNS wire-format encoder/parser outside smoltcp's own
//! supported type set -- not worth it until a real consumer needs it. See
//! PROPOSAL.md's phased plan.
//!
//! Implements the module ABI Copperline's `wasmboard.rs` hosts (`memory`,
//! `init`, `read`, `write`, `tick`, `int2`, `int6`) and imports the
//! capability-gated host functions it needs from module `env`. See
//! Copperline's `docs/zorro.md` ("WASM plugin boards") for the full
//! contract this must satisfy -- keep this file in sync with that doc, not
//! the other way around.
//!
//! `memory` is exported automatically: rustc gives a wasm32-unknown-unknown
//! `cdylib` a `memory` export with no extra code required.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy;
use smoltcp::socket::{dns, icmp, tcp, udp};
use smoltcp::storage::RingBuffer;
use smoltcp::time::Instant;
use smoltcp::wire::{
    ArpOperation, ArpPacket, ArpRepr, DnsFlags, DnsOpcode, DnsPacket, DnsQueryType, DnsQuestion,
    DnsRcode, DnsRecord, DnsRecordData, DnsRepr, EthernetAddress, EthernetFrame, EthernetProtocol,
    HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address, Ipv4Cidr, Ipv4Packet,
};

// -- Host imports (module "env"), per Copperline's WASM plugin ABI ---------
//
// `log` is always available. `dma_read`/`dma_write` require the `dma`
// capability; `net_send`/`net_recv` require the `net` capability;
// `config_get`/`resource_len`/`resource_read` are always available and used
// here to pull the guest stub ROM out of the `rom` file-typed config option
// (see src/hostsocket.rs) -- the same "plugin carries its own
// autoboot ROM" trick docs/zorro.md describes for the in-tree A2091.

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    fn log(ptr: i32, len: i32);

    fn dma_read(addr: i32, ptr: i32, len: i32);
    fn dma_write(addr: i32, ptr: i32, len: i32);

    fn net_send(ptr: i32, len: i32);
    fn net_recv(ptr: i32, cap: i32) -> i32;

    // Host-OS-resolver DNS lookup (the `resolve` capability): asks
    // Copperline's own process to resolve a hostname via `getaddrinfo` on a
    // background thread instead of this plugin having to speak DNS wire
    // format itself over `net`. `resolve_start` returns a request id (or
    // -1 if the host couldn't even start it); `resolve_poll` is a
    // non-blocking poll of that id (-2 pending, -1 failed, or 0 with the
    // resolved IPv4 address written into `out_ptr`, 4 bytes, in this
    // module's own linear memory).
    fn resolve_start(name_ptr: i32, name_len: i32) -> i32;
    fn resolve_poll(id: i32, out_ptr: i32) -> i32;

    // Host-socket passthrough (the `host_sockets` capability): direct,
    // non-blocking access to a real host OS socket, the Amiberry-style
    // alternative to this module's own smoltcp stack. Only actually used
    // when `[config] transport = "host"` selects it at init() -- see
    // `Board::host_backend` and `HOSTSOCKET-HOST-BACKEND-PLAN.md`. Return
    // values match `src/wasmboard.rs`'s own doc comment on these imports
    // exactly (a handle or a negative BSD-style errno from this same
    // numbering, e.g. `EAGAIN`/`EINPROGRESS` below); kept in sync with that
    // file by hand, the same convention this file's own errno table above
    // already follows.
    fn sock_open(domain: i32, type_: i32) -> i32;
    fn sock_connect(handle: i32, ip: i32, port: i32) -> i32;
    fn sock_send(handle: i32, ptr: i32, len: i32) -> i32;
    fn sock_recv(handle: i32, ptr: i32, cap: i32) -> i32;
    fn sock_poll(handle: i32) -> i32;
    fn sock_close(handle: i32);
    fn sock_bind(handle: i32, ip: i32, port: i32) -> i32;
    fn sock_listen(handle: i32, backlog: i32) -> i32;
    fn sock_accept(handle: i32) -> i32;
    fn sock_local_addr(handle: i32, out_ptr: i32) -> i32;
    fn sock_peer_addr(handle: i32, out_ptr: i32) -> i32;
    fn sock_sendto(handle: i32, ptr: i32, len: i32, ip: i32, port: i32) -> i32;
    fn sock_recvfrom(handle: i32, ptr: i32, cap: i32, out_addr_ptr: i32) -> i32;
    fn sock_setopt(handle: i32, level: i32, optname: i32, value: i32) -> i32;
    fn sock_getopt(handle: i32, level: i32, optname: i32, out_ptr: i32) -> i32;
    fn sock_dup(handle: i32) -> i32;
    fn sock_shutdown(handle: i32, how: i32) -> i32;
    fn sock_peek(handle: i32, ptr: i32, cap: i32) -> i32;
    fn sock_nread(handle: i32) -> i32;
    fn sock_send_oob(handle: i32, ptr: i32, len: i32) -> i32;
    fn sock_recv_oob(handle: i32, ptr: i32, cap: i32) -> i32;

    fn config_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32) -> i32;
    fn resource_len(key_ptr: i32, key_len: i32) -> i32;
    // Matches the host's real signature (src/wasmboard.rs, docs/zorro.md):
    // an `off` byte offset into the resource, between the name and the
    // output buffer. The Phase 0 scaffolding here had dropped that
    // parameter, which would have failed to link against the host at
    // instantiation time (a WASM import's arity is part of its type).
    fn resource_read(key_ptr: i32, key_len: i32, off: i32, out_ptr: i32, out_cap: i32) -> i32;
}

// Native builds (`cargo test`, clippy, rust-analyzer) never run inside
// Copperline's wasmtime host, so the imports above have nothing to link
// against there -- these no-op stand-ins exist purely so the crate builds
// and the register-layout test below can run natively. They are never
// meaningfully exercised: this project's real oracle for the RPC/DMA/
// smoltcp/NetBackend path is Copperline itself (see PROPOSAL.md's Testing
// section), not a native unit test faking a WASM linear-memory boundary.
#[cfg(not(target_arch = "wasm32"))]
#[allow(unused_variables, clippy::missing_safety_doc)]
mod native_host_stubs {
    pub unsafe fn log(ptr: i32, len: i32) {}
    pub unsafe fn dma_read(addr: i32, ptr: i32, len: i32) {}
    pub unsafe fn dma_write(addr: i32, ptr: i32, len: i32) {}
    pub unsafe fn net_send(ptr: i32, len: i32) {}
    pub unsafe fn net_recv(ptr: i32, cap: i32) -> i32 {
        0
    }
    pub unsafe fn resolve_start(name_ptr: i32, name_len: i32) -> i32 {
        -1
    }
    pub unsafe fn resolve_poll(id: i32, out_ptr: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_open(domain: i32, type_: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_connect(handle: i32, ip: i32, port: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_send(handle: i32, ptr: i32, len: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_recv(handle: i32, ptr: i32, cap: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_poll(handle: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_close(handle: i32) {}
    pub unsafe fn sock_bind(handle: i32, ip: i32, port: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_listen(handle: i32, backlog: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_accept(handle: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_local_addr(handle: i32, out_ptr: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_peer_addr(handle: i32, out_ptr: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_sendto(handle: i32, ptr: i32, len: i32, ip: i32, port: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_recvfrom(handle: i32, ptr: i32, cap: i32, out_addr_ptr: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_setopt(handle: i32, level: i32, optname: i32, value: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_getopt(handle: i32, level: i32, optname: i32, out_ptr: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_dup(handle: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_shutdown(handle: i32, how: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_peek(handle: i32, ptr: i32, cap: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_nread(handle: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_send_oob(handle: i32, ptr: i32, len: i32) -> i32 {
        -1
    }
    pub unsafe fn sock_recv_oob(handle: i32, ptr: i32, cap: i32) -> i32 {
        -1
    }
    pub unsafe fn config_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32) -> i32 {
        -1
    }
    pub unsafe fn resource_len(key_ptr: i32, key_len: i32) -> i32 {
        -1
    }
    pub unsafe fn resource_read(
        key_ptr: i32,
        key_len: i32,
        off: i32,
        out_ptr: i32,
        out_cap: i32,
    ) -> i32 {
        -1
    }
}
#[cfg(not(target_arch = "wasm32"))]
use native_host_stubs::*;

fn host_log(msg: &str) {
    // Safety: `msg` is a valid Rust `&str` backed by this module's own
    // linear memory, which is exactly what the `log` import expects.
    unsafe { log(msg.as_ptr() as i32, msg.len() as i32) };
}

fn load_resource(key: &str) -> Vec<u8> {
    let len = unsafe { resource_len(key.as_ptr() as i32, key.len() as i32) };
    if len <= 0 {
        return Vec::new();
    }
    let mut buf = vec![0u8; len as usize];
    let n = unsafe {
        resource_read(
            key.as_ptr() as i32,
            key.len() as i32,
            0,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    buf.truncate(n.max(0) as usize);
    buf
}

// config_get's own `[config] key = "value"` string settings (manifest.toml,
// e.g. `dns_server`), as opposed to load_resource's file-typed ones. 64
// bytes is generous for anything this plugin currently reads this way (an
// IPv4 address string); a longer value is truncated, matching config_get's
// own truncate-to-out_cap contract.
fn config_get_string(key: &str) -> Option<String> {
    let mut buf = [0u8; 64];
    let n = unsafe {
        config_get(
            key.as_ptr() as i32,
            key.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if n < 0 {
        return None;
    }
    let n = (n as usize).min(buf.len());
    Some(String::from_utf8_lossy(&buf[..n]).into_owned())
}

// -- Board-window register layout (see ../guest/hostsocket_board.h) --------
//
// Kept in sync with that file by hand (see its own header comment); the
// `board_layout` test below locks these values so a one-sided edit fails
// the build instead of silently drifting.

// Matches hostsocket_board.h's ROM_OFFSET: the guest ROM is served starting
// at this window offset, not window offset 0 (see read_byte below -- the
// window has nothing mapped before it in Phase 1/2).
const ROM_OFFSET: i32 = 0x0008;

const REG_ARGPTR: i32 = 0x7C00;
const REG_CALL: i32 = 0x7C04;
const REG_RESULT: i32 = 0x7C08;
const REG_WAKE_TASK: i32 = 0x7C0C;
const REG_WAKE_SIGNAL: i32 = 0x7C10;
const REG_WAKE_ACK: i32 = 0x7C14;

const CALL_SOCKET: i32 = 0;
const CALL_CONNECT: i32 = 1;
const CALL_SEND: i32 = 2;
const CALL_RECV: i32 = 3;
const CALL_CLOSESOCKET: i32 = 4;
const CALL_REGISTER_WAIT: i32 = 5;
const CALL_IOCTLSOCKET: i32 = 6;
const CALL_SETERRNOPTR: i32 = 7;
const CALL_ERRNO: i32 = 8;
const CALL_WAITSELECT: i32 = 9;
const CALL_BIND: i32 = 10;
const CALL_LISTEN: i32 = 11;
const CALL_ACCEPT: i32 = 12;
const CALL_SENDTO: i32 = 13;
const CALL_RECVFROM: i32 = 14;
const CALL_SHUTDOWN: i32 = 15;
const CALL_SETSOCKOPT: i32 = 16;
const CALL_GETSOCKOPT: i32 = 17;
const CALL_GETSOCKNAME: i32 = 18;
const CALL_GETPEERNAME: i32 = 19;
const CALL_DUP2SOCKET: i32 = 20;
const CALL_INET_NTOA: i32 = 21;
const CALL_INET_ADDR: i32 = 22;
const CALL_INET_LNAOF: i32 = 23;
const CALL_INET_NETOF: i32 = 24;
const CALL_INET_MAKEADDR: i32 = 25;
const CALL_INET_NETWORK: i32 = 26;
const CALL_GETDTABLESIZE: i32 = 27;
const CALL_GETHOSTBYNAME: i32 = 28;
const CALL_SOCKETBASETAGLIST: i32 = 29;
const CALL_GETSOCKETEVENTS: i32 = 30;
const CALL_GETHOSTNAME: i32 = 31;
const CALL_GETHOSTID: i32 = 32;
const CALL_SENDMSG: i32 = 33;
const CALL_RECVMSG: i32 = 34;
const CALL_GETHOSTBYADDR: i32 = 35;
const CALL_OBTAINSOCKET: i32 = 36;
const CALL_RELEASESOCKET: i32 = 37;
const CALL_RELEASECOPYOFSOCKET: i32 = 38;
const CALL_GETSERVBYNAME: i32 = 39;
const CALL_GETSERVBYPORT: i32 = 40;
const CALL_GETPROTOBYNAME: i32 = 41;
const CALL_GETPROTOBYNUMBER: i32 = 42;
const CALL_GETNETBYNAME: i32 = 43;
const CALL_GETNETBYADDR: i32 = 44;

const RES_PENDING: i32 = -2;

const FIONBIO: u32 = 0x8004667E;
const FIONREAD: u32 = 0x4004667F;

// SocketBaseTagList's SBTM_SETVAL(SBTC_ERRNOLONGPTR) tag code (amitcp/
// socketbasetags.h, not guessed -- that header isn't in this NDK's
// bsdsocket/socketbasetags.h, only its amitcp/ sibling):
// TAG_USER (1<<31) | (SBTC_ERRNOLONGPTR=24 << SBTB_CODE=1) | SBTF_SET (1).
const SBTC_ERRNOLONGPTR_SET: u32 = 0x8000_0031;
// SBTM_SETVAL(SBTC_SIGEVENTMASK): same macro, SBTC_SIGEVENTMASK=4 (amitcp/
// socketbasetags.h, confirmed in the local NDK checkout at
// /opt/amiga/m68k-amigaos/ndk-include/amitcp/socketbasetags.h -- registers
// which Amiga signal GetSocketEvents()-reportable events get delivered on,
// see `do_socketbasetaglist` and `process_socket_events`.
const SBTC_SIGEVENTMASK_SET: u32 = 0x8000_0009;
// The remaining SocketBaseTagList tag codes `do_socketbasetaglist` acts on,
// same macros/codes as above (amitcp/socketbasetags.h): SBTC_BREAKMASK=1,
// SBTC_DTABLESIZE=8 (both SET and GET(REF) real), SBTC_SIGEVENTMASK's own
// GET(REF) half, and SBTC_ERRNOLONGPTR=24/SBTC_HERRNOLONGPTR=25's GET(REF)
// halves (the header's own comment on SBTC_ERRNOLONGPTR calls its family
// "SETTING (only)", but bsdsocktest's own test still exercises GETREF on
// it and a real stack -- Roadshow -- answers that GETREF too, just not
// usefully; see do_socketbasetaglist's own comment for how this
// implementation answers it).
const SBTC_BREAKMASK_SET: u32 = 0x8000_0003;
const SBTC_BREAKMASK_GET: u32 = 0x8000_8002;
const SBTC_SIGEVENTMASK_GET: u32 = 0x8000_8008;
const SBTC_ERRNOLONGPTR_GET: u32 = 0x8000_8030;
// SBTM_SETVAL(SBTC_HERRNOLONGPTR): TAG_USER | (25<<1=50=0x32) | SBTF_SET(1).
const SBTC_HERRNOLONGPTR_SET: u32 = 0x8000_0033;
const SBTC_HERRNOLONGPTR_GET: u32 = 0x8000_8032;
const SBTC_DTABLESIZE_SET: u32 = 0x8000_0011;
const SBTC_DTABLESIZE_GET: u32 = 0x8000_8010;

// sys/socket.h constants (/opt/amiga/m68k-amigaos/ndk-include/sys/socket.h,
// not guessed).
const SOCK_STREAM: i32 = 1;
const SOCK_DGRAM: i32 = 2;
const SOCK_RAW: i32 = 3;
const SOL_SOCKET: i32 = 0xFFFF;
const SO_TYPE: i32 = 0x1008;
const SO_ERROR: i32 = 0x1007;
const SO_RCVBUF: i32 = 0x1002;
const SO_SNDBUF: i32 = 0x1001;
const SO_RCVTIMEO: i32 = 0x1006;
const SO_SNDTIMEO: i32 = 0x1005;
const SO_REUSEADDR: i32 = 0x0004;
const SO_KEEPALIVE: i32 = 0x0008;
const SO_LINGER: i32 = 0x0080;
// AmiTCP-specific socket option (docs/AMITCP_API.md, not part of stock
// sys/socket.h -- there is no local NDK header for it): per-socket bitmask
// of FD_* event types to report via GetSocketEvents(), see
// `process_socket_events`.
const SO_EVENTMASK: i32 = 0x2001;
// GetSocketEvents()'s own event bitmask bits (docs/AMITCP_API.md). FD_OOB
// and FD_ERROR are intentionally never generated: this project has no
// urgent/OOB data modeling (matching WaitSelect's own exceptfds gap) and no
// tracked source of "asynchronous error" distinct from what SO_ERROR
// already reports.
const FD_ACCEPT: u32 = 0x01;
const FD_CONNECT: u32 = 0x02;
const FD_READ: u32 = 0x08;
const FD_WRITE: u32 = 0x10;
const FD_CLOSE: u32 = 0x40;
// netinet/in.h
const AF_INET: i32 = 2;
const IPPROTO_TCP: i32 = 6;
const IPPROTO_ICMP: i32 = 1;
const MSG_OOB: i32 = 0x1;
const MSG_PEEK: i32 = 0x2;
// netinet/tcp.h
const TCP_NODELAY: i32 = 0x01;

// offsetof(struct Task, tc_SigRecvd), exec/tasks.h -- see task_sig_recvd.
const TC_SIGRECVD_OFFSET: u32 = 26;

// BSD errno values (classic 4.3BSD numbering bsdsocket.library also uses --
// see /opt/amiga/m68k-amigaos/sys-include/sys/errno.h, not guessed).
const EBADF: i32 = 9;
const EINVAL: i32 = 22;
const EAGAIN: i32 = 35; // EWOULDBLOCK
const EINPROGRESS: i32 = 36;
const EALREADY: i32 = 37;
const ENOTSOCK: i32 = 38;
const ENOTCONN: i32 = 57;
const ECONNREFUSED: i32 = 61;
const EPIPE: i32 = 32;
const ECONNRESET: i32 = 54;
const EADDRINUSE: i32 = 48;
const EOPNOTSUPP: i32 = 45;
const EMFILE: i32 = 24;
// Only ever produced by the host-socket backend's `do_connect_host` (a
// second non-blocking `connect()` on an already-connected socket -- a real
// POSIX success signal, not an error; see that function's own comment).
// The smoltcp path has no equivalent: it tracks connection state directly
// rather than by re-issuing `connect()`.
const EISCONN: i32 = 56;

// h_errno values (netdb.h, /opt/amiga/m68k-amigaos/sys-include/netdb.h,
// not guessed) -- do_gethostbyname's own failure/success paths, via
// `set_herrno`.
const HOST_NOT_FOUND: i32 = 1;
const TRY_AGAIN: i32 = 2;

// This project's own interface address (see `init()`). Phase 1/2 also used
// this as the target for a plugin-internal hard-coded echo listener; Phase
// 3's real bind/listen/accept replaces that with a guest-driven server, so
// this is now purely "what getsockname()/an outgoing connection's local
// address looks like" plumbing.
//
// 10.0.2.15, not an arbitrary address: this is Copperline's own NAT
// backend's hardcoded guest address (src/net/nat/mod.rs's GUEST_IP,
// alongside GATEWAY_IP 10.0.2.2 and DNS_IP 10.0.2.3 -- the classic
// SLIRP/QEMU user-mode-networking convention). That backend's DNS
// forwarder and general UDP NAT path both unconditionally address their
// reply frames to *exactly* this IP (confirmed in engine.rs's step()),
// not whatever source address the guest's own query happened to use --
// so under net = "nat" this is load-bearing, not cosmetic, and
// gethostbyname() silently got no replies at all under a placeholder
// 10.0.0.1 (found chasing down why DNS_SERVER's own NAT responses never
// arrived, see PROPOSAL.md's DNS section). Loopback doesn't care what
// this value is (frames just echo straight back, there is no real
// gateway to match addresses against), so this is a safe, harmless
// change for every existing Loopback-backed test.
const INTERFACE_ADDR: Ipv4Address = Ipv4Address::new(10, 0, 2, 15);
// This interface's own hardware address, used both to configure it
// (init()) and to source the synthetic ARP replies `TxTok::consume`
// sends itself for the loopback range (see that function's own comment).
const INTERFACE_MAC: EthernetAddress = EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
// Copperline NAT's virtual gateway (src/net/nat/mod.rs's GATEWAY_IP) --
// the default route for anything off the 10.0.2.0/24 segment (a real
// outbound TCP connection through NAT, not DNS itself: DNS_IP is on-link
// on that same /24, so it resolves via plain ARP, no route needed).
const NAT_GATEWAY_ADDR: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);
// Copperline NAT's own DNS forwarder (src/net/nat/mod.rs's DNS_IP,
// src/net/nat/dns.rs) -- answers A/PTR queries via the *host's* own
// resolver (getaddrinfo), not by routing the guest's raw UDP packet
// anywhere. This is the only DNS server net = "nat" can ever actually
// reach: it does not forward arbitrary UDP to arbitrary destinations (see
// INTERFACE_ADDR's own comment for the address scheme this is part of).
const NAT_DNS_ADDR: Ipv4Address = Ipv4Address::new(10, 0, 2, 3);

// 4096 used to be enough for this project's own tests, but not for
// bsdsocktest's: its own single-*process* transfer test (test_sendrecv.c's
// sendrecv_large_8192) fills an 8192-byte buffer and does one blocking
// send() before ever calling recv() -- on the very same guest task as the
// socket it's about to recv() from, since this project has no real
// preemptive multitasking of guest tasks. A too-small buffer here makes
// that a genuine, permanent deadlock, not just a slow transfer: send()
// blocks waiting for tx room, which only frees up once the peer's recv()
// drains its rx buffer, but that peer is the very same blocked task and
// will never get to call recv() at all. Real bsdsocket.library
// implementations don't hit this because their default per-socket buffers
// are already comfortably larger than 8192 (bsdsocktest's own SO_RCVBUF/
// SO_SNDBUF tests expect at least 32768 settable) -- 16384 is chosen to
// clear the loopback tier's largest single-shot transfer with real margin
// without going as far as that 32768 figure, which is a distinct, still-
// unimplemented SO_RCVBUF/SO_SNDBUF feature, not this constant.
const SOCKET_BUF_LEN: usize = 16384;
// UDP has no flow control of its own -- a real burst of datagrams (this
// project has no real preemptive multitasking of guest tasks, so nothing
// drains a receiver's queue mid-burst any more than TCP's own same-task
// send/recv deadlock above could) all lands in the receive queue at once,
// unlike TCP's byte stream which can at least apply backpressure. 8
// metadata slots and a 16KB byte buffer meant only 8 of a real 200-
// datagram, 1KB-each burst survived -- 96% loss -- found running
// bsdsocktest's own UDP throughput test, which (like real UDP senders
// routinely do) fires every sendto() in a tight loop before ever
// receiving anything. A separate constant from `SOCKET_BUF_LEN`, not a
// shared bump: TCP doesn't need anywhere near this much per socket, and
// this is sized for that one test's own burst, not a real capacity
// requirement either -- see docs/bsdsocktest-status.md for whether the
// remaining loss (if any) is worth chasing further.
const UDP_BUF_LEN: usize = 262_144;
const UDP_META_SLOTS: usize = 256;
// Raw ICMP sockets: bsdsocktest's own largest ping payload is 1024 bytes
// (plus an 8-byte echo header), and pings are synchronous request/reply --
// nothing here needs anywhere near UDP's own burst-sized buffers. A few
// metadata slots comfortably covers a handful of in-flight replies (the
// multi-ping test fires 5 pings back to back, each drained before the
// next).
const ICMP_BUF_LEN: usize = 2048;
const ICMP_META_SLOTS: usize = 8;

// PAL colour-clock rate, matching Copperline's own config::COLOR_CLOCK_HZ
// -- not guessed, `tick()`'s own `cck` argument is denominated in these
// (see `Board::micros`'s own doc comment for why that matters).
const CCK_HZ: f64 = 3_546_895.0;

// A real, bounded fd table (Phase 2): guest-visible fds are 1-based indices
// into this array (fd N lives at `fds[N - 1]`), matching a real select()
// fd_set's bit-per-fd convention directly (bit N = fd N) without an extra
// off-by-one translation in do_wait_select. 64 (Phase 4) matches
// bsdsocktest's getdtablesize() expectation (>= 64) and is a more
// realistic real-world default than Phase 1-3's placeholder 8.
const MAX_FDS: usize = 64;

// `CALL_WAITSELECT`'s wire format (see `guest/hostsocket_board.h`'s own
// comment on it) represents each of readfds/writefds/exceptfds as a single
// guest ULONG bitmask, bit N = fd N -- so fd values 1..31 are the only ones
// that fit; there is no bit 32 on a 32-bit word. `scan_select`/
// `record_connect_completion_errors` used to scan all the way to `MAX_FDS`
// (64, matching `getdtablesize()`) and compute `1u32 << fd` for fds up to
// 63: harmless up to 31, but `1u32 << 32` and beyond doesn't panic (no
// overflow-checks in release) -- Rust's shift semantics take the shift
// amount mod 32, so it silently wraps back to `1u32 << 0` and up,
// *aliasing* a high fd's readiness onto an unrelated low bit instead of
// reporting nothing for it. A guest that opens more than 31 fds and
// `WaitSelect()`s on one above 31 is not exercised by anything in
// bsdsocktest's own loopback/nat tiers, but this is a real, silent
// wrong-answer bug for a well-behaved caller, not just an adversarial one.
// `WaitSelect()` on fd 32+ is a permanent gap given this wire format (fixing
// it for real would need widening the ULONG to a multi-word bitmask on both
// the guest and plugin sides) -- scanning is capped here so those fds are
// simply never reported ready via `WaitSelect()`, rather than aliased onto
// a fd that might be.
const SELECT_MAX_FD: u32 = 32;

// sendmsg()/recvmsg()'s own bound on how many `struct iovec` entries
// `read_iovec_descriptors` will ever walk -- bsdsocktest's own tests use
// at most 3, so this is purely a defensive cap against a bogus/huge
// `msg_iovlen` turning one call into unbounded DMA traffic, not a real
// capacity limit anything here needs to reach.
const MAX_IOVEC: usize = 64;

// Ceiling on any single guest-supplied transfer length (send/recv/sendto/
// recvfrom's own `len`, and each individual `struct iovec`'s `iov_len`)
// before it's used to size a plugin-side `Vec` allocation. Without this, a
// guest (buggy or adversarial) supplying a length near i32::MAX/u32::MAX
// drives a multi-gigabyte allocation attempt inside this wasm32 module;
// under this workspace's `panic = "abort"` release profile that aborts the
// whole plugin instance on the very first such call, wedging the guest
// forever (the RPC never completes) rather than returning a clean error.
// No real transfer needs anywhere near this much in one call -- it's
// already larger than `UDP_BUF_LEN`, the biggest single buffer this plugin
// keeps anywhere, so nothing legitimate ever gets truncated by it.
const MAX_XFER_LEN: usize = UDP_BUF_LEN;

// gethostbyname()'s hostent blob layout within the guest's own
// LIB_HOSTENTBUF scratch buffer (see guest/entry.s) -- plugin-computed
// offsets from buf_addr, the same "trampoline already knows this address,
// plugin just fills it in" pattern Inet_NtoA uses for its own smaller
// LIB_INETBUF. Real hostent has 5 fields (20 bytes on this platform);
// h_aliases is always just a NULL terminator (an empty alias list --
// smoltcp's DNS socket doesn't expose the alias chain), and h_addr_list
// holds up to HOSTENT_MAX_ADDRS resolved addresses.
const HOSTENT_MAX_ADDRS: usize = 4;
const HOSTENT_NAME_CAP: usize = 64; // including the NUL terminator
const HOSTENT_ALIASES_OFF: u32 = 20;
const HOSTENT_ADDR_LIST_OFF: u32 = HOSTENT_ALIASES_OFF + 4;
const HOSTENT_ADDRS_OFF: u32 = HOSTENT_ADDR_LIST_OFF + (HOSTENT_MAX_ADDRS as u32 + 1) * 4;
const HOSTENT_NAME_OFF: u32 = HOSTENT_ADDRS_OFF + HOSTENT_MAX_ADDRS as u32 * 4;
const HOSTENT_BUF_LEN: u32 = HOSTENT_NAME_OFF + HOSTENT_NAME_CAP as u32;

// getservbyname()/getservbyport()'s servent blob layout within the guest's
// own LIB_SERVENTBUF scratch buffer -- same "trampoline already knows this
// address, plugin just fills it in" pattern as HOSTENT_*_OFF above. Real
// servent is 4 fields (16 bytes: s_name/s_aliases/s_port/s_proto, all
// pointer- or LONG-sized); s_aliases is always just a NULL terminator (no
// alternate-name data in SERVICES below to build a real alias list from).
const SERVENT_HDR_LEN: u32 = 16;
const SERVENT_ALIASES_OFF: u32 = SERVENT_HDR_LEN;
const SERVENT_PROTO_CAP: usize = 8; // "tcp\0"/"udp\0" with room to spare
const SERVENT_PROTO_OFF: u32 = SERVENT_ALIASES_OFF + 4;
const SERVENT_NAME_CAP: usize = 32; // longest real entry below is 12 bytes
const SERVENT_NAME_OFF: u32 = SERVENT_PROTO_OFF + SERVENT_PROTO_CAP as u32;
const SERVENT_BUF_LEN: u32 = SERVENT_NAME_OFF + SERVENT_NAME_CAP as u32;

// getprotobyname()/getprotobynumber()'s protoent blob layout within
// LIB_PROTOENTBUF. Real protoent is 3 fields (12 bytes); p_aliases is
// always just a NULL terminator.
const PROTOENT_HDR_LEN: u32 = 12;
const PROTOENT_ALIASES_OFF: u32 = PROTOENT_HDR_LEN;
const PROTOENT_NAME_CAP: usize = 16; // longest real entry below is 4 bytes
const PROTOENT_NAME_OFF: u32 = PROTOENT_ALIASES_OFF + 4;
const PROTOENT_BUF_LEN: u32 = PROTOENT_NAME_OFF + PROTOENT_NAME_CAP as u32;

// getnetbyname()/getnetbyaddr()'s netent blob layout within
// LIB_NETENTBUF. Real netent is 4 fields (16 bytes: n_name/n_aliases/
// n_addrtype/n_net); n_aliases is always just a NULL terminator.
const NETENT_HDR_LEN: u32 = 16;
const NETENT_ALIASES_OFF: u32 = NETENT_HDR_LEN;
const NETENT_NAME_CAP: usize = 16; // longest real entry below is 8 bytes
const NETENT_NAME_OFF: u32 = NETENT_ALIASES_OFF + 4;
const NETENT_BUF_LEN: u32 = NETENT_NAME_OFF + NETENT_NAME_CAP as u32;

// Small, static well-known-name tables backing getservbyname()/
// getservbyport()/getprotobyname()/getprotobynumber()/getnetbyname()/
// getnetbyaddr() -- this project is used by general Amiga software, not
// just as a CI testing tool (see README.md's own framing), so resolving
// common names is worth doing for real rather than leaving these `_hs_stub`.
// Deliberately NOT a parsed-services-file/protocols-file/networks-file
// database (a distinct, much bigger feature -- real config file parsing,
// live updates, an arbitrarily large entry set) -- a small compiled-in
// table of the entries real software actually asks for by name is exactly
// what several minimal AmiTCP-compatible stacks ship instead, and this
// project has no filesystem-config-file story to parse one from anyway.
// (name, port, protocol) -- port in host byte order (this plugin's own
// arithmetic never needs network order internally; do_getservbyport
// converts once at its own boundary, see that function's own comment for
// why m68k's big-endian byte order makes that conversion a no-op there in
// practice, but not in the general case this code is written for).
const SERVICES: &[(&str, u16, &str)] = &[
    ("ftp-data", 20, "tcp"),
    ("ftp", 21, "tcp"),
    ("ssh", 22, "tcp"),
    ("telnet", 23, "tcp"),
    ("smtp", 25, "tcp"),
    ("domain", 53, "tcp"),
    ("domain", 53, "udp"),
    ("tftp", 69, "udp"),
    ("finger", 79, "tcp"),
    ("http", 80, "tcp"),
    ("pop3", 110, "tcp"),
    ("nntp", 119, "tcp"),
    ("ntp", 123, "udp"),
    ("imap", 143, "tcp"),
    ("snmp", 161, "udp"),
    ("irc", 194, "tcp"),
    ("https", 443, "tcp"),
    ("submission", 587, "tcp"),
];

// (name, protocol number) -- IANA's own well-known assignments, the same
// small set most minimal /etc/protocols files ship.
const PROTOCOLS: &[(&str, i32)] = &[
    ("ip", 0),
    ("icmp", 1),
    ("igmp", 2),
    ("tcp", 6),
    ("egp", 8),
    ("udp", 17),
];

// (name, network number) -- real /etc/networks files are typically this
// short too; "loopback" is the one entry any real software could plausibly
// depend on.
const NETWORKS: &[(&str, u32)] = &[("loopback", 127), ("default", 0)];

// -- Host network device: net_send/net_recv as a smoltcp phy::Device ------
//
// This project's own `Interface` (below) has no dedicated loopback
// interface separate from its one "real" one -- 127.0.0.1 is just an
// extra address bolted onto the same interface `INTERFACE_ADDR` lives on
// (see `init()`). Under the deterministic `net = "loopback"` backend
// that's invisible: Copperline's own Loopback backend echoes back
// *everything* transmitted regardless of destination, so traffic to
// 127.0.0.1 "works" purely as a side effect of that backend bouncing
// literally everything. Under `net = "nat"` (real outbound connectivity,
// needed for bsdsocktest's NETWORK tier), Copperline's NAT engine
// actually inspects the destination and only knows what to do with its
// own gateway/DNS addresses or a real outbound one -- a frame addressed
// to 127.0.0.1 is neither, and gets silently dropped, so a self-connect
// to 127.0.0.1 never gets its SYN-ACK and hangs forever. Found running
// bsdsocktest's NETWORK tier for the first time: every earlier test in
// this project (including its own 127.0.0.1 fix, `init()`'s own comment)
// was exercised only under the Loopback backend, so this gap had nothing
// to surface on until a real backend was ever wired in.
//
// The real fix, matching what a real OS kernel's own loopback interface
// does: never let 127.0.0.0/8-addressed traffic reach the host backend
// at all. `TxTok::consume` intercepts it via `loopback_response` and
// queues a reply frame on `loopback_rx` instead of calling `net_send`;
// `receive()` drains that queue before ever calling `net_recv`. Shared
// via `Rc<RefCell<..>>` between `HostDevice` and every `TxTok`
// `transmit()` hands out, since a `TxToken` has no way to borrow back the
// `Device` that created it.
//
// Two distinct cases, found one at a time running bsdsocktest's NETWORK
// tier: an IPv4 packet already addressed to 127.0.0.0/8 just needs
// echoing straight back (the same frame is both the "request" and, once
// smoltcp's own interface reprocesses it as inbound, its own answer --
// this is a real TCP/UDP payload, smoltcp itself handles the actual
// protocol logic on the receiving end). But before smoltcp ever gets to
// emit that IPv4 packet at all, it first needs to resolve 127.0.0.1's
// *hardware* address via ARP, exactly like it would for any other
// on-link Ethernet destination -- 127.0.0.1 was only ever added as a
// second address on this one Ethernet-medium interface (`init()`'s own
// comment), not through any dedicated loopback-medium mechanism smoltcp
// might otherwise skip ARP for. Under `net = "loopback"` that ARP request
// also just gets echoed back to itself (a request, which is never
// mistaken for a reply, so it's mostly harmless noise) while the *real*
// reason 127.0.0.1 worked there was the IPv4 echo above; under `net =
// "nat"`, Copperline's NAT engine only answers ARP for its own
// gateway/DNS addresses (see Copperline's own
// net/nat/engine.rs), so a request for 127.0.0.1 goes unanswered forever
// and smoltcp never even attempts to send the real IPv4 packet -- no
// destination MAC, no transmission, silently stuck on ARP resolution
// alone. `loopback_response` answers that itself, synthesizing a real
// ARP reply from `INTERFACE_MAC` the same way a real loopback interface's
// own implicit answer would.
type LoopbackQueue = Rc<RefCell<VecDeque<Vec<u8>>>>;

fn is_loopback_addr(addr: Ipv4Address) -> bool {
    Ipv4Cidr::new(Ipv4Address::new(127, 0, 0, 0), 8).contains_addr(&addr)
}

// Returns the frame to queue onto `loopback_rx` in place of actually
// transmitting `frame`, or `None` if `frame` should go out for real.
// Malformed/unrecognized frames (not IPv4, not an ARP request) are never
// loopback traffic by definition, so a parse failure just means "send it
// normally," not an error.
fn loopback_response(frame: &[u8]) -> Option<Vec<u8>> {
    let eth = EthernetFrame::new_checked(frame).ok()?;
    match eth.ethertype() {
        EthernetProtocol::Ipv4 => {
            let ip = Ipv4Packet::new_checked(eth.payload()).ok()?;
            is_loopback_addr(ip.dst_addr()).then(|| frame.to_vec())
        }
        EthernetProtocol::Arp => {
            let arp = ArpPacket::new_checked(eth.payload()).ok()?;
            let ArpRepr::EthernetIpv4 {
                operation: ArpOperation::Request,
                source_hardware_addr,
                source_protocol_addr,
                target_protocol_addr,
                ..
            } = ArpRepr::parse(&arp).ok()?
            else {
                return None;
            };
            if !is_loopback_addr(target_protocol_addr) {
                return None;
            }
            let reply = ArpRepr::EthernetIpv4 {
                operation: ArpOperation::Reply,
                source_hardware_addr: INTERFACE_MAC,
                source_protocol_addr: target_protocol_addr,
                target_hardware_addr: source_hardware_addr,
                target_protocol_addr: source_protocol_addr,
            };
            let mut buf = vec![0u8; 14 + reply.buffer_len()];
            let mut out = EthernetFrame::new_unchecked(&mut buf);
            out.set_src_addr(INTERFACE_MAC);
            out.set_dst_addr(source_hardware_addr);
            out.set_ethertype(EthernetProtocol::Arp);
            reply.emit(&mut ArpPacket::new_unchecked(out.payload_mut()));
            Some(buf)
        }
        _ => None,
    }
}

struct RxTok(Vec<u8>);

impl phy::RxToken for RxTok {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

struct TxTok {
    loopback_rx: LoopbackQueue,
}

impl phy::TxToken for TxTok {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = vec![0u8; len];
        let result = f(&mut buf);
        match loopback_response(&buf) {
            Some(reply) => self.loopback_rx.borrow_mut().push_back(reply),
            None => {
                // Safety: `buf` is this module's own memory, exactly what net_send expects.
                unsafe { net_send(buf.as_ptr() as i32, buf.len() as i32) };
            }
        }
        result
    }
}

struct HostDevice {
    loopback_rx: LoopbackQueue,
}

impl phy::Device for HostDevice {
    type RxToken<'a> = RxTok;
    type TxToken<'a> = TxTok;

    fn receive(&mut self, _timestamp: Instant) -> Option<(RxTok, TxTok)> {
        if let Some(frame) = self.loopback_rx.borrow_mut().pop_front() {
            return Some((
                RxTok(frame),
                TxTok {
                    loopback_rx: self.loopback_rx.clone(),
                },
            ));
        }
        let mut buf = [0u8; 1536]; // one standard Ethernet frame
                                   // Safety: `buf` is this module's own memory, exactly what net_recv expects.
        let n = unsafe { net_recv(buf.as_mut_ptr() as i32, buf.len() as i32) };
        if n <= 0 {
            return None;
        }
        Some((
            RxTok(buf[..n as usize].to_vec()),
            TxTok {
                loopback_rx: self.loopback_rx.clone(),
            },
        ))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<TxTok> {
        Some(TxTok {
            loopback_rx: self.loopback_rx.clone(),
        })
    }

    fn capabilities(&self) -> phy::DeviceCapabilities {
        // DeviceCapabilities is #[non_exhaustive], so field assignment
        // (not a struct-literal with ..Default::default()) is the only way
        // to set it from outside smoltcp's own crate.
        let mut caps = phy::DeviceCapabilities::default();
        caps.medium = phy::Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps
    }
}

// -- Fd table (Phase 2, extended in Phase 3) --------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SockKind {
    Tcp,
    Udp,
    // socket(AF_INET, SOCK_RAW, IPPROTO_ICMP) -- see do_socket's own
    // comment for why this is its own kind rather than folded into the
    // pre-existing (and, before this, only) SOCK_RAW handling that
    // silently treated it as SockKind::Tcp.
    Icmp,
}

// -- Host-socket backend (`[config] transport = "host"`) -------------------
//
// A separate, much smaller sibling of the smoltcp fd table above: a TCP or
// UDP fd opened while `Board::host_backend` is set lives in
// `Board::host_fds` instead of `Board::fds`, and is driven directly through
// the `sock_*` host imports rather than this module's own smoltcp
// `Interface`. Deliberately NOT folded into `FdSlot` itself -- that struct
// is threaded through ~60 call sites (bind/listen/accept/Dup2Socket/
// ObtainSocket/WaitSelect/event sampling/setsockopt and more), all of it
// tuned against specific bsdsocktest failures (see FdSlot's own field
// comments). Grafting a second transport into it risks silently regressing
// that hard-won behaviour for zero benefit, since this backend only
// implements a subset of LVOs so far (see
// HOSTSOCKET-HOST-BACKEND-PLAN.md's phased scope) -- every unimplemented
// call (setsockopt/WaitSelect on some ops/Dup2Socket/...) on a host-backed
// fd is simply absent from `fds`, so `fd_index` reports it as
// no-such-descriptor (ENOTSOCK/EBADF) exactly like any other invalid fd,
// with no risk to the existing smoltcp path at all. `HostFdSlot.kind`
// *does* reuse `SockKind`'s plain enum (just its `Tcp`/`Udp` discriminants,
// not `FdSlot` itself) rather than defining a near-duplicate. `Copy`
// (unlike `FdSlot`, which holds an `Rc<()>` refcount for Dup2Socket
// aliasing -- this backend doesn't support that yet): every field here is
// plain data, so functions needing a snapshot of it (e.g.
// `do_getsockopt_host`) can just copy it out instead of juggling borrows
// against `self.host_fds`.
#[derive(Clone, Copy)]
struct HostFdSlot {
    // The host-side handle `sock_open` returned; passed back to every
    // other `sock_*` import unchanged.
    handle: i32,
    kind: SockKind,
    nonblocking: bool,
    // Mirrors `FdSlot::connect_started`: `do_connect_host` re-issues
    // `sock_connect` on every retry (real non-blocking BSD connect()
    // semantics -- a second call on the same socket reports EISCONN once
    // it succeeded, or the original failure again once it didn't, see
    // `do_connect_host`'s own comment), so this only gates whether the
    // *first* attempt should be treated as fresh (0/EINPROGRESS) or a
    // retry (EALREADY on still-pending, in non-blocking mode). Also
    // covers UDP `connect()`, which always completes immediately at the
    // OS level (no handshake) -- `sock_connect` still reports `0` on the
    // very first call, so this never actually gates a real wait there,
    // just keeps the bookkeeping uniform across both kinds.
    connect_started: bool,
    // Set by `do_listen_host` on success. Mirrors `FdSlot::is_listener`:
    // `sample_event_level_host` needs to know a fd is a listener to report
    // `accept_ready` instead of `read_ready`/`write_ready` for it (a
    // listening socket's own `sock_poll` readable bit means "a connection
    // is pending accept()", not "there is data to read").
    is_listener: bool,
    opts: HostSockOpts,
}

// The setsockopt() options this backend can't hand straight to
// `sock_setopt`/`sock_getopt` (see those imports' own doc comments in
// src/wasmboard.rs for why): SO_LINGER's own two-field struct doesn't fit
// that single-`value` ABI, and SO_RCVTIMEO/SO_SNDTIMEO have no real effect
// on a socket this backend always keeps OS-level non-blocking. Plain
// roundtrip storage, same reasoning (and the same real-BSD-numbering
// layout) as `SockOpts`'s own identical fields on the smoltcp path -- kept
// as a separate, smaller struct rather than reusing `SockOpts` wholesale,
// since every *other* field there (SO_REUSEADDR/SO_KEEPALIVE/SO_RCVBUF/
// SO_SNDBUF/TCP_NODELAY) goes to the real host socket on this backend
// instead. SO_EVENTMASK/`ev_prev` below are the one exception: those *do*
// need local bookkeeping here too, same reasoning as `SockOpts`'s own.
#[derive(Clone, Copy, Default)]
struct HostSockOpts {
    linger_onoff: i32,
    linger_secs: i32,
    rcvtimeo: (i32, i32),
    sndtimeo: (i32, i32),
    // Mirrors `SockOpts::eventmask`/`ev_prev` (same doc comments apply) --
    // `sample_event_level_host` and `process_socket_events`'s host-fd arm
    // are the host-backend counterparts of `sample_event_level`/that same
    // function's smoltcp arm.
    eventmask: i32,
    ev_prev: Option<EventLevel>,
}

// `sock_poll`'s readiness bitmask (src/wasmboard.rs's own doc comment on
// that import) -- hand-kept in sync with that file, same convention this
// file's own BSD errno table above already follows for `translate_errno`'s
// output.
const SOCK_READABLE: i32 = 1;
const SOCK_WRITABLE: i32 = 2;
const SOCK_ERROR: i32 = 4;
const SOCK_HUP: i32 = 8;

// Per-fd setsockopt()/getsockopt() state (Board::do_setsockopt/
// do_getsockopt). None of these actually change smoltcp's own behaviour
// -- they're plain roundtrip storage, matching bsdsocktest's own stated
// scope for several of them ("Set/get roundtrip only. Actual timeout
// enforcement... cannot be safely tested", its own SO_RCVTIMEO test
// comment) and simply not worth building real enforcement for the rest
// (SO_KEEPALIVE probes, SO_LINGER's close()-blocking behaviour,
// TCP_NODELAY's Nagle-algorithm toggle) until a real consumer needs the
// actual behaviour rather than just the option surviving a roundtrip.
#[derive(Clone, Copy)]
struct SockOpts {
    reuseaddr: bool,
    keepalive: bool,
    // (l_onoff != 0, l_linger), the two fields of a real `struct linger`.
    linger_onoff: i32,
    linger_secs: i32,
    // (tv_secs, tv_micro) of a real `struct timeval`.
    rcvtimeo: (i32, i32),
    sndtimeo: (i32, i32),
    nodelay: bool,
    rcvbuf: i32,
    sndbuf: i32,
    // SO_EVENTMASK: which FD_* event types (see those consts' own comment)
    // this fd should report via GetSocketEvents(). 0 (the default) means
    // event tracking is off for this fd -- `process_socket_events` skips
    // it entirely, so idle sockets never pay for the extra bookkeeping.
    eventmask: i32,
    // The last-sampled level-triggered readiness for this fd, used to
    // edge-detect the transitions `process_socket_events` reports (a real
    // FD_READ/FD_WRITE/etc event fires once *when a condition becomes
    // true*, not on every tick it stays true). `None` until do_setsockopt
    // first sets a non-zero `eventmask` -- see that arm's own comment for
    // why sampling has to happen there rather than defaulting to "nothing
    // ready", which would otherwise fire spurious events for a socket
    // that's already e.g. write-ready (can_send()) the moment its mask is
    // set (found while implementing this against bsdsocktest's own
    // eventmask_no_spurious test).
    ev_prev: Option<EventLevel>,
}

impl SockOpts {
    fn new() -> Self {
        SockOpts {
            reuseaddr: false,
            keepalive: false,
            linger_onoff: 0,
            linger_secs: 0,
            rcvtimeo: (0, 0),
            sndtimeo: (0, 0),
            nodelay: false,
            // Not 0: SO_RCVBUF/SO_SNDBUF read back *some* real, positive
            // default on every actual stack, and reporting one here for
            // free before any setsockopt() at all matches that rather
            // than implying no buffer exists.
            rcvbuf: SOCKET_BUF_LEN as i32,
            sndbuf: SOCKET_BUF_LEN as i32,
            eventmask: 0,
            ev_prev: None,
        }
    }
}

// Level-triggered readiness for one fd, sampled by `Board::sample_event_level`
// and diffed tick-over-tick by `process_socket_events` to synthesize the
// edge-triggered FD_* events GetSocketEvents() reports. Mirrors the same
// readiness rules `scan_select` already uses for WaitSelect() (see its own
// comments for why e.g. write_ready excludes SynSent/SynReceived) --
// intentionally not shared code with scan_select, since that function scans
// caller-supplied fd_sets while this one iterates every fd with a
// registered eventmask and additionally needs `may_recv`/`connecting`,
// which scan_select has no reason to track.
#[derive(Clone, Copy)]
struct EventLevel {
    read_ready: bool,
    write_ready: bool,
    // TCP listeners only: a connection is pending accept().
    accept_ready: bool,
    // TCP only: false once the peer has sent FIN/RST -- a separate signal
    // from `read_ready` (which also goes true on EOF, matching real
    // select() semantics) so FD_CLOSE can be its own distinct edge instead
    // of being folded into FD_READ.
    may_recv: bool,
    // TCP only: true while a connect() is still outstanding (SynSent/
    // SynReceived) -- lets FD_CONNECT's edge (leaving this state) be told
    // apart from a plain FD_WRITE edge (buffer space freeing up on an
    // already-established connection).
    connecting: bool,
}

struct FdSlot {
    kind: SockKind,
    socket: SocketHandle,
    // Shared between every fd aliasing the same underlying socket (Phase
    // 4's Dup2Socket -- real dup()/dup2() semantics: the socket itself
    // isn't actually closed until every alias is). `Rc::strong_count`
    // *is* the refcount -- no separate counter needed, see `do_close`.
    // A fresh socket (do_socket/do_accept) always gets its own new `Rc`;
    // only `do_dup2socket` ever clones an existing one. Simplification:
    // `nonblocking`/`bind_port`/`is_listener`/`udp_peer` below stay
    // per-fd rather than shared across aliases (real dup()'d descriptors
    // share file-status flags too, but nothing in this project's own
    // tests or bsdsocktest's Dup2Socket coverage exercises that).
    refcount: Rc<()>,
    nonblocking: bool,
    // TCP only: the port bind() recorded, consumed by listen()/connect() --
    // smoltcp's tcp::Socket has no "just bound, not yet listening/
    // connecting" state to hold this for us. Also doubles as "what port is
    // this listener on" once listen() has been called, since accept()
    // needs it again for each fresh replacement listener socket.
    bind_port: Option<u16>,
    // TCP only: the address bind() was given, if it named a specific one
    // rather than INADDR_ANY (0.0.0.0) -- do_getsockname needs this to
    // report back the address that was actually requested; used to always
    // report `INTERFACE_ADDR` regardless of what was bound, so a specific
    // (non-wildcard) bind() and a wildcard one were indistinguishable to a
    // caller checking its own bound address afterwards.
    bind_addr: Option<Ipv4Address>,
    // TCP only: true once listen() has been called -- governs accept()
    // semantics (see `Board::do_accept`).
    is_listener: bool,
    // UDP only: the default peer recorded by connect() on a SOCK_DGRAM fd
    // (real BSD semantics: connect() on UDP just records a peer, no
    // handshake), used by plain send()/recv() when no explicit
    // destination/source is given.
    udp_peer: Option<(Ipv4Address, u16)>,
    // TCP only: true once do_connect has issued smoltcp's connect() for
    // this fd. `do_connect` used to key its own "is this the first call or
    // a retry" decision off `socket.is_open()`, which is also false once a
    // refused connection's RST has landed (Closed looks identical to
    // never-connected) -- the guest's blocking-wait retry loop
    // (_ring_doorbell_blocking in guest/entry.s) calls CALL_CONNECT again
    // on every wake to collect the result, and re-entering the "first
    // attempt" branch there restarted the connection instead of returning
    // the already-known ECONNREFUSED. Restarting is worse than a wrong
    // answer: that guest loop only calls CALL_REGISTER_WAIT once per
    // operation, so the fresh RES_PENDING this produced had no waiter ever
    // registered for it and hung forever (found running bsdsocktest's own
    // "connect(): ECONNREFUSED to closed port" test, which -- unlike this
    // project's own earlier self-connect tests -- actually drives a
    // connect() through to a real refusal and then polls again).
    connect_started: bool,
    // TCP only: true once this fd's own connection has genuinely reached
    // `tcp::State::Established` at least once (set by do_connect the
    // first time it observes this, or immediately for do_accept's own
    // already-Established new connection). Distinguishes "never actually
    // connected" (a failed `connect()` -- send()/recv() on it should
    // still report `ECONNREFUSED`) from "was connected, then the peer
    // went away" (should report `ECONNRESET`/`EPIPE` instead, see
    // `shutdown_by_us`) once `may_send()`/`may_recv()` goes false --
    // smoltcp's own `tcp::Socket` has no public way to ask *why* a
    // connection reached `Closed`, so this project has to track the
    // "was it ever really up" half of that distinction itself.
    was_established: bool,
    // TCP only: true once `do_shutdown` has called `.close()` on this
    // fd's own socket -- i.e. *we* locally asked for the teardown.
    // send()/recv() use this to report `EPIPE` (we already said we're
    // done) instead of `ECONNRESET` (the peer tore it down out from
    // under us, e.g. via an RST -- see `was_established`'s own comment)
    // once the connection is no longer usable. Real dup()'d descriptors
    // share this (shutdown() operates on the underlying connection, not
    // a specific fd) but this project keeps it per-fd like `nonblocking`/
    // `bind_port`/etc above, for the same reason: nothing here exercises
    // shutdown() through one alias and send()/recv() through another.
    shutdown_by_us: bool,
    opts: SockOpts,
}

// -- Per-task state (Phase 2) -----------------------------------------------
//
// Keyed by the calling task's own pointer (FindTask(NULL), passed as arg0 of
// every call -- see hostsocket_board.h). A HashMap here does correctly and
// cheaply what would otherwise be a hand-rolled linear-scan-and-claim table
// in guest assembly; the guest's only job is fetching its own task pointer
// and passing it along (see entry.s's _stage_task).
#[derive(Default)]
struct TaskState {
    errno_ptr: Option<(u32, u16)>,
    last_errno: i32,
    // SocketBaseTags(SBTM_SETVAL(SBTC_HERRNOLONGPTR), ...)'s registered
    // pointer -- same idea as `errno_ptr`, but h_errno has no SetErrnoPtr-
    // style dedicated LVO of its own (SocketBaseTagList is the only way to
    // register it) and no size variants (always a 4-byte LONG, `extern int
    // h_errno` in netdb.h), so a plain `Option<u32>` is enough.
    herrno_ptr: Option<u32>,
    // SocketBaseTags(SBTM_SETVAL(SBTC_SIGEVENTMASK), ...)'s registered
    // signal bitmask: which Amiga signal(s) `process_socket_events` should
    // wake this task with when one of its fds reports an event. 0 (the
    // default) means this task hasn't registered -- it never receives
    // event wakeups even if some fd's SO_EVENTMASK happens to be set.
    sig_event_mask: u32,
    // SocketBaseTags(SBTM_SETVAL(SBTC_BREAKMASK), ...)'s registered Ctrl-C
    // signal bitmask -- pure roundtrip storage (GET reads back whatever
    // was last SET), like the "doesn't change smoltcp's actual behavior"
    // setsockopt options `SockOpts` already tracks: nothing in this
    // project actually delivers a real Ctrl-C break signal today.
    break_mask: u32,
}

// What a task registered as "waiting for" the last time one of its calls
// returned RES_PENDING (see `Board::last_pending`) -- filled in by
// CALL_REGISTER_WAIT once the guest is actually about to Wait() on it (see
// `Board::waiters`).
#[derive(Clone, Copy)]
enum WaitKind {
    Connect {
        fd: i32,
    },
    Recv {
        fd: i32,
    },
    // Blocking recv(MSG_OOB) on a host-backed fd, waiting for real TCP
    // urgent data to arrive -- host-backend only (see `do_recv_host`'s own
    // MSG_OOB branch), since the smoltcp path has no urgent-data support
    // to build this on at all.
    RecvOob {
        fd: i32,
    },
    // Blocking send() on a TCP fd, once a short write has left `len` bytes
    // only partially queued (see `Board::send_progress` and `do_send`'s
    // own comment for why -- real blocking BSD send() only returns once
    // everything requested is queued, not whenever the socket's own
    // buffer happens to run out).
    Send {
        fd: i32,
    },
    Accept {
        fd: i32,
    },
    Select {
        read_mask: u32,
        write_mask: u32,
        nfds: u32,
        deadline: Option<i64>,
    },
    // gethostbyname(): unlike every other WaitKind, there's no cheap
    // non-consuming "is this ready yet" check available -- smoltcp's
    // dns::Socket::get_query_result() is the only way to ask, and it
    // *consumes* the result once the query is done. process_waiters does
    // that consuming check itself (on every tick, until it stops being
    // Pending) and stashes the outcome in `Board::dns_results`, since
    // do_gethostbyname can't safely call get_query_result a second time
    // once process_waiters already has (see do_gethostbyname's own
    // comment for why this needs to be exactly this shape, not the
    // simpler "always ready" version an earlier draft used).
    Dns,
    // gethostbyname() under `resolver = "host"`: the same one-shot-consume
    // shape as WaitKind::Dns (resolve_poll's own -2/-1/0 states are exactly
    // as consuming -- calling it again after 0 or -1 finds no job left, see
    // resolve_poll's own comment), so process_waiters does the one real
    // poll here too and stashes the outcome in the SAME `Board::dns_results`
    // cache Dns uses -- do_gethostbyname's success/failure handling doesn't
    // need to know which resolver strategy actually produced the answer.
    HostResolve,
    // gethostbyaddr(): unlike WaitKind::Dns, a plain `udp::Socket`'s own
    // `can_recv()` is safely re-checkable any number of times (it isn't
    // consuming the way `dns::Socket::get_query_result()` is), so there's
    // no need for a matching `Board::ptr_results` cache -- process_waiters
    // just checks readiness (or the deadline) here, and `do_gethostbyaddr`
    // itself does the one actual (consuming) `recv_slice()` once it's
    // been woken.
    Ptr,
}

// The cached, one-shot result of a get_query_result() call process_waiters
// already made (see WaitKind::Dns's own comment) -- do_gethostbyname
// consumes this via Board::dns_results instead of ever calling
// get_query_result itself. A plain Vec, not get_query_result's own
// heapless::Vec<_, DNS_MAX_RESULT_COUNT>: write_hostent already takes a
// plain &[IpAddress] slice (it doesn't care which Vec flavor backs it),
// so converting once here avoids naming smoltcp's internal heapless
// dependency anywhere else in this file.
enum DnsOutcome {
    Ok(Vec<IpAddress>),
    Failed,
}

// One in-flight gethostbyaddr() reverse-DNS query -- see `Board::ptr_pending`'s
// own comment for why this exists as hand-rolled state instead of reusing
// `dns_queries`/`dns_results` (smoltcp's own `dns::Socket` can't do PTR
// lookups at all).
struct PtrQuery {
    task: u32,
    // Matched against the response's own transaction ID to reject a
    // stray/unrelated datagram landing on the same socket.
    transaction_id: u16,
    // The guest's own LIB_HOSTENTBUF scratch buffer (same convention
    // gethostbyname's `buf_addr` already uses) to write the resolved
    // struct hostent into on success.
    buf_addr: u32,
    // The address that was actually queried -- echoed back into the
    // resolved hostent's own h_addr_list, matching real gethostbyaddr()
    // semantics (the hostent describes the *address given*, now with a
    // name attached, not a fresh lookup of that name).
    orig_addr: Ipv4Address,
    // In `Board::micros` units (see that field's own doc comment on why
    // it's colour-clock ticks, not real microseconds) -- do_wait_select's
    // own CCK_HZ conversion pattern, not a literal duration.
    deadline: i64,
}

struct Waiter {
    task: u32,
    signal_mask: u32,
    kind: WaitKind,
}

// -- Board state -------------------------------------------------------------

struct Board {
    booted: bool,
    rom: Vec<u8>,
    device: HostDevice,
    // `Option`: these need a live `Device` to construct (Interface::new
    // calls device.capabilities()), so they can't exist before init() runs.
    iface: Option<Interface>,
    sockets: Option<SocketSet<'static>>,
    fds: [Option<FdSlot>; MAX_FDS],
    next_local_port: u16,
    argptr: u32,
    result: i32,
    // Coarse monotonic clock fed to smoltcp, advanced by tick()'s `cck`
    // (colour clocks, ~3.546895 MHz PAL -- Copperline's own
    // config::COLOR_CLOCK_HZ). Despite the field's name, this is *not*
    // real microseconds -- it's raw colour-clock ticks. smoltcp only
    // needs it non-decreasing for its RTT/retransmit timers to make
    // progress, so that mismatch never mattered there. It does matter for
    // WaitSelect's timeout deadlines, which are real caller-supplied
    // durations: `do_wait_select` converts those through `CCK_HZ` before
    // comparing against this clock, rather than assuming a 1:1 mapping
    // (an earlier version did, and a `struct timeval` of one second
    // elapsed in ~280ms of real time instead -- 1,000,000 / 3,546,895 ≈
    // that same ratio).
    micros: i64,
    tasks: HashMap<u32, TaskState>,
    // What each task was told (RES_PENDING) about, so CALL_REGISTER_WAIT
    // knows what to actually register once the guest commits to blocking.
    last_pending: HashMap<u32, WaitKind>,
    waiters: Vec<Waiter>,
    // Bytes of the current blocking send() already queued for this task,
    // keyed by task since only one call can be in flight per task at a
    // time (same rationale as `dns_queries`) -- `do_send` reads this back
    // on every retry the guest's blocking-doorbell loop makes (it re-
    // stages the SAME buf_addr/len every time, see entry.s's
    // _ring_doorbell_blocking) so it knows to resume from `buf_addr +
    // already_sent`, not re-send from the start.
    send_progress: HashMap<u32, (i32, usize)>,
    // TCP sockets CloseSocket() has orphaned (no fd points at them anymore)
    // but whose FIN handshake hasn't reached State::Closed yet -- see
    // do_close's own comment for why removing them from `sockets` right
    // away would silently drop the FIN. Still-live entries keep getting
    // polled every tick() exactly like any fd-owned socket (SocketSet
    // doesn't distinguish "owned by an fd" from "just sitting there"), so
    // this is only bookkeeping for when it's finally safe to reclaim them.
    // Second element: the deadline (`Board::micros` units, see that
    // field's own doc comment) past which this socket gets aborted
    // rather than waited on further -- see `tick()`'s own reaping
    // comment for why a real timeout is needed here, not just "wait
    // however long it takes".
    closing_sockets: Vec<(SocketHandle, i64)>,
    // See do_wait_select's own comment: the deadline a WaitSelect() call
    // computed on its first attempt, kept around so a retry reuses it
    // instead of computing a new one relative to "now" every time.
    wait_select_deadline: HashMap<u32, i64>,
    // (task, signal mask) pairs ready to be Signal()'d; the guest's
    // installed interrupt server drains this via REG_WAKE_TASK/
    // REG_WAKE_SIGNAL/REG_WAKE_ACK, and int2() is non-zero exactly while
    // it's non-empty.
    wake_queue: VecDeque<(u32, u32)>,
    // The high word of a 32-bit register write still waiting for its low
    // word (see `write`'s header comment for why this exists at all: a
    // real 68000's 16-bit external data bus splits every guest `move.l`
    // into two 16-bit bus cycles, so a write to e.g. REG_CALL can arrive
    // here as two separate size=2 calls instead of one size=4 call).
    pending_write_hi: Option<(i32, u16)>,
    // The one persistent DNS socket every gethostbyname() call shares
    // (unlike TCP/UDP, a DNS query isn't tied to a guest-visible fd, so
    // there's nothing per-call to hang a SocketHandle off of). `Option`
    // for the same reason `sockets`/`iface` are: it needs init() to have
    // run.
    dns_socket: Option<SocketHandle>,
    // In-flight query per calling task -- do_gethostbyname checks this on
    // every retry (the guest's blocking-doorbell loop re-dispatches
    // CALL_GETHOSTBYNAME with the same staged args until it stops
    // returning RES_PENDING) rather than storing state in the argblock
    // itself.
    dns_queries: HashMap<u32, dns::QueryHandle>,
    // Finished query outcomes, filled in by process_waiters (see
    // WaitKind::Dns's own comment for why do_gethostbyname itself never
    // calls dns::Socket::get_query_result directly).
    dns_results: HashMap<u32, DnsOutcome>,
    // Whether gethostbyname() should route through the host's own OS
    // resolver (the `resolve` capability) instead of this project's own
    // DNS-over-`net` query -- `[config] resolver = "host"` (see
    // src/hostsocket.rs), cached at init() time. Defaults to false (the
    // existing `dns_socket`/`dns_queries` path, unchanged).
    resolver_host: bool,
    // In-flight host-resolver request id per calling task, mirroring
    // `dns_queries`'s own shape -- funnels into the same `dns_results`
    // cache via process_waiters's WaitKind::HostResolve arm, so
    // do_gethostbyname's own success/failure handling is shared between
    // both resolver strategies.
    host_resolve_jobs: HashMap<u32, i32>,
    // Per-task queue of (fd, event_mask) pairs GetSocketEvents() hasn't
    // drained yet, filled in by `process_socket_events`. FIFO, coalesced by
    // fd (a second event on an fd already queued just ORs its bits into
    // the existing entry rather than growing the queue -- GetSocketEvents()
    // reports one *fd* at a time, with however many event types have
    // accumulated on it since the last drain, not one event-type at a
    // time).
    event_queues: HashMap<u32, VecDeque<(i32, u32)>>,
    // SocketBaseTags(SBTM_SETVAL(SBTC_DTABLESIZE), ...)'s last-requested
    // descriptor table size, reported back by both `do_getdtablesize` and
    // SBTC_DTABLESIZE's own GET(REF). Roundtrip-only, like `break_mask`
    // above -- `fds` stays a fixed `MAX_FDS`-sized array regardless of what
    // this claims; nothing in bsdsocktest's own SBTC_DTABLESIZE tests
    // actually opens enough sockets to notice the difference, only checks
    // the reported number. Monotonic (never shrinks on a later SET,
    // starting from `MAX_FDS`) -- matches bsdsocktest's own "Restore (may
    // not reduce)" expectation for this tag.
    reported_dtablesize: i32,
    // gethostbyaddr()'s own reverse (PTR) DNS resolver -- a plain UDP
    // socket, not smoltcp's own `dns::Socket` (`dns_socket` above):
    // that type's `start_query`/`get_query_result` API is hard-typed to
    // A/AAAA lookups returning `IpAddress` results, with no PTR query
    // type and no way to get a domain-name answer back out, so a real
    // reverse lookup means speaking DNS wire format directly instead
    // (`do_gethostbyaddr`'s own comment has the full account). Bound to
    // an ephemeral port once at init() time, the same "one persistent
    // socket, not tied to any guest fd" shape `dns_socket` already uses.
    ptr_socket: Option<SocketHandle>,
    // The single in-flight reverse-DNS query (this project's own
    // established "one active task at a time" simplification, same as
    // `dns_queries`/`send_progress`) -- `None` between calls.
    ptr_pending: Option<PtrQuery>,
    // Cached at init() time from the same `[config] dns_server` this
    // project's forward resolver (`dns_socket`) already reads --
    // `do_gethostbyaddr` needs this address too, but `init()`'s own
    // local only lived long enough to construct `dns::Socket::new`.
    dns_server_addr: Ipv4Address,
    // This interface's own address, cached at init() time from `[config]
    // address` (defaulting to INTERFACE_ADDR) -- see that const's own
    // comment for why the default can't just be assumed everywhere
    // (net = "bridge" needs a real LAN's own subnet, not NAT's virtual
    // one). getsockname()/gethostid()/ICMP-recv's synthesized IP header
    // all need the *effective* address, not the compile-time default.
    interface_addr: Ipv4Address,
    // ReleaseSocket()/ReleaseCopyOfSocket()/ObtainSocket()'s shared
    // socket pool, keyed by the caller-chosen (or library-assigned, for
    // UNIQUE_ID) `id` -- see do_release_socket's own comment for why a
    // second table keyed by `id` rather than fd number is the right
    // model here (this project has no real separate-process concept,
    // just separate calling tasks already sharing one fd table).
    socket_pool: HashMap<i32, FdSlot>,
    // The next `id` `resolve_pool_id` will try when asked to assign a
    // fresh one (UNIQUE_ID, -1) -- just a starting point for the search,
    // not itself guaranteed unused (that's what the pool lookup in
    // `resolve_pool_id` is for).
    next_pool_id: i32,
    // Host-socket-backed fds (see `HostFdSlot`'s own comment) -- disjoint
    // from `fds`: a given index is occupied in at most one of the two
    // tables at a time (`do_socket`'s own allocation loop checks both).
    host_fds: [Option<HostFdSlot>; MAX_FDS],
    // `[config] transport = "host"`, cached at init() time: whether a new
    // TCP/UDP `do_socket` should open a host-backed fd instead of a
    // smoltcp one. ICMP/DNS are unaffected either way -- see
    // `HostFdSlot`'s own comment for what this backend does and doesn't
    // cover.
    host_backend: bool,
    // `socket_pool`'s host-backend counterpart -- disjoint from it, same
    // "two different element types, two tables" pattern `fds`/`host_fds`
    // already follows. `resolve_pool_id` (id allocation) is shared
    // between both pools via `next_pool_id`.
    host_socket_pool: HashMap<i32, HostFdSlot>,
}

impl Board {
    fn new() -> Self {
        Board {
            booted: false,
            rom: Vec::new(),
            device: HostDevice {
                loopback_rx: Rc::new(RefCell::new(VecDeque::new())),
            },
            iface: None,
            sockets: None,
            fds: [const { None }; MAX_FDS],
            next_local_port: 49152,
            argptr: 0,
            result: 0,
            micros: 0,
            tasks: HashMap::new(),
            last_pending: HashMap::new(),
            waiters: Vec::new(),
            send_progress: HashMap::new(),
            closing_sockets: Vec::new(),
            wait_select_deadline: HashMap::new(),
            wake_queue: VecDeque::new(),
            pending_write_hi: None,
            dns_socket: None,
            dns_queries: HashMap::new(),
            dns_results: HashMap::new(),
            // Overwritten by init() with the real configured value.
            resolver_host: false,
            host_resolve_jobs: HashMap::new(),
            event_queues: HashMap::new(),
            reported_dtablesize: MAX_FDS as i32,
            ptr_socket: None,
            ptr_pending: None,
            // Overwritten by init() with the real configured/default
            // value -- this placeholder is never read before then.
            dns_server_addr: NAT_DNS_ADDR,
            interface_addr: INTERFACE_ADDR,
            socket_pool: HashMap::new(),
            next_pool_id: 0,
            host_fds: [const { None }; MAX_FDS],
            // Overwritten by init() with the real configured value.
            host_backend: false,
            host_socket_pool: HashMap::new(),
        }
    }

    fn init(&mut self) {
        self.rom = load_resource("rom");
        host_log(&format!(
            "hostsocket: init -- {} ROM bytes loaded",
            self.rom.len()
        ));

        // Interface address (and prefix): `[config] address = "a.b.c.d[/prefix]"`
        // (see src/hostsocket.rs), defaulting to INTERFACE_ADDR/24.
        // Copperline's own NAT backend hardcodes its guest address to
        // exactly INTERFACE_ADDR (see that const's own comment), so
        // overriding this only makes sense under net = "bridge", where a
        // real physical LAN's own subnet applies instead of NAT's virtual
        // one -- under net = "nat" or "loopback" this key should stay
        // unset. An unparsable value falls back to the default rather than
        // leaving the interface unconfigured with no diagnostic the guest
        // side could ever see.
        let (interface_addr, prefix) = config_get_string("address")
            .and_then(|s| parse_ipv4_cidr(&s))
            .unwrap_or((INTERFACE_ADDR, 24));
        self.interface_addr = interface_addr;

        let config = Config::new(HardwareAddress::Ethernet(INTERFACE_MAC));
        let mut iface = Interface::new(config, &mut self.device, Instant::from_micros(0));
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(IpAddress::Ipv4(interface_addr), prefix))
                .expect("two addresses always fit the interface's address list");
            // 127.0.0.1, not just INTERFACE_ADDR: real BSD sockets treat the
            // whole 127.0.0.0/8 loopback range as "myself" unconditionally,
            // regardless of the machine's actual configured address --
            // smoltcp has no such special-casing built in, so a guest
            // connect()ing to literal INADDR_LOOPBACK (127.0.0.1), as
            // bsdsocktest's own test suite's self-connect tests do rather
            // than using whatever getsockname() reports, would otherwise
            // never be recognized as addressed to us at all and hang
            // forever in SynSent (found running that suite for the first
            // time; every earlier phase's own self-connect testing used
            // INTERFACE_ADDR directly, never literal 127.0.0.1, so this
            // gap had no earlier test to surface on).
            addrs
                .push(IpCidr::new(
                    IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 1)),
                    8,
                ))
                .expect("two addresses always fit the interface's address list");
        });
        // Default gateway: `[config] gateway = "a.b.c.d"`, defaulting to
        // NAT_GATEWAY_ADDR -- same reasoning as `address` above, an
        // override only makes sense under net = "bridge" pointed at a real
        // LAN's own gateway (10.0.2.2 does not exist there, so every
        // off-subnet destination -- including a `dns_server` override --
        // would otherwise ARP for a gateway that can never answer and hang
        // until timeout). Off-link traffic (a real outbound connection
        // through net = "nat", once something needs one -- DNS itself
        // doesn't, see NAT_GATEWAY_ADDR's own comment) routes via this.
        // Harmless under Loopback: nothing is ever actually off-link there
        // since every frame just echoes straight back, so this route
        // simply never gets used.
        let gateway_addr = config_get_string("gateway")
            .and_then(|s| parse_dotted_quad(&s))
            .map(|v| {
                let [a, b, c, d] = v.to_be_bytes();
                Ipv4Address::new(a, b, c, d)
            })
            .unwrap_or(NAT_GATEWAY_ADDR);
        iface
            .routes_mut()
            .add_default_ipv4_route(gateway_addr)
            .expect("a fresh route table always has room for one default route");

        self.iface = Some(iface);
        let mut sockets = SocketSet::new(Vec::new());

        // DNS server: `[config] dns_server = "1.2.3.4"` in the manifest
        // (see src/hostsocket.rs), defaulting to Copperline NAT's
        // own DNS forwarder address (see INTERFACE_ADDR's own comment) if
        // unset -- reuses parse_dotted_quad (the same strict "a.b.c.d"
        // parser inet_addr/inet_network already share) rather than a
        // second parser for the same syntax. An unparsable value falls
        // back to the default too, rather than leaving DNS dead with no
        // diagnostic the guest side could ever see.
        let dns_server = config_get_string("dns_server")
            .and_then(|s| parse_dotted_quad(&s))
            .map(|v| {
                let [a, b, c, d] = v.to_be_bytes();
                Ipv4Address::new(a, b, c, d)
            })
            .unwrap_or(NAT_DNS_ADDR);
        let dns_socket = dns::Socket::new(&[IpAddress::Ipv4(dns_server)], Vec::new());
        self.dns_socket = Some(sockets.add(dns_socket));
        self.dns_server_addr = dns_server;

        // Resolver strategy: `[config] resolver = "host"` (see
        // src/hostsocket.rs and its own net = "nat"/"bridge"-only
        // validation) routes gethostbyname() through the `resolve`
        // capability's host-OS-resolver imports instead of the dns_socket
        // just built above. Absent or anything other than exactly "host"
        // keeps the existing behavior.
        self.resolver_host =
            config_get_string("resolver").is_some_and(|s| s.eq_ignore_ascii_case("host"));

        // Host-socket backend (see `Board::host_backend`'s own comment):
        // `[config] transport = "host"`. Independent of `resolver` above
        // and of whatever `net` backend this module's own smoltcp
        // interface is still using -- `do_socket`'s own routing reads
        // this for both TCP and UDP (`do_socket_host` creates either
        // kind); ICMP and DNS are the ones that keep going through
        // smoltcp exactly as before either way (`do_socket_host` never
        // creates an ICMP socket at all).
        self.host_backend =
            config_get_string("transport").is_some_and(|s| s.eq_ignore_ascii_case("host"));

        // gethostbyaddr()'s own reverse-DNS socket (see `ptr_socket`'s own
        // comment for why this is a plain UDP socket, not another
        // dns::Socket) -- bound once, up front, the same "always live"
        // shape `dns_socket` uses. 512 bytes comfortably covers a real
        // DNS-over-UDP response (the classic pre-EDNS UDP size limit);
        // this project only ever has one PTR query in flight at a time
        // (`ptr_pending`), so a handful of metadata slots is generous.
        let ptr_port = self.alloc_local_port();
        let mut ptr_socket = udp::Socket::new(
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0u8; 512]),
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0u8; 512]),
        );
        let _ = ptr_socket.bind(ptr_port);
        self.ptr_socket = Some(sockets.add(ptr_socket));

        self.sockets = Some(sockets);
        self.booted = true;
    }

    fn read(&self, off: i32, size: i32) -> i32 {
        match size {
            4 => {
                let hi = self.read(off, 2) & 0xFFFF;
                let lo = self.read(off.wrapping_add(2), 2) & 0xFFFF;
                (hi << 16) | lo
            }
            2 => {
                let hi = i32::from(self.read_byte(off));
                let lo = i32::from(self.read_byte(off.wrapping_add(1)));
                (hi << 8) | lo
            }
            _ => i32::from(self.read_byte(off)),
        }
    }

    // Serves the guest stub ROM starting at ROM_OFFSET, REG_RESULT's four
    // bytes, and REG_WAKE_TASK/REG_WAKE_SIGNAL (the front of the wake
    // queue, or 0 if it's empty -- see the interrupt server in entry.s);
    // everything else floats as 0.
    fn read_byte(&self, off: i32) -> u8 {
        let off = off as u32;
        let rom_offset = ROM_OFFSET as u32;
        if off >= rom_offset {
            let rom_idx = (off - rom_offset) as usize;
            if rom_idx < self.rom.len() {
                return self.rom[rom_idx];
            }
        }
        if let Some(idx) = byte_in_field(off, REG_RESULT) {
            return self.result.to_be_bytes()[idx];
        }
        if let Some(idx) = byte_in_field(off, REG_WAKE_TASK) {
            let task = self.wake_queue.front().map_or(0, |(t, _)| *t);
            return task.to_be_bytes()[idx];
        }
        if let Some(idx) = byte_in_field(off, REG_WAKE_SIGNAL) {
            let mask = self.wake_queue.front().map_or(0, |(_, s)| *s);
            return mask.to_be_bytes()[idx];
        }
        0
    }

    // Every register write here is a guest `move.l` (see entry.s's
    // trampolines), but "one `move.l`" is a guest-CPU-instruction fact, not
    // a bus-transaction fact: a real 68000's external data bus is 16 bits
    // wide, so Copperline's CPU emulation (correctly) splits a `move.l` to
    // this board's MMIO window into two separate 16-bit bus cycles -- two
    // `write(off, 2, hi)` / `write(off+2, 2, lo)` calls, not one
    // `write(off, 4, value)` call. An earlier version of this function
    // required `size == 4` and silently dropped everything else, which
    // made every RPC call a no-op on a real 68000 (68020+'s 32-bit bus
    // does send a single size=4 write, which is why this went unnoticed
    // until Phase 4's real-Kickstart end-to-end verification pass, run
    // under a 68000 machine profile). Decomposes size=4 into two size=2
    // halves unconditionally so both bus widths go through the exact same
    // reassembly path below instead of two separate implementations.
    fn write(&mut self, off: i32, size: i32, value: i32) {
        match size {
            4 => {
                self.write(off, 2, (value >> 16) & 0xFFFF);
                self.write(off.wrapping_add(2), 2, value & 0xFFFF);
            }
            2 => self.write_word(off, value as u16),
            _ => {}
        }
    }

    // Reassembles two consecutive 16-bit word writes (`off`, then `off+2`)
    // into the 32-bit register write they represent, then applies exactly
    // the side effect the old size==4-only code did. If a word arrives
    // that doesn't complete a pending high word at the expected `off+2`,
    // it's treated as a fresh high word instead of leaving the state
    // machine stuck -- defensive against any write this design doesn't
    // expect, not something a real `move.l` split should ever trigger.
    fn write_word(&mut self, off: i32, word: u16) {
        let Some((hi_off, hi)) = self.pending_write_hi else {
            self.pending_write_hi = Some((off, word));
            return;
        };
        if off != hi_off.wrapping_add(2) {
            self.pending_write_hi = Some((off, word));
            return;
        }
        self.pending_write_hi = None;
        let value = ((hi as i32) << 16) | (word as i32);
        match hi_off {
            REG_ARGPTR => self.argptr = value as u32,
            REG_CALL => self.result = self.dispatch(value),
            REG_WAKE_ACK => {
                self.wake_queue.pop_front();
            }
            _ => {}
        }
    }

    // The RPC doorbell: dma_read the 8-LONG argument block the trampoline
    // staged at `self.argptr` (see hostsocket_board.h's CALL_* table) and
    // drive the matching smoltcp call. arg(0) is always the calling task's
    // pointer.
    fn dispatch(&mut self, call: i32) -> i32 {
        let mut raw = [0u8; 32];
        // Safety: `raw` is this module's own memory; `self.argptr` is an
        // Amiga address the guest trampoline just wrote moments ago.
        unsafe { dma_read(self.argptr as i32, raw.as_mut_ptr() as i32, 32) };
        let arg = |i: usize| i32::from_be_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap());
        let task = arg(0) as u32;

        match call {
            CALL_SOCKET => self.do_socket(task, arg(1), arg(2), arg(3)),
            CALL_CONNECT => self.do_connect(task, arg(1), arg(2) as u32, arg(3)),
            CALL_SEND => self.do_send(task, arg(1), arg(2) as u32, arg(3), arg(4)),
            CALL_RECV => self.do_recv(task, arg(1), arg(2) as u32, arg(3), arg(4)),
            CALL_CLOSESOCKET => self.do_close(task, arg(1)),
            CALL_REGISTER_WAIT => self.do_register_wait(task, arg(7) as u32),
            CALL_IOCTLSOCKET => self.do_ioctl_socket(task, arg(1), arg(2) as u32, arg(3) as u32),
            CALL_SETERRNOPTR => self.do_set_errno_ptr(task, arg(1) as u32, arg(2)),
            CALL_ERRNO => self.do_errno(task),
            CALL_WAITSELECT => self.do_wait_select(
                task,
                arg(1),
                arg(2) as u32,
                arg(3) as u32,
                arg(4) as u32,
                arg(5) as u32,
                arg(6) as u32,
            ),
            CALL_BIND => self.do_bind(task, arg(1), arg(2) as u32, arg(3)),
            CALL_LISTEN => self.do_listen(task, arg(1), arg(2)),
            CALL_ACCEPT => self.do_accept(task, arg(1), arg(2) as u32, arg(3) as u32),
            CALL_SENDTO => {
                self.do_sendto(task, arg(1), arg(2) as u32, arg(3), arg(5) as u32, arg(6))
            }
            CALL_RECVFROM => self.do_recvfrom(
                task,
                arg(1),
                arg(2) as u32,
                arg(3),
                arg(5) as u32,
                arg(6) as u32,
            ),
            CALL_SHUTDOWN => self.do_shutdown(task, arg(1), arg(2)),
            CALL_SETSOCKOPT => {
                self.do_setsockopt(task, arg(1), arg(2), arg(3), arg(4) as u32, arg(5))
            }
            CALL_GETSOCKOPT => {
                self.do_getsockopt(task, arg(1), arg(2), arg(3), arg(4) as u32, arg(5) as u32)
            }
            CALL_GETSOCKNAME => self.do_getsockname(task, arg(1), arg(2) as u32, arg(3) as u32),
            CALL_GETPEERNAME => self.do_getpeername(task, arg(1), arg(2) as u32, arg(3) as u32),
            CALL_DUP2SOCKET => self.do_dup2socket(task, arg(1), arg(2)),
            CALL_INET_NTOA => self.do_inet_ntoa(task, arg(1) as u32, arg(2) as u32),
            CALL_INET_ADDR => self.do_inet_addr(task, arg(1) as u32),
            CALL_INET_LNAOF => self.do_inet_lnaof(task, arg(1) as u32),
            CALL_INET_NETOF => self.do_inet_netof(task, arg(1) as u32),
            CALL_INET_MAKEADDR => self.do_inet_makeaddr(task, arg(1) as u32, arg(2) as u32),
            CALL_INET_NETWORK => self.do_inet_network(task, arg(1) as u32),
            CALL_GETDTABLESIZE => self.do_getdtablesize(),
            CALL_GETHOSTBYNAME => self.do_gethostbyname(task, arg(1) as u32, arg(2) as u32),
            CALL_SOCKETBASETAGLIST => {
                self.do_socketbasetaglist(task, arg(1) as u32, arg(2) as u32, arg(3) as u32)
            }
            CALL_GETSOCKETEVENTS => self.do_get_socket_events(task, arg(1) as u32),
            CALL_GETHOSTNAME => self.do_gethostname(task, arg(1) as u32, arg(2)),
            CALL_GETHOSTID => self.do_gethostid(task),
            CALL_SENDMSG => self.do_sendmsg(task, arg(1), arg(2) as u32, arg(3)),
            CALL_RECVMSG => self.do_recvmsg(task, arg(1), arg(2) as u32, arg(3)),
            CALL_GETHOSTBYADDR => {
                self.do_gethostbyaddr(task, arg(1) as u32, arg(2), arg(3), arg(4) as u32)
            }
            CALL_OBTAINSOCKET => self.do_obtain_socket(task, arg(1), arg(2), arg(3), arg(4)),
            CALL_RELEASESOCKET => self.do_release_socket(task, arg(1), arg(2)),
            CALL_RELEASECOPYOFSOCKET => self.do_release_copy_of_socket(task, arg(1), arg(2)),
            CALL_GETSERVBYNAME => {
                self.do_getservbyname(task, arg(1) as u32, arg(2) as u32, arg(3) as u32)
            }
            CALL_GETSERVBYPORT => self.do_getservbyport(task, arg(1), arg(2) as u32, arg(3) as u32),
            CALL_GETPROTOBYNAME => self.do_getprotobyname(task, arg(1) as u32, arg(2) as u32),
            CALL_GETPROTOBYNUMBER => self.do_getprotobynumber(task, arg(1), arg(2) as u32),
            CALL_GETNETBYNAME => self.do_getnetbyname(task, arg(1) as u32, arg(2) as u32),
            CALL_GETNETBYADDR => self.do_getnetbyaddr(task, arg(1) as u32, arg(2), arg(3) as u32),
            _ => -1,
        }
    }

    // Records `errno` as this task's last error and, if it has registered a
    // pointer via SetErrnoPtr, writes it through to Amiga memory too --
    // matches real bsdsocket.library semantics, where errno is per-task,
    // not global.
    fn set_errno(&mut self, task: u32, errno: i32) {
        let entry = self.tasks.entry(task).or_default();
        entry.last_errno = errno;
        if let Some((ptr, size)) = entry.errno_ptr {
            // SetErrnoPtr's own size contract (amitcp/socketbasetags.h's
            // SBTC_ERRNOPTR(size) macro comment): only 1, 2, and 4 are
            // legal. 1 used to fall through to the 4-byte branch below --
            // on this big-endian target that writes errno's *first* byte
            // (usually 0 for any errno small enough to matter) into the
            // caller's 1-byte variable, and the other 3 bytes past the
            // end of it, instead of the actual low byte. Found running
            // bsdsocktest's own SetErrnoPtr(1-byte) test.
            match size {
                1 => {
                    let byte = [errno as u8];
                    unsafe { dma_write(ptr as i32, byte.as_ptr() as i32, 1) };
                }
                2 => {
                    let bytes = (errno as i16).to_be_bytes();
                    unsafe { dma_write(ptr as i32, bytes.as_ptr() as i32, 2) };
                }
                _ => {
                    let bytes = errno.to_be_bytes();
                    unsafe { dma_write(ptr as i32, bytes.as_ptr() as i32, 4) };
                }
            }
        }
    }

    // Mirrors `set_errno`, for h_errno (netdb.h's HOST_NOT_FOUND/TRY_AGAIN/
    // NO_RECOVERY/NO_DATA) -- only ever called from `do_gethostbyname`'s
    // own failure/success paths, since nothing else in this project
    // produces a DNS-specific error.
    fn set_herrno(&mut self, task: u32, herrno: i32) {
        let entry = self.tasks.entry(task).or_default();
        if let Some(ptr) = entry.herrno_ptr {
            let bytes = herrno.to_be_bytes();
            unsafe { dma_write(ptr as i32, bytes.as_ptr() as i32, 4) };
        }
    }

    fn fd_index(&self, fd: i32) -> Option<usize> {
        // `checked_sub`, not a plain `fd - 1`: `fd` is guest-controlled RPC
        // input reaching nearly every `do_*` handler through this function,
        // and `i32::MIN - 1` overflows. The subsequent `idx < MAX_FDS`
        // bounds check happens to catch the wrapped value under this
        // workspace's release profile (`overflow-checks` off), but a plain
        // debug build (e.g. `cargo test` without `--release`) has
        // overflow-checks on by default and panics on the subtraction
        // itself before ever reaching that check -- confirmed, not just
        // theoretical (see the code-review finding this pins). Explicit
        // `checked_sub` makes the "out of range fd -> None" contract hold
        // regardless of build profile, rather than by accident of this
        // one's settings.
        let idx = usize::try_from(fd.checked_sub(1)?).ok()?;
        (idx < MAX_FDS && self.fds[idx].is_some()).then_some(idx)
    }

    // `task` (arg0 of every RPC call, FindTask(NULL) on the guest side) is
    // itself the Amiga address of the calling task's own `struct Task` --
    // reads its `tc_SigRecvd` field directly out of guest memory so
    // do_wait_select can see real Signal()-delivered Amiga signals, which
    // arrive entirely outside this RPC layer. TC_SIGRECVD_OFFSET (26,
    // 0x1A) is exec/tasks.h's struct layout, not guessed -- verified
    // against the real NDK header with `offsetof` compiled through the
    // actual m68k-amigaos-gcc toolchain rather than hand-counted, since a
    // wrong offset here would silently read garbage instead of failing
    // loudly.
    fn task_sig_recvd(&self, task: u32) -> u32 {
        let mut raw = [0u8; 4];
        unsafe {
            dma_read(
                (task + TC_SIGRECVD_OFFSET) as i32,
                raw.as_mut_ptr() as i32,
                4,
            )
        };
        u32::from_be_bytes(raw)
    }

    // Real Wait() atomically clears exactly the signal bits it reports as
    // received; do_wait_select peeks at tc_SigRecvd directly instead of
    // calling Wait() (see task_sig_recvd's own comment for why), so it must
    // clear the bits it reports itself or they read as still-pending
    // forever -- every later WaitSelect (or a real Wait()) naming the same
    // bit would otherwise return immediately with a signal already
    // consumed by this call. Safe as a plain (non-atomic) read-modify-write:
    // this runs synchronously inside the guest's REG_CALL doorbell write,
    // so the calling task's own CPU execution -- the only thing that could
    // race a change to its own tc_SigRecvd -- is stopped for the duration.
    fn clear_task_sig_recvd(&self, task: u32, bits: u32) {
        let remaining = self.task_sig_recvd(task) & !bits;
        unsafe {
            dma_write(
                (task + TC_SIGRECVD_OFFSET) as i32,
                remaining.to_be_bytes().as_ptr() as i32,
                4,
            )
        };
    }

    fn fd_slot(&self, fd: i32) -> Option<&FdSlot> {
        self.fd_index(fd).map(|i| self.fds[i].as_ref().unwrap())
    }

    // `fd_index`'s counterpart for the host-socket backend (`host_fds`) --
    // see `HostFdSlot`'s own comment for why these are two separate
    // tables rather than one.
    fn host_fd_index(&self, fd: i32) -> Option<usize> {
        let idx = usize::try_from(fd.checked_sub(1)?).ok()?;
        (idx < MAX_FDS && self.host_fds[idx].is_some()).then_some(idx)
    }

    // `sock_poll`'s readiness bitmask for a host-backed fd, or `None` if
    // `fd` isn't one -- lets `process_waiters` fall back to the existing
    // smoltcp readiness checks unchanged when it isn't.
    fn host_socket_mask(&self, fd: i32) -> Option<i32> {
        let idx = self.host_fd_index(fd)?;
        let handle = self.host_fds[idx].as_ref()?.handle;
        Some(unsafe { sock_poll(handle) })
    }

    // Parses a big-endian sockaddr_in (same layout as the 68k -- see
    // PROPOSAL.md's "Struct marshaling scope") out of Amiga memory.
    fn parse_sockaddr(&self, addr: u32, namelen: i32) -> (Ipv4Address, u16) {
        let mut raw = [0u8; 16];
        let n = (namelen.max(0) as usize).min(raw.len());
        // Safety: reading a guest-supplied sockaddr_in out of Amiga memory.
        unsafe { dma_read(addr as i32, raw.as_mut_ptr() as i32, n as i32) };
        let port = u16::from_be_bytes([raw[2], raw[3]]);
        let ip = Ipv4Address::new(raw[4], raw[5], raw[6], raw[7]);
        (ip, port)
    }

    // Writes a sockaddr_in out-parameter, honouring the real in/out
    // `*addrlen` convention (`getsockname`/`getpeername`/`accept`/
    // `recvfrom` all share it): the caller's buffer capacity comes in
    // through `len_ptr`, and the actual size written goes back out through
    // it. `addr_out == 0` means the caller didn't ask for the address at
    // all (still writes 0 through `len_ptr` if given, matching real
    // semantics for "don't care").
    fn write_sockaddr_out(&self, addr_out: u32, len_ptr: u32, ip: Ipv4Address, port: u16) {
        if addr_out == 0 {
            if len_ptr != 0 {
                let zero = 0i32.to_be_bytes();
                unsafe { dma_write(len_ptr as i32, zero.as_ptr() as i32, 4) };
            }
            return;
        }
        let mut raw = [0u8; 16];
        raw[0..2].copy_from_slice(&2i16.to_be_bytes()); // AF_INET
        raw[2..4].copy_from_slice(&port.to_be_bytes());
        raw[4..8].copy_from_slice(&ip.octets());
        let cap = if len_ptr != 0 {
            let mut raw_len = [0u8; 4];
            unsafe { dma_read(len_ptr as i32, raw_len.as_mut_ptr() as i32, 4) };
            i32::from_be_bytes(raw_len).max(0) as usize
        } else {
            raw.len()
        };
        let n = cap.min(raw.len());
        unsafe { dma_write(addr_out as i32, raw.as_ptr() as i32, n as i32) };
        if len_ptr != 0 {
            let n_be = (n as i32).to_be_bytes();
            unsafe { dma_write(len_ptr as i32, n_be.as_ptr() as i32, 4) };
        }
    }

    // Checks a socket's read-readiness regardless of TCP/UDP kind -- used
    // by `process_waiters`, which can't assume a `WaitKind::Recv` is
    // TCP-specific now that UDP fds use the same wait kind (see
    // `do_recvfrom`). Returns `(can_recv, may_recv)`; UDP has no "peer
    // closed" concept, so it always reports `may_recv = true`.
    fn socket_can_recv(&self, fd: i32) -> Option<(bool, bool)> {
        let idx = self.fd_index(fd)?;
        let slot = self.fds[idx].as_ref()?;
        let sockets = self.sockets.as_ref()?;
        Some(match slot.kind {
            SockKind::Tcp => {
                let socket = sockets.get::<tcp::Socket>(slot.socket);
                (socket.can_recv(), socket.may_recv())
            }
            SockKind::Udp => (sockets.get::<udp::Socket>(slot.socket).can_recv(), true),
            SockKind::Icmp => (sockets.get::<icmp::Socket>(slot.socket).can_recv(), true),
        })
    }

    fn alloc_local_port(&mut self) -> u16 {
        let port = self.next_local_port;
        self.next_local_port = if self.next_local_port == u16::MAX {
            49152
        } else {
            self.next_local_port + 1
        };
        port
    }

    // socket(domain, type, protocol): claims the first free fd-table slot.
    // `type` selects TCP (SOCK_STREAM), UDP (SOCK_DGRAM), or SOCK_RAW --
    // which itself splits on `protocol`: `IPPROTO_ICMP` gets a real
    // `icmp::Socket` (see do_sendto/do_recv's own ICMP comments for how
    // it's actually driven), anything else falls back to the historical
    // "silently TCP-shaped" placeholder nothing in this project's own
    // tests or bsdsocktest's own coverage ever exercises for real (every
    // `SOCK_RAW` use in that suite is `IPPROTO_ICMP`). `domain` is
    // validated (AF_INET only). This used to accept anything at all for
    // both `domain` and `type` -- any garbage value silently became a
    // working TCP socket -- which is more permissive than any real
    // bsdsocket.library implementation and was never exercised until
    // bsdsocktest's own two negative-validation tests asked for one
    // specifically to fail.
    fn do_socket(&mut self, task: u32, domain: i32, type_: i32, protocol: i32) -> i32 {
        if domain != AF_INET {
            self.set_errno(task, EINVAL);
            return -1;
        }
        let kind = match type_ {
            SOCK_STREAM => SockKind::Tcp,
            SOCK_DGRAM => SockKind::Udp,
            SOCK_RAW if protocol == IPPROTO_ICMP => SockKind::Icmp,
            SOCK_RAW => SockKind::Tcp,
            _ => {
                self.set_errno(task, EINVAL);
                return -1;
            }
        };
        if self.host_backend && matches!(kind, SockKind::Tcp | SockKind::Udp) {
            return self.do_socket_host(task, kind);
        }
        let sockets = self.sockets.as_mut().expect("init() has run");
        for (i, slot) in self.fds.iter_mut().enumerate() {
            // `host_fds[i]` too, not just this table: the two share one
            // fd-number space (see `HostFdSlot`'s own comment), so under
            // `host_backend` an ICMP socket (the one kind that still
            // reaches this smoltcp allocator even then, TCP/UDP having
            // already routed to `do_socket_host` above) must not pick an
            // index a host-backed fd already occupies.
            if slot.is_none() && self.host_fds[i].is_none() {
                let handle = match kind {
                    SockKind::Tcp => sockets.add(tcp::Socket::new(
                        RingBuffer::new(vec![0u8; SOCKET_BUF_LEN]),
                        RingBuffer::new(vec![0u8; SOCKET_BUF_LEN]),
                    )),
                    SockKind::Udp => sockets.add(udp::Socket::new(
                        udp::PacketBuffer::new(
                            vec![udp::PacketMetadata::EMPTY; UDP_META_SLOTS],
                            vec![0u8; UDP_BUF_LEN],
                        ),
                        udp::PacketBuffer::new(
                            vec![udp::PacketMetadata::EMPTY; UDP_META_SLOTS],
                            vec![0u8; UDP_BUF_LEN],
                        ),
                    )),
                    SockKind::Icmp => sockets.add(icmp::Socket::new(
                        icmp::PacketBuffer::new(
                            vec![icmp::PacketMetadata::EMPTY; ICMP_META_SLOTS],
                            vec![0u8; ICMP_BUF_LEN],
                        ),
                        icmp::PacketBuffer::new(
                            vec![icmp::PacketMetadata::EMPTY; ICMP_META_SLOTS],
                            vec![0u8; ICMP_BUF_LEN],
                        ),
                    )),
                };
                *slot = Some(FdSlot {
                    kind,
                    socket: handle,
                    refcount: Rc::new(()),
                    nonblocking: false,
                    bind_port: None,
                    bind_addr: None,
                    is_listener: false,
                    udp_peer: None,
                    connect_started: false,
                    was_established: false,
                    shutdown_by_us: false,
                    opts: SockOpts::new(),
                });
                return (i + 1) as i32;
            }
        }
        // fd table full -- real BSD's own answer for "no descriptor slots
        // left" (EMFILE, "too many open files"), matching the other two
        // fd-exhaustion sites (do_accept, do_obtain_socket) rather than
        // leaving this one the odd one out with no errno set at all.
        self.set_errno(task, EMFILE);
        -1
    }

    // do_socket's host-backend branch (`Board::host_backend`, TCP only --
    // see `HostFdSlot`'s own comment): claims a fd index free in *both*
    // tables (so a later plain do_socket() reusing this index never
    // collides with a live host_fds entry) and opens a real host socket
    // via `sock_open`. `sock_open`'s own failure return is already a
    // negative value in this crate's own BSD errno numbering (see the
    // `sock_*` import block's own comment), so it can be reported via
    // `set_errno` directly with no separate translation step.
    fn do_socket_host(&mut self, task: u32, kind: SockKind) -> i32 {
        let Some(idx) = (0..MAX_FDS).find(|&i| self.fds[i].is_none() && self.host_fds[i].is_none())
        else {
            self.set_errno(task, EMFILE);
            return -1;
        };
        let type_ = match kind {
            SockKind::Tcp => SOCK_STREAM,
            SockKind::Udp => SOCK_DGRAM,
            SockKind::Icmp => unreachable!("do_socket's own caller only routes Tcp/Udp here"),
        };
        let handle = unsafe { sock_open(AF_INET, type_) };
        if handle < 0 {
            self.set_errno(task, -handle);
            return -1;
        }
        self.host_fds[idx] = Some(HostFdSlot {
            handle,
            kind,
            nonblocking: false,
            connect_started: false,
            is_listener: false,
            opts: HostSockOpts::default(),
        });
        (idx + 1) as i32
    }

    // connect(sock, name, namelen): on a UDP fd, this just records a
    // default peer (real BSD semantics -- no handshake) and returns
    // immediately. On a TCP fd: `name` is the Amiga address of a
    // sockaddr_in; parse it once, issue smoltcp's connect(), then just
    // report progress on every re-issued call. Non-blocking mode
    // (IoctlSocket(FIONBIO), per-fd) turns RES_PENDING into an immediate
    // -1/EINPROGRESS (first call) or -1/EALREADY (later calls) instead of
    // the blocking-loop registration below.
    fn do_connect(&mut self, task: u32, fd: i32, name_addr: u32, namelen: i32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_connect_host(task, fd, name_addr, namelen);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let kind = self.fds[idx].as_ref().unwrap().kind;
        if kind == SockKind::Udp {
            let peer = self.parse_sockaddr(name_addr, namelen);
            self.fds[idx].as_mut().unwrap().udp_peer = Some(peer);
            return 0;
        }
        if kind == SockKind::Icmp {
            // bsdsocktest's own raw ICMP usage never calls connect() --
            // reject cleanly rather than fall through into the TCP-only
            // code below, which would call `sockets.get::<tcp::Socket>`
            // on an ICMP handle and panic (SocketSet::get traps on a
            // type mismatch).
            self.set_errno(task, EINVAL);
            return -1;
        }

        let slot = self.fds[idx].as_ref().unwrap();
        let handle = slot.socket;
        let nonblocking = slot.nonblocking;
        // Not `!socket.is_open()`: a refused connection's RST leaves the
        // socket Closed, indistinguishable from "never connected" by
        // is_open() alone, and the guest's blocking-wait retry loop calls
        // CALL_CONNECT again to collect that very result (see
        // `connect_started`'s own doc comment on FdSlot).
        let first_attempt = !slot.connect_started;

        if first_attempt {
            self.fds[idx].as_mut().unwrap().connect_started = true;
            let (ip, port) = self.parse_sockaddr(name_addr, namelen);
            let local_port = self.alloc_local_port();

            let iface = self.iface.as_mut().expect("init() has run");
            let cx = iface.context();
            let sockets = self.sockets.as_mut().expect("init() has run");
            let socket = sockets.get_mut::<tcp::Socket>(handle);
            if socket
                .connect(cx, (IpAddress::Ipv4(ip), port), local_port)
                .is_err()
            {
                self.set_errno(task, ECONNREFUSED);
                return -1;
            }
        }

        let sockets = self.sockets.as_ref().expect("init() has run");
        let socket = sockets.get::<tcp::Socket>(handle);
        match socket.state() {
            tcp::State::Established => {
                // Records that this connection genuinely came up at
                // least once -- send_tcp_stream/do_recv need this later
                // to tell "never actually connected" (ECONNREFUSED)
                // apart from "was connected, then the peer went away"
                // (ECONNRESET/EPIPE) once may_send()/may_recv() goes
                // false, since smoltcp's own tcp::Socket has no public
                // way to ask why a connection reached Closed (see
                // FdSlot::was_established's own comment).
                self.fds[idx].as_mut().unwrap().was_established = true;
                0
            }
            tcp::State::Closed => {
                self.set_errno(task, ECONNREFUSED);
                -1
            }
            _ if nonblocking => {
                self.set_errno(task, if first_attempt { EINPROGRESS } else { EALREADY });
                -1
            }
            _ => {
                self.last_pending.insert(task, WaitKind::Connect { fd });
                RES_PENDING
            }
        }
    }

    // do_connect's host-backend branch. Real non-blocking BSD connect()
    // semantics make this simpler than the smoltcp path above: every call
    // (first attempt or a retry) just re-issues `sock_connect` and reads
    // its result -- a second `connect()` on a socket whose first attempt
    // already succeeded reports EISCONN (a real success, not an error;
    // POSIX guarantees this), and a second call on one that's still
    // pending or already failed reports EINPROGRESS/EALREADY or the
    // original failure again. So unlike `do_connect`, there's no need to
    // separately track "was this ever Established" here -- the host
    // kernel's own socket state already is that record, and `sock_poll`
    // (used only by `process_waiters`, to decide whether to wake a
    // blocked waiter at all -- see its own comment) doesn't need to be
    // authoritative for the same reason: this function re-validates for
    // real on every call regardless of what woke it.
    fn do_connect_host(&mut self, task: u32, fd: i32, name_addr: u32, namelen: i32) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let slot = self.host_fds[idx].as_ref().unwrap();
        let handle = slot.handle;
        let nonblocking = slot.nonblocking;
        let first_attempt = !slot.connect_started;
        self.host_fds[idx].as_mut().unwrap().connect_started = true;

        let (ip, port) = self.parse_sockaddr(name_addr, namelen);
        let packed_ip = u32::from_be_bytes(ip.octets()) as i32;
        let rc = unsafe { sock_connect(handle, packed_ip, port as i32) };

        if rc == 0 || rc == -EISCONN {
            return 0;
        }
        if rc == -EINPROGRESS || rc == -EALREADY {
            if nonblocking {
                self.set_errno(task, if first_attempt { EINPROGRESS } else { EALREADY });
                return -1;
            }
            self.last_pending.insert(task, WaitKind::Connect { fd });
            return RES_PENDING;
        }
        self.set_errno(task, -rc);
        -1
    }

    // send(sock, buf, len, flags): on a UDP fd, delegates to sendto() using
    // the peer connect() recorded (arg addr 0 = "use the recorded peer",
    // see do_sendto) -- a UDP write is one atomic datagram, no blocking-
    // retry shape needed there. On a TCP fd, blocking mode (the default)
    // matches real BSD send(): if the socket's own send buffer is too
    // small to take all `len` bytes right now, it queues as much as fits
    // and then blocks (RES_PENDING + wait registration, same
    // _ring_doorbell_blocking shape do_connect/do_recv already use) until
    // more room frees up, resuming from `send_progress`'s saved offset on
    // every retry -- the guest's own blocking loop re-stages the SAME
    // buf_addr/len each time, it never adjusts them (see entry.s's
    // _ring_doorbell_blocking), so this function has to remember where it
    // left off itself. This wasn't always the shape: an earlier version
    // did one send_slice() and returned whatever fit as a "legitimate
    // short byte count", reasoning that matches real non-blocking
    // semantics but not blocking ones -- found hanging bsdsocktest's own
    // 8192-byte transfer test forever, which -- like real BSD callers of a
    // blocking socket routinely do -- never checks send()'s return value,
    // simply trusting it to have queued everything before returning.
    // Non-blocking mode keeps the old short-write behavior, now correctly
    // reporting -1/EWOULDBLOCK when literally nothing could be queued
    // (previously returned a bare 0, which for TCP is not the same thing).
    fn do_send(&mut self, task: u32, fd: i32, buf_addr: u32, len: i32, flags: i32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_send_host(task, fd, buf_addr, len, flags);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        if flags & MSG_OOB != 0 {
            // No urgent-pointer/OOB support at all: smoltcp's own TCP
            // socket has none to build on (no URG flag/urgent-pointer
            // handling anywhere in socket::tcp), so treating MSG_OOB data
            // as ordinary in-band bytes (the previous behavior here, since
            // `flags` used to be silently dropped by the dispatcher) is
            // actively misleading -- a caller polling WaitSelect's
            // exceptfds for it would wait out its full timeout every time
            // (see `scan_select`'s own comment on that gap). A clean
            // EOPNOTSUPP instead matches bsdsocktest's own explicit
            // fallback: both its MSG_OOB send test and its exceptfds/OOB
            // WaitSelect test treat a negative send() return here as a
            // legitimate "not supported" outcome, not a failure.
            self.set_errno(task, EOPNOTSUPP);
            return -1;
        }
        let kind = self.fds[idx].as_ref().unwrap().kind;
        if kind == SockKind::Udp || kind == SockKind::Icmp {
            // Neither UDP nor ICMP have a real "connected" peer concept
            // bsdsocktest's own tests ever set up (raw ICMP always uses
            // sendto() with an explicit destination) -- do_sendto's own
            // `to_addr == 0` path reports ENOTCONN for both rather than
            // this falling through into the TCP-only `send_tcp_stream`
            // call below, which would panic on an ICMP handle
            // (SocketSet::get traps on a type mismatch).
            return self.do_sendto(task, fd, buf_addr, len, 0, 0);
        }

        let n = (len.max(0) as usize).min(MAX_XFER_LEN);
        self.send_tcp_stream(task, fd, n, |already, remaining| {
            let mut data = vec![0u8; remaining];
            // Safety: reading the guest's send buffer out of Amiga memory,
            // offset by whatever a previous retry already queued.
            unsafe {
                dma_read(
                    (buf_addr as usize + already) as i32,
                    data.as_mut_ptr() as i32,
                    remaining as i32,
                )
            };
            data
        })
    }

    // do_send's host-backend branch. Reuses `send_progress` (already
    // task-keyed, not smoltcp-specific) for the same partial-queue resume
    // `send_tcp_stream` needs -- see that function's own comment for why a
    // blocking send() can't just return whatever fit on the first
    // short write. Unlike the smoltcp path, no hand-rolled
    // ECONNREFUSED-vs-EPIPE-vs-ECONNRESET tracking is needed here: the
    // host kernel's own `write()` already reports the right one of those
    // (via `sock_send`'s own errno translation, see src/wasmboard.rs), so
    // any negative `sock_send` result other than `-EAGAIN` is simply
    // passed straight through.
    fn do_send_host(&mut self, task: u32, fd: i32, buf_addr: u32, len: i32, flags: i32) -> i32 {
        if flags & MSG_OOB != 0 {
            return self.do_send_oob_host(task, fd, buf_addr, len);
        }
        let n = (len.max(0) as usize).min(MAX_XFER_LEN);
        self.send_host_stream(task, fd, n, |already, remaining| {
            let mut data = vec![0u8; remaining];
            // Safety: reading the guest's send buffer out of Amiga
            // memory, offset by whatever a previous retry already
            // queued.
            unsafe {
                dma_read(
                    (buf_addr as usize + already) as i32,
                    data.as_mut_ptr() as i32,
                    remaining as i32,
                )
            };
            data
        })
    }

    // send(MSG_OOB) on a host-backed fd: a real `send(2)` with `MSG_OOB`
    // set (`sock_send_oob`, `socket2::Socket::send_out_of_band`) -- unlike
    // the smoltcp path (`do_send`'s own MSG_OOB branch, a permanent
    // `EOPNOTSUPP` since `socket::tcp` has no urgent-pointer support to
    // build on at all), a real host TCP socket genuinely supports this.
    // No partial-progress retry loop the way `send_host_stream` needs for
    // an ordinary blocking send: a real urgent-data send is a single small
    // atomic write (bsdsocktest's own coverage only ever sends 1 byte, and
    // real BSD urgent-data semantics only support one outstanding OOB byte
    // at a time regardless), so a plain retry-on-EAGAIN is enough.
    fn do_send_oob_host(&mut self, task: u32, fd: i32, buf_addr: u32, len: i32) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let slot = self.host_fds[idx].as_ref().unwrap();
        let handle = slot.handle;
        let nonblocking = slot.nonblocking;
        let n = (len.max(0) as usize).min(MAX_XFER_LEN);
        let mut data = vec![0u8; n];
        // Safety: reading the guest's send buffer out of Amiga memory.
        unsafe { dma_read(buf_addr as i32, data.as_mut_ptr() as i32, n as i32) };
        let rc = unsafe { sock_send_oob(handle, data.as_ptr() as i32, data.len() as i32) };
        if rc >= 0 {
            return rc;
        }
        if rc == -EAGAIN {
            if nonblocking {
                self.set_errno(task, EAGAIN);
                return -1;
            }
            self.last_pending.insert(task, WaitKind::Send { fd });
            return RES_PENDING;
        }
        self.set_errno(task, -rc);
        -1
    }

    // Shared blocking/partial-progress host-socket send logic behind both
    // do_send_host and do_sendmsg_host -- the host-backend counterpart of
    // the smoltcp path's own send_tcp_stream, same parameterization (see
    // that function's own comment for why: scattered iovec segments
    // can't be expressed as a single "address + offset" the way a plain
    // send()'s own buf_addr can, but already/remaining still address
    // correctly into an already-flattened byte vector either way).
    fn send_host_stream(
        &mut self,
        task: u32,
        fd: i32,
        n: usize,
        read_at: impl FnOnce(usize, usize) -> Vec<u8>,
    ) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let slot = self.host_fds[idx].as_ref().unwrap();
        let handle = slot.handle;
        let nonblocking = slot.nonblocking;

        let already = self
            .send_progress
            .remove(&task)
            .filter(|&(pfd, _)| pfd == fd)
            .map_or(0, |(_, sent)| sent);
        if already >= n {
            return n as i32; // a previous retry already queued it all
        }

        let remaining = n - already;
        let data = read_at(already, remaining);
        let rc = unsafe { sock_send(handle, data.as_ptr() as i32, data.len() as i32) };

        if rc >= 0 {
            let total = already + rc as usize;
            if total >= n {
                return n as i32;
            }
            if nonblocking {
                return total as i32;
            }
            self.send_progress.insert(task, (fd, total));
            self.last_pending.insert(task, WaitKind::Send { fd });
            return RES_PENDING;
        }
        if rc == -EAGAIN {
            if nonblocking {
                if already > 0 {
                    return already as i32;
                }
                self.set_errno(task, EAGAIN);
                return -1;
            }
            self.send_progress.insert(task, (fd, already));
            self.last_pending.insert(task, WaitKind::Send { fd });
            return RES_PENDING;
        }
        self.set_errno(task, -rc);
        -1
    }

    // Shared blocking/partial-progress TCP send logic behind both do_send
    // and do_sendmsg, parameterized over how to fetch the next `remaining`
    // bytes starting at flattened offset `already` in the `n`-byte stream
    // being sent (do_send's own `buf_addr`-relative dma_read for a single
    // contiguous guest buffer, or do_sendmsg's pre-gathered iovec bytes --
    // gathering those upfront rather than teaching this function to
    // understand iovecs directly is the simpler design: scattered iovec
    // segments can't be expressed as a single "address + offset" the way
    // send()'s own buf_addr can, but `already`/`remaining` still address
    // correctly into an already-flattened byte vector).
    fn send_tcp_stream(
        &mut self,
        task: u32,
        fd: i32,
        n: usize,
        read_at: impl FnOnce(usize, usize) -> Vec<u8>,
    ) -> i32 {
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let slot = self.fds[idx].as_ref().unwrap();
        let handle = slot.socket;
        let nonblocking = slot.nonblocking;
        let already = self
            .send_progress
            .remove(&task)
            .filter(|&(pfd, _)| pfd == fd)
            .map_or(0, |(_, sent)| sent);

        if already >= n {
            return n as i32; // a previous retry already queued it all
        }

        let sockets = self.sockets.as_mut().expect("init() has run");
        let socket = sockets.get_mut::<tcp::Socket>(handle);

        if !socket.may_send() {
            // Three distinct reasons a TCP fd can't be written to, real
            // BSD gives three distinct errnos for (see
            // FdSlot::was_established/shutdown_by_us's own comments for
            // why this project has to track the first two itself --
            // smoltcp's own tcp::Socket state alone can't tell them
            // apart): never actually connected (a failed connect()),
            // we ourselves called shutdown() on it, or the peer tore it
            // down out from under us (e.g. an RST after it fully closed
            // -- see docs/bsdsocktest-status.md's own account of test 35
            // for exactly this scenario: smoltcp's own `Interface::
            // process_tcp` auto-generates a real RST for a segment that
            // matches no socket in the SocketSet once the peer's own fd
            // is fully closed and reaped).
            let slot = self.fds[idx].as_ref().unwrap();
            let errno = if !slot.was_established {
                ECONNREFUSED
            } else if slot.shutdown_by_us {
                EPIPE
            } else {
                ECONNRESET
            };
            self.set_errno(task, errno);
            return -1;
        }
        if !socket.can_send() {
            if nonblocking {
                if already > 0 {
                    return already as i32;
                }
                self.set_errno(task, EAGAIN);
                return -1;
            }
            self.send_progress.insert(task, (fd, already));
            self.last_pending.insert(task, WaitKind::Send { fd });
            return RES_PENDING;
        }

        // Capped at SOCKET_BUF_LEN, not just `n - already`: `send_slice`
        // can never consume more than the socket's own send-buffer
        // capacity in one call regardless, so this never truncates what
        // actually gets queued -- it just stops `read_at` from
        // materializing (and DMA-reading) a much larger guest-claimed
        // `remaining` than any single call could ever use, the same
        // over-allocation `MAX_XFER_LEN` already guards against one level
        // up (see that const's own comment). A short read here just means
        // one more RES_PENDING retry, exactly like a socket buffer that's
        // genuinely part-full already does.
        let remaining = (n - already).min(SOCKET_BUF_LEN);
        let data = read_at(already, remaining);
        let sent = socket.send_slice(&data).unwrap_or(0);
        let total = already + sent;

        if total >= n {
            return n as i32;
        }
        if nonblocking {
            return total as i32;
        }
        self.send_progress.insert(task, (fd, total));
        self.last_pending.insert(task, WaitKind::Send { fd });
        RES_PENDING
    }

    // recv(sock, buf, len, flags): on a UDP fd, delegates to recvfrom()
    // with no sender out-param (also drops `flags` -- MSG_PEEK on UDP
    // isn't exercised by anything this project targets yet, unlike TCP's
    // below). On a TCP fd: blocks (RES_PENDING + wait registration) until
    // data is buffered, returns 0 on a clean EOF (peer closed, no data
    // left -- not an error), or -1 on a real error. Non-blocking mode maps
    // the "not ready yet" case to -1/EWOULDBLOCK instead of registering a
    // wait, same split as do_connect. MSG_PEEK reads without consuming
    // (smoltcp's own peek_slice, not recv_slice) -- this used to be
    // ignored entirely, silently consuming on every recv() regardless of
    // flags, which is a correctness bug on its own but also a real hang:
    // bsdsocktest's own MSG_PEEK test peeks once then does a real recv()
    // expecting to see the *same* bytes again, and a peek that actually
    // consumed left nothing there for that second call to find -- since
    // the connection's still open (may_recv() true), that second call
    // would register a wait for data that was never coming and block
    // forever. MSG_OOB is a separate, real TCP urgent-data feature that
    // stays unimplemented (bsdsocktest's own known_failures.c already
    // expects `recv(MSG_OOB)` to fail even on real bsdsocket.library
    // implementations, so there's no conformance reason to build it).
    fn do_recv(&mut self, task: u32, fd: i32, buf_addr: u32, len: i32, flags: i32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_recv_host(task, fd, buf_addr, len, flags);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let kind = self.fds[idx].as_ref().unwrap().kind;
        if kind == SockKind::Udp || kind == SockKind::Icmp {
            // bsdsocktest's own ICMP ping implementation calls plain
            // recv() (never recvfrom()) on its raw socket -- do_recvfrom
            // is where the real ICMP-vs-UDP receive logic lives (see its
            // own comment on the synthetic IP header raw ICMP reads need
            // to match real BSD raw-socket semantics).
            return self.do_recvfrom(task, fd, buf_addr, len, 0, 0);
        }

        let slot = self.fds[idx].as_ref().unwrap();
        let handle = slot.socket;
        let nonblocking = slot.nonblocking;
        let peek = flags & MSG_PEEK != 0;
        let sockets = self.sockets.as_mut().expect("init() has run");
        let socket = sockets.get_mut::<tcp::Socket>(handle);

        if !socket.can_recv() {
            if !socket.may_recv() {
                // `may_recv() == false` alone just means "the peer sent
                // its FIN" -- CloseWait, a completely ordinary state a
                // graceful peer close reaches on *every* connection
                // (real BSD's own recv() correctly reports a plain EOF
                // there, and every earlier-passing test that shuts one
                // side down and expects the other to see rc=0 relies on
                // exactly that). Reaching *fully* `Closed` -- both
                // directions dead, not just the read side -- is the
                // narrower signal an RST actually produces (see
                // FdSlot::was_established's own comment for why smoltcp's
                // state alone can't say *why* it got there). So this
                // only overrides the plain-EOF default when the
                // connection is fully closed, not merely half.
                if socket.state() == tcp::State::Closed {
                    let slot = self.fds[idx].as_ref().unwrap();
                    if slot.was_established && !slot.shutdown_by_us {
                        self.set_errno(task, ECONNRESET);
                        return -1;
                    }
                }
                return 0; // clean EOF: peer closed, nothing left to read
            }
            if nonblocking {
                self.set_errno(task, EAGAIN);
                return -1;
            }
            self.last_pending.insert(task, WaitKind::Recv { fd });
            return RES_PENDING;
        }

        let cap = (len.max(0) as usize).min(MAX_XFER_LEN);
        let mut data = vec![0u8; cap];
        let n = if peek {
            socket.peek_slice(&mut data).unwrap_or(0)
        } else {
            socket.recv_slice(&mut data).unwrap_or(0)
        };
        // Safety: writing into the guest's receive buffer in Amiga memory.
        unsafe { dma_write(buf_addr as i32, data.as_ptr() as i32, n as i32) };
        n as i32
    }

    // do_recv's host-backend branch. Unlike the smoltcp path, no
    // ECONNRESET-vs-plain-EOF tracking is needed: `sock_recv`/`sock_peek`
    // returning `0` genuinely means EOF on a real host socket (read()
    // semantics), and a real error already comes back as the right
    // negative errno (see src/wasmboard.rs's own errno translation).
    // MSG_PEEK uses `sock_peek` (a real, non-consuming `MSG_PEEK` at the
    // OS level) instead of `sock_recv` -- consuming on a peek would
    // corrupt a caller's own "peek then recv the same bytes again"
    // expectation.
    fn do_recv_host(&mut self, task: u32, fd: i32, buf_addr: u32, len: i32, flags: i32) -> i32 {
        if flags & MSG_OOB != 0 {
            return self.do_recv_oob_host(task, fd, buf_addr, len);
        }
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let slot = self.host_fds[idx].as_ref().unwrap();
        let handle = slot.handle;
        let nonblocking = slot.nonblocking;
        let peek = flags & MSG_PEEK != 0;

        let cap = (len.max(0) as usize).min(MAX_XFER_LEN);
        let mut data = vec![0u8; cap];
        let rc = if peek {
            unsafe { sock_peek(handle, data.as_mut_ptr() as i32, data.len() as i32) }
        } else {
            unsafe { sock_recv(handle, data.as_mut_ptr() as i32, data.len() as i32) }
        };

        if rc >= 0 {
            let n = rc as usize;
            // Safety: writing into the guest's receive buffer in Amiga
            // memory.
            unsafe { dma_write(buf_addr as i32, data.as_ptr() as i32, n as i32) };
            return n as i32;
        }
        if rc == -EAGAIN {
            if nonblocking {
                self.set_errno(task, EAGAIN);
                return -1;
            }
            self.last_pending.insert(task, WaitKind::Recv { fd });
            return RES_PENDING;
        }
        self.set_errno(task, -rc);
        -1
    }

    // recv(MSG_OOB) on a host-backed fd: a real `recv(2)` with `MSG_OOB`
    // set (`sock_recv_oob`, `socket2::Socket::recv_out_of_band`), retrieving
    // the real urgent byte a plain `recv`/`peek` never surfaces. Blocks
    // (via `WaitKind::RecvOob`) until urgent data actually arrives, same
    // shape as `do_recv_host`'s own blocking path -- but with no dedicated
    // readiness bit to wait on (see that `WaitKind`'s own comment for why
    // `sock_poll` can't reliably detect this).
    fn do_recv_oob_host(&mut self, task: u32, fd: i32, buf_addr: u32, len: i32) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let slot = self.host_fds[idx].as_ref().unwrap();
        let handle = slot.handle;
        let nonblocking = slot.nonblocking;
        let cap = (len.max(0) as usize).min(MAX_XFER_LEN);
        let mut data = vec![0u8; cap];
        let rc = unsafe { sock_recv_oob(handle, data.as_mut_ptr() as i32, data.len() as i32) };
        if rc >= 0 {
            let n = rc as usize;
            // Safety: writing into the guest's receive buffer in Amiga
            // memory.
            unsafe { dma_write(buf_addr as i32, data.as_ptr() as i32, n as i32) };
            return n as i32;
        }
        if rc == -EAGAIN {
            if nonblocking {
                self.set_errno(task, EAGAIN);
                return -1;
            }
            self.last_pending.insert(task, WaitKind::RecvOob { fd });
            return RES_PENDING;
        }
        self.set_errno(task, -rc);
        -1
    }

    // sendmsg(sock, msg, flags): TCP-only (nothing in bsdsocktest's own
    // coverage exercises this on UDP, and a real UDP sendmsg would need
    // `msg_name` handling this project has no other reason to build --
    // see msg_control/msg_name's own gaps below), scatter/gather send
    // via a `struct msghdr`'s `msg_iov`/`msg_iovlen` array instead of a
    // single flat buffer. `msg_name`/`msg_namelen`/`msg_control`/
    // `msg_controllen`/`msg_flags` are all ignored -- no test sets them
    // (bsdsocktest's own tests `memset(&msg, 0, ...)` and only ever
    // populate `msg_iov`/`msg_iovlen`), and ancillary-data (`msg_control`)
    // support is a distinct, much bigger feature (SCM_RIGHTS and friends)
    // nothing here needs. Reuses `send_tcp_stream` (see its own comment)
    // by gathering every iovec's bytes into one flat buffer upfront, then
    // sourcing `send_tcp_stream`'s per-attempt reads from *that* instead
    // of a guest buf_addr -- correct to re-gather on every retry (the
    // guest re-stages the same `msg` unchanged each time, same convention
    // `_ring_doorbell_blocking` already relies on for do_send's own
    // buf_addr), if wasteful for already-sent bytes; nothing here is
    // remotely hot enough for that to matter.
    fn do_sendmsg(&mut self, task: u32, fd: i32, msg_addr: u32, flags: i32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_sendmsg_host(task, fd, msg_addr, flags);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        if flags & MSG_OOB != 0 {
            self.set_errno(task, EOPNOTSUPP);
            return -1;
        }
        if self.fds[idx].as_ref().unwrap().kind != SockKind::Tcp {
            self.set_errno(task, EOPNOTSUPP);
            return -1;
        }
        let Some(data) = read_iovec_bytes(msg_addr) else {
            self.set_errno(task, EINVAL);
            return -1;
        };
        let n = data.len();
        self.send_tcp_stream(task, fd, n, move |already, remaining| {
            data[already..already + remaining].to_vec()
        })
    }

    // do_sendmsg's host-backend branch: gathers the iovecs up front
    // (same reasoning as the smoltcp version above), then reuses
    // send_host_stream -- the same shared blocking/partial-progress
    // logic do_send_host itself is built on.
    fn do_sendmsg_host(&mut self, task: u32, fd: i32, msg_addr: u32, flags: i32) -> i32 {
        if flags & MSG_OOB != 0 {
            self.set_errno(task, EOPNOTSUPP);
            return -1;
        }
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        if self.host_fds[idx].as_ref().unwrap().kind != SockKind::Tcp {
            self.set_errno(task, EOPNOTSUPP);
            return -1;
        }
        let Some(data) = read_iovec_bytes(msg_addr) else {
            self.set_errno(task, EINVAL);
            return -1;
        };
        let n = data.len();
        self.send_host_stream(task, fd, n, move |already, remaining| {
            data[already..already + remaining].to_vec()
        })
    }

    // recvmsg(sock, msg, flags): TCP-only, same scope limits as
    // do_sendmsg (msg_name/msg_control ignored, nothing exercises UDP).
    // Single-shot like do_recv (short reads are legitimate for recv-
    // family calls, unlike send()'s own "block until everything's queued"
    // contract -- see do_send's comment for why those two calls aren't
    // symmetric here): reads up to the combined capacity of every iovec
    // in one `recv_slice`/`peek_slice` call, then scatters the result
    // across the iovecs in order, filling each to its own `iov_len`
    // before moving to the next (real readv()/recvmsg() scatter
    // semantics) -- matches do_recv's own MSG_PEEK handling exactly, just
    // writing to multiple guest buffers instead of one.
    fn do_recvmsg(&mut self, task: u32, fd: i32, msg_addr: u32, flags: i32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_recvmsg_host(task, fd, msg_addr, flags);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        if self.fds[idx].as_ref().unwrap().kind != SockKind::Tcp {
            self.set_errno(task, EOPNOTSUPP);
            return -1;
        }
        let Some(iovecs) = read_iovec_descriptors(msg_addr) else {
            self.set_errno(task, EINVAL);
            return -1;
        };

        let slot = self.fds[idx].as_ref().unwrap();
        let handle = slot.socket;
        let nonblocking = slot.nonblocking;
        let peek = flags & MSG_PEEK != 0;
        let sockets = self.sockets.as_mut().expect("init() has run");
        let socket = sockets.get_mut::<tcp::Socket>(handle);

        if !socket.can_recv() {
            if !socket.may_recv() {
                // See do_recv's own identical check for the full
                // reasoning -- ECONNRESET only once the connection is
                // *fully* Closed (both directions dead, the RST
                // signature), not merely half-closed via an ordinary
                // peer FIN (CloseWait), which stays a plain EOF.
                if socket.state() == tcp::State::Closed {
                    let slot = self.fds[idx].as_ref().unwrap();
                    if slot.was_established && !slot.shutdown_by_us {
                        self.set_errno(task, ECONNRESET);
                        return -1;
                    }
                }
                return 0; // clean EOF: peer closed, nothing left to read
            }
            if nonblocking {
                self.set_errno(task, EAGAIN);
                return -1;
            }
            self.last_pending.insert(task, WaitKind::Recv { fd });
            return RES_PENDING;
        }

        let cap: usize = iovecs.iter().map(|&(_, len)| len).sum();
        let mut data = vec![0u8; cap];
        let n = if peek {
            socket.peek_slice(&mut data).unwrap_or(0)
        } else {
            socket.recv_slice(&mut data).unwrap_or(0)
        };
        let mut off = 0usize;
        for (base, len) in iovecs {
            if off >= n {
                break;
            }
            let chunk = len.min(n - off);
            // Safety: writing into the guest's own receive buffer(s), same
            // as do_recv's single-buffer case.
            unsafe {
                dma_write(
                    base as i32,
                    data[off..off + chunk].as_ptr() as i32,
                    chunk as i32,
                )
            };
            off += chunk;
        }
        n as i32
    }

    // do_recvmsg's host-backend branch: reads up to the combined
    // capacity of every iovec in one `sock_recv`/`sock_peek` call (same
    // shape as `do_recv_host`'s own single-buffer version -- see that
    // function's own comment for why no ECONNRESET-vs-EOF tracking is
    // needed here either), then scatters the result across the iovecs in
    // order, matching the smoltcp version's own real readv()/recvmsg()
    // semantics.
    fn do_recvmsg_host(&mut self, task: u32, fd: i32, msg_addr: u32, flags: i32) -> i32 {
        if flags & MSG_OOB != 0 {
            self.set_errno(task, EOPNOTSUPP);
            return -1;
        }
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let slot = self.host_fds[idx].as_ref().unwrap();
        if slot.kind != SockKind::Tcp {
            self.set_errno(task, EOPNOTSUPP);
            return -1;
        }
        let handle = slot.handle;
        let nonblocking = slot.nonblocking;
        let peek = flags & MSG_PEEK != 0;
        let Some(iovecs) = read_iovec_descriptors(msg_addr) else {
            self.set_errno(task, EINVAL);
            return -1;
        };

        let cap: usize = iovecs.iter().map(|&(_, len)| len).sum();
        let mut data = vec![0u8; cap];
        let rc = if peek {
            unsafe { sock_peek(handle, data.as_mut_ptr() as i32, data.len() as i32) }
        } else {
            unsafe { sock_recv(handle, data.as_mut_ptr() as i32, data.len() as i32) }
        };

        if rc >= 0 {
            let n = rc as usize;
            let mut off = 0usize;
            for (base, len) in iovecs {
                if off >= n {
                    break;
                }
                let chunk = len.min(n - off);
                // Safety: writing into the guest's own receive buffer(s),
                // same as do_recv_host's single-buffer case.
                unsafe {
                    dma_write(
                        base as i32,
                        data[off..off + chunk].as_ptr() as i32,
                        chunk as i32,
                    )
                };
                off += chunk;
            }
            return n as i32;
        }
        if rc == -EAGAIN {
            if nonblocking {
                self.set_errno(task, EAGAIN);
                return -1;
            }
            self.last_pending.insert(task, WaitKind::Recv { fd });
            return RES_PENDING;
        }
        self.set_errno(task, -rc);
        -1
    }

    fn do_close(&mut self, task: u32, fd: i32) -> i32 {
        if let Some(idx) = self.host_fd_index(fd) {
            let handle = self.host_fds[idx].take().unwrap().handle;
            unsafe { sock_close(handle) };
            self.scrub_stale_waits(fd);
            return 0;
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, EBADF);
            return -1;
        };
        let slot = self.fds[idx].take().unwrap();
        // Dup2Socket (Phase 4) can alias this socket from another fd --
        // only actually close/remove it once this was the last reference,
        // same as real dup()/dup2() semantics. `slot.refcount` is itself
        // one of the live clones, so checking before it drops at the end
        // of this function is exactly "am I the only one left."
        if Rc::strong_count(&slot.refcount) == 1 {
            let sockets = self.sockets.as_mut().expect("init() has run");
            match slot.kind {
                SockKind::Tcp => {
                    // Not an immediate remove(): tcp::Socket::close() only
                    // flips the state machine to FinWait1 (or similar) --
                    // the actual FIN segment isn't transmitted until a
                    // later iface.poll() processes that state, which never
                    // happens if the socket is yanked out of the SocketSet
                    // in this same call. That used to be exactly what this
                    // did, and the peer would never observe the close at
                    // all: its own socket just sits in Established forever
                    // (no FIN, no RST -- CloseSocket() on this side isn't
                    // a protocol event once the handle's gone), so any
                    // blocking recv() over there waits for data that's
                    // never coming. Found running bsdsocktest's own
                    // send-after-peer-close test, the first one in that
                    // suite to actually check that a close is *observable*
                    // from the other end rather than only checking the
                    // closing side's own return value. `closing_sockets`
                    // (reaped in tick(), once iface.poll() has actually
                    // driven the state machine to Closed) gives the FIN a
                    // real chance to go out first.
                    let socket = sockets.get_mut::<tcp::Socket>(slot.socket);
                    socket.close();
                    if socket.state() == tcp::State::Closed {
                        sockets.remove(slot.socket);
                    } else {
                        // 2 real seconds -- see tick()'s own reaping
                        // comment for why this needs a deadline at all,
                        // not just "wait however long it takes".
                        let deadline = self.micros + (2.0 * CCK_HZ) as i64;
                        self.closing_sockets.push((slot.socket, deadline));
                    }
                }
                SockKind::Udp => {
                    sockets.get_mut::<udp::Socket>(slot.socket).close();
                    sockets.remove(slot.socket);
                }
                // No half-open state to wind down (unlike TCP) and no
                // close() of its own to call (unlike UDP, which just
                // clears its bound endpoint) -- an ICMP socket is either
                // in the SocketSet or it isn't.
                SockKind::Icmp => {
                    sockets.remove(slot.socket);
                }
            }
        }
        self.scrub_stale_waits(fd);
        0
    }

    // Drops any stale wait bookkeeping for `fd` -- a task blocked on (or
    // about to block on) a socket that gets closed/released out from under
    // it (by another task, since fds are shared library-wide state)
    // shouldn't wake into, or block forever waiting on, a handle that no
    // longer means what it used to. Shared by do_close/do_release_socket,
    // the two places an fd number can start meaning something else --
    // scrubs `waiters` and `send_progress` (already-registered waits) and
    // `last_pending` (a wait a task hasn't registered yet, via
    // CALL_REGISTER_WAIT). The `last_pending` half used to be missing
    // here: `do_socket`/`do_obtain_socket` can immediately reuse a freed
    // fd-table slot for a *different* `SockKind` (UDP/ICMP where the stale
    // entry assumed TCP), and every `WaitKind::{Connect,Send,Accept}` arm
    // in `process_waiters` unconditionally does
    // `sockets.get::<tcp::Socket>(slot.socket)` -- a type-mismatched
    // `SocketSet::get` traps the whole wasm module, not just the one task,
    // if a task's own `CALL_REGISTER_WAIT` ever moved that stale entry
    // into `waiters` unchanged. Losing the wake path here (rather than
    // panicking) leaves that one task's own `Wait()` un-woken -- an
    // existing, narrower gap this project already accepts for a waiter
    // closed out from under it (see the comment this helper's `waiters`
    // scrub carries forward), not a new one.
    fn scrub_stale_waits(&mut self, fd: i32) {
        self.waiters.retain(|w| {
            !matches!(w.kind,
                WaitKind::Connect { fd: wfd } | WaitKind::Recv { fd: wfd }
                | WaitKind::Send { fd: wfd } | WaitKind::Accept { fd: wfd }
                    if wfd == fd)
        });
        self.send_progress.retain(|_, &mut (pfd, _)| pfd != fd);
        self.last_pending.retain(|_, kind| {
            !matches!(*kind,
                WaitKind::Connect { fd: wfd } | WaitKind::Recv { fd: wfd }
                | WaitKind::Send { fd: wfd } | WaitKind::Accept { fd: wfd }
                    if wfd == fd)
        });
    }

    // bind(sock, name, namelen): on TCP, just records the port (and
    // address, if a specific one rather than INADDR_ANY was given -- see
    // FdSlot::bind_addr's own comment) for listen()/connect()/
    // getsockname() to consume later -- smoltcp's tcp::Socket has no
    // "bound but not listening/connecting" state to hold it in directly.
    // A requested port of 0 resolves to a real ephemeral port up front
    // (the same `alloc_local_port()` connect()/listen()'s own auto-bind
    // paths already use) rather than being stored as a literal 0 -- this
    // used to just record whatever was asked for verbatim, so a real
    // caller asking for "any free port" via port 0 got back port 0 from
    // getsockname() instead of one it could actually reconnect to later.
    // Also now rejects a port another TCP fd already has bound
    // (EADDRINUSE) -- this used to accept unlimited binds to the same
    // port with no conflict check at all. On UDP, bind() takes effect
    // immediately (a standalone bind() is meaningful for a datagram
    // socket) -- unlike TCP, no ephemeral-port-0 or EADDRINUSE handling
    // here, since nothing in bsdsocktest's own loopback tier exercises
    // either for UDP.
    fn do_bind(&mut self, task: u32, fd: i32, addr: u32, namelen: i32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_bind_host(task, fd, addr, namelen);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let (ip, port) = self.parse_sockaddr(addr, namelen);
        match self.fds[idx].as_ref().unwrap().kind {
            SockKind::Tcp => {
                let port = if port == 0 {
                    self.alloc_local_port()
                } else {
                    port
                };
                let in_use = self.fds.iter().enumerate().any(|(i, slot)| {
                    i != idx
                        && slot
                            .as_ref()
                            .is_some_and(|s| s.kind == SockKind::Tcp && s.bind_port == Some(port))
                });
                if in_use {
                    self.set_errno(task, EADDRINUSE);
                    return -1;
                }
                let slot = self.fds[idx].as_mut().unwrap();
                slot.bind_port = Some(port);
                slot.bind_addr = (ip != Ipv4Address::new(0, 0, 0, 0)).then_some(ip);
                0
            }
            SockKind::Udp => {
                let handle = self.fds[idx].as_ref().unwrap().socket;
                let sockets = self.sockets.as_mut().expect("init() has run");
                match sockets.get_mut::<udp::Socket>(handle).bind(port) {
                    Ok(()) => 0,
                    Err(_) => {
                        self.set_errno(task, EINVAL);
                        -1
                    }
                }
            }
            // Nothing in bsdsocktest's own ICMP coverage ever calls
            // bind() on the raw socket -- it relies entirely on
            // do_sendto's own lazy bind-by-identifier (see that
            // function's own comment). Reject cleanly rather than
            // build out an untested path.
            SockKind::Icmp => {
                self.set_errno(task, EINVAL);
                -1
            }
        }
    }

    // do_bind's host-backend branch: a plain passthrough to `sock_bind`.
    // No EADDRINUSE bookkeeping of its own is needed here (unlike the
    // smoltcp path's own duplicate-bind scan) -- the host kernel already
    // enforces that for real, and `sock_bind`'s own errno translation
    // reports it exactly as any other `sock_bind` failure. A port of `0`
    // is passed straight through too (the OS picks a real ephemeral one);
    // unlike the smoltcp path, that choice doesn't need caching anywhere
    // for `getsockname()` to resolve it later -- `do_getsockname_host`
    // just asks `sock_local_addr` fresh each time, which always reports
    // whatever the OS actually bound the socket to, live.
    fn do_bind_host(&mut self, task: u32, fd: i32, addr: u32, namelen: i32) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let handle = self.host_fds[idx].as_ref().unwrap().handle;
        let (ip, port) = self.parse_sockaddr(addr, namelen);
        let packed_ip = u32::from_be_bytes(ip.octets()) as i32;
        let rc = unsafe { sock_bind(handle, packed_ip, port as i32) };
        if rc == 0 {
            return 0;
        }
        self.set_errno(task, -rc);
        -1
    }

    // listen(sock, backlog): puts the fd's TCP socket into Listen on
    // whatever port bind() recorded (or a fresh ephemeral one if bind()
    // was never called). `backlog` is accepted but not enforced -- smoltcp
    // has no connection-queue depth to bound (see PROPOSAL.md's Phase 3
    // design notes).
    fn do_listen(&mut self, task: u32, fd: i32, _backlog: i32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_listen_host(task, fd, _backlog);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let slot = self.fds[idx].as_ref().unwrap();
        let kind = slot.kind;
        let handle = slot.socket;
        let bind_port = slot.bind_port;
        if kind != SockKind::Tcp {
            self.set_errno(task, EINVAL);
            return -1;
        }
        let port = bind_port.unwrap_or_else(|| self.alloc_local_port());
        let sockets = self.sockets.as_mut().expect("init() has run");
        if sockets.get_mut::<tcp::Socket>(handle).listen(port).is_err() {
            self.set_errno(task, EINVAL);
            return -1;
        }
        let slot = self.fds[idx].as_mut().unwrap();
        slot.is_listener = true;
        slot.bind_port = Some(port);
        0
    }

    // do_listen's host-backend branch: a plain passthrough to
    // `sock_listen`, plus recording `is_listener` for
    // `sample_event_level_host`'s benefit (see that field's own comment).
    // No replacement-socket bookkeeping is needed here -- see
    // `do_accept_host`'s own comment for why a real host listening socket
    // doesn't need the smoltcp path's "swap in a fresh listener on every
    // accept" trick.
    fn do_listen_host(&mut self, task: u32, fd: i32, backlog: i32) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let slot = self.host_fds[idx].as_ref().unwrap();
        // Same explicit rejection the smoltcp path's own do_listen gives
        // a UDP fd -- real listen()/accept() are TCP-only concepts, and
        // an explicit EINVAL here is a clearer answer than whatever a
        // real OS's own listen(2) on SOCK_DGRAM happens to report.
        if slot.kind != SockKind::Tcp {
            self.set_errno(task, EINVAL);
            return -1;
        }
        let handle = slot.handle;
        let rc = unsafe { sock_listen(handle, backlog) };
        if rc == 0 {
            self.host_fds[idx].as_mut().unwrap().is_listener = true;
            return 0;
        }
        self.set_errno(task, -rc);
        -1
    }

    // accept(sock, addr, addrlen): smoltcp's tcp::Socket has no accept() --
    // a Listen-state socket transitions itself directly into the
    // connection once a SYN arrives. So: if the listener is still
    // Listening, there is nothing to accept yet (block or EWOULDBLOCK, same
    // split as do_connect/do_recv). Otherwise a connection arrived and
    // `handle` itself is now that connection: hand it to the caller under
    // a *new* fd, and put a *fresh* socket back in Listen on the same port
    // under the *original* fd so it keeps accepting more connections.
    fn do_accept(&mut self, task: u32, fd: i32, addr_out: u32, len_ptr: u32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_accept_host(task, fd, addr_out, len_ptr);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let slot = self.fds[idx].as_ref().unwrap();
        if !slot.is_listener {
            self.set_errno(task, EINVAL);
            return -1;
        }
        let handle = slot.socket;
        let port = slot.bind_port.expect("a listener always has a bound port");
        let nonblocking = slot.nonblocking;
        let bind_addr = slot.bind_addr;
        let opts = slot.opts;

        let sockets = self.sockets.as_ref().expect("init() has run");
        if sockets.get::<tcp::Socket>(handle).is_listening() {
            if nonblocking {
                self.set_errno(task, EAGAIN);
                return -1;
            }
            self.last_pending.insert(task, WaitKind::Accept { fd });
            return RES_PENDING;
        }

        let (peer_ip, peer_port) = ipv4_of(sockets.get::<tcp::Socket>(handle).remote_endpoint());

        let Some(new_idx) = self.fds.iter().position(Option::is_none) else {
            // No free fd for the accepted connection -- leave the listener
            // as-is (still not Listening) so the guest can free a fd and
            // retry; EMFILE ("too many open files") is real accept()'s own
            // answer for exactly this.
            self.set_errno(task, EMFILE);
            return -1;
        };

        let sockets = self.sockets.as_mut().expect("init() has run");
        let fresh_listener = sockets.add(tcp::Socket::new(
            RingBuffer::new(vec![0u8; SOCKET_BUF_LEN]),
            RingBuffer::new(vec![0u8; SOCKET_BUF_LEN]),
        ));
        if sockets
            .get_mut::<tcp::Socket>(fresh_listener)
            .listen(port)
            .is_err()
        {
            // Extremely unlikely (a fresh socket can always listen); if it
            // somehow fails, drop it and leave the original fd's listener
            // alone rather than losing the slot silently.
            sockets.remove(fresh_listener);
            return -1;
        }

        self.fds[idx] = Some(FdSlot {
            kind: SockKind::Tcp,
            socket: fresh_listener,
            refcount: Rc::new(()),
            nonblocking,
            bind_port: Some(port),
            bind_addr,
            is_listener: true,
            udp_peer: None,
            connect_started: false,
            // Meaningless for a listener (nothing calls send()/recv() on
            // one), but every field needs a value -- matches the same
            // "never actually connected" default do_socket's own fresh
            // slots use.
            was_established: false,
            shutdown_by_us: false,
            opts,
        });
        self.fds[new_idx] = Some(FdSlot {
            kind: SockKind::Tcp,
            socket: handle,
            refcount: Rc::new(()),
            nonblocking: false,
            bind_port: None,
            bind_addr: None,
            is_listener: false,
            udp_peer: None,
            // Already Established via accept(), not do_connect() -- moot
            // either way, but true is the accurate description.
            connect_started: true,
            was_established: true,
            shutdown_by_us: false,
            opts: SockOpts::new(),
        });

        self.write_sockaddr_out(addr_out, len_ptr, peer_ip, peer_port);
        (new_idx + 1) as i32
    }

    // do_accept's host-backend branch. Simpler than the smoltcp path
    // above: a real host listening socket keeps listening for more
    // connections on its own after `accept()` returns one, so there is no
    // "swap in a fresh listener" dance to do here -- `sock_accept` is
    // just called again on the *same* handle every time, exactly like
    // `do_connect_host`/`do_send_host`/`do_recv_host` re-issue their own
    // `sock_*` call on every retry.
    fn do_accept_host(&mut self, task: u32, fd: i32, addr_out: u32, len_ptr: u32) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let slot = self.host_fds[idx].as_ref().unwrap();
        let listener_handle = slot.handle;
        let nonblocking = slot.nonblocking;

        let rc = unsafe { sock_accept(listener_handle) };
        if rc == -EAGAIN {
            if nonblocking {
                self.set_errno(task, EAGAIN);
                return -1;
            }
            self.last_pending.insert(task, WaitKind::Accept { fd });
            return RES_PENDING;
        }
        if rc < 0 {
            self.set_errno(task, -rc);
            return -1;
        }
        let new_handle = rc;

        let Some(new_idx) =
            (0..MAX_FDS).find(|&i| self.fds[i].is_none() && self.host_fds[i].is_none())
        else {
            unsafe { sock_close(new_handle) };
            self.set_errno(task, EMFILE);
            return -1;
        };
        self.host_fds[new_idx] = Some(HostFdSlot {
            handle: new_handle,
            // accept() is TCP-only (a listening socket is always
            // SockKind::Tcp -- do_listen_host has no UDP path to have
            // ever produced one).
            kind: SockKind::Tcp,
            nonblocking: false,
            // Already connected via accept(), not do_connect_host() --
            // moot either way (nothing re-issues sock_connect on this
            // fd), but true is the accurate description, same reasoning
            // as the smoltcp path's own identical comment just above.
            connect_started: true,
            is_listener: false,
            opts: HostSockOpts::default(),
        });

        // A `sock_peer_addr` failure (rare -- the connection would have
        // to have already died between `accept()` returning it and this
        // very next call) reports 0.0.0.0:0 rather than failing the whole
        // accept() -- the new fd is real and usable regardless of whether
        // its address could be queried.
        let mut buf = [0u8; 6];
        let (peer_ip, peer_port) =
            if unsafe { sock_peer_addr(new_handle, buf.as_mut_ptr() as i32) } == 0 {
                (
                    Ipv4Address::new(buf[0], buf[1], buf[2], buf[3]),
                    u16::from_be_bytes([buf[4], buf[5]]),
                )
            } else {
                (Ipv4Address::new(0, 0, 0, 0), 0)
            };
        self.write_sockaddr_out(addr_out, len_ptr, peer_ip, peer_port);
        (new_idx + 1) as i32
    }

    // sendto(sock, buf, len, flags, to, tolen): on a TCP fd, behaves like
    // send() (real systems allow sendto() on a connected stream socket).
    // On UDP, `to_addr == 0` means "use the peer connect() recorded"
    // (do_send's delegation path, and real UDP send()-without-explicit-
    // destination semantics); auto-binds an ephemeral local port on first
    // use if the fd was never bound, matching real UDP semantics.
    fn do_sendto(
        &mut self,
        task: u32,
        fd: i32,
        buf_addr: u32,
        len: i32,
        to_addr: u32,
        tolen: i32,
    ) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_sendto_host(task, fd, buf_addr, len, to_addr, tolen);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let slot = self.fds[idx].as_ref().unwrap();
        let kind = slot.kind;
        let udp_peer = slot.udp_peer;
        let handle = slot.socket;
        if kind == SockKind::Tcp {
            // sendto()'s own `flags` isn't threaded down to this function
            // (CALL_SENDTO's dispatch wiring drops arg4 the same way
            // CALL_SEND's used to, and nothing in bsdsocktest's own
            // MSG_OOB coverage ever calls sendto() rather than plain
            // send() -- see do_send's own MSG_OOB comment for why this
            // would matter if it ever did).
            return self.do_send(task, fd, buf_addr, len, 0);
        }

        let dest = if to_addr != 0 {
            Some(self.parse_sockaddr(to_addr, tolen))
        } else {
            udp_peer
        };
        let Some((ip, port)) = dest else {
            self.set_errno(task, ENOTCONN);
            return -1;
        };

        let n = (len.max(0) as usize).min(MAX_XFER_LEN);
        let mut data = vec![0u8; n];
        // Safety: reading the guest's send buffer out of Amiga memory.
        unsafe { dma_read(buf_addr as i32, data.as_mut_ptr() as i32, n as i32) };

        if kind == SockKind::Icmp {
            return self.do_icmp_sendto(task, idx, &data, ip);
        }

        let needs_bind = {
            let sockets = self.sockets.as_ref().expect("init() has run");
            !sockets.get::<udp::Socket>(handle).is_open()
        };
        if needs_bind {
            let local_port = self.alloc_local_port();
            let sockets = self.sockets.as_mut().expect("init() has run");
            let _ = sockets.get_mut::<udp::Socket>(handle).bind(local_port);
        }

        let sockets = self.sockets.as_mut().expect("init() has run");
        let socket = sockets.get_mut::<udp::Socket>(handle);
        match socket.send_slice(&data, (IpAddress::Ipv4(ip), port)) {
            Ok(()) => n as i32,
            Err(_) => {
                self.set_errno(task, ECONNREFUSED);
                -1
            }
        }
    }

    // do_sendto's host-backend branch. `to_addr == 0` (real UDP
    // send()-without-explicit-destination semantics, on a connected
    // socket) and a TCP fd both delegate to `do_send_host` -- a connected
    // host socket's plain `sock_send` already sends to the peer
    // `sock_connect` recorded, so there is no separate `udp_peer`
    // bookkeeping to maintain here the way the smoltcp path's own
    // `FdSlot::udp_peer` needs (real BSD `connect()` on the *host* kernel
    // already does that recording for us). A non-zero `to_addr` is the
    // real new path: an explicit per-call destination via `sock_sendto`,
    // no prior `connect()` needed.
    fn do_sendto_host(
        &mut self,
        task: u32,
        fd: i32,
        buf_addr: u32,
        len: i32,
        to_addr: u32,
        tolen: i32,
    ) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let slot = self.host_fds[idx].as_ref().unwrap();
        if slot.kind == SockKind::Tcp || to_addr == 0 {
            return self.do_send_host(task, fd, buf_addr, len, 0);
        }
        let handle = slot.handle;
        let nonblocking = slot.nonblocking;
        let (ip, port) = self.parse_sockaddr(to_addr, tolen);

        let n = (len.max(0) as usize).min(MAX_XFER_LEN);
        let mut data = vec![0u8; n];
        // Safety: reading the guest's send buffer out of Amiga memory.
        unsafe { dma_read(buf_addr as i32, data.as_mut_ptr() as i32, n as i32) };
        let packed_ip = u32::from_be_bytes(ip.octets()) as i32;
        let rc = unsafe {
            sock_sendto(
                handle,
                data.as_ptr() as i32,
                data.len() as i32,
                packed_ip,
                port as i32,
            )
        };

        if rc >= 0 {
            return rc;
        }
        if rc == -EAGAIN {
            if nonblocking {
                self.set_errno(task, EAGAIN);
                return -1;
            }
            // Real UDP has no per-connection flow control, so this
            // almost never actually happens -- kept for correctness
            // (a momentarily full local send buffer is still possible)
            // rather than assumed away. No `send_progress` bookkeeping
            // needed here unlike TCP's own blocking send: a datagram is
            // sent whole or not at all, never partially.
            self.last_pending.insert(task, WaitKind::Send { fd });
            return RES_PENDING;
        }
        self.set_errno(task, -rc);
        -1
    }

    // sendto() on a raw ICMP socket: the caller supplies just the ICMP
    // message itself (type/code/checksum/ident/seq/payload -- real BSD
    // raw-socket write semantics without IP_HDRINCL, matching
    // bsdsocktest's own icmp_ping(), which builds exactly that and never
    // touches an IP header). Binds the underlying `icmp::Socket` lazily,
    // the first time this fd ever sends: smoltcp's ICMP socket API has
    // no "receive everything" mode (`Endpoint::Unspecified` isn't a
    // legal bind target, see icmp::Socket::bind's own doc comment) --
    // only `Endpoint::Ident(id)`, matching a real "ping socket"'s own
    // demux-by-identifier model, so the identifier is pulled out of the
    // echo request's own header (bytes 4-5, the same position real ping
    // programs put it) rather than hardcoding bsdsocktest's own fixed
    // 0xBD51 test constant.
    fn do_icmp_sendto(&mut self, task: u32, idx: usize, data: &[u8], to_ip: Ipv4Address) -> i32 {
        let handle = self.fds[idx].as_ref().unwrap().socket;
        let sockets = self.sockets.as_mut().expect("init() has run");
        let socket = sockets.get_mut::<icmp::Socket>(handle);
        if !socket.is_open() {
            if data.len() < 6 {
                self.set_errno(task, EINVAL);
                return -1;
            }
            let ident = u16::from_be_bytes([data[4], data[5]]);
            // Only fails if already open (just checked) or the endpoint
            // is unaddressable (never true for `Ident`, see `is_specified`).
            let _ = socket.bind(icmp::Endpoint::Ident(ident));
        }
        match socket.send_slice(data, IpAddress::Ipv4(to_ip)) {
            Ok(()) => data.len() as i32,
            Err(_) => {
                self.set_errno(task, ECONNREFUSED);
                -1
            }
        }
    }

    // recvfrom(sock, buf, len, flags, addr, addrlen): on a TCP fd, behaves
    // like recv() (the sender is always the connected peer, so there is
    // nothing extra to report). On UDP, blocks (RES_PENDING + wait
    // registration, or -1/EWOULDBLOCK non-blocking) until a datagram
    // arrives, then fills in the sender's address if asked.
    fn do_recvfrom(
        &mut self,
        task: u32,
        fd: i32,
        buf_addr: u32,
        len: i32,
        addr_out: u32,
        len_ptr: u32,
    ) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_recvfrom_host(task, fd, buf_addr, len, addr_out, len_ptr);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let slot = self.fds[idx].as_ref().unwrap();
        let kind = slot.kind;
        let handle = slot.socket;
        let nonblocking = slot.nonblocking;
        if kind == SockKind::Tcp {
            return self.do_recv(task, fd, buf_addr, len, 0);
        }
        if kind == SockKind::Icmp {
            return self.do_icmp_recv(task, fd, idx, buf_addr, len, addr_out, len_ptr);
        }

        let sockets = self.sockets.as_mut().expect("init() has run");
        let socket = sockets.get_mut::<udp::Socket>(handle);

        if !socket.can_recv() {
            if nonblocking {
                self.set_errno(task, EAGAIN);
                return -1;
            }
            self.last_pending.insert(task, WaitKind::Recv { fd });
            return RES_PENDING;
        }

        let cap = (len.max(0) as usize).min(MAX_XFER_LEN);
        // Real recvfrom() truncates an oversized datagram to the caller's
        // buffer and returns the truncated byte count, dropping the rest --
        // it never discards the whole datagram. `recv_slice` dequeues via
        // `recv()` internally and *then* checks the buffer size, so on a
        // too-small buffer it returns `Truncated` after the datagram is
        // already gone: using it here would silently drop every datagram
        // larger than `cap`. `recv()` hands back the full payload instead,
        // so the truncation can be applied on the way to the guest, the
        // same as it would be by the guest's own buffer.
        let (n, ip, port) = match socket.recv() {
            Ok((payload, meta)) => {
                let n = payload.len().min(cap);
                // Safety: writing into the guest's receive buffer in Amiga memory.
                unsafe { dma_write(buf_addr as i32, payload.as_ptr() as i32, n as i32) };
                let (ip, port) = ipv4_of(Some(meta.endpoint));
                (n, ip, port)
            }
            Err(_) => return 0,
        };
        self.write_sockaddr_out(addr_out, len_ptr, ip, port);
        n as i32
    }

    // do_recvfrom's host-backend branch: a TCP fd delegates to
    // `do_recv_host` (the sender is always the connected peer, nothing
    // extra to report -- same reasoning as the smoltcp path's own
    // identical delegation above). A UDP fd calls `sock_recvfrom`
    // directly, which always reports the sender's address regardless of
    // whether the guest asked for it (cheap -- the host kernel returns it
    // as part of the same `recvfrom(2)` call either way); it's simply not
    // forwarded when `addr_out == 0` (`write_sockaddr_out`'s existing
    // convention).
    fn do_recvfrom_host(
        &mut self,
        task: u32,
        fd: i32,
        buf_addr: u32,
        len: i32,
        addr_out: u32,
        len_ptr: u32,
    ) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let slot = self.host_fds[idx].as_ref().unwrap();
        if slot.kind == SockKind::Tcp {
            return self.do_recv_host(task, fd, buf_addr, len, 0);
        }
        let handle = slot.handle;
        let nonblocking = slot.nonblocking;

        let cap = (len.max(0) as usize).min(MAX_XFER_LEN);
        let mut data = vec![0u8; cap];
        let mut addr_buf = [0u8; 6];
        let rc = unsafe {
            sock_recvfrom(
                handle,
                data.as_mut_ptr() as i32,
                data.len() as i32,
                addr_buf.as_mut_ptr() as i32,
            )
        };

        if rc >= 0 {
            let n = rc as usize;
            // Safety: writing into the guest's receive buffer in Amiga
            // memory.
            unsafe { dma_write(buf_addr as i32, data.as_ptr() as i32, n as i32) };
            let ip = Ipv4Address::new(addr_buf[0], addr_buf[1], addr_buf[2], addr_buf[3]);
            let port = u16::from_be_bytes([addr_buf[4], addr_buf[5]]);
            self.write_sockaddr_out(addr_out, len_ptr, ip, port);
            return n as i32;
        }
        if rc == -EAGAIN {
            if nonblocking {
                self.set_errno(task, EAGAIN);
                return -1;
            }
            self.last_pending.insert(task, WaitKind::Recv { fd });
            return RES_PENDING;
        }
        self.set_errno(task, -rc);
        -1
    }

    // recv()/recvfrom() on a raw ICMP socket: real BSD raw sockets deliver
    // the *full* IP packet on read (header included), unlike the write
    // side's header-less convention (see do_icmp_sendto's own comment) --
    // bsdsocktest's own icmp_ping() relies on exactly this asymmetry,
    // reading `icmp_rbuf[0] & 0x0F` as the IP header's IHL nibble to find
    // where the ICMP payload starts. smoltcp's own `icmp::Socket::
    // recv_slice` hands back only the ICMP payload (the interface layer
    // already parsed and stripped the real IP header before delivery), so
    // this synthesizes a minimal-but-valid 20-byte IPv4 header -- version/
    // IHL, total length, TTL, protocol, a real computed checksum, and the
    // sender/our-own addresses -- and prepends it, truncating to `cap`
    // (`len`) the same way a real short guest buffer would truncate a
    // real oversized datagram.
    fn do_icmp_recv(
        &mut self,
        task: u32,
        fd: i32,
        idx: usize,
        buf_addr: u32,
        len: i32,
        addr_out: u32,
        len_ptr: u32,
    ) -> i32 {
        let slot = self.fds[idx].as_ref().unwrap();
        let handle = slot.socket;
        let nonblocking = slot.nonblocking;
        let sockets = self.sockets.as_mut().expect("init() has run");
        let socket = sockets.get_mut::<icmp::Socket>(handle);

        if !socket.can_recv() {
            if nonblocking {
                self.set_errno(task, EAGAIN);
                return -1;
            }
            self.last_pending.insert(task, WaitKind::Recv { fd });
            return RES_PENDING;
        }

        let mut payload = vec![0u8; ICMP_BUF_LEN];
        let (payload_len, src) = match socket.recv_slice(&mut payload) {
            Ok(v) => v,
            Err(_) => return 0,
        };
        let src_ip = match src {
            IpAddress::Ipv4(v4) => v4,
        };
        let ip_header = synth_ipv4_header(payload_len, src_ip, self.interface_addr);

        let cap = len.max(0) as usize;
        let mut out = ip_header.to_vec();
        out.extend_from_slice(&payload[..payload_len]);
        out.truncate(cap);
        // Safety: writing into the guest's receive buffer in Amiga memory.
        unsafe { dma_write(buf_addr as i32, out.as_ptr() as i32, out.len() as i32) };
        self.write_sockaddr_out(addr_out, len_ptr, src_ip, 0);
        out.len() as i32
    }

    fn do_shutdown(&mut self, task: u32, fd: i32, _how: i32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_shutdown_host(task, fd, _how);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let slot = self.fds[idx].as_ref().unwrap();
        match slot.kind {
            SockKind::Tcp => {
                let handle = slot.socket;
                let sockets = self.sockets.as_mut().expect("init() has run");
                // No half-duplex shutdown primitive in smoltcp to
                // distinguish SHUT_RD/SHUT_WR/SHUT_RDWR -- `how` is
                // accepted but always maps to a full close (documented
                // simplification, see PROPOSAL.md's Phase 3 design notes).
                sockets.get_mut::<tcp::Socket>(handle).close();
                // Marks this as a locally-requested teardown -- send_tcp_
                // stream/do_recv use this afterward to report EPIPE
                // ("we already said we're done") rather than ECONNRESET
                // ("the peer tore it down out from under us") once the
                // connection is no longer usable (see
                // FdSlot::shutdown_by_us's own comment).
                self.fds[idx].as_mut().unwrap().shutdown_by_us = true;
                0
            }
            // No real notion of "shutdown" for a datagram socket; accepted
            // as a no-op.
            SockKind::Udp => 0,
            // Same -- no connection state, nothing to shut down.
            SockKind::Icmp => 0,
        }
    }

    // do_shutdown's host-backend branch: unlike the smoltcp path, a real
    // host socket has a genuine half-duplex `shutdown(2)`, so `how`
    // (0 = SHUT_RD, 1 = SHUT_WR, 2 = SHUT_RDWR) is honoured for real
    // instead of always mapping to a full close.
    fn do_shutdown_host(&mut self, task: u32, fd: i32, how: i32) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let handle = self.host_fds[idx].as_ref().unwrap().handle;
        let rc = unsafe { sock_shutdown(handle, how) };
        if rc == 0 {
            return 0;
        }
        self.set_errno(task, -rc);
        -1
    }

    // setsockopt: a small, real subset, not the full option space (see
    // PROPOSAL.md's Phase 3 design notes) -- plain per-fd roundtrip
    // storage in `FdSlot::opts` for SOL_SOCKET's SO_REUSEADDR/
    // SO_KEEPALIVE/SO_LINGER/SO_RCVTIMEO/SO_SNDTIMEO/SO_RCVBUF/SO_SNDBUF
    // and IPPROTO_TCP's TCP_NODELAY, matching `SockOpts`'s own doc
    // comment for why none of these actually change smoltcp's behaviour.
    // This used to not even read `optname` at all -- every option calling
    // this "succeeded" while doing nothing, including the roundtrip.
    fn do_setsockopt(
        &mut self,
        task: u32,
        fd: i32,
        level: i32,
        optname: i32,
        optval: u32,
        optlen: i32,
    ) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_setsockopt_host(task, fd, level, optname, optval, optlen);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let read_i32 = |addr: u32| -> i32 {
            let mut raw = [0u8; 4];
            unsafe { dma_read(addr as i32, raw.as_mut_ptr() as i32, 4) };
            i32::from_be_bytes(raw)
        };
        // (secs, other) -- the shared 8-byte, two-BE-LONG shape of both
        // `struct timeval` (tv_secs/tv_micro) and `struct linger`
        // (l_onoff/l_linger).
        let read_pair = |addr: u32| -> (i32, i32) {
            let mut raw = [0u8; 8];
            unsafe { dma_read(addr as i32, raw.as_mut_ptr() as i32, 8) };
            (
                i32::from_be_bytes(raw[0..4].try_into().unwrap()),
                i32::from_be_bytes(raw[4..8].try_into().unwrap()),
            )
        };
        // Handled separately from the generic roundtrip-storage match below:
        // setting a non-zero eventmask needs to *sample* the fd's current
        // readiness (via self.sockets) as the edge-detection baseline, not
        // just store the raw value -- see `SockOpts::ev_prev`'s own comment
        // for why (skipping this makes an already-ready socket fire a
        // spurious event on the very next tick).
        if level == SOL_SOCKET && optname == SO_EVENTMASK {
            let mask = read_i32(optval);
            let baseline = self.sample_event_level(idx);
            let opts = &mut self.fds[idx].as_mut().unwrap().opts;
            opts.eventmask = mask;
            opts.ev_prev = Some(baseline);
            return 0;
        }
        let opts = &mut self.fds[idx].as_mut().unwrap().opts;
        match (level, optname) {
            (SOL_SOCKET, SO_REUSEADDR) => opts.reuseaddr = read_i32(optval) != 0,
            (SOL_SOCKET, SO_KEEPALIVE) => opts.keepalive = read_i32(optval) != 0,
            (SOL_SOCKET, SO_LINGER) if optlen >= 8 => {
                (opts.linger_onoff, opts.linger_secs) = read_pair(optval);
            }
            (SOL_SOCKET, SO_RCVTIMEO) if optlen >= 8 => opts.rcvtimeo = read_pair(optval),
            (SOL_SOCKET, SO_SNDTIMEO) if optlen >= 8 => opts.sndtimeo = read_pair(optval),
            (SOL_SOCKET, SO_RCVBUF) => opts.rcvbuf = read_i32(optval),
            (SOL_SOCKET, SO_SNDBUF) => opts.sndbuf = read_i32(optval),
            (IPPROTO_TCP, TCP_NODELAY) => opts.nodelay = read_i32(optval) != 0,
            _ => {
                self.set_errno(task, EINVAL);
                return -1;
            }
        }
        0
    }

    // do_setsockopt's host-backend branch: SO_LINGER/SO_RCVTIMEO/
    // SO_SNDTIMEO stay plugin-side roundtrip storage in `HostFdSlot::opts`
    // (see that type's own doc comment for why); everything else is a
    // real `sock_setopt` call applied directly to the host socket.
    fn do_setsockopt_host(
        &mut self,
        task: u32,
        fd: i32,
        level: i32,
        optname: i32,
        optval: u32,
        optlen: i32,
    ) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let read_i32 = |addr: u32| -> i32 {
            let mut raw = [0u8; 4];
            unsafe { dma_read(addr as i32, raw.as_mut_ptr() as i32, 4) };
            i32::from_be_bytes(raw)
        };
        let read_pair = |addr: u32| -> (i32, i32) {
            let mut raw = [0u8; 8];
            unsafe { dma_read(addr as i32, raw.as_mut_ptr() as i32, 8) };
            (
                i32::from_be_bytes(raw[0..4].try_into().unwrap()),
                i32::from_be_bytes(raw[4..8].try_into().unwrap()),
            )
        };
        // Same reasoning as do_setsockopt's own identical special-case:
        // sample the current readiness as the edge-detection baseline
        // before storing the mask, via the host-backed
        // `sample_event_level_host` rather than `sample_event_level`.
        if level == SOL_SOCKET && optname == SO_EVENTMASK {
            let mask = read_i32(optval);
            let baseline = self.sample_event_level_host(idx);
            let opts = &mut self.host_fds[idx].as_mut().unwrap().opts;
            opts.eventmask = mask;
            opts.ev_prev = Some(baseline);
            return 0;
        }
        match (level, optname) {
            (SOL_SOCKET, SO_LINGER) if optlen >= 8 => {
                let (onoff, secs) = read_pair(optval);
                let opts = &mut self.host_fds[idx].as_mut().unwrap().opts;
                opts.linger_onoff = onoff;
                opts.linger_secs = secs;
                return 0;
            }
            (SOL_SOCKET, SO_RCVTIMEO) if optlen >= 8 => {
                self.host_fds[idx].as_mut().unwrap().opts.rcvtimeo = read_pair(optval);
                return 0;
            }
            (SOL_SOCKET, SO_SNDTIMEO) if optlen >= 8 => {
                self.host_fds[idx].as_mut().unwrap().opts.sndtimeo = read_pair(optval);
                return 0;
            }
            _ => {}
        }
        let handle = self.host_fds[idx].as_ref().unwrap().handle;
        let value = read_i32(optval);
        let rc = unsafe { sock_setopt(handle, level, optname, value) };
        if rc == 0 {
            return 0;
        }
        self.set_errno(task, -rc);
        -1
    }

    // getsockopt: SOL_SOCKET/SO_ERROR is the one option real programs
    // actually poll after a failed non-blocking operation -- reading it
    // clears the task's last errno, matching real SO_ERROR semantics
    // ("get error status and clear"). SO_TYPE reports the fd's real kind.
    // Everything else just reads back whatever do_setsockopt's roundtrip
    // storage in `FdSlot::opts` has (see its own doc comment for why nothing
    // here changes smoltcp's actual behaviour).
    fn do_getsockopt(
        &mut self,
        task: u32,
        fd: i32,
        level: i32,
        optname: i32,
        optval: u32,
        optlen_ptr: u32,
    ) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_getsockopt_host(task, fd, level, optname, optval, optlen_ptr);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let slot = self.fds[idx].as_ref().unwrap();
        let kind = slot.kind;
        let opts = slot.opts;
        // Real getsockopt() is a value-result call: *optlen is the caller's
        // buffer size on input, silently truncating the copy if the option
        // is larger, and the actual number of bytes written on output. The
        // buffer sizes here (4 or 8 bytes) are fixed by option, not by the
        // caller, so without this the SO_LINGER/SO_RCVTIMEO/SO_SNDTIMEO
        // 8-byte writes would clobber whatever follows a caller's
        // 4-byte-sized buffer. optlen_ptr == 0 has no declared cap to
        // honor (also not a shape real getsockopt allows when optval is
        // non-null), so it keeps writing the option's natural size.
        let declared_len = if optlen_ptr != 0 {
            let mut raw = [0u8; 4];
            unsafe { dma_read(optlen_ptr as i32, raw.as_mut_ptr() as i32, 4) };
            i32::from_be_bytes(raw).max(0) as usize
        } else {
            usize::MAX
        };
        let write_capped = |bytes: &[u8]| {
            let n = bytes.len().min(declared_len);
            if n > 0 {
                unsafe { dma_write(optval as i32, bytes.as_ptr() as i32, n as i32) };
            }
            if optlen_ptr != 0 {
                let out = (n as i32).to_be_bytes();
                unsafe { dma_write(optlen_ptr as i32, out.as_ptr() as i32, 4) };
            }
        };
        let write_i32 = |value: i32| write_capped(&value.to_be_bytes());
        let write_pair = |pair: (i32, i32)| {
            let mut bytes = [0u8; 8];
            bytes[0..4].copy_from_slice(&pair.0.to_be_bytes());
            bytes[4..8].copy_from_slice(&pair.1.to_be_bytes());
            write_capped(&bytes);
        };
        if level == SOL_SOCKET && optname == SO_ERROR {
            let err = self.tasks.get(&task).map_or(0, |t| t.last_errno);
            if let Some(t) = self.tasks.get_mut(&task) {
                t.last_errno = 0;
            }
            write_i32(err);
            return 0;
        }
        if level == SOL_SOCKET && optname == SO_TYPE {
            write_i32(match kind {
                SockKind::Tcp => SOCK_STREAM,
                SockKind::Udp => SOCK_DGRAM,
                SockKind::Icmp => SOCK_RAW,
            });
            return 0;
        }
        match (level, optname) {
            (SOL_SOCKET, SO_REUSEADDR) => write_i32(opts.reuseaddr as i32),
            (SOL_SOCKET, SO_KEEPALIVE) => write_i32(opts.keepalive as i32),
            (SOL_SOCKET, SO_LINGER) => write_pair((opts.linger_onoff, opts.linger_secs)),
            (SOL_SOCKET, SO_RCVTIMEO) => write_pair(opts.rcvtimeo),
            (SOL_SOCKET, SO_SNDTIMEO) => write_pair(opts.sndtimeo),
            (SOL_SOCKET, SO_RCVBUF) => write_i32(opts.rcvbuf),
            (SOL_SOCKET, SO_SNDBUF) => write_i32(opts.sndbuf),
            (SOL_SOCKET, SO_EVENTMASK) => write_i32(opts.eventmask),
            (IPPROTO_TCP, TCP_NODELAY) => write_i32(opts.nodelay as i32),
            _ => {
                self.set_errno(task, EINVAL);
                return -1;
            }
        }
        0
    }

    // do_getsockopt's host-backend branch: SO_TYPE reads the fd's real
    // kind, SO_LINGER/SO_RCVTIMEO/SO_SNDTIMEO/SO_EVENTMASK read back
    // `HostFdSlot::opts`'s roundtrip storage (see that type's own doc
    // comment), and everything else -- including SO_ERROR, which here is
    // a *real* "get pending error and clear" via the host socket's own
    // `SO_ERROR` (see `sock_getopt`'s own doc comment in
    // src/wasmboard.rs), not the smoltcp path's task-level-errno proxy --
    // is a real `sock_getopt` call against the host socket.
    fn do_getsockopt_host(
        &mut self,
        task: u32,
        fd: i32,
        level: i32,
        optname: i32,
        optval: u32,
        optlen_ptr: u32,
    ) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let slot = *self.host_fds[idx].as_ref().unwrap();

        let declared_len = if optlen_ptr != 0 {
            let mut raw = [0u8; 4];
            unsafe { dma_read(optlen_ptr as i32, raw.as_mut_ptr() as i32, 4) };
            i32::from_be_bytes(raw).max(0) as usize
        } else {
            usize::MAX
        };
        let write_capped = |bytes: &[u8]| {
            let n = bytes.len().min(declared_len);
            if n > 0 {
                unsafe { dma_write(optval as i32, bytes.as_ptr() as i32, n as i32) };
            }
            if optlen_ptr != 0 {
                let out = (n as i32).to_be_bytes();
                unsafe { dma_write(optlen_ptr as i32, out.as_ptr() as i32, 4) };
            }
        };
        let write_i32 = |value: i32| write_capped(&value.to_be_bytes());

        if level == SOL_SOCKET && optname == SO_TYPE {
            write_i32(match slot.kind {
                SockKind::Tcp => SOCK_STREAM,
                SockKind::Udp => SOCK_DGRAM,
                SockKind::Icmp => unreachable!("do_socket_host only ever creates Tcp/Udp"),
            });
            return 0;
        }
        match (level, optname) {
            (SOL_SOCKET, SO_LINGER) => {
                let mut bytes = [0u8; 8];
                bytes[0..4].copy_from_slice(&slot.opts.linger_onoff.to_be_bytes());
                bytes[4..8].copy_from_slice(&slot.opts.linger_secs.to_be_bytes());
                write_capped(&bytes);
                return 0;
            }
            (SOL_SOCKET, SO_RCVTIMEO) => {
                let mut bytes = [0u8; 8];
                bytes[0..4].copy_from_slice(&slot.opts.rcvtimeo.0.to_be_bytes());
                bytes[4..8].copy_from_slice(&slot.opts.rcvtimeo.1.to_be_bytes());
                write_capped(&bytes);
                return 0;
            }
            (SOL_SOCKET, SO_SNDTIMEO) => {
                let mut bytes = [0u8; 8];
                bytes[0..4].copy_from_slice(&slot.opts.sndtimeo.0.to_be_bytes());
                bytes[4..8].copy_from_slice(&slot.opts.sndtimeo.1.to_be_bytes());
                write_capped(&bytes);
                return 0;
            }
            (SOL_SOCKET, SO_EVENTMASK) => {
                write_i32(slot.opts.eventmask);
                return 0;
            }
            _ => {}
        }
        let mut buf = [0u8; 4];
        let rc = unsafe { sock_getopt(slot.handle, level, optname, buf.as_mut_ptr() as i32) };
        if rc != 0 {
            self.set_errno(task, -rc);
            return -1;
        }
        write_capped(&buf);
        0
    }

    fn do_getsockname(&mut self, task: u32, fd: i32, addr_out: u32, len_ptr: u32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_getsockname_host(task, fd, addr_out, len_ptr);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let slot = self.fds[idx].as_ref().unwrap();
        let (kind, handle, bind_port, bind_addr) =
            (slot.kind, slot.socket, slot.bind_port, slot.bind_addr);
        let sockets = self.sockets.as_ref().expect("init() has run");
        let (ip, port) = match kind {
            SockKind::Tcp => match sockets.get::<tcp::Socket>(handle).local_endpoint() {
                Some(ep) => ipv4_of(Some(ep)),
                // Not connected/listening yet -- report back whatever
                // bind() actually recorded (a specific address, if one was
                // given) instead of always claiming the interface's own
                // address regardless of what was bound.
                None => (
                    bind_addr.unwrap_or(self.interface_addr),
                    bind_port.unwrap_or(0),
                ),
            },
            SockKind::Udp => (
                self.interface_addr,
                sockets.get::<udp::Socket>(handle).endpoint().port,
            ),
            // No port concept for a raw ICMP socket -- 0, like an
            // unconnected/unbound TCP fd's own fallback above.
            SockKind::Icmp => (self.interface_addr, 0),
        };
        self.write_sockaddr_out(addr_out, len_ptr, ip, port);
        0
    }

    fn do_getpeername(&mut self, task: u32, fd: i32, addr_out: u32, len_ptr: u32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_getpeername_host(task, fd, addr_out, len_ptr);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let slot = self.fds[idx].as_ref().unwrap();
        let (ip, port) = match slot.kind {
            SockKind::Tcp => {
                let sockets = self.sockets.as_ref().expect("init() has run");
                ipv4_of(sockets.get::<tcp::Socket>(slot.socket).remote_endpoint())
            }
            SockKind::Udp => match slot.udp_peer {
                Some(peer) => peer,
                None => {
                    self.set_errno(task, ENOTCONN);
                    return -1;
                }
            },
            // bsdsocktest's own raw ICMP usage never calls connect() --
            // no peer to report, same as an unconnected UDP fd above.
            SockKind::Icmp => {
                self.set_errno(task, ENOTCONN);
                return -1;
            }
        };
        if port == 0 {
            self.set_errno(task, ENOTCONN);
            return -1;
        }
        self.write_sockaddr_out(addr_out, len_ptr, ip, port);
        0
    }

    // do_getsockname's host-backend branch: a plain passthrough to
    // `sock_local_addr`. No `bind_port`/`bind_addr` fallback bookkeeping
    // is needed the way the smoltcp path above has to maintain (a real
    // host `getsockname()` already reports the wildcard 0.0.0.0:0 for a
    // never-bound socket, and the real bound address -- including
    // whatever real port the OS picked for a `bind()` to port `0` --
    // once one exists, live and always current with no caching needed).
    fn do_getsockname_host(&mut self, task: u32, fd: i32, addr_out: u32, len_ptr: u32) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let handle = self.host_fds[idx].as_ref().unwrap().handle;
        let mut buf = [0u8; 6];
        let rc = unsafe { sock_local_addr(handle, buf.as_mut_ptr() as i32) };
        if rc != 0 {
            self.set_errno(task, -rc);
            return -1;
        }
        let ip = Ipv4Address::new(buf[0], buf[1], buf[2], buf[3]);
        let port = u16::from_be_bytes([buf[4], buf[5]]);
        self.write_sockaddr_out(addr_out, len_ptr, ip, port);
        0
    }

    // do_getpeername's host-backend branch: a plain passthrough to
    // `sock_peer_addr`, which itself reports the real OS errno (normally
    // `ENOTCONN`, via `sock_peer_addr`'s own `translate_errno`) for an
    // unconnected socket -- no separate `udp_peer`-style bookkeeping or
    // port-0 special case needed, unlike the smoltcp path above.
    fn do_getpeername_host(&mut self, task: u32, fd: i32, addr_out: u32, len_ptr: u32) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let handle = self.host_fds[idx].as_ref().unwrap().handle;
        let mut buf = [0u8; 6];
        let rc = unsafe { sock_peer_addr(handle, buf.as_mut_ptr() as i32) };
        if rc != 0 {
            self.set_errno(task, -rc);
            return -1;
        }
        let ip = Ipv4Address::new(buf[0], buf[1], buf[2], buf[3]);
        let port = u16::from_be_bytes([buf[4], buf[5]]);
        self.write_sockaddr_out(addr_out, len_ptr, ip, port);
        0
    }

    // Dup2Socket(fd, newfd): the "any new fd" form (newfd < 0) is the only
    // one load-bearing for conformance (bsdsocktest's own test_transfer.c
    // accepts a plain -1 for the specific-target form too, scoring it a
    // pass either way -- see PROPOSAL.md's Phase 4 scope decisions), but
    // the specific-target form gets a real implementation anyway: closes
    // whatever was already at that index first (real dup2() semantics),
    // same as a specific target being a genuinely different, currently-
    // open fd from `fd` itself -- `newfd == fd` is a real no-op success,
    // handled before that close-then-recreate path so it can't drop the
    // very socket it's supposed to be duplicating.
    fn do_dup2socket(&mut self, task: u32, fd: i32, newfd: i32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_dup2socket_host(task, fd, newfd);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        let target_idx = if newfd >= 0 {
            let Some(target_idx) = usize::try_from(newfd - 1).ok().filter(|&i| i < MAX_FDS) else {
                self.set_errno(task, EINVAL);
                return -1;
            };
            // dup2(fd, fd) is a real no-op success (the same fd, nothing
            // to close or replace) -- worth its own check rather than
            // letting the general path below close-then-recreate it,
            // since do_close's own refcount logic would drop this to
            // zero references and actually remove the underlying socket
            // out from under the still-live `fd` if it were the last
            // alias.
            if target_idx == idx {
                return newfd;
            }
            // Real dup2() semantics: an already-open target gets closed
            // first (dropping only *that* fd's own reference -- the
            // underlying socket stays alive if some other fd still
            // aliases it, same `Rc<()>` refcount `do_close` already
            // uses for the `-1`/"any new fd" form). Checks `host_fds` too
            // (via the generic `do_close` dispatcher, not this table's
            // own removal): the target fd number could currently belong
            // to either table.
            if self.fds[target_idx].is_some() || self.host_fds[target_idx].is_some() {
                self.do_close(task, (target_idx + 1) as i32);
            }
            target_idx
        } else {
            let Some(target_idx) =
                (0..MAX_FDS).find(|&i| self.fds[i].is_none() && self.host_fds[i].is_none())
            else {
                return -1;
            };
            target_idx
        };
        self.fds[target_idx] = Some(self.alias_fd_slot(idx));
        (target_idx + 1) as i32
    }

    // do_dup2socket's host-backend branch: real dup2() semantics via a
    // real `sock_dup` (see that import's own doc comment in
    // src/wasmboard.rs for why no manual refcounting is needed here the
    // way `alias_fd_slot`'s own `Rc<()>` is for the smoltcp path) --
    // mirrors `do_dup2socket`'s own shape exactly, just producing a
    // fresh *handle* per alias instead of a fresh fd aliasing the same
    // handle.
    fn do_dup2socket_host(&mut self, task: u32, fd: i32, newfd: i32) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let target_idx = if newfd >= 0 {
            let Some(target_idx) = usize::try_from(newfd - 1).ok().filter(|&i| i < MAX_FDS) else {
                self.set_errno(task, EINVAL);
                return -1;
            };
            if target_idx == idx {
                return newfd;
            }
            if self.fds[target_idx].is_some() || self.host_fds[target_idx].is_some() {
                self.do_close(task, (target_idx + 1) as i32);
            }
            target_idx
        } else {
            let Some(target_idx) =
                (0..MAX_FDS).find(|&i| self.fds[i].is_none() && self.host_fds[i].is_none())
            else {
                return -1;
            };
            target_idx
        };
        let slot = *self.host_fds[idx].as_ref().unwrap();
        let new_handle = unsafe { sock_dup(slot.handle) };
        if new_handle < 0 {
            self.set_errno(task, -new_handle);
            return -1;
        }
        self.host_fds[target_idx] = Some(HostFdSlot {
            handle: new_handle,
            kind: slot.kind,
            nonblocking: false,
            connect_started: slot.connect_started,
            is_listener: slot.is_listener,
            opts: slot.opts,
        });
        (target_idx + 1) as i32
    }

    // Builds a new FdSlot aliasing fd-table slot `idx`'s own underlying
    // socket (shared via `Rc<()>` refcount, so it isn't actually closed
    // until every alias is -- see `FdSlot::refcount`'s own doc comment).
    // Shared by do_dup2socket (the "any new fd"/specific-target forms)
    // and do_release_copy_of_socket (which needs the exact same "new
    // handle, same underlying socket" construction for the pool copy it
    // inserts while leaving the original fd untouched).
    fn alias_fd_slot(&self, idx: usize) -> FdSlot {
        let slot = self.fds[idx].as_ref().unwrap();
        FdSlot {
            kind: slot.kind,
            socket: slot.socket,
            refcount: slot.refcount.clone(),
            nonblocking: false,
            bind_port: slot.bind_port,
            bind_addr: slot.bind_addr,
            is_listener: slot.is_listener,
            udp_peer: slot.udp_peer,
            connect_started: slot.connect_started,
            was_established: slot.was_established,
            shutdown_by_us: slot.shutdown_by_us,
            opts: slot.opts,
        }
    }

    // Resolves a ReleaseSocket()/ReleaseCopyOfSocket() `id` argument to
    // the real pool key: a non-negative `id` is used directly (the caller
    // and whichever process later calls ObtainSocket() must agree on it
    // out of band), while `UNIQUE_ID` (-1, AmiTCP's own sentinel) asks
    // this library to assign a fresh one and hand it back -- avoiding
    // collisions is the whole point, so this actually checks the pool
    // rather than just incrementing blindly.
    fn resolve_pool_id(&mut self, id: i32) -> i32 {
        if id >= 0 {
            return id;
        }
        loop {
            let candidate = self.next_pool_id;
            self.next_pool_id = self.next_pool_id.wrapping_add(1).max(0);
            // Both pools share one `id` namespace from the guest's own
            // point of view (ObtainSocket() doesn't say which pool to
            // look in), so a freshly assigned id must be unused in
            // either.
            if !self.socket_pool.contains_key(&candidate)
                && !self.host_socket_pool.contains_key(&candidate)
            {
                return candidate;
            }
        }
    }

    // ReleaseSocket(sock, id): moves `sock` out of the caller's own
    // fd table into the library-wide shared pool (`Board::socket_pool`,
    // "library-wide" here just means "this Board's own state" -- this
    // project has no real separate-process concept, only separate
    // calling tasks sharing one fd table already, so the pool is simply
    // a second table keyed by `id` instead of a plain fd number). `sock`
    // becomes invalid in the caller's context immediately, same as a
    // real CloseSocket() -- reuses do_close's own stale-wait cleanup
    // for exactly that reason, just without actually destroying the
    // underlying socket the way do_close does.
    fn do_release_socket(&mut self, task: u32, fd: i32, id: i32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_release_socket_host(fd, id);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, EBADF);
            return -1;
        };
        let key = self.resolve_pool_id(id);
        let slot = self.fds[idx].take().unwrap();
        self.socket_pool.insert(key, slot);
        self.scrub_stale_waits(fd);
        key
    }

    // do_release_socket's host-backend branch: pure Rust-side data
    // movement, no `sock_*` import needed at all -- the host handle
    // itself doesn't change, only which table (`host_fds` ->
    // `host_socket_pool`) owns the `HostFdSlot` struct that names it.
    fn do_release_socket_host(&mut self, fd: i32, id: i32) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let key = self.resolve_pool_id(id);
        let slot = self.host_fds[idx].take().unwrap();
        self.host_socket_pool.insert(key, slot);
        self.scrub_stale_waits(fd);
        key
    }

    // ReleaseCopyOfSocket(sock, id): same pool insertion as
    // do_release_socket, but `sock` stays valid and usable in the
    // caller's own fd table afterward -- so this inserts an *alias*
    // (`alias_fd_slot`, the same underlying-socket-sharing construction
    // do_dup2socket uses) into the pool instead of moving the original.
    fn do_release_copy_of_socket(&mut self, task: u32, fd: i32, id: i32) -> i32 {
        if self.host_fd_index(fd).is_some() {
            return self.do_release_copy_of_socket_host(task, fd, id);
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, EBADF);
            return -1;
        };
        let key = self.resolve_pool_id(id);
        let alias = self.alias_fd_slot(idx);
        self.socket_pool.insert(key, alias);
        key
    }

    // do_release_copy_of_socket's host-backend branch: unlike
    // do_release_socket_host, this genuinely needs `sock_dup` (a real
    // `dup(2)`) -- the original fd must keep working *and* the pool
    // needs an independently valid, independently closeable copy.
    fn do_release_copy_of_socket_host(&mut self, task: u32, fd: i32, id: i32) -> i32 {
        let idx = self
            .host_fd_index(fd)
            .expect("caller already checked host_fd_index");
        let slot = *self.host_fds[idx].as_ref().unwrap();
        let new_handle = unsafe { sock_dup(slot.handle) };
        if new_handle < 0 {
            self.set_errno(task, -new_handle);
            return -1;
        }
        let key = self.resolve_pool_id(id);
        self.host_socket_pool.insert(
            key,
            HostFdSlot {
                handle: new_handle,
                kind: slot.kind,
                nonblocking: false,
                connect_started: slot.connect_started,
                is_listener: slot.is_listener,
                opts: slot.opts,
            },
        );
        key
    }

    // ObtainSocket(id, domain, type, protocol): the other half of the
    // shared-pool mechanism -- looks `id` up in `socket_pool` (inserted
    // there by an earlier ReleaseSocket()/ReleaseCopyOfSocket() call,
    // possibly from a different task), checks `domain`/`type`/`protocol`
    // match what was actually released (a safety check, not guesswork:
    // `domain` is always AF_INET in this project, `type` is derived from
    // the pooled socket's own `SockKind`, and `protocol == 0` matches
    // any protocol per this LVO's own documented "common usage"), and if
    // everything matches, moves it into a fresh slot in the caller's own
    // fd table. Removes the entry from the pool either way it's found --
    // a given pooled socket can only be obtained once, matching real
    // AmiTCP semantics.
    fn do_obtain_socket(
        &mut self,
        task: u32,
        id: i32,
        domain: i32,
        type_: i32,
        protocol: i32,
    ) -> i32 {
        if domain != AF_INET {
            self.set_errno(task, EINVAL);
            return -1;
        }
        // The two pools share one `id` namespace from the guest's own
        // point of view (see `resolve_pool_id`'s own comment) -- check
        // `host_socket_pool` first since it's the cheaper lookup, and
        // route there if that's where the id actually is.
        if self.host_socket_pool.contains_key(&id) {
            return self.do_obtain_socket_host(task, id, type_, protocol);
        }
        let Some(slot) = self.socket_pool.get(&id) else {
            self.set_errno(task, EINVAL);
            return -1;
        };
        let expected_type = match slot.kind {
            SockKind::Tcp => SOCK_STREAM,
            SockKind::Udp => SOCK_DGRAM,
            SockKind::Icmp => SOCK_RAW,
        };
        let protocol_ok = protocol == 0
            || match slot.kind {
                SockKind::Icmp => protocol == IPPROTO_ICMP,
                SockKind::Tcp => protocol == IPPROTO_TCP,
                SockKind::Udp => true,
            };
        if type_ != expected_type || !protocol_ok {
            self.set_errno(task, EINVAL);
            return -1;
        }
        let Some(target_idx) =
            (0..MAX_FDS).find(|&i| self.fds[i].is_none() && self.host_fds[i].is_none())
        else {
            // No free fd for it -- same EMFILE do_accept's own identical
            // case reports (this used to report EINVAL here instead, the
            // wrong errno for "no descriptor slots left" and inconsistent
            // with do_accept's own case besides).
            self.set_errno(task, EMFILE);
            return -1;
        };
        self.fds[target_idx] = self.socket_pool.remove(&id);
        (target_idx + 1) as i32
    }

    // do_obtain_socket's host-backend branch: same `type_`/`protocol`
    // validation against `HostFdSlot::kind` (only `Tcp`/`Udp` are ever
    // pooled here -- `do_socket_host` never creates an ICMP one), then
    // pure Rust-side data movement into a fresh `host_fds` slot, no
    // `sock_*` import needed (mirrors `do_release_socket_host`'s own
    // reasoning: the host handle itself doesn't change).
    fn do_obtain_socket_host(&mut self, task: u32, id: i32, type_: i32, protocol: i32) -> i32 {
        let slot = *self
            .host_socket_pool
            .get(&id)
            .expect("caller already checked host_socket_pool");
        let expected_type = match slot.kind {
            SockKind::Tcp => SOCK_STREAM,
            SockKind::Udp => SOCK_DGRAM,
            SockKind::Icmp => unreachable!("do_socket_host only ever creates Tcp/Udp"),
        };
        let protocol_ok = protocol == 0
            || match slot.kind {
                SockKind::Tcp => protocol == IPPROTO_TCP,
                SockKind::Udp => true,
                SockKind::Icmp => unreachable!("do_socket_host only ever creates Tcp/Udp"),
            };
        if type_ != expected_type || !protocol_ok {
            self.set_errno(task, EINVAL);
            return -1;
        }
        let Some(target_idx) =
            (0..MAX_FDS).find(|&i| self.fds[i].is_none() && self.host_fds[i].is_none())
        else {
            self.set_errno(task, EMFILE);
            return -1;
        };
        self.host_fds[target_idx] = self.host_socket_pool.remove(&id);
        (target_idx + 1) as i32
    }

    // Inet_NtoA(addr): formats a dotted-quad string into the guest's own
    // LIB_INETBUF scratch buffer (the trampoline already knows and
    // returns that address -- see entry.s's _hs_inet_ntoa -- so this just
    // needs to fill it in). `addr`'s bytes, taken MSB-first, are already
    // the address's octets in order: the RPC path preserves the guest's
    // original big-endian (network) byte order bit-for-bit, and 68k is
    // itself big-endian, so no ntohl()-equivalent conversion is needed
    // anywhere in this file's Inet_*/inet_* functions.
    fn do_inet_ntoa(&mut self, _task: u32, addr: u32, bufaddr: u32) -> i32 {
        let [a, b, c, d] = addr.to_be_bytes();
        let s = format!("{a}.{b}.{c}.{d}\0");
        unsafe { dma_write(bufaddr as i32, s.as_ptr() as i32, s.len() as i32) };
        0
    }

    // inet_addr(str): strict "a.b.c.d" (exactly 4 decimal octets), -1
    // (INADDR_NONE, 0xffffffff) if unparsable -- the same bit pattern a
    // legitimate 255.255.255.255 also produces, a real, documented
    // ambiguity in the BSD API itself, not a bug here.
    fn do_inet_addr(&mut self, _task: u32, straddr: u32) -> i32 {
        match self.parse_dotted_quad_at(straddr) {
            Some(v) => v as i32,
            None => -1,
        }
    }

    // Inet_LnaOf(addr): the classful host part (standard BSD algorithm,
    // unchanged since 4.2BSD -- general knowledge, not confirmed against
    // a local header, flagged as such per this project's own convention
    // for that distinction).
    fn do_inet_lnaof(&mut self, _task: u32, addr: u32) -> i32 {
        let host = if addr & 0x8000_0000 == 0 {
            addr & 0x00FF_FFFF // class A
        } else if addr & 0xC000_0000 == 0x8000_0000 {
            addr & 0x0000_FFFF // class B
        } else {
            addr & 0x0000_00FF // class C
        };
        host as i32
    }

    // Inet_NetOf(addr): the classful network part, complementing LnaOf.
    fn do_inet_netof(&mut self, _task: u32, addr: u32) -> i32 {
        let net = if addr & 0x8000_0000 == 0 {
            (addr & 0xFF00_0000) >> 24 // class A
        } else if addr & 0xC000_0000 == 0x8000_0000 {
            (addr & 0xFFFF_0000) >> 16 // class B
        } else {
            (addr & 0xFFFF_FF00) >> 8 // class C
        };
        net as i32
    }

    // Inet_MakeAddr(net, host): recombines a network/host pair, classful
    // on the magnitude of `net` (standard BSD algorithm) -- round-trips
    // with LnaOf/NetOf by construction.
    fn do_inet_makeaddr(&mut self, _task: u32, net: u32, host: u32) -> i32 {
        let addr = if net < 128 {
            (net << 24) | (host & 0x00FF_FFFF)
        } else if net < 65536 {
            (net << 16) | (host & 0x0000_FFFF)
        } else {
            (net << 8) | (host & 0x0000_00FF)
        };
        addr as i32
    }

    // inet_network(str): same strict dotted-quad parse as inet_addr --
    // the "host byte order" result it returns is bit-for-bit identical to
    // inet_addr's "network byte order" one on this big-endian (m68k)
    // guest, so both share `parse_dotted_quad_at`.
    fn do_inet_network(&mut self, _task: u32, straddr: u32) -> i32 {
        match self.parse_dotted_quad_at(straddr) {
            Some(v) => v as i32,
            None => -1,
        }
    }

    // getdtablesize(): the plugin's MAX_FDS is the single source of truth
    // for the fd table size (bsdsocktest's own getdtablesize_default test
    // just checks `>= 64`, see PROPOSAL.md's Phase 4 scope decisions).
    fn do_getdtablesize(&self) -> i32 {
        self.reported_dtablesize
    }

    // gethostname(name, namelen): unlike gethostbyname (a real DNS lookup),
    // this just reports a fixed, configurable string -- `[config] hostname
    // = "..."` in the manifest (see src/hostsocket.rs), same
    // config_get_string pattern init()'s own dns_server uses, defaulting
    // to "amiga" if unset. Never fails once dispatched (a real caller
    // passing a null buffer or non-positive length is the guest ROM's own
    // problem, not something the RPC layer can usefully diagnose) --
    // truncates to `namelen` if the hostname doesn't fit, silently and
    // without a guaranteed trailing NUL in that case, matching real BSD
    // gethostname()'s own permissive truncation behavior (bsdsocktest's
    // own small-buffer test accepts either a truncated write or a distinct
    // error return, so there's no single "correct" choice to match here).
    fn do_gethostname(&mut self, _task: u32, name_addr: u32, namelen: i32) -> i32 {
        if name_addr == 0 || namelen <= 0 {
            return 0;
        }
        let mut bytes = config_get_string("hostname")
            .unwrap_or_else(|| "amiga".to_string())
            .into_bytes();
        bytes.push(0);
        let n = bytes.len().min(namelen as usize);
        unsafe { dma_write(name_addr as i32, bytes.as_ptr() as i32, n as i32) };
        0
    }

    // gethostid(): real BSD systems have historically returned the
    // primary interface's own IPv4 address here (there's no separate
    // "host ID" concept this project tracks) -- reusing `interface_addr`
    // both keeps this consistent with getsockname()'s own default address
    // and guarantees bsdsocktest's own "non-zero" check always holds under
    // the default INTERFACE_ADDR (10.0.2.15 is never all-zero).
    fn do_gethostid(&mut self, _task: u32) -> i32 {
        u32::from_be_bytes(self.interface_addr.octets().try_into().unwrap()) as i32
    }

    // gethostbyname(name): forward DNS (A records only) via smoltcp's
    // socket-dns. `name_addr` is the guest's NUL-terminated hostname
    // string (a0, matching inet_addr's argument shape); `buf_addr` is the
    // guest's own LIB_HOSTENTBUF scratch area (arg2, the same "trampoline
    // already knows this address" pattern Inet_NtoA uses for LIB_INETBUF)
    // -- the whole marshaled struct hostent (header + h_aliases/
    // h_addr_list arrays + address bytes + the name string itself) is
    // written there, and the trampoline returns that same address (or
    // NULL on failure) instead of round-tripping a pointer through
    // REG_RESULT.
    //
    // A DNS round trip takes real network time (seconds, not the
    // microsecond scale everything else in this file resolves at), so
    // this follows the same start-then-poll shape as do_connect/
    // do_accept: the first call starts a query and returns RES_PENDING;
    // every retry (the guest's _ring_doorbell_blocking loop re-dispatching
    // this same call) re-checks it via dns_queries[task] until smoltcp's
    // own socket reports it done.
    //
    // Real bsdsocket.library failures here set h_errno, not errno; this
    // project has never wired up h_errno at all (no LVO for it exists
    // yet), so a failed lookup just returns NULL with nothing else
    // observable -- a known gap, not a silent wrong answer, until a real
    // consumer needs h_errno.
    fn do_gethostbyname(&mut self, task: u32, name_addr: u32, buf_addr: u32) -> i32 {
        // Unlike every other blocking call here, the guest only calls
        // CALL_REGISTER_WAIT *once* per operation (see entry.s's
        // _ring_doorbell_blocking: once it holds an allocated signal, a
        // later RES_PENDING skips straight back to Wait() on that same
        // signal, trusting the plugin to remember it and signal it again
        // once ready -- exactly what process_waiters' persistent
        // `self.waiters` list does for Connect/Recv/Accept/Select by
        // re-checking live socket state every tick without ever being
        // asked to). A first version of this function returned
        // RES_PENDING here without anything backing that promise, so the
        // second wait cycle had no waiter left to ever wake it -- a
        // genuine deadlock the end-to-end test rig caught (every earlier
        // blocking call's *own* readiness check is a cheap, repeatable,
        // non-consuming read of live socket state, so this class of bug
        // had no earlier LVO to surface on). The fix: process_waiters
        // does the real (consuming) dns::Socket::get_query_result() check
        // itself, on every tick, and caches the outcome in
        // dns_results -- this function then only ever needs to consult
        // that cache, never the query handle directly.
        if let Some(outcome) = self.dns_results.remove(&task) {
            self.dns_queries.remove(&task);
            self.host_resolve_jobs.remove(&task);
            return match outcome {
                DnsOutcome::Failed => {
                    self.set_herrno(task, HOST_NOT_FOUND);
                    -1
                }
                DnsOutcome::Ok(addrs) => {
                    let Some(name) = self.read_c_string(name_addr, HOSTENT_NAME_CAP - 1) else {
                        return -1;
                    };
                    self.set_herrno(task, 0);
                    self.write_hostent(buf_addr, &name, &addrs)
                }
            };
        }
        if self.dns_queries.contains_key(&task) || self.host_resolve_jobs.contains_key(&task) {
            // Still in flight -- process_waiters hasn't seen it complete
            // yet. No last_pending here: the guest won't call
            // CALL_REGISTER_WAIT again for this cycle (see above), and
            // the waiter it already registered is what's keeping this
            // alive.
            return RES_PENDING;
        }

        let Some(name) = self.read_c_string(name_addr, HOSTENT_NAME_CAP - 1) else {
            return -1;
        };
        // "localhost" resolves locally on every real system, hosts file
        // or not -- it's a resolver-level special case (glibc, BSD libc,
        // and AmigaOS stacks alike all special-case it, precisely so it
        // keeps working with no hosts file/network config at all), not
        // something that should ever go out as a real DNS query. This
        // project has no hosts-file concept and no DNS server would
        // actually know "localhost" either, so without this,
        // gethostbyname("localhost") always failed -- found running
        // bsdsocktest's own test for exactly this. Case-insensitive to
        // match real resolvers (DNS names are case-insensitive). Applies
        // regardless of resolver strategy: real host resolvers special-case
        // it too, so there's no reason to spend a background thread on it.
        if name.eq_ignore_ascii_case("localhost") {
            self.set_herrno(task, 0);
            return self.write_hostent(
                buf_addr,
                &name,
                &[IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 1))],
            );
        }

        if self.resolver_host {
            // `resolver = "host"` (see init()'s own comment): ask
            // Copperline's own process to resolve via its OS resolver
            // instead of this project's own DNS-over-`net` query below.
            let id = unsafe { resolve_start(name.as_ptr() as i32, name.len() as i32) };
            return if id >= 0 {
                self.host_resolve_jobs.insert(task, id);
                self.last_pending.insert(task, WaitKind::HostResolve);
                RES_PENDING
            } else {
                // The host couldn't even start the lookup (e.g. its own
                // outstanding-request budget is exhausted) -- a resolver-
                // level "couldn't ask", matching the smoltcp branch's own
                // TRY_AGAIN below for the equivalent failure there.
                self.set_herrno(task, TRY_AGAIN);
                -1
            };
        }

        let dns_handle = self.dns_socket.expect("init() has run");
        let iface = self.iface.as_mut().expect("init() has run");
        let cx = iface.context();
        let sockets = self.sockets.as_mut().expect("init() has run");
        let socket = sockets.get_mut::<dns::Socket>(dns_handle);
        match socket.start_query(cx, &name, DnsQueryType::A) {
            Ok(handle) => {
                self.dns_queries.insert(task, handle);
                self.last_pending.insert(task, WaitKind::Dns);
                RES_PENDING
            }
            // smoltcp couldn't even start the query (e.g. its own DNS
            // socket is already busy with another one) -- a resolver-
            // level "couldn't ask", not "asked and got a negative
            // answer/timeout" (that's DnsOutcome::Failed above), so
            // TRY_AGAIN fits better than HOST_NOT_FOUND here.
            Err(_) => {
                self.set_herrno(task, TRY_AGAIN);
                -1
            }
        }
    }

    // gethostbyaddr(addr, len, type): reverse (PTR) DNS lookup. Unlike
    // gethostbyname, this can't reuse smoltcp's own `dns::Socket` at all
    // -- that type's `start_query` only accepts A/AAAA/CNAME-shaped
    // `DnsQueryType`s query-side, and `get_query_result` is hard-typed to
    // return `Vec<IpAddress>`, with no way to get a domain-name answer
    // back out even if a PTR query were accepted. So this speaks DNS
    // wire format directly over a plain UDP socket (`ptr_socket`) instead:
    // builds a real query packet by hand (`wire::dns::Repr`/`Question`,
    // the same public primitives `dns::Socket` itself is built on, just
    // used one layer down), sends it to the configured resolver, and
    // parses the raw response for a PTR answer -- `parse_ptr_response`'s
    // own comment has the parsing details, including why a compression-
    // aware name decoder is needed. Same start-then-poll shape as
    // gethostbyname (`RES_PENDING` + `CALL_REGISTER_WAIT`), but doesn't
    // need `dns_results`' consuming-result cache trick: `udp::Socket::
    // can_recv()` is safely re-checkable, unlike `get_query_result()`
    // (see `WaitKind::Ptr`'s own comment).
    fn do_gethostbyaddr(
        &mut self,
        task: u32,
        addr_ptr: u32,
        len: i32,
        type_: i32,
        buf_addr: u32,
    ) -> i32 {
        if self.ptr_pending.as_ref().is_some_and(|p| p.task == task) {
            return self.poll_ptr_query(task);
        }
        if type_ != AF_INET || len < 4 || addr_ptr == 0 {
            self.set_herrno(task, HOST_NOT_FOUND);
            return -1;
        }
        let mut raw = [0u8; 4];
        // Safety: reading the guest-supplied raw address bytes (a real
        // `struct in_addr`, not a string -- see this LVO's own calling
        // convention, confirmed against the guest toolchain, not guessed).
        unsafe { dma_read(addr_ptr as i32, raw.as_mut_ptr() as i32, 4) };
        let orig_addr = Ipv4Address::new(raw[0], raw[1], raw[2], raw[3]);

        let o = orig_addr.octets();
        let name = format!("{}.{}.{}.{}.in-addr.arpa", o[3], o[2], o[1], o[0]);
        let qname = encode_dns_name(&name);
        let question = DnsQuestion {
            name: &qname,
            type_: DnsQueryType::Unknown(12), // PTR (RFC 1035 §3.2.2) -- no
                                              // named variant, see this
                                              // file's own DnsQueryType
                                              // import comment.
        };
        // A transaction ID unique enough to reject a stray old reply --
        // this project has no real RNG (wasm32-unknown-unknown, no OS
        // entropy source), so `self.micros` (already used the same
        // truncated-to-what's-available way `send_progress`'s own retry
        // bookkeeping doesn't need real randomness either) stands in.
        let transaction_id = self.micros as u16;
        let repr = DnsRepr {
            transaction_id,
            opcode: DnsOpcode::Query,
            flags: DnsFlags::RECURSION_DESIRED,
            question,
        };
        let mut buf = vec![0u8; repr.buffer_len()];
        {
            let mut packet = DnsPacket::new_unchecked(&mut buf[..]);
            repr.emit(&mut packet);
        }

        let handle = self.ptr_socket.expect("init() has run");
        let dns_server = self.dns_server_addr;
        let sockets = self.sockets.as_mut().expect("init() has run");
        let socket = sockets.get_mut::<udp::Socket>(handle);
        // Drain any stale datagram left over from an earlier query (e.g.
        // a slow reply that arrived after this project's own timeout had
        // already given up on it) -- otherwise the very next recv_slice
        // below could consume that leftover instead of this query's own
        // real answer.
        while socket.can_recv() {
            let _ = socket.recv();
        }
        if socket
            .send_slice(&buf, (IpAddress::Ipv4(dns_server), 53))
            .is_err()
        {
            self.set_herrno(task, TRY_AGAIN);
            return -1;
        }

        // 5 real seconds -- generous for a single UDP round trip, but
        // bounded: under `net = "loopback"` (no real DNS server behind
        // it at all) or a resolver that simply doesn't answer PTR
        // queries, nothing would ever complete this otherwise.
        let deadline = self.micros + (5.0 * CCK_HZ) as i64;
        self.ptr_pending = Some(PtrQuery {
            task,
            transaction_id,
            buf_addr,
            orig_addr,
            deadline,
        });
        self.last_pending.insert(task, WaitKind::Ptr);
        RES_PENDING
    }

    // The retry half of do_gethostbyaddr, reached once `self.ptr_pending`
    // already holds this task's own in-flight query (see that function's
    // own comment for the full design). Only ever called after
    // process_waiters has confirmed (via `WaitKind::Ptr`) that either a
    // response is ready or the deadline has passed, but re-checks both
    // itself anyway -- CALL_REGISTER_WAIT's own blocking-retry protocol
    // means this can in principle be reached before that, same as every
    // other RES_PENDING call here.
    fn poll_ptr_query(&mut self, task: u32) -> i32 {
        let pending = self.ptr_pending.take().expect("caller checked Some");
        let handle = self.ptr_socket.expect("init() has run");
        let sockets = self.sockets.as_mut().expect("init() has run");
        let socket = sockets.get_mut::<udp::Socket>(handle);

        if !socket.can_recv() {
            if self.micros >= pending.deadline {
                self.set_herrno(task, HOST_NOT_FOUND);
                return -1;
            }
            self.ptr_pending = Some(pending);
            self.last_pending.insert(task, WaitKind::Ptr);
            return RES_PENDING;
        }

        let mut raw = [0u8; 512];
        let (n, _meta) = match socket.recv_slice(&mut raw) {
            Ok(v) => v,
            Err(_) => {
                self.set_herrno(task, TRY_AGAIN);
                return -1;
            }
        };
        match parse_ptr_response(&raw[..n], pending.transaction_id) {
            Some(name) => {
                self.set_herrno(task, 0);
                self.write_hostent(
                    pending.buf_addr,
                    &name,
                    &[IpAddress::Ipv4(pending.orig_addr)],
                )
            }
            None => {
                self.set_herrno(task, HOST_NOT_FOUND);
                -1
            }
        }
    }

    // Marshals a resolved (name, addrs) pair into the guest's
    // LIB_HOSTENTBUF scratch buffer as a real struct hostent, matching
    // HOSTENT_*_OFF's layout exactly. Always returns 0 -- the trampoline
    // itself decides NULL-vs-buf_addr from do_gethostbyname's own -1/0
    // result (see entry.s's _hs_gethostbyname), the same split Inet_NtoA
    // uses for its own buffer.
    fn write_hostent(&mut self, buf_addr: u32, name: &str, addrs: &[IpAddress]) -> i32 {
        let mut blob = vec![0u8; HOSTENT_BUF_LEN as usize];
        let addr_count = addrs.len().min(HOSTENT_MAX_ADDRS);

        blob[0..4].copy_from_slice(&(buf_addr + HOSTENT_NAME_OFF).to_be_bytes());
        blob[4..8].copy_from_slice(&(buf_addr + HOSTENT_ALIASES_OFF).to_be_bytes());
        blob[8..12].copy_from_slice(&2i32.to_be_bytes()); // h_addrtype = AF_INET
        blob[12..16].copy_from_slice(&4i32.to_be_bytes()); // h_length = 4 (IPv4)
        blob[16..20].copy_from_slice(&(buf_addr + HOSTENT_ADDR_LIST_OFF).to_be_bytes());
        // h_aliases[0] = NULL (empty alias list): already zeroed.

        for (i, addr) in addrs.iter().take(addr_count).enumerate() {
            // IpAddress has no variant but Ipv4 with only "proto-ipv4"
            // enabled (see plugin/Cargo.toml) -- irrefutable, not a
            // defensive match.
            let IpAddress::Ipv4(v4) = *addr;
            let entry_ptr = buf_addr + HOSTENT_ADDRS_OFF + (i as u32) * 4;
            let list_off = HOSTENT_ADDR_LIST_OFF as usize + i * 4;
            blob[list_off..list_off + 4].copy_from_slice(&entry_ptr.to_be_bytes());
            let addr_off = HOSTENT_ADDRS_OFF as usize + i * 4;
            blob[addr_off..addr_off + 4].copy_from_slice(&v4.octets());
        }
        // h_addr_list's NULL terminator (index addr_count) and any unused
        // trailing address slots: already zeroed.

        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len().min(HOSTENT_NAME_CAP - 1);
        let name_off = HOSTENT_NAME_OFF as usize;
        blob[name_off..name_off + name_len].copy_from_slice(&name_bytes[..name_len]);
        // Trailing NUL: already zeroed.

        unsafe { dma_write(buf_addr as i32, blob.as_ptr() as i32, blob.len() as i32) };
        0
    }

    // Marshals a (name, port, proto) SERVICES entry into the guest's
    // LIB_SERVENTBUF scratch buffer as a real struct servent, matching
    // SERVENT_*_OFF's layout exactly -- same shape as write_hostent.
    fn write_servent(&mut self, buf_addr: u32, name: &str, port: u16, proto: &str) -> i32 {
        let mut blob = vec![0u8; SERVENT_BUF_LEN as usize];
        blob[0..4].copy_from_slice(&(buf_addr + SERVENT_NAME_OFF).to_be_bytes());
        blob[4..8].copy_from_slice(&(buf_addr + SERVENT_ALIASES_OFF).to_be_bytes());
        // s_aliases[0] = NULL (empty alias list): already zeroed.
        blob[8..12].copy_from_slice(&(port as i32).to_be_bytes());
        blob[12..16].copy_from_slice(&(buf_addr + SERVENT_PROTO_OFF).to_be_bytes());

        let proto_bytes = proto.as_bytes();
        let proto_len = proto_bytes.len().min(SERVENT_PROTO_CAP - 1);
        let proto_off = SERVENT_PROTO_OFF as usize;
        blob[proto_off..proto_off + proto_len].copy_from_slice(&proto_bytes[..proto_len]);

        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len().min(SERVENT_NAME_CAP - 1);
        let name_off = SERVENT_NAME_OFF as usize;
        blob[name_off..name_off + name_len].copy_from_slice(&name_bytes[..name_len]);

        unsafe { dma_write(buf_addr as i32, blob.as_ptr() as i32, blob.len() as i32) };
        0
    }

    // Marshals a (name, number) PROTOCOLS entry into LIB_PROTOENTBUF as a
    // real struct protoent, matching PROTOENT_*_OFF's layout exactly.
    fn write_protoent(&mut self, buf_addr: u32, name: &str, proto: i32) -> i32 {
        let mut blob = vec![0u8; PROTOENT_BUF_LEN as usize];
        blob[0..4].copy_from_slice(&(buf_addr + PROTOENT_NAME_OFF).to_be_bytes());
        blob[4..8].copy_from_slice(&(buf_addr + PROTOENT_ALIASES_OFF).to_be_bytes());
        // p_aliases[0] = NULL (empty alias list): already zeroed.
        blob[8..12].copy_from_slice(&proto.to_be_bytes());

        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len().min(PROTOENT_NAME_CAP - 1);
        let name_off = PROTOENT_NAME_OFF as usize;
        blob[name_off..name_off + name_len].copy_from_slice(&name_bytes[..name_len]);

        unsafe { dma_write(buf_addr as i32, blob.as_ptr() as i32, blob.len() as i32) };
        0
    }

    // Marshals a (name, net) NETWORKS entry into LIB_NETENTBUF as a real
    // struct netent, matching NETENT_*_OFF's layout exactly.
    fn write_netent(&mut self, buf_addr: u32, name: &str, net: u32) -> i32 {
        let mut blob = vec![0u8; NETENT_BUF_LEN as usize];
        blob[0..4].copy_from_slice(&(buf_addr + NETENT_NAME_OFF).to_be_bytes());
        blob[4..8].copy_from_slice(&(buf_addr + NETENT_ALIASES_OFF).to_be_bytes());
        // n_aliases[0] = NULL (empty alias list): already zeroed.
        blob[8..12].copy_from_slice(&2i32.to_be_bytes()); // n_addrtype = AF_INET
        blob[12..16].copy_from_slice(&net.to_be_bytes());

        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len().min(NETENT_NAME_CAP - 1);
        let name_off = NETENT_NAME_OFF as usize;
        blob[name_off..name_off + name_len].copy_from_slice(&name_bytes[..name_len]);

        unsafe { dma_write(buf_addr as i32, blob.as_ptr() as i32, blob.len() as i32) };
        0
    }

    // getservbyname(name, proto): looks `name` up in SERVICES, optionally
    // constrained to a specific `proto` ("tcp"/"udp") -- a NULL `proto_addr`
    // (real callers do pass this) matches any protocol, taking the first
    // SERVICES entry for that name. Both comparisons are case-insensitive,
    // matching every real getservbyname() implementation (service/protocol
    // names are conventionally lowercase, but callers routinely pass mixed
    // case). Returns 0 on success (do_getservbyname's own -1/0 split is
    // what entry.s's _hs_getservbyname trampoline uses to decide
    // NULL-vs-buf_addr, matching every other write_*ent-backed LVO here),
    // -1 if nothing matched -- not an error, just "not found", so no
    // errno is set (real getservbyname() doesn't set one either).
    fn do_getservbyname(
        &mut self,
        _task: u32,
        name_addr: u32,
        proto_addr: u32,
        buf_addr: u32,
    ) -> i32 {
        let Some(name) = self.read_c_string(name_addr, SERVENT_NAME_CAP - 1) else {
            return -1;
        };
        let proto = if proto_addr == 0 {
            None
        } else {
            self.read_c_string(proto_addr, SERVENT_PROTO_CAP - 1)
        };
        let found = SERVICES.iter().find(|&&(n, _, p)| {
            n.eq_ignore_ascii_case(&name)
                && proto
                    .as_deref()
                    .is_none_or(|want| p.eq_ignore_ascii_case(want))
        });
        match found {
            Some(&(n, port, proto)) => self.write_servent(buf_addr, n, port, proto),
            None => -1,
        }
    }

    // getservbyport(port, proto): real callers pass htons(port) themselves
    // (e.g. bsdsocktest's own `getservbyport(htons(21), "tcp")`), but
    // network byte order *is* m68k's own big-endian byte order, so
    // htons() is a no-op on the guest -- the register still holds the
    // plain value 21, not a byte-swapped one. This RPC layer's own arg
    // marshaling (a big-endian `move.l` on the wire, reconstructed via
    // `i32::from_be_bytes` on this side, same as every other argument
    // here) then reproduces that same value with no further conversion
    // needed -- an earlier draft added an extra `u16::from_be` here
    // anyway, reasoning (wrongly) that it was undoing a swap that had
    // already happened; that silently double-swapped the port and made
    // every real lookup fail, caught immediately by this file's own
    // `getservbyport_finds_a_known_port_regardless_of_proto_filter` test.
    // `s_port` is written back through the identical plain value, which
    // is simultaneously "host byte order" and "network byte order" here
    // for the same reason -- exactly what real bsdsocket.library callers
    // expect back.
    fn do_getservbyport(&mut self, _task: u32, port: i32, proto_addr: u32, buf_addr: u32) -> i32 {
        let port = port as u16;
        let proto = if proto_addr == 0 {
            None
        } else {
            self.read_c_string(proto_addr, SERVENT_PROTO_CAP - 1)
        };
        let found = SERVICES.iter().find(|&&(_, p, proto_name)| {
            p == port
                && proto
                    .as_deref()
                    .is_none_or(|want| proto_name.eq_ignore_ascii_case(want))
        });
        match found {
            Some(&(n, port, proto)) => self.write_servent(buf_addr, n, port, proto),
            None => -1,
        }
    }

    // getprotobyname(name): looks `name` up in PROTOCOLS, case-insensitive.
    fn do_getprotobyname(&mut self, _task: u32, name_addr: u32, buf_addr: u32) -> i32 {
        let Some(name) = self.read_c_string(name_addr, PROTOENT_NAME_CAP - 1) else {
            return -1;
        };
        match PROTOCOLS
            .iter()
            .find(|&&(n, _)| n.eq_ignore_ascii_case(&name))
        {
            Some(&(n, proto)) => self.write_protoent(buf_addr, n, proto),
            None => -1,
        }
    }

    // getprotobynumber(proto): exact match against PROTOCOLS.
    fn do_getprotobynumber(&mut self, _task: u32, proto: i32, buf_addr: u32) -> i32 {
        match PROTOCOLS.iter().find(|&&(_, p)| p == proto) {
            Some(&(n, p)) => self.write_protoent(buf_addr, n, p),
            None => -1,
        }
    }

    // getnetbyname(name): looks `name` up in NETWORKS, case-insensitive.
    fn do_getnetbyname(&mut self, _task: u32, name_addr: u32, buf_addr: u32) -> i32 {
        let Some(name) = self.read_c_string(name_addr, NETENT_NAME_CAP - 1) else {
            return -1;
        };
        match NETWORKS
            .iter()
            .find(|&&(n, _)| n.eq_ignore_ascii_case(&name))
        {
            Some(&(n, net)) => self.write_netent(buf_addr, n, net),
            None => -1,
        }
    }

    // getnetbyaddr(net, type): `type` must be AF_INET (real getnetbyaddr()
    // rejects anything else the same way) -- `net` is a raw network number
    // (bsdsocktest's own `getnetbyaddr(127, AF_INET)`, not a packed
    // struct in_addr the way gethostbyaddr's `addr` is), compared directly
    // against NETWORKS.
    fn do_getnetbyaddr(&mut self, _task: u32, net: u32, type_: i32, buf_addr: u32) -> i32 {
        if type_ != AF_INET {
            return -1;
        }
        match NETWORKS.iter().find(|&&(_, n)| n == net) {
            Some(&(n, net)) => self.write_netent(buf_addr, n, net),
            None => -1,
        }
    }

    // Reads up to `max_len` bytes from `addr`, looking for a NUL
    // terminator -- the general-purpose version of parse_dotted_quad_at's
    // fixed 32-byte reader, for strings whose length isn't bounded that
    // tightly (hostnames, not dotted-quads).
    fn read_c_string(&self, addr: u32, max_len: usize) -> Option<String> {
        let mut raw = vec![0u8; max_len + 1];
        // Safety: reading a guest-supplied NUL-terminated string out of
        // Amiga memory, same as parse_dotted_quad_at.
        unsafe { dma_read(addr as i32, raw.as_mut_ptr() as i32, raw.len() as i32) };
        let nul = raw.iter().position(|&b| b == 0)?;
        std::str::from_utf8(&raw[..nul]).ok().map(String::from)
    }

    // Reads up to 31 bytes from `addr`, looks for a NUL terminator, and
    // hands the result to `parse_dotted_quad` -- shared by inet_addr/
    // inet_network, the only two Inet_*/inet_* functions that take a
    // string input.
    fn parse_dotted_quad_at(&self, addr: u32) -> Option<u32> {
        let mut raw = [0u8; 32];
        // Safety: reading a guest-supplied NUL-terminated string out of
        // Amiga memory; the fixed 32-byte cap is generous for any
        // legitimate dotted-quad ("255.255.255.255\0" is 16 bytes).
        unsafe { dma_read(addr as i32, raw.as_mut_ptr() as i32, 32) };
        let nul = raw.iter().position(|&b| b == 0)?;
        let s = std::str::from_utf8(&raw[..nul]).ok()?;
        parse_dotted_quad(s)
    }

    // IoctlSocket(FIONBIO/FIONREAD). FIONBIO's `argp` points at a LONG:
    // non-zero enables non-blocking mode for this fd. FIONREAD's `argp`
    // is an out-parameter: the number of bytes (TCP) or the next
    // datagram's size (UDP) available to read right now without
    // blocking, via smoltcp's own `recv_queue()` on either socket kind.
    fn do_ioctl_socket(&mut self, task: u32, fd: i32, request: u32, argp: u32) -> i32 {
        if let Some(idx) = self.host_fd_index(fd) {
            match request {
                FIONBIO => {
                    let mut raw = [0u8; 4];
                    unsafe { dma_read(argp as i32, raw.as_mut_ptr() as i32, 4) };
                    let nonblocking = i32::from_be_bytes(raw) != 0;
                    self.host_fds[idx].as_mut().unwrap().nonblocking = nonblocking;
                    return 0;
                }
                FIONREAD => {
                    let handle = self.host_fds[idx].as_ref().unwrap().handle;
                    let rc = unsafe { sock_nread(handle) };
                    if rc < 0 {
                        self.set_errno(task, -rc);
                        return -1;
                    }
                    let bytes = rc.to_be_bytes();
                    unsafe { dma_write(argp as i32, bytes.as_ptr() as i32, 4) };
                    return 0;
                }
                _ => {
                    self.set_errno(task, EINVAL);
                    return -1;
                }
            }
        }
        let Some(idx) = self.fd_index(fd) else {
            self.set_errno(task, ENOTSOCK);
            return -1;
        };
        match request {
            FIONBIO => {
                let mut raw = [0u8; 4];
                unsafe { dma_read(argp as i32, raw.as_mut_ptr() as i32, 4) };
                let nonblocking = i32::from_be_bytes(raw) != 0;
                self.fds[idx].as_mut().expect("checked above").nonblocking = nonblocking;
                0
            }
            FIONREAD => {
                let slot = self.fds[idx].as_ref().unwrap();
                let sockets = self.sockets.as_ref().expect("init() has run");
                let pending = match slot.kind {
                    SockKind::Tcp => sockets.get::<tcp::Socket>(slot.socket).recv_queue(),
                    SockKind::Udp => sockets.get::<udp::Socket>(slot.socket).recv_queue(),
                    SockKind::Icmp => sockets.get::<icmp::Socket>(slot.socket).recv_queue(),
                };
                let bytes = (pending as i32).to_be_bytes();
                unsafe { dma_write(argp as i32, bytes.as_ptr() as i32, 4) };
                0
            }
            _ => {
                self.set_errno(task, EINVAL);
                -1
            }
        }
    }

    fn do_set_errno_ptr(&mut self, task: u32, ptr: u32, size: i32) -> i32 {
        let entry = self.tasks.entry(task).or_default();
        entry.errno_ptr = if ptr == 0 {
            None
        } else {
            Some((ptr, size.max(0) as u16))
        };
        0
    }

    // Mirrors `do_set_errno_ptr` for h_errno -- only ever reached via
    // SocketBaseTagList's own SBTC_HERRNOLONGPTR SET (there's no
    // SetHErrnoPtr LVO), see `do_socketbasetaglist`.
    fn do_set_herrno_ptr(&mut self, task: u32, ptr: u32) {
        self.tasks.entry(task).or_default().herrno_ptr = if ptr == 0 { None } else { Some(ptr) };
    }

    // SocketBaseTagList(tags): walks a TagItem array (8 bytes/entry: BE
    // ti_Tag, BE ti_Data), TAG_DONE (0)-terminated. Real tags:
    // SBTC_ERRNOLONGPTR (SET and GET), SBTC_HERRNOLONGPTR (SET and GET),
    // SBTC_SIGEVENTMASK (SET and GET), SBTC_BREAKMASK (SET and GET), and
    // SBTC_DTABLESIZE (SET and GET). Every other tag (SBTC_LOGTAGPTR,
    // SBTC_FDCALLBACK, the *STRPTR family...) is silently skipped, same as
    // an unrecognized TagItem's ti_Tag is meant to be treated by
    // convention -- those back real features (syslog configuration, a
    // link-library fd-allocation callback) this project has deliberately
    // never wired up. TAG_MORE (chaining to a second array) isn't handled
    // either -- bsdsocktest never emits it, always a flat TAG_DONE-
    // terminated array.
    //
    // GET(REF) tags all share a shape real BSTC's own comments don't quite
    // spell out: `data` isn't the result, it's the *address to write the
    // result into* (a real `ULONG *`, matching SBTM_GETREF's own
    // ti_Data-is-a-pointer convention) -- `write_u32` below.
    //
    // SBTC_ERRNOLONGPTR's GET is a special case worth its own note: amitcp/
    // socketbasetags.h's own comment calls this tag family "SETTING
    // (only)", and this project's own Compatibility doc used to cite a
    // real stack (Roadshow) sharing that same GET-not-supported gap.
    // bsdsocktest's own test still calls GETREF on it and only checks the
    // result is non-null, though -- so rather than leave it a no-op
    // forever, this answers it the way a real implementation that *does*
    // support the GET would: hand back wherever errno already gets
    // written. If a task never registered its own pointer via an earlier
    // SET (SocketBaseTags or SetErrnoPtr), one gets registered now,
    // pointing at this library's own `LIB_ERRNO_SLOT` scratch LONG
    // (`errno_slot_addr`, entry.s's own comment) -- real guest RAM, so
    // `set_errno` keeps it live from here on, not just this one snapshot.
    // SBTC_HERRNOLONGPTR's SET/GET mirror ERRNOLONGPTR's exactly --
    // `do_gethostbyname`'s own failure/success paths call `set_herrno` the
    // same way every other failing call here calls `set_errno`, and
    // bsdsocktest's own `open_bsdsocket()` always registers its h_errno
    // pointer via SET before any DNS lookup ever runs (this project's own
    // GET path, and its `LIB_HERRNO_SLOT` fallback, mainly exist for test
    // 77's own direct GETREF check).
    fn do_socketbasetaglist(
        &mut self,
        task: u32,
        tags_addr: u32,
        errno_slot_addr: u32,
        herrno_slot_addr: u32,
    ) -> i32 {
        let write_u32 = |addr: u32, value: u32| {
            if addr == 0 {
                return;
            }
            let bytes = value.to_be_bytes();
            unsafe { dma_write(addr as i32, bytes.as_ptr() as i32, 4) };
        };
        let mut addr = tags_addr;
        while addr != 0 {
            let mut raw = [0u8; 8];
            unsafe { dma_read(addr as i32, raw.as_mut_ptr() as i32, 8) };
            let tag = u32::from_be_bytes(raw[0..4].try_into().unwrap());
            let data = u32::from_be_bytes(raw[4..8].try_into().unwrap());
            if tag == 0 {
                break; // TAG_DONE
            }
            match tag {
                SBTC_ERRNOLONGPTR_SET => {
                    self.do_set_errno_ptr(task, data, 4);
                }
                SBTC_ERRNOLONGPTR_GET => {
                    if self.tasks.get(&task).and_then(|t| t.errno_ptr).is_none() {
                        self.do_set_errno_ptr(task, errno_slot_addr, 4);
                    }
                    let ptr = self
                        .tasks
                        .get(&task)
                        .and_then(|t| t.errno_ptr)
                        .map_or(errno_slot_addr, |(p, _)| p);
                    write_u32(data, ptr);
                }
                SBTC_HERRNOLONGPTR_SET => self.do_set_herrno_ptr(task, data),
                SBTC_HERRNOLONGPTR_GET => {
                    if self.tasks.get(&task).and_then(|t| t.herrno_ptr).is_none() {
                        self.do_set_herrno_ptr(task, herrno_slot_addr);
                    }
                    let ptr = self
                        .tasks
                        .get(&task)
                        .and_then(|t| t.herrno_ptr)
                        .unwrap_or(herrno_slot_addr);
                    write_u32(data, ptr);
                }
                SBTC_BREAKMASK_SET => self.tasks.entry(task).or_default().break_mask = data,
                SBTC_BREAKMASK_GET => {
                    write_u32(data, self.tasks.get(&task).map_or(0, |t| t.break_mask));
                }
                SBTC_SIGEVENTMASK_SET => {
                    self.tasks.entry(task).or_default().sig_event_mask = data;
                }
                SBTC_SIGEVENTMASK_GET => {
                    write_u32(data, self.tasks.get(&task).map_or(0, |t| t.sig_event_mask));
                }
                SBTC_DTABLESIZE_SET => {
                    self.reported_dtablesize = self.reported_dtablesize.max(data as i32);
                }
                SBTC_DTABLESIZE_GET => write_u32(data, self.reported_dtablesize as u32),
                _ => {}
            }
            addr = addr.wrapping_add(8);
        }
        0
    }

    fn do_errno(&mut self, task: u32) -> i32 {
        self.tasks.get(&task).map_or(0, |t| t.last_errno)
    }

    fn read_fd_mask(&self, addr: u32) -> u32 {
        if addr == 0 {
            return 0;
        }
        let mut raw = [0u8; 4];
        unsafe { dma_read(addr as i32, raw.as_mut_ptr() as i32, 4) };
        u32::from_be_bytes(raw)
    }

    fn write_fd_mask(&self, addr: u32, mask: u32) {
        if addr == 0 {
            return;
        }
        let raw = mask.to_be_bytes();
        unsafe { dma_write(addr as i32, raw.as_ptr() as i32, 4) };
    }

    // Checks which fds named in `read_mask`/`write_mask` (bit N = fd N,
    // matching a real fd_set directly since our fds are already small
    // integers) are currently ready, bounded to `nfds`. `nfds` itself is
    // real select()/WaitSelect() semantics for "highest fd + 1" -- the
    // valid range to scan is fds strictly *below* it, `1..nfds`, not
    // `1..=nfds`. Used to be inclusive, which checked one fd too many
    // (`nfds` itself): a caller deliberately passing a too-low `nfds` to
    // exclude a specific fd (real programs do this, and bsdsocktest's own
    // test explicitly checks for it) would still see that fd checked
    // anyway. Also bounded to `SELECT_MAX_FD` (32), independent of `nfds`
    // and `MAX_FDS` -- see that const's own comment for why fd 32+ can't be
    // represented in this wire format's single-ULONG bitmask at all.
    // `WaitSelect()`'s `exceptfds` is *not* handled by this function --
    // real TCP urgent (out-of-band) data pending is the only condition a
    // real `exceptfds` would report, and there is no reliable way to
    // detect it non-consumingly on a host-backed fd either: `poll(2)`'s
    // `POLLPRI` was tried (`sock_poll` upgraded to surface it) and found
    // unreliable on this project's own macOS dev host -- it never fired
    // for a genuine `MSG_OOB` send in isolation, but *did* fire spuriously
    // coincident with an unrelated `POLLHUP` (peer socket closing). Rather
    // than ship a signal that fires on the wrong condition, `do_wait_select`
    // always reports `exceptfds` empty, matching the smoltcp path's own
    // permanent gap here (no urgent-pointer support in `socket::tcp` at
    // all) -- see `do_send`'s own `MSG_OOB` comment. `recv(MSG_OOB)` itself
    // (`do_recv_oob_host`) is unaffected: retrieving a real pending urgent
    // byte works fine without polling for it first.
    fn scan_select(&self, read_mask: u32, write_mask: u32, nfds: u32) -> (u32, u32) {
        let sockets = self.sockets.as_ref().expect("init() has run");
        let mut ready_read = 0u32;
        let mut ready_write = 0u32;
        for fd in 1..nfds.min(SELECT_MAX_FD) {
            let bit = 1u32 << fd;
            let idx = (fd - 1) as usize;
            // Host-backed fds live in a separate table (see `HostFdSlot`'s
            // own comment) -- `sock_poll` is a real `poll(2)` on that
            // side (src/wasmboard.rs's `poll_socket_mask`), so this is
            // exactly as correct for a listening/connecting/connected
            // host socket as the smoltcp checks below are for their own
            // fds, including accept-readiness on a host-backed listener.
            if let Some(hslot) = self.host_fds[idx].as_ref() {
                let mask = unsafe { sock_poll(hslot.handle) };
                let read_ready = mask & (SOCK_READABLE | SOCK_ERROR) != 0;
                let write_ready = mask & (SOCK_WRITABLE | SOCK_ERROR) != 0;
                if read_mask & bit != 0 && read_ready {
                    ready_read |= bit;
                }
                if write_mask & bit != 0 && write_ready {
                    ready_write |= bit;
                }
                continue;
            }
            let Some(slot) = self.fds[idx].as_ref() else {
                continue;
            };
            // `sockets.get` panics if the handle's actual stored type
            // doesn't match the requested one ("handle refers to a socket
            // of a wrong type", smoltcp's own SocketSet::get) -- this used
            // to always ask for tcp::Socket regardless of `slot.kind`,
            // which is fine right up until a UDP fd is ever named in a
            // WaitSelect() mask, at which point it panics the whole wasm
            // module outright (no REG_RESULT ever gets written, so the
            // guest just sits in Wait() forever -- a hang from the
            // outside, even though the real failure is a trap, not a
            // logic bug). Found running bsdsocktest's own UDP throughput
            // test, the first thing in the loopback tier to WaitSelect on
            // a UDP socket rather than only ever calling recvfrom()
            // directly on one.
            let (read_ready, write_ready) = match slot.kind {
                SockKind::Tcp => {
                    let socket = sockets.get::<tcp::Socket>(slot.socket);
                    // A listening socket's own "read ready" means "a
                    // connection arrived", not "there's data to read" --
                    // can_recv()/may_recv() are both false for a freshly-
                    // Established (just-accepted, nothing sent yet)
                    // socket, so the ordinary data-availability check
                    // never fires here. do_accept's own WaitKind::Accept
                    // already keys off exactly this same signal
                    // (`!is_listening()`); scan_select just never learned
                    // it, since select()-based accept-readiness (as
                    // opposed to a direct accept() call) hadn't been
                    // exercised until bsdsocktest's own NULL-timeout
                    // WaitSelect test, which WaitSelects on a listener
                    // instead of just calling accept() directly like
                    // every earlier accept-related test here did.
                    let read_ready = if slot.is_listener {
                        !socket.is_listening()
                    } else {
                        socket.can_recv() || !socket.may_recv()
                    };
                    // Not just can_send(): real select()/WaitSelect()
                    // write-readiness on a still-connecting socket means
                    // "connect() has concluded" (successfully or not),
                    // not "there's buffer room" -- can_send() alone is
                    // false for BOTH "still connecting" and "connect()
                    // failed", so a refused non-blocking connect() would
                    // never show up as write-ready and WaitSelect() would
                    // wait out its full timeout on every single call
                    // forever (found running bsdsocktest's own
                    // SO_ERROR-after-failed-connect test, which
                    // WaitSelects on the write side to learn when a
                    // non-blocking connect() has settled one way or the
                    // other before reading SO_ERROR). SynSent/SynReceived
                    // is excluded explicitly since `!may_send()` alone is
                    // *also* true during those states, which would
                    // wrongly report ready before the attempt has even
                    // concluded.
                    let write_ready = match socket.state() {
                        tcp::State::SynSent | tcp::State::SynReceived => false,
                        _ => socket.can_send() || !socket.may_send(),
                    };
                    (read_ready, write_ready)
                }
                SockKind::Udp => {
                    let socket = sockets.get::<udp::Socket>(slot.socket);
                    (socket.can_recv(), socket.can_send())
                }
                SockKind::Icmp => {
                    let socket = sockets.get::<icmp::Socket>(slot.socket);
                    (socket.can_recv(), socket.can_send())
                }
            };
            if read_mask & bit != 0 && read_ready {
                ready_read |= bit;
            }
            if write_mask & bit != 0 && write_ready {
                ready_write |= bit;
            }
        }
        (ready_read, ready_write)
    }

    // WaitSelect(nfds, read_fds, write_fds, except_fds, timeout, signals):
    // `timeout`, when non-zero, points to a real `struct timeval` (0 addr
    // = block indefinitely) -- see the real-timeval-parsing comment below
    // for why this wasn't always true. except_fds is accepted but always
    // reported empty -- smoltcp's TCP sockets have no exceptional-
    // condition state this project models.
    //
    // The deadline itself, once computed, is persisted in
    // `wait_select_deadline` across retries (keyed by task, same
    // rationale as `send_progress`) rather than recomputed from
    // `timeout_addr` on every call: the guest's blocking-doorbell loop
    // re-issues the SAME CALL_WAITSELECT on every wake with the args
    // unchanged, so recomputing "now + timeout" fresh each time means the
    // deadline perpetually stays just out of reach -- once one retry's
    // deadline finally elapses and wakes the guest, the very next retry
    // would set a brand new one before ever reporting the timeout,
    // forever. Found hanging bsdsocktest's own WaitSelect timeout test,
    // which -- unlike every WaitSelect call this project's own earlier
    // tests exercised -- is the first one that expects a *real* timeout
    // to actually fire rather than something always becoming ready first.
    // `signals_addr`, if non-zero, points to a caller-owned ULONG: on
    // input, the Amiga signal bits (of the *calling task itself* --
    // real WaitSelect never allocates its own) that should also wake this
    // call; on a signal-interrupted return, overwritten with whichever of
    // those bits actually arrived. Checked by reading the calling task's
    // own `tc_SigRecvd` directly out of its `struct Task` in Amiga memory
    // (see `task_sig_recvd`) -- these are real signals delivered by the
    // guest's own Signal() calls, entirely outside this RPC layer, so
    // there's no other way to observe them. Only checked at call time
    // (the first synchronous attempt, and again on every blocking retry):
    // a signal that arrives *after* the guest has already committed to
    // Wait()ing on this call's own privately-allocated wake signal (see
    // _ring_doorbell_blocking) won't wake it early, since exec.library's
    // Wait() only reacts to the specific bits passed to it, and this
    // call's own signal is a different bit than whatever the caller asked
    // about -- a real gap, but not one bsdsocktest's own loopback-tier
    // signal tests exercise (they either pre-signal before calling, or
    // don't rely on signal delivery to begin with).
    // Real BSD/AmigaOS TCP stacks record a completed non-blocking
    // connect()'s pending error (readable via getsockopt(SO_ERROR)) as
    // part of resolving the connection itself -- select()/WaitSelect()'s
    // write-readiness is only the *notification* that it's done, not the
    // thing that determines the error. This project has no such central
    // place: `do_connect`'s own errno handling only ever runs when the
    // guest calls connect() again, and a non-blocking connect() whose
    // completion is observed via WaitSelect() instead (real programs do
    // exactly this -- bsdsocktest's own SO_ERROR-after-failed-connect
    // test is one) never triggers that retry at all, so the pending error
    // was never recorded -- getsockopt(SO_ERROR) read back whatever stale
    // value the original EINPROGRESS left behind. Called wherever
    // do_wait_select reports fds as write-ready, covering both the
    // immediate first-call check and every retry after a wake (both paths
    // funnel through that same check). Imprecise in one respect: this
    // fires for *any* write-ready TCP fd found Closed, connect()'d or
    // not, so a socket that connected successfully and was later closed
    // through the normal path could also have its task's last errno
    // overwritten if a caller happens to WaitSelect on it afterwards --
    // narrow enough in practice (nothing in this project's own tests hits
    // it) not to be worth a more precise per-fd tracking mechanism today.
    //
    // Also clears the error to 0 on a *successful* completion (state
    // reaches Established), not just recording one on failure: this used
    // to only ever set ECONNREFUSED, so a non-blocking connect() that
    // actually succeeded left the earlier EINPROGRESS sitting in
    // last_errno forever -- getsockopt(SO_ERROR) read back "still
    // connecting" instead of "no error" for a connection that had
    // already fully completed. Found running bsdsocktest's own
    // WaitSelect-driven non-blocking-connect-completion test, which
    // (unlike the SO_ERROR-after-*failed*-connect test that found the
    // ECONNREFUSED half of this) exercises the success path specifically.
    fn record_connect_completion_errors(
        &mut self,
        task: u32,
        write_mask: u32,
        ready_write: u32,
        nfds: u32,
    ) {
        for fd in 1..nfds.min(SELECT_MAX_FD) {
            let bit = 1u32 << fd;
            if write_mask & bit == 0 || ready_write & bit == 0 {
                continue;
            }
            let Some(slot) = self.fd_slot(fd as i32) else {
                continue;
            };
            if slot.kind != SockKind::Tcp || !slot.connect_started {
                continue;
            }
            let sockets = self.sockets.as_ref().expect("init() has run");
            match sockets.get::<tcp::Socket>(slot.socket).state() {
                tcp::State::Closed => self.set_errno(task, ECONNREFUSED),
                tcp::State::Established => self.set_errno(task, 0),
                _ => {}
            }
        }
    }

    // Samples fd `idx`'s current level-triggered readiness for
    // `process_socket_events`'s edge detection (see `EventLevel`'s own
    // comment). Mirrors `scan_select`'s own readiness rules for TCP/UDP
    // read/write-ready; `accept_ready`/`may_recv`/`connecting` are this
    // function's own additions scan_select has no use for.
    fn sample_event_level(&self, idx: usize) -> EventLevel {
        let slot = self.fds[idx].as_ref().expect("caller checked Some");
        let sockets = self.sockets.as_ref().expect("init() has run");
        match slot.kind {
            SockKind::Tcp => {
                let socket = sockets.get::<tcp::Socket>(slot.socket);
                let connecting = matches!(
                    socket.state(),
                    tcp::State::SynSent | tcp::State::SynReceived
                );
                if slot.is_listener {
                    EventLevel {
                        read_ready: false,
                        write_ready: false,
                        accept_ready: !socket.is_listening(),
                        may_recv: true,
                        connecting: false,
                    }
                } else {
                    EventLevel {
                        read_ready: socket.can_recv() || !socket.may_recv(),
                        write_ready: if connecting {
                            false
                        } else {
                            socket.can_send() || !socket.may_send()
                        },
                        accept_ready: false,
                        may_recv: socket.may_recv(),
                        connecting,
                    }
                }
            }
            SockKind::Udp => {
                let socket = sockets.get::<udp::Socket>(slot.socket);
                EventLevel {
                    read_ready: socket.can_recv(),
                    write_ready: socket.can_send(),
                    accept_ready: false,
                    may_recv: true,
                    connecting: false,
                }
            }
            SockKind::Icmp => {
                let socket = sockets.get::<icmp::Socket>(slot.socket);
                EventLevel {
                    read_ready: socket.can_recv(),
                    write_ready: socket.can_send(),
                    accept_ready: false,
                    may_recv: true,
                    connecting: false,
                }
            }
        }
    }

    // `sample_event_level`'s host-backend counterpart: a real `sock_poll`
    // stands in for smoltcp's `can_recv()`/`can_send()`/`may_recv()`/
    // `is_listening()`/`state()`. `SOCK_HUP` (set alongside `SOCK_READABLE`
    // on a real peer hangup, see that const's own comment in
    // src/wasmboard.rs) is what lets `may_recv` go false here the same way
    // `!socket.may_recv()` does on the smoltcp arm, without this call
    // itself consuming any data. "Still connecting" has no host-backed
    // socket-state query the way smoltcp's `tcp::State::SynSent` is one --
    // instead, `connect_started` combined with "no WRITABLE/ERROR bit yet"
    // stands in for it: a real non-blocking `connect()`'s completion is
    // exactly the transition `sock_poll`'s WRITABLE/ERROR bits report, and
    // `do_connect_host`'s own retry loop already relies on that same
    // signal.
    fn sample_event_level_host(&self, idx: usize) -> EventLevel {
        let slot = self.host_fds[idx].as_ref().expect("caller checked Some");
        let mask = unsafe { sock_poll(slot.handle) };
        if slot.is_listener {
            EventLevel {
                read_ready: false,
                write_ready: false,
                accept_ready: mask & (SOCK_READABLE | SOCK_ERROR) != 0,
                may_recv: true,
                connecting: false,
            }
        } else {
            let connecting = slot.connect_started && mask & (SOCK_WRITABLE | SOCK_ERROR) == 0;
            EventLevel {
                read_ready: mask & (SOCK_READABLE | SOCK_ERROR) != 0,
                write_ready: if connecting {
                    false
                } else {
                    mask & (SOCK_WRITABLE | SOCK_ERROR) != 0
                },
                accept_ready: false,
                may_recv: mask & SOCK_HUP == 0,
                connecting,
            }
        }
    }

    // Synthesizes GetSocketEvents()-reportable FD_* events from tick-over-
    // tick transitions in `sample_event_level`'s readiness, for every fd
    // with a non-zero SO_EVENTMASK (do_setsockopt). Two-pass, not one:
    // the first pass only reads (self.fds/self.sockets), the second only
    // writes (self.fds[..].opts.ev_prev, self.event_queues, self.wake_queue)
    // -- collecting fires/updates first avoids interleaving a shared borrow
    // of self.sockets with the mutable borrows the second pass needs.
    //
    // `fds` and `host_fds` share one index space (an fd lives in exactly
    // one of the two tables, see `do_socket_host`/`do_obtain_socket`'s own
    // collision-avoidance comment), so one loop over `0..MAX_FDS` checks
    // both, tagging each update with which table it came from. The
    // edge-detection arithmetic itself is identical either way -- only
    // `sample_event_level`/`sample_event_level_host` and which struct's
    // `opts` gets read/written differ.
    fn process_socket_events(&mut self) {
        let mut updates: Vec<(usize, bool, EventLevel)> = Vec::new();
        let mut fires: Vec<(i32, u32)> = Vec::new();
        for idx in 0..MAX_FDS {
            let (eventmask, ev_prev, connect_started, cur, is_host) =
                if let Some(slot) = self.fds[idx].as_ref() {
                    if slot.opts.eventmask == 0 {
                        continue;
                    }
                    let Some(prev) = slot.opts.ev_prev else {
                        continue;
                    };
                    (
                        slot.opts.eventmask,
                        prev,
                        slot.connect_started,
                        self.sample_event_level(idx),
                        false,
                    )
                } else if let Some(slot) = self.host_fds[idx].as_ref() {
                    if slot.opts.eventmask == 0 {
                        continue;
                    }
                    let Some(prev) = slot.opts.ev_prev else {
                        continue;
                    };
                    (
                        slot.opts.eventmask,
                        prev,
                        slot.connect_started,
                        self.sample_event_level_host(idx),
                        true,
                    )
                } else {
                    continue;
                };
            let mut bits = 0u32;
            if !ev_prev.accept_ready && cur.accept_ready {
                bits |= FD_ACCEPT;
            }
            // A connect() completing (leaving SynSent/SynReceived, or on
            // the host backend the WRITABLE/ERROR bit finally landing)
            // reports FD_CONNECT, never FD_WRITE, even though the same
            // underlying write_ready transition also goes true then --
            // real AmiTCP disambiguates these two conditions rather than
            // reporting both for the same edge.
            if connect_started && ev_prev.connecting && !cur.connecting {
                bits |= FD_CONNECT;
            } else if !ev_prev.write_ready && cur.write_ready {
                bits |= FD_WRITE;
            }
            if !ev_prev.read_ready && cur.read_ready {
                bits |= FD_READ;
            }
            if ev_prev.may_recv && !cur.may_recv {
                bits |= FD_CLOSE;
            }
            bits &= eventmask as u32;
            updates.push((idx, is_host, cur));
            if bits != 0 {
                fires.push(((idx + 1) as i32, bits));
            }
        }
        for (idx, is_host, cur) in updates {
            if is_host {
                if let Some(slot) = self.host_fds[idx].as_mut() {
                    slot.opts.ev_prev = Some(cur);
                }
            } else if let Some(slot) = self.fds[idx].as_mut() {
                slot.opts.ev_prev = Some(cur);
            }
        }
        if fires.is_empty() {
            return;
        }
        // Broadcast to every task that's registered a SIGEVENTMASK signal,
        // not just one -- SBTC_SIGEVENTMASK is per-task (SocketBaseTags'
        // own `task` argument, see do_socketbasetaglist), matching real
        // AmigaOS where each opener of bsdsocket.library gets its own
        // event notification. In practice this project's own tests only
        // ever have one task registered at a time.
        let wakeups: Vec<(u32, u32)> = self
            .tasks
            .iter()
            .filter(|(_, ts)| ts.sig_event_mask != 0)
            .map(|(task, ts)| (*task, ts.sig_event_mask))
            .collect();
        for (fd, bits) in fires {
            for &(task, sigmask) in &wakeups {
                let q = self.event_queues.entry(task).or_default();
                if let Some(entry) = q.iter_mut().find(|(f, _)| *f == fd) {
                    entry.1 |= bits;
                } else {
                    q.push_back((fd, bits));
                }
                self.wake_queue.push_back((task, sigmask));
            }
        }
    }

    // GetSocketEvents(ULONG *event_ptr): dequeues one pending (fd, mask)
    // event for this task (see `event_queues`'s own comment), or returns
    // -1 with `event_ptr` left untouched if none are pending -- matches
    // docs/AMITCP_API.md's own "-1: No events are pending" (a real,
    // meaningful sentinel here, not `_hs_stub`'s generic placeholder --
    // see entry.s's own comment on why GetSocketEvents stayed on
    // `_hs_stub` until now: -1 happens to be the *correct* empty-queue
    // return value for this one LVO specifically).
    fn do_get_socket_events(&mut self, task: u32, event_ptr: u32) -> i32 {
        let Some((fd, mask)) = self
            .event_queues
            .get_mut(&task)
            .and_then(VecDeque::pop_front)
        else {
            return -1;
        };
        if event_ptr != 0 {
            let raw = mask.to_be_bytes();
            unsafe { dma_write(event_ptr as i32, raw.as_ptr() as i32, 4) };
        }
        fd
    }

    fn do_wait_select(
        &mut self,
        task: u32,
        nfds: i32,
        read_addr: u32,
        write_addr: u32,
        except_addr: u32,
        timeout_addr: u32,
        signals_addr: u32,
    ) -> i32 {
        let nfds = nfds.max(0) as u32;
        let read_mask = self.read_fd_mask(read_addr);
        let write_mask = self.read_fd_mask(write_addr);

        let (ready_read, ready_write) = self.scan_select(read_mask, write_mask, nfds);
        if ready_read != 0 || ready_write != 0 {
            self.wait_select_deadline.remove(&task);
            self.record_connect_completion_errors(task, write_mask, ready_write, nfds);
            self.write_fd_mask(read_addr, ready_read);
            self.write_fd_mask(write_addr, ready_write);
            self.write_fd_mask(except_addr, 0);
            return (ready_read.count_ones() + ready_write.count_ones()) as i32;
        }

        if signals_addr != 0 {
            let mut raw = [0u8; 4];
            unsafe { dma_read(signals_addr as i32, raw.as_mut_ptr() as i32, 4) };
            let requested = u32::from_be_bytes(raw);
            let received = requested & self.task_sig_recvd(task);
            if received != 0 {
                self.clear_task_sig_recvd(task, received);
                self.wait_select_deadline.remove(&task);
                self.write_fd_mask(read_addr, 0);
                self.write_fd_mask(write_addr, 0);
                self.write_fd_mask(except_addr, 0);
                unsafe {
                    dma_write(
                        signals_addr as i32,
                        received.to_be_bytes().as_ptr() as i32,
                        4,
                    )
                };
                return 0;
            }
        }

        let deadline = if let Some(&d) = self.wait_select_deadline.get(&task) {
            Some(d)
        } else if timeout_addr != 0 {
            // A real `struct timeval` (devices/timer.h): two BE ULONGs,
            // tv_secs then tv_micro, 8 bytes total -- not a single 4-byte
            // microsecond count. Used to read only the first 4 bytes and
            // treat them as the whole timeout, so a real `tv_secs=1`
            // (bsdsocktest's own WaitSelect timeout tests pass one) was
            // misread as "1 microsecond" instead of one second.
            let mut raw = [0u8; 8];
            unsafe { dma_read(timeout_addr as i32, raw.as_mut_ptr() as i32, 8) };
            let secs = i32::from_be_bytes(raw[0..4].try_into().unwrap()).max(0) as i64;
            let micro = i32::from_be_bytes(raw[4..8].try_into().unwrap()).max(0) as i64;
            let micros = secs * 1_000_000 + micro;
            if micros == 0 {
                // Poll-only: already checked above and nothing was ready.
                self.write_fd_mask(read_addr, 0);
                self.write_fd_mask(write_addr, 0);
                self.write_fd_mask(except_addr, 0);
                return 0;
            }
            // `micros` is a real wall-clock duration (from the caller's
            // own struct timeval); `self.micros` is raw colour-clock
            // ticks -- convert before combining them, see `Board::micros`'s
            // own doc comment for why a direct 1:1 add would be wrong.
            let cck_timeout = (micros as f64 * CCK_HZ / 1_000_000.0) as i64;
            let d = self.micros + cck_timeout;
            self.wait_select_deadline.insert(task, d);
            Some(d)
        } else {
            None
        };

        if deadline.is_some_and(|d| self.micros >= d) {
            self.wait_select_deadline.remove(&task);
            self.write_fd_mask(read_addr, 0);
            self.write_fd_mask(write_addr, 0);
            self.write_fd_mask(except_addr, 0);
            return 0;
        }

        self.last_pending.insert(
            task,
            WaitKind::Select {
                read_mask,
                write_mask,
                nfds,
                deadline,
            },
        );
        RES_PENDING
    }

    // CALL_REGISTER_WAIT: the guest is about to Wait() on `signal_mask`
    // for whatever its last RES_PENDING call was about (see
    // `last_pending`, filled in by do_connect/do_recv/do_wait_select).
    fn do_register_wait(&mut self, task: u32, signal_mask: u32) -> i32 {
        if let Some(kind) = self.last_pending.remove(&task) {
            self.waiters.push(Waiter {
                task,
                signal_mask,
                kind,
            });
        }
        0
    }

    // Re-checks every registered wait; anything now satisfied moves from
    // `waiters` onto the wake queue for the guest's interrupt server to
    // drain. Takes `waiters` out of `self` for the duration so the
    // per-waiter checks can borrow `self` freely (avoids fighting the
    // borrow checker over `self.waiters` vs. `self` as a whole).
    fn process_waiters(&mut self) {
        let waiters = std::mem::take(&mut self.waiters);
        for w in waiters {
            let ready = match w.kind {
                // Each arm checks the host-socket backend first (see
                // `host_socket_mask`'s own comment): a host-backed fd
                // never has a `fd_slot`/`socket_can_recv` entry (it isn't
                // in `fds` at all), so falling through to those unchanged
                // would wrongly read as "fd is gone" and fire immediately.
                // `sock_poll`'s mask only has to be an approximate
                // "should I wake this waiter" signal here -- whichever
                // `do_connect_host`/`do_send_host`/`do_recv_host` call
                // this wakeup lets the guest retry is what authoritatively
                // re-checks and reports the real result.
                // Any bit at all, not just WRITABLE/ERROR: a refused
                // connect's own `poll(2)` result is platform-dependent
                // (found running bsdsocktest for real -- macOS reports
                // *only* POLLHUP for a loopback ECONNREFUSED, not
                // POLLOUT/POLLERR, which this module classifies as
                // SOCK_READABLE; checking only WRITABLE|ERROR here left
                // the waiter permanently unsatisfied and the guest
                // livelocked in Wait() forever, the exact hang shape
                // this same test number caused on the smoltcp path
                // before its own `connect_started` fix -- see
                // bsdsocktest-status.md's hang #2). Safe to treat any
                // readiness bit as "go recheck": `do_connect_host`'s own
                // retry doesn't trust this signal either way, it just
                // re-issues `sock_connect` and reports whatever that
                // says.
                WaitKind::Connect { fd } => match self.host_socket_mask(fd) {
                    Some(mask) => mask != 0,
                    None => self.fd_slot(fd).is_none_or(|slot| {
                        let sockets = self.sockets.as_ref().expect("init() has run");
                        let st = sockets.get::<tcp::Socket>(slot.socket).state();
                        !matches!(st, tcp::State::SynSent | tcp::State::SynReceived)
                    }),
                },
                WaitKind::Recv { fd } => match self.host_socket_mask(fd) {
                    Some(mask) => mask & (SOCK_READABLE | SOCK_ERROR) != 0,
                    None => self
                        .socket_can_recv(fd)
                        .is_none_or(|(can_recv, may_recv)| can_recv || !may_recv),
                },
                // Host-only (see `WaitKind::RecvOob`'s own comment) --
                // `host_socket_mask` returning `None` means the fd is gone
                // (closed while waiting), not "fall back to smoltcp": wake
                // up rather than block forever, the retry will report
                // ENOTSOCK on its own. No dedicated bit to check readiness
                // against (`sock_poll` can't reliably detect pending
                // urgent data at all -- see `scan_select`'s own comment on
                // why `POLLPRI` was tried and abandoned): any readiness
                // bit is treated as "go recheck", same reasoning as
                // `WaitKind::Connect`'s own identical `mask != 0` --
                // `do_recv_oob_host`'s retry re-issues `sock_recv_oob` and
                // trusts nothing from `sock_poll` either way. In practice
                // this means "recheck every tick" for an otherwise-idle
                // connected socket (its own `SOCK_WRITABLE` bit is
                // normally always set), bounded and harmless.
                WaitKind::RecvOob { fd } => self.host_socket_mask(fd).is_none_or(|mask| mask != 0),
                WaitKind::Send { fd } => match self.host_socket_mask(fd) {
                    Some(mask) => mask & (SOCK_WRITABLE | SOCK_ERROR) != 0,
                    None => self.fd_slot(fd).is_none_or(|slot| {
                        let sockets = self.sockets.as_ref().expect("init() has run");
                        let socket = sockets.get::<tcp::Socket>(slot.socket);
                        socket.can_send() || !socket.may_send()
                    }),
                },
                // `sock_poll`'s READABLE bit on a *listening* socket means
                // exactly "a connection is ready to accept" (real POSIX
                // `poll(2)` semantics, see src/wasmboard.rs's own
                // `poll_socket_mask`) -- so this is a real, cheap
                // readiness check, not the busy-poll approximation an
                // earlier version of this arm used before `sock_poll` was
                // upgraded from a `peek()`-based heuristic to a real
                // `poll(2)` call.
                WaitKind::Accept { fd } if self.host_fd_index(fd).is_some() => self
                    .host_socket_mask(fd)
                    .is_some_and(|mask| mask & (SOCK_READABLE | SOCK_ERROR) != 0),
                WaitKind::Accept { fd } => self.fd_slot(fd).is_none_or(|slot| {
                    let sockets = self.sockets.as_ref().expect("init() has run");
                    !sockets.get::<tcp::Socket>(slot.socket).is_listening()
                }),
                WaitKind::Select {
                    read_mask,
                    write_mask,
                    nfds,
                    deadline,
                } => {
                    let (r, wr) = self.scan_select(read_mask, write_mask, nfds);
                    r != 0 || wr != 0 || deadline.is_some_and(|d| self.micros >= d)
                }
                // The one consuming check in this whole match: see
                // WaitKind::Dns's and DnsOutcome's own comments for why
                // this has to happen here (not in do_gethostbyname) and
                // exactly once per completed query.
                WaitKind::Dns => match self.dns_queries.get(&w.task).copied() {
                    None => true, // no query on record -- don't hang on it forever
                    Some(handle) => {
                        let dns_handle = self.dns_socket.expect("init() has run");
                        let sockets = self.sockets.as_mut().expect("init() has run");
                        let socket = sockets.get_mut::<dns::Socket>(dns_handle);
                        match socket.get_query_result(handle) {
                            Err(dns::GetQueryResultError::Pending) => false,
                            Err(dns::GetQueryResultError::Failed) => {
                                self.dns_results.insert(w.task, DnsOutcome::Failed);
                                true
                            }
                            Ok(addrs) => {
                                self.dns_results.insert(
                                    w.task,
                                    DnsOutcome::Ok(addrs.iter().copied().collect()),
                                );
                                true
                            }
                        }
                    }
                },
                // Host-resolver counterpart of the arm above -- same
                // one-shot-consume shape, `resolve_poll` in place of
                // `dns::Socket::get_query_result`.
                WaitKind::HostResolve => match self.host_resolve_jobs.get(&w.task).copied() {
                    None => true, // no request on record -- don't hang on it forever
                    Some(id) => {
                        let mut addr = [0u8; 4];
                        let rc = unsafe { resolve_poll(id, addr.as_mut_ptr() as i32) };
                        match rc {
                            -2 => false,
                            0 => {
                                self.host_resolve_jobs.remove(&w.task);
                                let ip = Ipv4Address::from_octets(addr);
                                self.dns_results
                                    .insert(w.task, DnsOutcome::Ok(vec![IpAddress::Ipv4(ip)]));
                                true
                            }
                            // -1 (failed) and anything else unexpected both
                            // resolve as a plain failure -- there is no
                            // finer-grained error the host import reports.
                            _ => {
                                self.host_resolve_jobs.remove(&w.task);
                                self.dns_results.insert(w.task, DnsOutcome::Failed);
                                true
                            }
                        }
                    }
                },
                // Non-consuming (see WaitKind::Ptr's own comment): just
                // checks readiness or the deadline, never touches the
                // socket's own rx queue -- do_gethostbyaddr's retry path
                // (poll_ptr_query) does the one real recv_slice() once
                // this fires.
                WaitKind::Ptr => match self.ptr_pending.as_ref() {
                    None => true, // no query on record -- don't hang on it forever
                    Some(pending) => {
                        let handle = self.ptr_socket.expect("init() has run");
                        let sockets = self.sockets.as_ref().expect("init() has run");
                        sockets.get::<udp::Socket>(handle).can_recv()
                            || self.micros >= pending.deadline
                    }
                },
            };
            if ready {
                self.wake_queue.push_back((w.task, w.signal_mask));
            } else {
                self.waiters.push(w);
            }
        }
    }

    fn tick(&mut self, cck: i32) {
        self.micros += i64::from(cck.max(0));
        let now = Instant::from_micros(self.micros);

        let iface = self.iface.as_mut().expect("init() has run");
        let sockets = self.sockets.as_mut().expect("init() has run");
        iface.poll(now, &mut self.device, sockets);

        // Reap orphaned closing sockets once their FIN handshake has
        // actually finished (see do_close's own comment on
        // `closing_sockets` for why this can't happen synchronously
        // inside do_close itself) -- or, if the peer never completes
        // *its* own half (e.g. never sends its own FIN back, real BSD
        // application code has no obligation to), once the deadline
        // do_close set passes instead. Without this, an orphaned socket
        // that's still (half-)alive would sit in the SocketSet forever:
        // still a real match for the peer's own 4-tuple, so it keeps
        // silently absorbing anything that peer sends -- which is
        // exactly what suppresses smoltcp's own `Interface::process_tcp`
        // "no socket accepts this segment -> send a real RST" fallback.
        // `abort()` (not `close()`) sends that RST itself once given up
        // on waiting -- the same thing a real OS kernel's own FIN_WAIT_2
        // timeout does (e.g. Linux's `tcp_fin_timeout`, defaulting to
        // ~60s -- 2 real seconds here is plenty for what this project's
        // own tests need, not an attempt to match that exact figure).
        // Found running bsdsocktest's own send-after-peer-close test,
        // which checks that a peer's *later* write to a connection this
        // side already closed gets a real error -- but never closes its
        // own side first, so this side's own FIN was never going to
        // arrive either.
        let now_micros = self.micros;
        self.closing_sockets.retain(|&(handle, deadline)| {
            let socket = sockets.get_mut::<tcp::Socket>(handle);
            if socket.state() == tcp::State::Closed {
                sockets.remove(handle);
                return false;
            }
            if now_micros >= deadline {
                // `abort()`'s own promised RST is only actually
                // transmitted the *next* time `iface.poll()` dispatches
                // this socket -- which already happened earlier this
                // same tick, before this reaping pass runs. Removing the
                // handle immediately would destroy the socket before
                // smoltcp ever got that chance, silently swallowing the
                // RST this whole timeout exists to produce. So this
                // keeps it in `closing_sockets` for exactly one more
                // tick instead: `abort()` flips it to `Closed` right
                // away, poll() gets to dispatch the RST on the next
                // tick's own first step, and *then* the branch above
                // catches and removes it.
                socket.abort();
            }
            true
        });

        self.process_socket_events();
        self.process_waiters();
    }
}

// Returns the byte index within `field` (a 4-byte, big-endian register) that
// `off` addresses, or None if `off` doesn't fall within it.
fn byte_in_field(off: u32, field: i32) -> Option<usize> {
    let field = field as u32;
    (field..field + 4)
        .contains(&off)
        .then(|| (off - field) as usize)
}

// Pulls the IPv4 address/port out of an `IpEndpoint`, or (0.0.0.0, 0) if
// there wasn't one (not yet connected/bound) -- a real getsockname()/
// getpeername() on such a socket is itself an error case its caller checks
// for separately (a 0 port is never valid), so this just gives a safe
// default rather than plumbing an Option through every call site.
fn ipv4_of(ep: Option<IpEndpoint>) -> (Ipv4Address, u16) {
    match ep.map(|e| e.addr) {
        Some(IpAddress::Ipv4(ip)) => (ip, ep.unwrap().port),
        _ => (Ipv4Address::new(0, 0, 0, 0), 0),
    }
}

// do_icmp_recv's own synthetic 20-byte IPv4 header -- see that function's
// own comment for why raw ICMP reads need one at all (real BSD raw-socket
// semantics deliver the full IP packet on read, but smoltcp's icmp::Socket
// hands back only the ICMP payload). No options (IHL=5), not fragmented,
// TTL 64, protocol ICMP, a real computed checksum.
fn synth_ipv4_header(payload_len: usize, src: Ipv4Address, dst: Ipv4Address) -> [u8; 20] {
    let mut hdr = [0u8; 20];
    hdr[0] = 0x45; // version 4, IHL 5
    let total_len = (20 + payload_len).min(u16::MAX as usize) as u16;
    hdr[2..4].copy_from_slice(&total_len.to_be_bytes());
    hdr[8] = 64; // TTL
    hdr[9] = IPPROTO_ICMP as u8;
    // hdr[10..12] (checksum) computed below, over these bytes with the
    // checksum field itself still zero.
    hdr[12..16].copy_from_slice(&src.octets());
    hdr[16..20].copy_from_slice(&dst.octets());
    let checksum = ipv4_header_checksum(&hdr);
    hdr[10..12].copy_from_slice(&checksum.to_be_bytes());
    hdr
}

// Standard IPv4 header checksum (RFC 791 §3.1): 16-bit one's-complement
// sum of every 16-bit word, folded and complemented.
fn ipv4_header_checksum(hdr: &[u8; 20]) -> u16 {
    let mut sum: u32 = 0;
    for chunk in hdr.chunks_exact(2) {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

// Wire-encodes a dotted name ("1.0.0.127.in-addr.arpa") into DNS's own
// length-prefixed-label format (RFC 1035 §3.1): one length byte followed
// by that many raw bytes, per label, terminated by a zero-length label.
// do_gethostbyaddr's own query construction; never needs to *decode* this
// shape itself (`parse_ptr_response`, and smoltcp's own `Packet::
// parse_name`, handle the response side, which can also use compression
// pointers this project never needs to emit).
fn encode_dns_name(name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 2);
    for label in name.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

// Parses a raw DNS response looking for a PTR answer, returning its
// target name (dot-joined labels) if found. `expected_id` rejects a stray
// response that isn't for the query this project just sent (a late reply
// to an earlier, already-timed-out lookup landing on the same socket).
//
// Can't use `dns::Socket::get_query_result()` here (do_gethostbyaddr's
// own comment has the full reason) -- this instead walks the packet by
// hand with `wire::dns`'s own lower-level, public primitives: skip the
// echoed question (`DnsQuestion::parse`), then each answer `Record`
// (`DnsRecord::parse`) until one's `data` is `DnsRecordData::Other(_,
// rdata)` with numeric type 12 (PTR -- no named `DnsQueryType` variant,
// see that import's own comment). The tricky part is `rdata` itself: a
// PTR record's own target name is very commonly compressed (a pointer
// back into the question's own name, per RFC 1035 §4.1.4), and
// `Record::parse` only skips over a compressed name's *length*, it
// doesn't resolve it -- decoding the actual labels needs `Packet::
// parse_name`, which (unlike a plain sub-slice) knows how to re-seek
// into the *original* buffer a pointer refers to. `rdata` is still a
// genuine sub-slice of that same original `data` buffer (Record::parse
// never copies), so calling `packet.parse_name(rdata)` on it resolves
// correctly even when the PTR name is fully compressed down to a single
// two-byte pointer.
fn parse_ptr_response(data: &[u8], expected_id: u16) -> Option<String> {
    let packet = DnsPacket::new_checked(data).ok()?;
    if packet.transaction_id() != expected_id {
        return None;
    }
    if packet.rcode() != DnsRcode::NoError {
        return None;
    }
    if packet.answer_record_count() == 0 {
        return None;
    }
    let (mut rest, _question) = DnsQuestion::parse(packet.payload()).ok()?;
    for _ in 0..packet.answer_record_count() {
        let (next, record) = DnsRecord::parse(rest).ok()?;
        rest = next;
        let DnsRecordData::Other(record_type, rdata) = record.data else {
            continue;
        };
        if u16::from(record_type) != 12 {
            continue;
        }
        let mut labels = Vec::new();
        for label in packet.parse_name(rdata) {
            labels.push(String::from_utf8_lossy(label.ok()?).into_owned());
        }
        if labels.is_empty() {
            return None;
        }
        return Some(labels.join("."));
    }
    None
}

// Strict "a.b.c.d" dotted-quad parse (exactly 4 decimal octets, 0-255
// each) -- shared by do_inet_addr/do_inet_network's DMA-reading wrappers
// and this file's own native unit tests (which can't exercise dma_read
// itself -- see the `native_host_stubs` module doc comment).
fn parse_dotted_quad(s: &str) -> Option<u32> {
    let mut parts = s.split('.');
    let mut octets = [0u8; 4];
    for o in octets.iter_mut() {
        *o = parts.next()?.parse::<u8>().ok()?;
    }
    if parts.next().is_some() {
        return None; // more than 4 parts
    }
    Some(u32::from_be_bytes(octets))
}

// Parses `[hostsocket]`'s own `address` config key ("a.b.c.d" or
// "a.b.c.d/prefix"), defaulting the prefix to /24 (this project's own
// historical default, matching NAT's 10.0.2.0/24) when omitted. Used by
// init() to configure the interface's address under net = "bridge" (see
// INTERFACE_ADDR's own comment for why the plain address alone can't just
// be a constant everywhere).
fn parse_ipv4_cidr(s: &str) -> Option<(Ipv4Address, u8)> {
    let (addr, prefix) = match s.split_once('/') {
        Some((addr, prefix)) => (addr, prefix.parse::<u8>().ok()?),
        None => (s, 24),
    };
    if prefix > 32 {
        return None;
    }
    let v = parse_dotted_quad(addr)?;
    let [a, b, c, d] = v.to_be_bytes();
    Some((Ipv4Address::new(a, b, c, d), prefix))
}

// Reads a `struct msghdr`'s `msg_iov`/`msg_iovlen` array as (iov_base,
// iov_len) pairs -- shared by do_sendmsg/do_recvmsg. Plain functions, not
// `Board` methods: they only ever touch guest memory via dma_read (a free
// unsafe extern fn, not something needing `&self`), which keeps them
// usable from inside `send_tcp_stream`'s closure without any borrow of
// `self`. Layout is `struct msghdr` from this project's own guest
// toolchain (m68k-amigaos-gcc's clib2, confirmed against a running
// container -- this NDK's own headers don't define `msghdr` at all, only
// reference it): msg_name@0, msg_namelen@4, msg_iov@8, msg_iovlen@12,
// msg_control@16, msg_controllen@20, msg_flags@24 (28 bytes, all fields
// natural-aligned 4-byte LONGs on this 32-bit target -- no packing
// pragma applies to non-PPC gcc). `struct iovec` (`sys/uio.h`, same
// toolchain): iov_base@0, iov_len@4 (8 bytes). Bounded to `MAX_IOVEC`
// entries -- see that const's own comment.
fn read_iovec_descriptors(msg_addr: u32) -> Option<Vec<(u32, usize)>> {
    if msg_addr == 0 {
        return None;
    }
    let mut hdr = [0u8; 28];
    // Safety: reading a guest-supplied struct msghdr out of Amiga memory.
    unsafe { dma_read(msg_addr as i32, hdr.as_mut_ptr() as i32, hdr.len() as i32) };
    let iov_addr = u32::from_be_bytes(hdr[8..12].try_into().unwrap());
    let iovlen = u32::from_be_bytes(hdr[12..16].try_into().unwrap()) as usize;
    if iov_addr == 0 || iovlen == 0 {
        return None;
    }
    let count = iovlen.min(MAX_IOVEC);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut entry = [0u8; 8];
        // Safety: reading one guest-supplied struct iovec out of Amiga
        // memory, `i` entries past `iov_addr`.
        unsafe {
            dma_read(
                (iov_addr + (i as u32) * 8) as i32,
                entry.as_mut_ptr() as i32,
                entry.len() as i32,
            )
        };
        let base = u32::from_be_bytes(entry[0..4].try_into().unwrap());
        // Clamped the same way do_send/do_recv clamp their own guest-
        // supplied `len` -- see `MAX_XFER_LEN`'s own comment. Without this,
        // one bogus `iov_len` (independent of `MAX_IOVEC`'s cap on entry
        // *count*) could still drive an unbounded per-entry allocation in
        // `read_iovec_bytes`/`do_recvmsg`.
        let len = (u32::from_be_bytes(entry[4..8].try_into().unwrap()) as usize).min(MAX_XFER_LEN);
        out.push((base, len));
    }
    Some(out)
}

// do_sendmsg's own flattening of `read_iovec_descriptors`' output into one
// contiguous buffer -- see that function's own comment for why gathering
// upfront (rather than teaching `send_tcp_stream` to understand iovecs
// directly) is the simpler design.
fn read_iovec_bytes(msg_addr: u32) -> Option<Vec<u8>> {
    let iovecs = read_iovec_descriptors(msg_addr)?;
    let mut data = Vec::new();
    for (base, len) in iovecs {
        let start = data.len();
        data.resize(start + len, 0);
        // Safety: reading one guest-supplied iovec's own buffer out of
        // Amiga memory.
        unsafe { dma_read(base as i32, data[start..].as_mut_ptr() as i32, len as i32) };
    }
    Some(data)
}

thread_local! {
    static BOARD: RefCell<Board> = RefCell::new(Board::new());
}

// -- Copperline WASM board ABI ------------------------------------------------
//
// #[cfg(target_arch = "wasm32")] on every export below: `read`/`write` are
// also libc symbol names (read(2)/write(2)). Exporting them unconditionally
// as #[no_mangle] would shadow libc's own read/write the moment this crate
// is linked into a native binary (e.g. `cargo test`) -- every native
// caller of the real read()/write() syscalls, including the test harness's
// own stdout handling, would silently jump into these instead, which is
// exactly why `cargo test -p hostsocket-plugin` used to exit immediately
// with no output at all before this file gated the exports to wasm32.

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn init() {
    BOARD.with(|b| b.borrow_mut().init());
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn read(off: i32, size: i32) -> i32 {
    BOARD.with(|b| b.borrow().read(off, size))
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn write(off: i32, size: i32, value: i32) {
    BOARD.with(|b| b.borrow_mut().write(off, size, value));
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn tick(cck: i32) {
    BOARD.with(|b| b.borrow_mut().tick(cck));
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn int2() -> i32 {
    BOARD.with(|b| i32::from(!b.borrow().wake_queue.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Locks this file's register-layout constants against
    // ../guest/hostsocket_board.h's copy -- that header has no way to be
    // checked from a Rust test directly, so this at least turns an
    // accidental one-sided edit here into a failing assertion instead of a
    // silent drift (see the header's own "change both sides in the same
    // commit" note).
    #[test]
    fn board_layout_matches_guest_header() {
        assert_eq!(ROM_OFFSET, 0x0008);
        assert_eq!(REG_ARGPTR, 0x7C00);
        assert_eq!(REG_CALL, 0x7C04);
        assert_eq!(REG_RESULT, 0x7C08);
        assert_eq!(REG_WAKE_TASK, 0x7C0C);
        assert_eq!(REG_WAKE_SIGNAL, 0x7C10);
        assert_eq!(REG_WAKE_ACK, 0x7C14);
        assert_eq!(CALL_SOCKET, 0);
        assert_eq!(CALL_CONNECT, 1);
        assert_eq!(CALL_SEND, 2);
        assert_eq!(CALL_RECV, 3);
        assert_eq!(CALL_CLOSESOCKET, 4);
        assert_eq!(CALL_REGISTER_WAIT, 5);
        assert_eq!(CALL_IOCTLSOCKET, 6);
        assert_eq!(CALL_SETERRNOPTR, 7);
        assert_eq!(CALL_ERRNO, 8);
        assert_eq!(CALL_WAITSELECT, 9);
        assert_eq!(CALL_DUP2SOCKET, 20);
        assert_eq!(CALL_INET_NTOA, 21);
        assert_eq!(CALL_INET_ADDR, 22);
        assert_eq!(CALL_INET_LNAOF, 23);
        assert_eq!(CALL_INET_NETOF, 24);
        assert_eq!(CALL_INET_MAKEADDR, 25);
        assert_eq!(CALL_INET_NETWORK, 26);
        assert_eq!(CALL_GETDTABLESIZE, 27);
        assert_eq!(CALL_GETHOSTBYNAME, 28);
        assert_eq!(CALL_SOCKETBASETAGLIST, 29);
        assert_eq!(CALL_GETSOCKETEVENTS, 30);
        assert_eq!(CALL_GETHOSTNAME, 31);
        assert_eq!(CALL_GETHOSTID, 32);
        assert_eq!(CALL_SENDMSG, 33);
        assert_eq!(CALL_RECVMSG, 34);
        assert_eq!(CALL_GETHOSTBYADDR, 35);
        assert_eq!(CALL_OBTAINSOCKET, 36);
        assert_eq!(CALL_RELEASESOCKET, 37);
        assert_eq!(CALL_RELEASECOPYOFSOCKET, 38);
        assert_eq!(CALL_GETSERVBYNAME, 39);
        assert_eq!(CALL_GETSERVBYPORT, 40);
        assert_eq!(CALL_GETPROTOBYNAME, 41);
        assert_eq!(CALL_GETPROTOBYNUMBER, 42);
        assert_eq!(CALL_GETNETBYNAME, 43);
        assert_eq!(CALL_GETNETBYADDR, 44);
        assert_eq!(RES_PENDING, -2);
        assert_eq!(FIONBIO, 0x8004667E);
    }

    #[test]
    fn fd_table_allocates_and_frees_slots() {
        let mut board = Board::new();
        board.init();
        let a = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        let b = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert_eq!(board.do_close(1, a), 0);
        // The freed slot is reused before growing further.
        let c = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        assert_eq!(c, 1);
    }

    #[test]
    fn fd_table_reports_full_when_exhausted() {
        let mut board = Board::new();
        board.init();
        for i in 0..MAX_FDS {
            assert_eq!(board.do_socket(1, AF_INET, SOCK_STREAM, 0), (i + 1) as i32);
        }
        assert_eq!(board.do_socket(1, AF_INET, SOCK_STREAM, 0), -1);
        // Real BSD's own errno for "no descriptor slots left" -- this used
        // to leave errno unset entirely here (see the code-review finding
        // this pins), inconsistent with do_accept/do_obtain_socket's own
        // identical fd-exhaustion case.
        assert_eq!(board.do_errno(1), EMFILE);
    }

    #[test]
    fn do_socket_udp_creates_a_udp_kind_slot() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_DGRAM, 0);
        assert_eq!(fd, 1);
        assert_eq!(board.fd_slot(fd).map(|s| s.kind), Some(SockKind::Udp));
        let tcp_fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        assert_eq!(board.fd_slot(tcp_fd).map(|s| s.kind), Some(SockKind::Tcp));
    }

    #[test]
    fn do_bind_port_zero_resolves_a_real_ephemeral_port() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        // addr/namelen are unread here -- dma_read is a no-op stub on this
        // native target (see native_host_stubs's own doc comment), so
        // parse_sockaddr() always reads back port 0 regardless of what's
        // passed, which is exactly the "give me any free port" case this
        // is testing.
        assert_eq!(board.do_bind(1, fd, 0, 0), 0);
        let port = board.fd_slot(fd).and_then(|s| s.bind_port);
        assert!(
            port.is_some_and(|p| p != 0),
            "expected a real ephemeral port, got {port:?}"
        );
    }

    #[test]
    fn do_bind_rejects_a_port_already_bound_by_another_tcp_fd() {
        let mut board = Board::new();
        board.init();
        let taken = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        // Whatever `taken`'s own real ephemeral bind actually landed on
        // -- not a hardcoded literal (init()'s own `ptr_socket`, added
        // for gethostbyaddr(), already claims one ephemeral port before
        // any test code runs, so "the allocator's first port is always
        // 49152" stopped holding).
        assert_eq!(board.do_bind(1, taken, 0, 0), 0);
        let taken_port = board.fd_slot(taken).unwrap().bind_port;
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        board.fds[(fd - 1) as usize].as_mut().unwrap().bind_port = None;
        // Fake this second fd requesting that exact same port (bind()'s
        // own dma_read-dependent path can't be driven to a *specific*
        // port natively, but the conflict-detection logic under test
        // doesn't care how a port ended up recorded on `taken`, only
        // that `fd`'s own do_bind() call asks for the same one) by
        // pre-seeding next_local_port so port-0 resolution lands there --
        // simpler than faking a real sockaddr_in via dma_read.
        board.next_local_port = taken_port.unwrap();
        assert_eq!(board.do_bind(1, fd, 0, 0), -1);
        assert_eq!(board.do_errno(1), EADDRINUSE);
    }

    #[test]
    fn fd_index_rejects_extreme_guest_supplied_fd_values_without_overflowing() {
        // Regression test for a code-review finding: `fd` reaching
        // `fd_index` is guest-controlled RPC input, and `fd_index` used to
        // compute a plain `fd - 1`. `i32::MIN - 1` overflows -- harmless
        // under this workspace's release profile (`overflow-checks` off,
        // the wrapped value fails the subsequent bounds check), but a
        // plain debug build (`cargo test` without `--release`) has
        // overflow-checks on by default and panics on the subtraction
        // itself, before ever reaching that check. `fd_index` is called
        // from nearly every `do_*` handler, so this reached a large
        // fraction of the RPC surface.
        let mut board = Board::new();
        board.init();
        assert_eq!(board.fd_index(i32::MIN), None);
        assert_eq!(board.fd_index(0), None);
        assert_eq!(board.fd_index(-1), None);
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        assert_eq!(board.fd_index(fd), Some(0));
    }

    #[test]
    fn do_socket_rejects_invalid_domain_or_type() {
        let mut board = Board::new();
        board.init();
        assert_eq!(board.do_socket(1, -1, SOCK_STREAM, 0), -1);
        assert_eq!(board.do_errno(1), EINVAL);
        assert_eq!(board.do_socket(1, AF_INET, 999, 0), -1);
        assert_eq!(board.do_errno(1), EINVAL);
        // SOCK_RAW with anything other than IPPROTO_ICMP is accepted
        // (silently TCP-shaped, see do_socket's own comment) -- only
        // genuinely unknown type values are rejected.
        assert_eq!(board.do_socket(1, AF_INET, SOCK_RAW, 0), 1);
    }

    #[test]
    fn do_socket_raw_icmp_creates_an_icmp_kind_slot() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_RAW, IPPROTO_ICMP);
        assert_eq!(fd, 1);
        assert_eq!(board.fd_slot(fd).map(|s| s.kind), Some(SockKind::Icmp));
        // Only the exact IPPROTO_ICMP protocol gets the real Icmp kind --
        // SOCK_RAW with any other protocol value keeps the historical
        // TCP-shaped fallback (see do_socket's own comment).
        let tcp_shaped = board.do_socket(1, AF_INET, SOCK_RAW, 0);
        assert_eq!(
            board.fd_slot(tcp_shaped).map(|s| s.kind),
            Some(SockKind::Tcp)
        );
    }

    #[test]
    fn icmp_sendto_binds_by_ident_from_the_echo_header_and_queues_the_packet() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_RAW, IPPROTO_ICMP);
        let idx = (fd - 1) as usize;
        // A minimal ICMP echo request: type=8 code=0 checksum=0 (unchecked
        // here -- do_icmp_sendto doesn't validate the packet, only reads
        // bytes 4-5 as the identifier to bind by) ident=0xBD51 seq=1,
        // matching bsdsocktest's own icmp_echo header shape.
        let packet = [8u8, 0, 0, 0, 0xBD, 0x51, 0, 1];
        assert!(!board
            .sockets
            .as_ref()
            .unwrap()
            .get::<icmp::Socket>(board.fds[idx].as_ref().unwrap().socket)
            .is_open());
        let rc = board.do_icmp_sendto(1, idx, &packet, INTERFACE_ADDR);
        assert_eq!(rc, packet.len() as i32);
        assert!(board
            .sockets
            .as_ref()
            .unwrap()
            .get::<icmp::Socket>(board.fds[idx].as_ref().unwrap().socket)
            .is_open());
    }

    // Hand-builds a minimal (header-only, no payload) Ethernet+IPv4 frame
    // -- unlike this file's DMA-touching RPC logic, `loopback_response`
    // only ever parses a raw byte slice, so it's fully testable natively
    // without the real wasm/dma_read boundary (see native_host_stubs's
    // own doc comment for why most of this module's logic isn't).
    fn eth_ipv4_frame(dst: Ipv4Address) -> Vec<u8> {
        let mut frame = vec![0u8; 14 + 20];
        frame[12] = 0x08;
        frame[13] = 0x00; // EtherType::Ipv4
        let ip = &mut frame[14..];
        ip[0] = 0x45; // version 4, IHL 5 (20-byte header, no options)
        ip[2..4].copy_from_slice(&20u16.to_be_bytes()); // total_len
        ip[16..20].copy_from_slice(&dst.octets());
        frame
    }

    fn eth_arp_request(target: Ipv4Address, requester_mac: EthernetAddress) -> Vec<u8> {
        let repr = ArpRepr::EthernetIpv4 {
            operation: ArpOperation::Request,
            source_hardware_addr: requester_mac,
            source_protocol_addr: Ipv4Address::new(10, 0, 2, 15),
            target_hardware_addr: EthernetAddress([0; 6]),
            target_protocol_addr: target,
        };
        let mut buf = vec![0u8; 14 + repr.buffer_len()];
        let mut eth = EthernetFrame::new_unchecked(&mut buf);
        eth.set_src_addr(requester_mac);
        eth.set_dst_addr(EthernetAddress::BROADCAST);
        eth.set_ethertype(EthernetProtocol::Arp);
        repr.emit(&mut ArpPacket::new_unchecked(eth.payload_mut()));
        buf
    }

    #[test]
    fn loopback_response_echoes_ipv4_addressed_to_127_0_0_0_8() {
        assert!(loopback_response(&eth_ipv4_frame(Ipv4Address::new(127, 0, 0, 1))).is_some());
        assert!(loopback_response(&eth_ipv4_frame(Ipv4Address::new(127, 255, 255, 254))).is_some());
        assert!(loopback_response(&eth_ipv4_frame(INTERFACE_ADDR)).is_none());
        assert!(loopback_response(&eth_ipv4_frame(Ipv4Address::new(10, 0, 2, 2))).is_none());
        assert!(loopback_response(&[0u8; 4]).is_none()); // too short to even parse
    }

    #[test]
    fn loopback_response_answers_arp_for_127_0_0_1_only() {
        let requester = EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        let request = eth_arp_request(Ipv4Address::new(127, 0, 0, 1), requester);
        let reply = loopback_response(&request).expect("ARP reply for 127.0.0.1");
        let eth = EthernetFrame::new_checked(&reply[..]).unwrap();
        assert_eq!(eth.ethertype(), EthernetProtocol::Arp);
        assert_eq!(eth.dst_addr(), requester); // straight back to the requester
        match ArpRepr::parse(&ArpPacket::new_checked(eth.payload()).unwrap()).unwrap() {
            ArpRepr::EthernetIpv4 {
                operation: ArpOperation::Reply,
                source_hardware_addr,
                source_protocol_addr,
                target_hardware_addr,
                ..
            } => {
                assert_eq!(source_hardware_addr, INTERFACE_MAC);
                assert_eq!(source_protocol_addr, Ipv4Address::new(127, 0, 0, 1));
                assert_eq!(target_hardware_addr, requester);
            }
            other => panic!("unexpected ARP repr: {other:?}"),
        }

        // Not our concern -- some other on-link address (e.g. the NAT
        // gateway) should be left alone for the real backend to answer.
        let not_loopback = eth_arp_request(Ipv4Address::new(10, 0, 2, 2), requester);
        assert!(loopback_response(&not_loopback).is_none());
    }

    #[test]
    fn setsockopt_getsockopt_accept_known_options_reject_others() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        assert_eq!(
            board.do_setsockopt(1, fd, SOL_SOCKET, SO_REUSEADDR, 0, 4),
            0
        );
        assert_eq!(
            board.do_setsockopt(1, fd, SOL_SOCKET, SO_KEEPALIVE, 0, 4),
            0
        );
        assert_eq!(board.do_setsockopt(1, fd, SOL_SOCKET, SO_LINGER, 0, 8), 0);
        assert_eq!(board.do_setsockopt(1, fd, SOL_SOCKET, SO_RCVTIMEO, 0, 8), 0);
        assert_eq!(board.do_setsockopt(1, fd, SOL_SOCKET, SO_SNDTIMEO, 0, 8), 0);
        assert_eq!(board.do_setsockopt(1, fd, SOL_SOCKET, SO_RCVBUF, 0, 4), 0);
        assert_eq!(board.do_setsockopt(1, fd, SOL_SOCKET, SO_SNDBUF, 0, 4), 0);
        assert_eq!(
            board.do_setsockopt(1, fd, IPPROTO_TCP, TCP_NODELAY, 0, 4),
            0
        );
        assert_eq!(
            board.do_setsockopt(1, fd, SOL_SOCKET, SO_EVENTMASK, 0, 4),
            0
        );
        assert_eq!(board.do_setsockopt(1, fd, SOL_SOCKET, 0x9999, 0, 4), -1);
        assert_eq!(board.do_errno(1), EINVAL);

        assert_eq!(board.do_getsockopt(1, fd, SOL_SOCKET, SO_TYPE, 0, 0), 0);
        assert_eq!(board.do_getsockopt(1, fd, SOL_SOCKET, SO_ERROR, 0, 0), 0);
        assert_eq!(
            board.do_getsockopt(1, fd, SOL_SOCKET, SO_REUSEADDR, 0, 0),
            0
        );
        assert_eq!(
            board.do_getsockopt(1, fd, IPPROTO_TCP, TCP_NODELAY, 0, 0),
            0
        );
        assert_eq!(
            board.do_getsockopt(1, fd, SOL_SOCKET, SO_EVENTMASK, 0, 0),
            0
        );
        assert_eq!(board.do_getsockopt(1, fd, SOL_SOCKET, 0x9999, 0, 0), -1);
        assert_eq!(board.do_errno(1), EINVAL);
    }

    #[test]
    fn so_eventmask_roundtrips_and_seeds_an_edge_detection_baseline() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        let idx = (fd - 1) as usize;
        assert!(board.fds[idx].as_ref().unwrap().opts.ev_prev.is_none());
        // dma_read is a no-op stub under cfg(test) (see the module's own
        // native_host_stubs doc comment), so `optval`'s contents always
        // read back as 0 -- fine here, this is only checking that setting
        // *any* mask (0) still seeds the edge-detection baseline rather
        // than leaving it None.
        assert_eq!(
            board.do_setsockopt(1, fd, SOL_SOCKET, SO_EVENTMASK, 0, 4),
            0
        );
        assert!(board.fds[idx].as_ref().unwrap().opts.ev_prev.is_some());
        assert_eq!(
            board.do_getsockopt(1, fd, SOL_SOCKET, SO_EVENTMASK, 0, 0),
            0
        );
        // A freshly eventmask'd, never-touched socket must not report any
        // event on the very next tick -- this is bsdsocktest's own
        // eventmask_no_spurious test (81) in miniature: the baseline
        // sampled by do_setsockopt should already match what
        // process_socket_events sees a moment later, since nothing about
        // the socket actually changed in between.
        board.process_socket_events();
        assert_eq!(board.do_get_socket_events(1, 0), -1);
    }

    #[test]
    fn get_socket_events_returns_minus_one_when_no_events_are_pending() {
        let mut board = Board::new();
        board.init();
        // No SO_EVENTMASK was ever set on anything, and no task ever
        // registered SBTC_SIGEVENTMASK -- docs/AMITCP_API.md's own "-1: No
        // events are pending", the same sentinel this LVO's old `_hs_stub`
        // placeholder used to return by coincidence (see entry.s's own
        // comment on why that stub choice happened to already be correct
        // for this one LVO).
        assert_eq!(board.do_get_socket_events(1, 0), -1);
    }

    #[test]
    fn dup2socket_aliases_dont_double_free_the_socket() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        let dup = board.do_dup2socket(1, fd, -1);
        assert_ne!(dup, -1);
        assert_ne!(dup, fd);
        // Closing one alias must not remove the underlying socket while
        // the other is still open -- before the refcount fix, this
        // sequence would call SocketSet::remove() on the same handle
        // twice and panic.
        assert_eq!(board.do_close(1, fd), 0);
        assert_eq!(board.do_close(1, dup), 0);
    }

    #[test]
    fn dup2socket_specific_target_places_the_duplicate_there() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        let target = fd + 10;
        assert_eq!(board.do_dup2socket(1, fd, target), target);
        assert_eq!(
            board.fd_slot(target).map(|s| s.socket),
            board.fd_slot(fd).map(|s| s.socket)
        );
        // A specific target that's already open gets closed first (real
        // dup2() semantics) -- redirect it to alias `fd` a second time
        // and confirm the fd table still only shows the one occupant.
        let other = board.do_socket(1, AF_INET, SOCK_DGRAM, 0);
        assert_eq!(board.do_dup2socket(1, fd, other), other);
        assert_eq!(board.fd_slot(other).map(|s| s.kind), Some(SockKind::Tcp));
        // dup2(fd, fd) is a real no-op success, not a close-then-recreate.
        assert_eq!(board.do_dup2socket(1, fd, fd), fd);
    }

    #[test]
    fn dup2socket_rejects_an_out_of_range_target() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        assert_eq!(board.do_dup2socket(1, fd, (MAX_FDS as i32) + 1), -1);
        assert_eq!(board.do_errno(1), EINVAL);
    }

    #[test]
    fn release_socket_then_obtain_socket_round_trips() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        let original_socket = board.fd_slot(fd).unwrap().socket;
        assert_eq!(board.do_release_socket(1, fd, 42), 42);
        // Real ReleaseSocket() semantics: `fd` is invalid in the
        // caller's own context immediately, same as CloseSocket().
        assert!(board.fd_slot(fd).is_none());
        assert!(board.socket_pool.contains_key(&42));
        let obtained = board.do_obtain_socket(1, 42, AF_INET, SOCK_STREAM, 0);
        assert!(obtained >= 1);
        assert_eq!(
            board.fd_slot(obtained).map(|s| s.socket),
            Some(original_socket)
        );
        // A given pooled socket can only be obtained once.
        assert!(!board.socket_pool.contains_key(&42));
        assert_eq!(board.do_obtain_socket(1, 42, AF_INET, SOCK_STREAM, 0), -1);
    }

    #[test]
    fn obtain_socket_rejects_a_type_mismatch() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        assert_eq!(board.do_release_socket(1, fd, 7), 7);
        assert_eq!(board.do_obtain_socket(1, 7, AF_INET, SOCK_DGRAM, 0), -1);
        assert_eq!(board.do_errno(1), EINVAL);
        // The entry stays in the pool after a failed match -- only a
        // successful ObtainSocket() removes it.
        assert!(board.socket_pool.contains_key(&7));
    }

    #[test]
    fn release_copy_of_socket_keeps_the_original_fd_valid() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        assert_eq!(board.do_release_copy_of_socket(1, fd, 9), 9);
        // Unlike ReleaseSocket(), the original descriptor stays valid.
        assert!(board.fd_slot(fd).is_some());
        let obtained = board.do_obtain_socket(1, 9, AF_INET, SOCK_STREAM, 0);
        assert!(obtained >= 1);
        assert_ne!(obtained, fd);
        assert_eq!(
            board.fd_slot(obtained).map(|s| s.socket),
            board.fd_slot(fd).map(|s| s.socket)
        );
    }

    #[test]
    fn release_socket_unique_id_assigns_distinct_fresh_keys() {
        let mut board = Board::new();
        board.init();
        let fd1 = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        let fd2 = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        let id1 = board.do_release_socket(1, fd1, -1);
        let id2 = board.do_release_socket(1, fd2, -1);
        assert!(id1 >= 0 && id2 >= 0 && id1 != id2);
    }

    // The three tests below drive send_tcp_stream directly rather than
    // through a real connect()/RST sequence (native tests can't produce a
    // real one, see native_host_stubs's own doc comment) -- may_send() is
    // false for a freshly-created, never-connected TCP socket the same
    // way it's false for one that's fully closed, so poking
    // `was_established`/`shutdown_by_us` directly isolates exactly the
    // errno-selection logic under test without needing a real network
    // round trip.

    #[test]
    fn send_on_a_never_connected_socket_reports_econnrefused() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        assert_eq!(board.send_tcp_stream(1, fd, 1, |_, _| vec![0u8]), -1);
        assert_eq!(board.do_errno(1), ECONNREFUSED);
    }

    #[test]
    fn send_after_local_shutdown_reports_epipe() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        let slot = board.fds[(fd - 1) as usize].as_mut().unwrap();
        slot.was_established = true;
        slot.shutdown_by_us = true;
        assert_eq!(board.send_tcp_stream(1, fd, 1, |_, _| vec![0u8]), -1);
        assert_eq!(board.do_errno(1), EPIPE);
    }

    #[test]
    fn send_after_the_peer_tears_it_down_reports_econnreset() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        board.fds[(fd - 1) as usize]
            .as_mut()
            .unwrap()
            .was_established = true;
        assert_eq!(board.send_tcp_stream(1, fd, 1, |_, _| vec![0u8]), -1);
        assert_eq!(board.do_errno(1), ECONNRESET);
    }

    #[test]
    fn recv_after_the_peer_tears_it_down_reports_econnreset() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        board.fds[(fd - 1) as usize]
            .as_mut()
            .unwrap()
            .was_established = true;
        assert_eq!(board.do_recv(1, fd, 0, 16, 0), -1);
        assert_eq!(board.do_errno(1), ECONNRESET);
    }

    // Exercises the actual `closing_sockets` deadline/abort mechanism end
    // to end (unlike `send_after_the_peer_tears_it_down_reports_econnreset`
    // and its recv sibling above, which only check the errno-mapping logic
    // given an already-`was_established`, already-`Closed` socket -- they
    // never touch `do_close`, `tick()`'s reaping loop, or whether an
    // aborted socket's RST actually makes it back to the peer over the
    // loopback device). Drives a real connect/accept handshake through
    // `tick()`, closes the server side (orphaning it, since the client
    // never closes its own -- the exact bsdsocktest test-35 scenario), and
    // checks that once the abort deadline passes the client genuinely
    // observes a reset via `do_recv`, not just a timeout.
    #[test]
    fn orphaned_closing_socket_gets_aborted_and_the_peer_sees_a_reset() {
        let mut board = Board::new();
        board.init();

        let listener_fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        assert_eq!(board.do_bind(1, listener_fd, 0, 0), 0);
        let listen_port = board.fd_slot(listener_fd).unwrap().bind_port.unwrap();
        assert_eq!(board.do_listen(1, listener_fd, 5), 0);

        // Connect directly via smoltcp's own API rather than do_connect:
        // that call parses the target address out of guest memory via
        // dma_read, which is a no-op stub outside the wasm target, so a
        // real 127.0.0.1 target can't flow through the RPC-shaped entry
        // point in a native test.
        let client_fd = board.do_socket(2, AF_INET, SOCK_STREAM, 0);
        let client_handle = board.fd_slot(client_fd).unwrap().socket;
        let local_port = board.alloc_local_port();
        {
            let iface = board.iface.as_mut().unwrap();
            let cx = iface.context();
            let sockets = board.sockets.as_mut().unwrap();
            let socket = sockets.get_mut::<tcp::Socket>(client_handle);
            socket
                .connect(
                    cx,
                    (IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 1)), listen_port),
                    local_port,
                )
                .unwrap();
        }
        board.fds[(client_fd - 1) as usize]
            .as_mut()
            .unwrap()
            .connect_started = true;

        for _ in 0..10 {
            board.tick(1000);
        }
        let client_established = board
            .sockets
            .as_ref()
            .unwrap()
            .get::<tcp::Socket>(client_handle)
            .state()
            == tcp::State::Established;
        assert!(client_established, "client never reached Established");
        // do_connect itself sets this on the Established transition (see
        // its own comment); bypassing it above to drive the handshake
        // means this side has to be set by hand too.
        board.fds[(client_fd - 1) as usize]
            .as_mut()
            .unwrap()
            .was_established = true;

        let server_fd = board.do_accept(1, listener_fd, 0, 0);
        assert!(server_fd > 0, "accept did not return a connected socket");

        assert_eq!(board.do_close(1, server_fd), 0);
        assert!(
            !board.closing_sockets.is_empty(),
            "server socket should be orphaned into closing_sockets, not removed synchronously"
        );

        // A few ticks to let the server's own graceful FIN actually reach
        // FinWait2 (it needs the client's ACK, which smoltcp sends
        // automatically -- but never the client's own FIN, since this
        // test's client never closes, exactly like bsdsocktest's own
        // test 35).
        for _ in 0..5 {
            board.tick(1000);
        }

        // Past the 2-real-second abort deadline (in CCK units): one tick
        // to have tick() see the expired deadline and call abort(), a
        // second so the *next* iface.poll() actually dispatches the RST
        // (abort() alone only flips local state, per tick()'s own
        // comment), and a *third* so the client's own poll() call
        // consumes that RST from the loopback queue -- the queue a
        // transmit lands in is only drained on a *later* poll()'s own
        // receive() phase, not the same one that just transmitted into
        // it.
        board.tick((2.1 * CCK_HZ) as i32);
        board.tick(1000);
        board.tick(1000);

        assert_eq!(board.do_recv(2, client_fd, 0, 16, 0), -1);
        assert_eq!(board.do_errno(2), ECONNRESET);
    }

    #[test]
    fn recv_on_a_never_connected_socket_reports_clean_eof() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        // `was_established` stays false -- matches a socket that never
        // connected at all, where a plain EOF (not ECONNRESET) is the
        // right call.
        assert_eq!(board.do_recv(1, fd, 0, 16, 0), 0);
    }

    #[test]
    fn inet_lnaof_netof_makeaddr_roundtrip_class_a() {
        let mut board = Board::new();
        board.init();
        let addr: u32 = 0x0a010203; // 10.1.2.3, class A
        assert_eq!(board.do_inet_lnaof(1, addr), 0x010203);
        assert_eq!(board.do_inet_netof(1, addr), 0x0a);
        let net = board.do_inet_netof(1, addr) as u32;
        let host = board.do_inet_lnaof(1, addr) as u32;
        assert_eq!(board.do_inet_makeaddr(1, net, host) as u32, addr);
    }

    #[test]
    fn services_protocols_networks_tables_match_bsdsocktest_expectations() {
        // Pins the actual static-table data (not just the lookup logic)
        // against bsdsocktest's own test_dns.c assertions -- a typo in one
        // of these entries would compile clean and pass every RPC-layer
        // test below (which only exercise dispatch/lookup correctness),
        // so this checks the data those tests actually depend on.
        assert!(SERVICES.contains(&("http", 80, "tcp")));
        assert!(SERVICES.contains(&("ftp", 21, "tcp")));
        assert!(PROTOCOLS.contains(&("tcp", 6)));
        assert!(PROTOCOLS.contains(&("udp", 17)));
        assert!(NETWORKS.contains(&("loopback", 127)));
    }

    #[test]
    fn getservbyport_finds_a_known_port_regardless_of_proto_filter() {
        // proto_addr/name_addr are guest-memory string reads, a no-op
        // stub outside the wasm target (see native_host_stubs's own doc
        // comment) -- but port itself is a direct RPC integer argument
        // (not read via dma_read), so the actual lookup-by-port logic is
        // fully exercisable natively, same class of exception
        // write_reassembles_a_real_68000_split_move_l's own comment
        // documents for register-level args in general.
        let mut board = Board::new();
        board.init();
        // proto_addr = 0 means "no protocol filter" -- doesn't touch
        // dma_read at all, so this exercises real matching logic, not the
        // stub's always-empty-string fallback.
        assert_eq!(board.do_getservbyport(1, 21, 0, 0x2000), 0);
        assert_eq!(board.do_getservbyport(1, 99999, 0, 0x2000), -1);
    }

    #[test]
    fn getprotobynumber_finds_a_known_protocol() {
        let mut board = Board::new();
        board.init();
        assert_eq!(board.do_getprotobynumber(1, 6, 0x2000), 0);
        assert_eq!(board.do_getprotobynumber(1, 17, 0x2000), 0);
        assert_eq!(board.do_getprotobynumber(1, 255, 0x2000), -1);
    }

    #[test]
    fn getnetbyaddr_finds_loopback_and_rejects_wrong_type() {
        let mut board = Board::new();
        board.init();
        assert_eq!(board.do_getnetbyaddr(1, 127, AF_INET, 0x2000), 0);
        assert_eq!(board.do_getnetbyaddr(1, 127, AF_INET + 1, 0x2000), -1);
        assert_eq!(board.do_getnetbyaddr(1, 999_999, AF_INET, 0x2000), -1);
    }

    #[test]
    fn getservbyname_getprotobyname_getnetbyname_reject_bad_input_cleanly() {
        // name_addr is a guest-memory string read, a no-op stub outside
        // the wasm target (see native_host_stubs's own doc comment), so
        // it always reads back an empty string here -- which correctly
        // matches nothing in SERVICES/PROTOCOLS/NETWORKS. Real name-based
        // matching is only verifiable against the actual wasm/dma
        // boundary (a real bsdsocktest run), same limitation
        // sendmsg_rejects_bad_input_cleanly's own comment documents.
        let mut board = Board::new();
        board.init();
        assert_eq!(board.do_getservbyname(1, 0x1000, 0, 0x2000), -1);
        assert_eq!(board.do_getprotobyname(1, 0x1000, 0x2000), -1);
        assert_eq!(board.do_getnetbyname(1, 0x1000, 0x2000), -1);
    }

    #[test]
    fn parse_dotted_quad_matches_bsdsocktest_cases() {
        assert_eq!(parse_dotted_quad("127.0.0.1"), Some(0x7f000001));
        assert_eq!(parse_dotted_quad("255.255.255.255"), Some(0xffffffff));
        assert_eq!(parse_dotted_quad("10.0.0.0"), Some(0x0a000000));
        assert_eq!(parse_dotted_quad("not.an.ip"), None);
        assert_eq!(parse_dotted_quad("1.2.3"), None);
        assert_eq!(parse_dotted_quad("1.2.3.4.5"), None);
        assert_eq!(parse_dotted_quad("1.2.3.256"), None);
    }

    #[test]
    fn parse_ipv4_cidr_defaults_prefix_and_rejects_garbage() {
        assert_eq!(
            parse_ipv4_cidr("192.168.1.50/24"),
            Some((Ipv4Address::new(192, 168, 1, 50), 24))
        );
        assert_eq!(
            parse_ipv4_cidr("192.168.1.50/32"),
            Some((Ipv4Address::new(192, 168, 1, 50), 32))
        );
        // No "/prefix" at all defaults to /24, this project's own
        // historical default (matching NAT's 10.0.2.0/24).
        assert_eq!(
            parse_ipv4_cidr("192.168.1.50"),
            Some((Ipv4Address::new(192, 168, 1, 50), 24))
        );
        assert_eq!(parse_ipv4_cidr("192.168.1.50/33"), None); // out of range
        assert_eq!(parse_ipv4_cidr("not.an.ip/24"), None);
        assert_eq!(parse_ipv4_cidr("192.168.1.50/not-a-number"), None);
    }

    #[test]
    fn synth_ipv4_header_has_valid_checksum_and_fields() {
        let src = Ipv4Address::new(10, 0, 2, 15);
        let dst = Ipv4Address::new(127, 0, 0, 1);
        let hdr = synth_ipv4_header(64, src, dst);
        assert_eq!(hdr[0], 0x45); // version 4, IHL 5
        assert_eq!(u16::from_be_bytes([hdr[2], hdr[3]]), 84); // 20 + 64
        assert_eq!(hdr[8], 64); // TTL
        assert_eq!(hdr[9], IPPROTO_ICMP as u8);
        assert_eq!(&hdr[12..16], &src.octets()[..]);
        assert_eq!(&hdr[16..20], &dst.octets()[..]);
        // A correct checksum makes the header's own 16-bit-word sum fold
        // to exactly 0xFFFF (one's-complement "all ones") -- the standard
        // self-check any real IP stack performs on receipt.
        let mut sum: u32 = 0;
        for chunk in hdr.chunks_exact(2) {
            sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        assert_eq!(sum as u16, 0xFFFF);
    }

    #[test]
    fn encode_dns_name_matches_wire_format() {
        let encoded = encode_dns_name("1.0.0.127.in-addr.arpa");
        let expected: Vec<u8> = [
            &[1u8][..],
            b"1",
            &[1u8][..],
            b"0",
            &[1u8][..],
            b"0",
            &[3u8][..],
            b"127",
            &[7u8][..],
            b"in-addr",
            &[4u8][..],
            b"arpa",
            &[0u8][..],
        ]
        .concat();
        assert_eq!(encoded, expected);
    }

    // Hand-builds a raw DNS response (header + echoed question + one PTR
    // answer, its own record name compressed via a pointer back into the
    // question -- real resolvers do exactly this) to exercise
    // parse_ptr_response natively, without needing a real DNS server or
    // the wasm/dma boundary.
    fn build_ptr_response(id: u16, flags: u16, rdata: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&id.to_be_bytes());
        buf.extend_from_slice(&flags.to_be_bytes());
        buf.extend_from_slice(&1u16.to_be_bytes()); // qdcount
        buf.extend_from_slice(&1u16.to_be_bytes()); // ancount
        buf.extend_from_slice(&0u16.to_be_bytes()); // nscount
        buf.extend_from_slice(&0u16.to_be_bytes()); // arcount
        let qname_offset = buf.len() as u16; // 12, header is a fixed size
        buf.extend_from_slice(&encode_dns_name("1.0.0.127.in-addr.arpa"));
        buf.extend_from_slice(&12u16.to_be_bytes()); // qtype PTR
        buf.extend_from_slice(&1u16.to_be_bytes()); // qclass IN
                                                    // Answer record: name compressed as a pointer back to the question.
        buf.extend_from_slice(&(0xC000 | qname_offset).to_be_bytes());
        buf.extend_from_slice(&12u16.to_be_bytes()); // type PTR
        buf.extend_from_slice(&1u16.to_be_bytes()); // class IN
        buf.extend_from_slice(&300u32.to_be_bytes()); // ttl
        buf.extend_from_slice(&(rdata.len() as u16).to_be_bytes()); // rdlength
        buf.extend_from_slice(rdata);
        buf
    }

    #[test]
    fn parse_ptr_response_extracts_the_answer_name() {
        let id = 0x1234;
        let response = build_ptr_response(id, 0x8180, &encode_dns_name("localhost"));
        assert_eq!(
            parse_ptr_response(&response, id),
            Some("localhost".to_string())
        );
    }

    #[test]
    fn parse_ptr_response_rejects_a_mismatched_transaction_id() {
        let response = build_ptr_response(0x1234, 0x8180, &encode_dns_name("localhost"));
        assert_eq!(parse_ptr_response(&response, 0x5678), None);
    }

    #[test]
    fn parse_ptr_response_rejects_an_error_rcode() {
        // Low nibble of flags is the rcode -- 0x8183 = NXDomain (3).
        let id = 0x1234;
        let response = build_ptr_response(id, 0x8183, &encode_dns_name("localhost"));
        assert_eq!(parse_ptr_response(&response, id), None);
    }

    #[test]
    fn getdtablesize_matches_max_fds() {
        let board = Board::new();
        assert_eq!(board.do_getdtablesize(), MAX_FDS as i32);
        assert!(board.do_getdtablesize() >= 64); // bsdsocktest's own bar
    }

    #[test]
    fn getdtablesize_reflects_reported_dtablesize_and_never_shrinks() {
        let mut board = Board::new();
        // Same `.max()` idiom do_socketbasetaglist's own SBTC_DTABLESIZE_SET
        // arm uses -- dma_read is a no-op stub under cfg(test) (see
        // native_host_stubs's own doc comment), so that arm can't be driven
        // through a real TagItem array natively; this exercises the
        // monotonic-growth formula directly instead.
        board.reported_dtablesize = board.reported_dtablesize.max(128);
        assert_eq!(board.do_getdtablesize(), 128);
        // A later "restore" to a smaller value must not actually shrink it
        // -- bsdsocktest's own test relies on exactly this ("Restore (may
        // not reduce)").
        board.reported_dtablesize = board.reported_dtablesize.max(64);
        assert_eq!(board.do_getdtablesize(), 128);
    }

    #[test]
    fn closing_a_pending_accept_fd_scrubs_last_pending_before_a_reused_slot_can_type_confuse_it() {
        // Regression test for a code-review finding: `do_close`/
        // `do_release_socket` used to scrub `waiters` and `send_progress`
        // for a closed fd but not `last_pending` -- the record of a task
        // that was told RES_PENDING but hasn't called CALL_REGISTER_WAIT
        // yet. If a *different* task closes that same fd and the freed
        // slot gets reused for a different SockKind before the first task
        // registers, `do_register_wait` would move the stale (TCP-typed)
        // WaitKind into `waiters` unchanged, and `process_waiters`'
        // Accept/Connect/Send arms unconditionally call
        // `sockets.get::<tcp::Socket>(...)` -- a type mismatch there traps
        // the whole wasm module, not just the one task.
        let mut board = Board::new();
        board.init();

        let listener_fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        assert_eq!(board.do_bind(1, listener_fd, 0, 0), 0);
        assert_eq!(board.do_listen(1, listener_fd, 5), 0);
        // Nothing has connected yet, so this parks: task 1 is told
        // RES_PENDING and, per bsdsocket.library's own blocking
        // convention, is about to CALL_REGISTER_WAIT and Wait() on it.
        assert_eq!(board.do_accept(1, listener_fd, 0, 0), RES_PENDING);

        // A *different* task closes the same fd out from under task 1,
        // before task 1 ever calls CALL_REGISTER_WAIT.
        assert_eq!(board.do_close(2, listener_fd), 0);
        // The freed slot is immediately reused for a different SockKind --
        // do_socket always claims the first free slot, so this lands on
        // the exact same fd number.
        let udp_fd = board.do_socket(3, AF_INET, SOCK_DGRAM, 0);
        assert_eq!(udp_fd, listener_fd);

        // Without the fix, this would move task 1's stale WaitKind::Accept
        // (now pointing at a UDP-kind slot) into `waiters` unchanged.
        assert_eq!(board.do_register_wait(1, 0x1), 0);
        assert!(
            board.waiters.is_empty(),
            "a stale wait for a closed/reused fd must never be armed"
        );

        // Would panic pre-fix (SocketSet::get::<tcp::Socket> trapping on
        // the now-UDP handle) if the stale waiter had been armed above.
        board.tick(1000);
    }

    #[test]
    fn wait_select_never_scans_or_aliases_fd_32_and_above() {
        // Regression test for a code-review finding: `CALL_WAITSELECT`'s
        // wire format represents each fd_set as a single guest ULONG
        // bitmask (bit N = fd N, see `SELECT_MAX_FD`'s own comment) -- fd
        // 32 has no representable bit. `scan_select` used to scan all the
        // way to `MAX_FDS` (64, matching `getdtablesize()`), computing
        // `1u32 << fd` for fd >= 32 -- on the real wasm32 target, where
        // WebAssembly's `i32.shl` masks the shift amount by 31, this
        // silently aliased a high fd's readiness onto an unrelated low
        // bit instead of just never reporting it.
        let mut board = Board::new();
        board.init();
        let mut fds = vec![];
        for _ in 0..33 {
            fds.push(board.do_socket(1, AF_INET, SOCK_DGRAM, 0));
        }
        assert_eq!(fds.len(), 33);
        let fd1 = fds[0];
        assert_eq!(fd1, 1);
        // A freshly created UDP socket is write-ready immediately (no
        // backpressure until its send buffer is actually full), so asking
        // only about fd1 should report exactly fd1's own bit back.
        let want = 1u32 << (fd1 as u32);
        let (_, ready) = board.scan_select(0, want, 32);
        assert_eq!(ready, want, "fd1 alone should already be write-ready");
        // Scanning further -- nfds well past 32, matching MAX_FDS/
        // getdtablesize()'s own 64 -- must not change the answer: if fd
        // 33 (also write-ready, and among those created above) were still
        // being scanned and its readiness aliased onto a low bit, this
        // would differ from the `nfds = 32` case above.
        let (_, ready_wide) = board.scan_select(0, want, 64);
        assert_eq!(
            ready_wide, want,
            "fds >= 32 must never be scanned, aliased onto a lower bit, or reported"
        );
    }

    #[test]
    fn send_rejects_msg_oob_with_eopnotsupp() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        // The MSG_OOB check happens before any socket-state access (see
        // do_send's own comment on why: no urgent-pointer support exists
        // at all, so this is a hard rejection, not something that only
        // matters once actually connected) -- exercisable on a freshly
        // created, unconnected socket.
        assert_eq!(board.do_send(1, fd, 0, 1, MSG_OOB), -1);
        assert_eq!(board.do_errno(1), EOPNOTSUPP);
    }

    #[test]
    fn sendto_clamps_an_oversized_guest_supplied_length_instead_of_trusting_it() {
        // Regression test for the DoS this project's own code review found:
        // `len` here comes straight from the RPC call's own register
        // argument (not out of guest memory via dma_read, so this is
        // exercisable natively -- see MAX_XFER_LEN's own comment for why an
        // unclamped guest-supplied length used to size a `Vec` allocation
        // is a real, single-call plugin-abort DoS on the wasm32 target).
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_DGRAM, 0);
        // Set a real peer by hand rather than via do_connect(): that parses
        // the target address out of guest memory via dma_read, a no-op stub
        // outside the wasm target (see sendmsg_rejects_bad_input_cleanly's
        // own comment on the same limitation), which would leave the peer
        // 0.0.0.0:0 -- an unspecified endpoint smoltcp's own udp::Socket
        // correctly refuses to send to, masking the very path this test
        // means to exercise.
        board.fds[(fd - 1) as usize].as_mut().unwrap().udp_peer =
            Some((Ipv4Address::new(127, 0, 0, 1), 12345));
        // A length near i32::MAX would, pre-fix, have driven a multi-
        // gigabyte `vec![0u8; n]` allocation attempt. Asserting the exact
        // clamped return value (not just "didn't crash") pins the fix:
        // send succeeded, and it queued MAX_XFER_LEN bytes, not i32::MAX.
        assert_eq!(
            board.do_sendto(1, fd, 0, i32::MAX, 0, 0),
            MAX_XFER_LEN as i32
        );
    }

    #[test]
    fn sendmsg_rejects_bad_input_cleanly() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        // dma_read is a no-op stub under cfg(test) (see native_host_stubs's
        // own doc comment), so a non-null msg_addr still reads back an
        // all-zero struct msghdr -- msg_iov/msg_iovlen both 0, which
        // read_iovec_descriptors treats the same as a null msg_addr. Real
        // iovec walking is only verifiable against the actual wasm/dma
        // boundary (the real bsdsocktest run).
        assert_eq!(board.do_sendmsg(1, fd, 0, 0), -1);
        assert_eq!(board.do_errno(1), EINVAL);
        assert_eq!(board.do_sendmsg(1, fd, 0, MSG_OOB), -1);
        assert_eq!(board.do_errno(1), EOPNOTSUPP);
        let udp_fd = board.do_socket(1, AF_INET, SOCK_DGRAM, 0);
        assert_eq!(board.do_sendmsg(1, udp_fd, 0, 0), -1);
        assert_eq!(board.do_errno(1), EOPNOTSUPP);
    }

    #[test]
    fn recvmsg_rejects_bad_input_cleanly() {
        let mut board = Board::new();
        board.init();
        let fd = board.do_socket(1, AF_INET, SOCK_STREAM, 0);
        assert_eq!(board.do_recvmsg(1, fd, 0, 0), -1);
        assert_eq!(board.do_errno(1), EINVAL);
        let udp_fd = board.do_socket(1, AF_INET, SOCK_DGRAM, 0);
        assert_eq!(board.do_recvmsg(1, udp_fd, 0, 0), -1);
        assert_eq!(board.do_errno(1), EOPNOTSUPP);
    }

    #[test]
    fn gethostid_returns_a_nonzero_value_matching_interface_addr() {
        let mut board = Board::new();
        board.init();
        let id = board.do_gethostid(1);
        assert_ne!(id, 0);
        assert_eq!(id, i32::from_be_bytes([10, 0, 2, 15])); // INTERFACE_ADDR
    }

    #[test]
    fn gethostname_reports_success_and_rejects_a_degenerate_buffer_cleanly() {
        let mut board = Board::new();
        board.init();
        // dma_write is a no-op stub under cfg(test) (see
        // native_host_stubs's own doc comment), so the actual bytes
        // written can't be checked natively -- this exercises the return
        // value and the two early-out guards (null buffer, non-positive
        // length) instead, which don't touch it.
        assert_eq!(board.do_gethostname(1, 0x1000, 64), 0);
        assert_eq!(board.do_gethostname(1, 0, 64), 0);
        assert_eq!(board.do_gethostname(1, 0x1000, 0), 0);
    }

    #[test]
    fn hostent_layout_is_self_consistent() {
        // Every region lands inside the buffer, in the declared order, with
        // no overlap -- the kind of off-by-one this arithmetic is exactly
        // the sort of thing that goes unnoticed until dma_write scribbles
        // past the guest's LIB_HOSTENTBUF (a real Copperline boot is the
        // only oracle for the DMA content itself; see write_hostent's own
        // header comment and this file's `native_host_stubs` module doc
        // comment for why).
        assert_eq!(HOSTENT_ALIASES_OFF, 20); // struct hostent is 20 bytes
        assert!(HOSTENT_ADDR_LIST_OFF > HOSTENT_ALIASES_OFF);
        assert!(HOSTENT_ADDRS_OFF > HOSTENT_ADDR_LIST_OFF);
        assert!(HOSTENT_NAME_OFF > HOSTENT_ADDRS_OFF);
        assert!(HOSTENT_BUF_LEN > HOSTENT_NAME_OFF);
        // h_addr_list has room for HOSTENT_MAX_ADDRS pointers plus a NULL
        // terminator before the address bytes region starts.
        assert_eq!(
            HOSTENT_ADDRS_OFF - HOSTENT_ADDR_LIST_OFF,
            (HOSTENT_MAX_ADDRS as u32 + 1) * 4
        );
    }

    #[test]
    fn gethostbyname_rejects_an_unreadable_name_without_panicking() {
        // dma_read is a no-op on the native test target (see
        // native_host_stubs's own doc comment), so `name_addr` reads back
        // as an empty string here -- smoltcp's start_query() rejects that
        // outright. This just locks down that the empty-name path returns
        // a plain failure rather than panicking (e.g. on an `.expect()`
        // this function reaches before ever needing a real dma_read/write
        // round trip) -- the success path needs the real DMA/network
        // stack, so it's Copperline's own oracle to verify, not this one.
        let mut board = Board::new();
        board.init();
        assert_eq!(board.do_gethostbyname(1, 0, 0), -1);
    }

    #[test]
    fn errno_round_trips_per_task() {
        let mut board = Board::new();
        board.init();
        board.set_errno(1, ECONNREFUSED);
        board.set_errno(2, EINVAL);
        assert_eq!(board.do_errno(1), ECONNREFUSED);
        assert_eq!(board.do_errno(2), EINVAL);
        assert_eq!(board.do_errno(3), 0); // never set -- default
    }

    #[test]
    fn socketbasetaglist_tag_done_is_a_harmless_noop() {
        let mut board = Board::new();
        board.init();
        // addr 0 (dma_read stubbed out under cfg(test), see the module's
        // own dma_read/dma_write no-op impl) reads back as all-zero, i.e.
        // an immediate TAG_DONE -- exercises the loop's termination path
        // without needing real guest memory to back it.
        assert_eq!(board.do_socketbasetaglist(1, 0, 0, 0), 0);
        assert_eq!(SBTC_ERRNOLONGPTR_SET, 0x8000_0031);
        assert_eq!(SBTC_ERRNOLONGPTR_GET, 0x8000_8030);
        assert_eq!(SBTC_HERRNOLONGPTR_SET, 0x8000_0033);
        assert_eq!(SBTC_HERRNOLONGPTR_GET, 0x8000_8032);
        assert_eq!(SBTC_SIGEVENTMASK_SET, 0x8000_0009);
        assert_eq!(SBTC_SIGEVENTMASK_GET, 0x8000_8008);
        assert_eq!(SBTC_BREAKMASK_SET, 0x8000_0003);
        assert_eq!(SBTC_BREAKMASK_GET, 0x8000_8002);
        assert_eq!(SBTC_DTABLESIZE_SET, 0x8000_0011);
        assert_eq!(SBTC_DTABLESIZE_GET, 0x8000_8010);
    }

    #[test]
    fn gethostbyname_under_resolver_host_fails_cleanly_when_the_host_cant_start_it() {
        // The native dma/resolve stubs (see native_host_stubs's own module
        // doc comment) can't exercise a real background lookup -- resolve_
        // start always reports failure to start one at all -- but this
        // still proves the resolver_host branch is wired: it must return
        // -1 without leaving a dangling host_resolve_jobs/dns_results
        // entry behind (a leftover entry there would wedge every later
        // gethostbyname() for the same task on the "still in flight"
        // check). The real request/poll/success path is exercised for
        // real in wasmboard.rs's own resolve_start_and_poll_round_trip_a_
        // real_background_lookup test, which runs the actual host side of
        // this ABI, not this crate's own native stand-ins.
        let mut board = Board::new();
        board.init();
        board.resolver_host = true;
        // name_addr/buf_addr are unread Amiga addresses under the no-op
        // dma_read stub (reads back an empty string, not "localhost"), so
        // this exercises the resolver_host branch, not the localhost
        // short-circuit above it.
        assert_eq!(board.do_gethostbyname(1, 0, 0), -1);
        assert!(!board.host_resolve_jobs.contains_key(&1));
        assert!(!board.dns_results.contains_key(&1));
    }

    #[test]
    fn ioctl_on_unknown_fd_fails_with_enotsock() {
        let mut board = Board::new();
        board.init();
        assert_eq!(board.do_ioctl_socket(1, 99, FIONBIO, 0), -1);
        assert_eq!(board.do_errno(1), ENOTSOCK);
    }

    #[test]
    fn wake_queue_drives_int2_and_read_registers() {
        let mut board = Board::new();
        board.init();
        assert_eq!(board.wake_queue.is_empty(), true);
        assert_eq!(board.read(REG_WAKE_TASK, 4), 0);

        board.wake_queue.push_back((0x1234, 0x8));
        assert_eq!(board.read(REG_WAKE_TASK, 4), 0x1234);
        assert_eq!(board.read(REG_WAKE_SIGNAL, 4), 0x8);

        // REG_WAKE_ACK pops the front entry.
        board.write(REG_WAKE_ACK, 4, 1);
        assert!(board.wake_queue.is_empty());
        assert_eq!(board.read(REG_WAKE_TASK, 4), 0);
    }

    #[test]
    fn write_reassembles_a_real_68000_split_move_l() {
        // A real 68000's 16-bit external data bus splits a guest `move.l`
        // into two separate word writes (high word at `off`, low word at
        // `off+2`) instead of one size=4 write -- confirmed against a real
        // Copperline boot with a 68000 CPU model during Phase 4's
        // end-to-end verification pass (every RPC call silently no-op'd
        // before this fix, since the old code required size==4 exactly).
        let mut board = Board::new();
        board.init();
        board.write(REG_ARGPTR, 2, 0x00EA);
        board.write(REG_ARGPTR.wrapping_add(2), 2, 0x7C00);
        assert_eq!(board.argptr, 0x00EA_7C00);

        // A size=4 write (68020+'s 32-bit bus) must still work identically.
        board.write(REG_ARGPTR, 4, 0x00EAu32 as i32 * 0x10000 + 0x7D00);
        assert_eq!(board.argptr, 0x00EA_7D00);

        // REG_CALL's dispatch only fires once the low word completes the
        // pair, not on the high word alone (CALL_SOCKET's own value, 0, is
        // deliberately used as the high word here so a premature dispatch
        // on the high word alone would be indistinguishable from a correct
        // one -- only a real dispatch result proves it fired once).
        // do_socket rejects this specific call (domain reads back 0, not
        // AF_INET, since dma_read is a no-op stub on this native target --
        // see the module doc comment on native_host_stubs) -- that's fine,
        // a deterministic -1 is just as good a proof dispatch actually ran
        // as a real fd would be, and this test only cares about the
        // register/dispatch mechanics, not socket() validation semantics.
        board.write(REG_CALL, 2, CALL_SOCKET as u16 as i32);
        assert_eq!(board.result, 0); // not dispatched yet (never written)
        board.write(REG_CALL.wrapping_add(2), 2, 0);
        assert_eq!(board.result, -1); // dispatch ran and rejected it
    }

    // No native test drives Board's smoltcp/RPC logic through the real
    // dma_read/dma_write/net_send/net_recv host imports, which only mean
    // anything inside Copperline's wasmtime host (see native_host_stubs
    // above) -- faithfully faking that boundary natively would mean
    // re-deriving the wasm32 i32-address ABI on a native 64-bit target,
    // more likely to hide a real bug behind a wrong-but-passing fake than
    // to catch one. PROPOSAL.md's Testing section names the real oracle
    // instead: a Copperline boot exercising this ROM + plugin together over
    // the Loopback backend.
}
