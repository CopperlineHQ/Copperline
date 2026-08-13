// Board-window register layout for the HostSocket WASM plugin. Mirrors the
// role of Copperline's own guest/services/copperline_board.h -- keep this
// file in sync with the offsets crates/hostsocket-plugin/src/lib.rs's read()/write() use.
// There is no cross-repo unit test to lock the layout (Copperline's own
// board does this with an in-repo Rust test); until one exists here too,
// change both sides in the same commit and say the offset in the commit
// message. (crates/hostsocket-plugin/src/lib.rs does carry an in-crate test locking its own
// copies of these constants -- see its `board_layout` test module.)
//
// Phase 2 scope (see PROPOSAL.md "Call path" and its phased plan): a real
// fd table, per-task errno + blocking-wait state (keyed by the calling
// task's own pointer, tracked in the plugin -- see crates/hostsocket-plugin/src/lib.rs's
// `TaskState`), IoctlSocket(FIONBIO) non-blocking mode, and genuine
// Wait()/Signal()-based blocking via int2() and an installed interrupt
// server, replacing Phase 1's guest-side busy-spin.
//
// Phase 3 scope: the real server-side path (bind/listen/accept -- Phase
// 1/2's internal hard-coded echo listener is gone, replaced by a real
// guest-driven server), UDP (sendto/recvfrom, plus plain send/recv against
// a peer set by connect() on a UDP fd), and the remaining common LVOs
// (shutdown/setsockopt/getsockopt/getsockname/getpeername). DNS
// (gethostbyname and friends) is deferred -- it needs a real DNS server to
// test against, unlike everything else here, which runs fully over the
// deterministic Loopback backend.
//
// Every call's argument block now carries the calling task's own pointer
// (FindTask(NULL)) as its first field -- not because the plugin cares who
// is calling for its own sake, but so it can key per-task errno/wait state
// without the guest needing to manage a lookup table in assembly (see
// entry.s's EXECCALL macro and crates/hostsocket-plugin/src/lib.rs's `tasks: HashMap`).
//
// Concurrency note (found designing Phase 2, fixed the same way Phase 2's
// own new registers need anyway): a bare REG_ARGPTR-then-REG_CALL write
// pair is not atomic under real AmigaOS preemptive multitasking -- a task
// switch between the two writes lets a second task's call clobber
// REG_ARGPTR first. Every trampoline (including the four Phase 1 shipped)
// now brackets its whole doorbell sequence in Forbid()/Permit().
//
// 64K window layout:
//   0x0008  guest stub code (hostsocket_rom.bin). Entry table:
//             +0     process entry (unused -- bsdsocket.library is a plain
//                    NT_LIBRARY, not a DOS handler process; no RunHandler
//                    ever jumps here, but the slot is kept so the entry
//                    table shape matches copperline_board.h's convention)
//             +4     rt_Init entry (patched into the Romtag's rt_Init
//                    field by the DiagPoint entry below): Kickstart's
//                    cold-start resident scan jsr's here once every board
//                    has been DiagPoint-ed. This is where the library is
//                    actually built and AddLibrary()'d and the INTB_PORTS
//                    interrupt server installed -- deliberately deferred
//                    here rather than done at DiagPoint time (+8); see
//                    entry.s's own header comment for why building it from
//                    DiagPoint corrupts real Kickstart's boot.
//             +8     expansion-init entry (DiagPoint jsr target): only
//                    patches the Romtag's PC-relative fields into the diag
//                    copy and returns -- does NOT build the library.
//             +0x40  struct DiagArea
//   0x7C00  REG_ARGPTR (write): Amiga address of an 8x LONG argument block;
//           per-call-number meaning, see CALL_* below. arg0 is always the
//           calling task's FindTask(NULL) pointer.
//   0x7C04  REG_CALL (write): call number -- the doorbell. The plugin's
//           write() export runs synchronously within this write, exactly
//           like the hostfs handler's REG_DOSPKT doorbell: it dma_reads
//           the argument block, drives smoltcp, and latches REG_RESULT
//           before the next guest instruction runs.
//   0x7C08  REG_RESULT (read): D0 result -- an fd, a byte count, 0, or -1
//           on error. RES_PENDING is a sentinel some calls (connect, recv,
//           WaitSelect) can return meaning "call it again" -- see CALL_*
//           below, and CALL_REGISTER_WAIT for what to do about it.
//   0x7C0C  REG_WAKE_TASK (read): the task pointer at the front of the
//           plugin's wake queue, or 0 if the queue is empty. Only the
//           installed interrupt server reads this.
//   0x7C10  REG_WAKE_SIGNAL (read): the signal *mask* (not bit number --
//           already shifted, ready for Signal()'s d0 input) to raise on
//           REG_WAKE_TASK, valid only while REG_WAKE_TASK != 0.
//   0x7C14  REG_WAKE_ACK (write): pop the front of the wake queue (any
//           value) -- the interrupt server writes this immediately after
//           Signal()ing the task it just read, then re-reads
//           REG_WAKE_TASK/REG_WAKE_SIGNAL to drain the rest of the queue
//           before returning.

#ifndef HOSTSOCKET_BOARD_H
#define HOSTSOCKET_BOARD_H

#define BOARD_MANUFACTURER 0x1448 // COPPERLINE_MANUFACTURER_ID -- see src/zorro.rs
#define BOARD_PRODUCT      0x06   // PRODUCT_HOSTSOCKET

#define ROM_OFFSET  0x0008
#define DIAG_OFFSET (ROM_OFFSET + 0x40)

#define REG_ARGPTR      0x7C00
#define REG_CALL        0x7C04
#define REG_RESULT      0x7C08
#define REG_WAKE_TASK   0x7C0C
#define REG_WAKE_SIGNAL 0x7C10
#define REG_WAKE_ACK    0x7C14

// Call numbers written to REG_CALL. The argument block is 8 LONGs at
// REG_ARGPTR, big-endian (same byte order as the 68k -- no translation
// needed); arg0 is always the calling task's pointer, arg1.. are
// call-specific:
//
//   CALL_SOCKET        arg1=domain arg2=type arg3=protocol
//                       -> REG_RESULT = fd, or -1
//   CALL_CONNECT       arg1=fd arg2=sockaddr_in addr (Amiga ptr) arg3=addrlen
//                       -> REG_RESULT = 0 (connected), -1 (error, errno set
//                          if registered), or RES_PENDING (see
//                          CALL_REGISTER_WAIT)
//   CALL_SEND          arg1=fd arg2=buf addr (Amiga ptr) arg3=len arg4=flags
//                       -> REG_RESULT = bytes sent, or -1
//   CALL_RECV          arg1=fd arg2=buf addr (Amiga ptr) arg3=len arg4=flags
//                       -> REG_RESULT = bytes received, -1, or RES_PENDING
//   CALL_CLOSESOCKET   arg1=fd
//                       -> REG_RESULT = 0, or -1
//   CALL_REGISTER_WAIT arg7=signal mask (already shifted, see
//                       REG_WAKE_SIGNAL) -- arg7 specifically, not arg1:
//                       WaitSelect (below) is the greediest real call and
//                       only reaches arg6, so arg7 can never collide with
//                       a real call's own in-flight arguments while this
//                       one is staged over the top of them. Tells the
//                       plugin "this task is now asleep waiting on
//                       whatever its last RES_PENDING call from this same
//                       task was about";
//                       the plugin already knows which fd/condition that
//                       was (see crates/hostsocket-plugin/src/lib.rs's `last_pending`).
//                       -> REG_RESULT unused; only call this once you are
//                          about to Wait() on that same mask.
//   CALL_IOCTLSOCKET   arg1=fd arg2=request (FIONBIO=0x8004667E is the only
//                       one implemented) arg3=argp (Amiga ptr to a LONG)
//                       -> REG_RESULT = 0, or -1
//   CALL_SETERRNOPTR   arg1=errno_ptr (Amiga ptr, or 0 to clear) arg2=size
//                       (2 or 4 bytes)
//                       -> REG_RESULT = 0 always
//   CALL_ERRNO         (no args beyond the task pointer)
//                       -> REG_RESULT = this task's last errno
//   CALL_WAITSELECT    arg1=nfds arg2=readfds addr (Amiga ptr to a ULONG
//                       bitmask, bit N = fd N) arg3=writefds addr
//                       arg4=exceptfds addr (0 = that set is not of
//                       interest) arg5=timeout addr (Amiga ptr to a real
//                       `struct timeval` -- devices/timer.h: two BE ULONGs,
//                       tv_secs then tv_micro, 8 bytes total -- or 0 to
//                       block indefinitely)
//                       arg6=signals addr (Amiga ptr to a ULONG, or 0 --
//                       real WaitSelect's own in/out "signals" parameter:
//                       on input, Amiga signal bits on the *calling task*
//                       to also wake for; on a signal-interrupted return,
//                       overwritten with whichever requested bits actually
//                       arrived. The plugin reads these directly out of
//                       the calling task's own struct Task in Amiga memory
//                       (see crates/hostsocket-plugin/src/lib.rs's task_sig_recvd) -- only
//                       checked at call time, not while already parked in
//                       Wait() on this call's own private wake signal, see
//                       do_wait_select's own comment for why)
//                       -> REG_RESULT = ready descriptor count (fd_sets
//                          rewritten in place via dma_write), 0 on
//                          timeout or signal interrupt, -1 on error, or
//                          RES_PENDING
//
// Phase 3 additions (server-side path, UDP, remaining common LVOs --
// DNS/gethostbyname deferred, see PROPOSAL.md):
//
//   CALL_BIND          arg1=fd arg2=sockaddr_in addr (Amiga ptr) arg3=namelen
//                       -> REG_RESULT = 0, or -1
//   CALL_LISTEN        arg1=fd arg2=backlog (accepted, not enforced --
//                       smoltcp has no connection-queue depth to bound)
//                       -> REG_RESULT = 0, or -1
//   CALL_ACCEPT        arg1=fd arg2=sockaddr_in out addr (Amiga ptr, or 0)
//                       arg3=addrlen (Amiga ptr to a LONG: in = buffer
//                       size, out = actual size written, or 0)
//                       -> REG_RESULT = the newly accepted fd, -1, or
//                          RES_PENDING (see CALL_REGISTER_WAIT)
//   CALL_SENDTO        arg1=fd arg2=buf addr arg3=len arg4=flags
//                       arg5=sockaddr_in dest addr (Amiga ptr, or 0 to use
//                       a peer already set by CALL_CONNECT) arg6=tolen
//                       -> REG_RESULT = bytes sent, or -1
//   CALL_RECVFROM      arg1=fd arg2=buf addr arg3=len arg4=flags
//                       arg5=sockaddr_in out addr (Amiga ptr, or 0)
//                       arg6=addrlen (Amiga ptr to a LONG, in/out, or 0)
//                       -> REG_RESULT = bytes received, -1, or RES_PENDING
//   CALL_SHUTDOWN      arg1=fd arg2=how (accepted but not distinguished --
//                       always a full close(), see crates/hostsocket-plugin/src/lib.rs)
//                       -> REG_RESULT = 0, or -1
//   CALL_SETSOCKOPT    arg1=fd arg2=level arg3=optname arg4=optval addr
//                       (Amiga ptr) arg5=optlen
//                       -> REG_RESULT = 0, or -1
//   CALL_GETSOCKOPT    arg1=fd arg2=level arg3=optname arg4=optval out addr
//                       (Amiga ptr) arg5=optlen (Amiga ptr to a LONG,
//                       in/out)
//                       -> REG_RESULT = 0, or -1
//   CALL_GETSOCKNAME   arg1=fd arg2=sockaddr_in out addr (Amiga ptr)
//                       arg3=namelen (Amiga ptr to a LONG, in/out)
//                       -> REG_RESULT = 0, or -1
//   CALL_GETPEERNAME   arg1=fd arg2=sockaddr_in out addr (Amiga ptr)
//                       arg3=namelen (Amiga ptr to a LONG, in/out)
//                       -> REG_RESULT = 0, or -1
//
// Phase 4 additions (Dup2Socket + the address-conversion utility LVOs --
// ObtainSocket/ReleaseSocket/ReleaseCopyOfSocket/SocketBaseTagList/
// GetSocketEvents/gethostby*/getservby*/getprotoby*/getnetby* all get real
// bodies too, documented in their own sections above/below; only
// SetSocketSignals/vsyslog stay `_hs_stub` -- see PROPOSAL.md's Phase 4
// scope decisions):
//
//   CALL_DUP2SOCKET    arg1=fd arg2=newfd (-1 = "any free fd", matching
//                       real Dup2Socket semantics; a specific target is
//                       allowed to just return -1, see crates/hostsocket-plugin/src/lib.rs)
//                       -> REG_RESULT = the duplicate fd (aliasing the
//                          same underlying socket), or -1
//   CALL_INET_NTOA     arg1=addr (in_addr_t, network byte order)
//                       arg2=bufaddr (Amiga ptr to the guest's own
//                       LIB_INETBUF scratch area -- the trampoline
//                       already knows this address and returns it
//                       directly, see entry.s's _hs_inet_ntoa)
//                       -> REG_RESULT = 0 always (writes the formatted
//                          "a.b.c.d\0" string to bufaddr)
//   CALL_INET_ADDR     arg1=straddr (Amiga ptr to a NUL-terminated
//                       dotted-quad string)
//                       -> REG_RESULT = the parsed in_addr_t (network
//                          byte order), or -1 (INADDR_NONE) if unparsable
//   CALL_INET_LNAOF    arg1=addr (in_addr_t, network byte order)
//                       -> REG_RESULT = the classful host part (host byte
//                          order)
//   CALL_INET_NETOF    arg1=addr (in_addr_t, network byte order)
//                       -> REG_RESULT = the classful network part (host
//                          byte order)
//   CALL_INET_MAKEADDR arg1=net arg2=host (both host byte order)
//                       -> REG_RESULT = the combined in_addr_t (network
//                          byte order)
//   CALL_INET_NETWORK  arg1=straddr (Amiga ptr to a NUL-terminated
//                       dotted-quad string)
//                       -> REG_RESULT = the parsed value (host byte
//                          order), or -1 if unparsable
//   CALL_GETDTABLESIZE (no args)
//                       -> REG_RESULT = MAX_FDS (see crates/hostsocket-plugin/src/lib.rs) --
//                          the plugin is the single source of truth for
//                          the fd table size, so this is a real RPC round
//                          trip rather than a guest-side constant that
//                          could drift from it
//
// gethostbyname (forward DNS, A records only -- see crates/hostsocket-plugin/src/lib.rs's
// module doc comment for why reverse/PTR lookup, gethostbyaddr, stays
// _hs_stub):
//
//   CALL_GETHOSTBYNAME arg1=name (Amiga ptr to a NUL-terminated hostname
//                       string) arg2=bufaddr (Amiga ptr to the guest's own
//                       LIB_HOSTENTBUF scratch area -- the trampoline
//                       already knows this address, same as
//                       CALL_INET_NTOA's bufaddr, see entry.s's
//                       _hs_gethostbyname)
//                       -> REG_RESULT = 0 (writes a real struct hostent,
//                          plus its h_aliases/h_addr_list arrays and the
//                          resolved address bytes and name string, all
//                          into bufaddr -- see crates/hostsocket-plugin/src/lib.rs's
//                          HOSTENT_*_OFF layout constants), -1 (lookup
//                          failed), or RES_PENDING (DNS takes real network
//                          round-trip time -- this is a
//                          CALL_REGISTER_WAIT-style blocking call, not a
//                          same-tick one like everything above)
//
// SocketBaseTagList (SET and GET(REF) for SBTC_ERRNOLONGPTR,
// SBTC_HERRNOLONGPTR, SBTC_SIGEVENTMASK, SBTC_BREAKMASK, and
// SBTC_DTABLESIZE; GET(REF)-only for the capability-detection tags
// SBTC_HAVE_DNS_API/SBTC_HAVE_LOCAL_DATABASE_API/
// SBTC_HAVE_ADDRESS_CONVERSION_API/SBTC_HAVE_GETHOSTADDR_R_API, always
// answering TRUE (this project implements all four families) -- real
// callers (curl's own amigaos.c among them) check these before ever
// calling getaddrinfo()/the *ent iterators/inet_aton and friends/
// gethostbyname_r, so leaving them unanswered would make those LVOs
// unreachable to compliant software regardless of how real the
// implementation behind them is -- see
// crates/hostsocket-plugin/src/lib.rs's do_socketbasetaglist):
//
//   CALL_SOCKETBASETAGLIST arg1=tags (Amiga ptr to a TagItem array: 8
//                           bytes/entry, BE ti_Tag then BE ti_Data,
//                           TAG_DONE (0)-terminated)
//                           arg2=errno_slot (Amiga ptr to this library's
//                           own LIB_ERRNO_SLOT scratch LONG -- entry.s's
//                           own comment on that field) arg3=herrno_slot
//                           (same, LIB_HERRNO_SLOT) -- both always passed,
//                           only consulted for a GETREF that needs a real
//                           guest address to hand back and has nothing
//                           more specific registered
//                           -> REG_RESULT = 0 always
//
// GetSocketEvents (SO_EVENTMASK/SBTC_SIGEVENTMASK async event notification
// -- see crates/hostsocket-plugin/src/lib.rs's do_get_socket_events/process_socket_events):
//
//   CALL_GETSOCKETEVENTS arg1=event_ptr (Amiga ptr to a ULONG the plugin
//                         writes the dequeued event's FD_* bitmask into --
//                         left untouched if REG_RESULT is -1)
//                         -> REG_RESULT = the fd an event fired on (also
//                            written to *event_ptr), or -1 if no events are
//                            pending. Never RES_PENDING: this call always
//                            polls, it never blocks -- real callers
//                            WaitSelect()/Wait() on the SBTC_SIGEVENTMASK
//                            signal first, then drain this in a loop.
//
// gethostname/gethostid (see crates/hostsocket-plugin/src/lib.rs's do_gethostname/
// do_gethostid):
//
//   CALL_GETHOSTNAME arg1=name (Amiga ptr to the caller's own buffer)
//                     arg2=namelen (its size)
//                     -> REG_RESULT = 0 (a NUL-terminated hostname string,
//                        truncated to fit `namelen` with no guaranteed
//                        trailing NUL if it doesn't, written to `name`) --
//                        never -1, this call can't fail once dispatched
//
//   CALL_GETHOSTID (no args)
//                   -> REG_RESULT = this interface's own address
//                      (crates/hostsocket-plugin/src/lib.rs's INTERFACE_ADDR) as a big-endian
//                      LONG -- never 0, matching bsdsocktest's own "returns
//                      non-zero" check
//
// sendmsg/recvmsg (TCP-only scatter/gather via a struct msghdr's
// msg_iov/msg_iovlen array -- msg_name/msg_control ignored, see
// crates/hostsocket-plugin/src/lib.rs's do_sendmsg/do_recvmsg):
//
//   CALL_SENDMSG arg1=sock arg2=msg (Amiga ptr to the caller's own struct
//                msghdr) arg3=flags
//                -> REG_RESULT = total bytes queued (may be less than the
//                   iovecs' combined length only in non-blocking mode),
//                   -1, or RES_PENDING (same blocking-retry shape as
//                   CALL_SEND)
//
//   CALL_RECVMSG arg1=sock arg2=msg arg3=flags
//                -> REG_RESULT = bytes received (scattered across the
//                   iovecs in order, each filled to its own iov_len
//                   before the next), 0 (clean EOF), -1, or RES_PENDING
//                   (same blocking-retry shape as CALL_RECV)
//
// gethostbyaddr (reverse/PTR DNS -- see crates/hostsocket-plugin/src/lib.rs's
// do_gethostbyaddr/parse_ptr_response):
//
//   CALL_GETHOSTBYADDR arg1=addr (Amiga ptr to a real struct in_addr --
//                       raw address bytes, not a string) arg2=len
//                       arg3=type (AF_INET) arg4=bufaddr (the guest's own
//                       LIB_HOSTENTBUF scratch area, same convention as
//                       CALL_GETHOSTBYNAME's own bufaddr)
//                       -> REG_RESULT = 0 (writes a real struct hostent
//                          into bufaddr, h_name the resolved PTR target),
//                          -1 (lookup failed), or RES_PENDING (a real DNS
//                          round trip, same CALL_REGISTER_WAIT-style
//                          blocking call as CALL_GETHOSTBYNAME)
//
// ObtainSocket/ReleaseSocket/ReleaseCopyOfSocket (the shared socket-pool
// transfer mechanism -- see crates/hostsocket-plugin/src/lib.rs's do_obtain_socket/
// do_release_socket/do_release_copy_of_socket):
//
//   CALL_OBTAINSOCKET arg1=id arg2=domain arg3=type arg4=protocol
//                      -> REG_RESULT = the new fd (removed from the pool,
//                         inserted into the caller's own fd table), or -1
//                         if no pool entry matches `id`/domain/type/protocol
//
//   CALL_RELEASESOCKET arg1=sock arg2=id (a non-negative caller-chosen
//                       key, or -1/UNIQUE_ID to have the plugin assign
//                       one)
//                       -> REG_RESULT = the effective key (== arg2 unless
//                          UNIQUE_ID was given), or -1. `sock` is invalid
//                          in the caller's own fd table after this, same
//                          as CloseSocket() -- the underlying socket
//                          moves to the pool instead of being destroyed
//
//   CALL_RELEASECOPYOFSOCKET arg1=sock arg2=id
//                             -> REG_RESULT = the effective key, or -1.
//                                Same as CALL_RELEASESOCKET, except
//                                `sock` stays valid in the caller's own
//                                fd table (a pool *alias* is inserted,
//                                not a move)
//
// getservbyname/getservbyport/getprotobyname/getprotobynumber/
// getnetbyname/getnetbyaddr (small static well-known-name tables -- see
// crates/hostsocket-plugin/src/lib.rs's SERVICES/PROTOCOLS/NETWORKS and do_getservbyname
// and friends): all pure local lookups, no network round trip, so none
// of these ever return RES_PENDING.
//
//   CALL_GETSERVBYNAME arg1=name (Amiga ptr to a NUL-terminated string)
//                       arg2=proto (Amiga ptr to a NUL-terminated string,
//                       or 0 for "any protocol") arg3=bufaddr (the
//                       guest's own LIB_SERVENTBUF scratch area)
//                       -> REG_RESULT = 0 (writes a real struct servent
//                          into bufaddr), or -1 (no match)
//
//   CALL_GETSERVBYPORT arg1=port arg2=proto (same as CALL_GETSERVBYNAME's
//                       own arg2) arg3=bufaddr (LIB_SERVENTBUF)
//                       -> REG_RESULT = 0 or -1, same as
//                          CALL_GETSERVBYNAME
//
//   CALL_GETPROTOBYNAME arg1=name arg2=bufaddr (the guest's own
//                        LIB_PROTOENTBUF scratch area)
//                        -> REG_RESULT = 0 (writes a real struct protoent
//                           into bufaddr), or -1 (no match)
//
//   CALL_GETPROTOBYNUMBER arg1=proto arg2=bufaddr (LIB_PROTOENTBUF)
//                          -> REG_RESULT = 0 or -1, same as
//                             CALL_GETPROTOBYNAME
//
//   CALL_GETNETBYNAME  arg1=name arg2=bufaddr (the guest's own
//                       LIB_NETENTBUF scratch area)
//                       -> REG_RESULT = 0 (writes a real struct netent
//                          into bufaddr), or -1 (no match)
//
//   CALL_GETNETBYADDR  arg1=net (a raw network number, not a packed
//                       struct in_addr) arg2=type (AF_INET; anything else
//                       is rejected) arg3=bufaddr (LIB_NETENTBUF)
//                       -> REG_RESULT = 0 or -1, same as CALL_GETNETBYNAME
//
// AmiTCP 4.0 tail additions: the real bsdsocket_lib.fd LVO order continues
// well past GetSocketEvents (LVO -300, Phase 4's own ceiling) -- see
// entry.s's jump-table comment for the full accounting of everything
// between there and the real table's own end (-858, per the authoritative
// bsdsocket_lib.sfd v1.12 -- ObtainServerSocket at -696 was this project's
// own first, too-early stopping point, not the real end), most of which
// stays _hs_stub because it has no equivalent in this project's model (raw
// packet capture, host routing tables, live interface reconfiguration,
// direct BSD mbuf-chain manipulation, Roadshow's own global-data-access
// functions). These are the ones that do fit -- inet_aton/inet_ntop/
// inet_pton round out the existing Inet_*/inet_* family, In_LocalAddr/
// In_CanForward are small local predicates, and the three *ent iterator
// triples walk the same static SERVICES/PROTOCOLS/NETWORKS tables
// getservbyname and friends already use (see crates/hostsocket-plugin/
// src/lib.rs's do_inet_aton and neighbors):
//
//   CALL_INET_ATON     arg1=cp (Amiga ptr to a NUL-terminated dotted-quad
//                       string) arg2=out (Amiga ptr to a struct in_addr,
//                       written on success only)
//                       -> REG_RESULT = 1 (parsed, *out written) or 0
//                          (unparsable, *out untouched) -- the inverse of
//                          CALL_INET_ADDR's -1-on-failure convention,
//                          matching real inet_aton()'s int/bool return
//
//   CALL_INET_NTOP     arg1=af (AF_INET; anything else fails) arg2=src
//                       (Amiga ptr to 4 raw address bytes) arg3=dst (Amiga
//                       ptr to the caller's own buffer) arg4=size (its
//                       length)
//                       -> REG_RESULT = dst (the formatted "a.b.c.d\0"
//                          string written there) on success, or 0 (NULL)
//                          if size is too small or af isn't AF_INET
//
//   CALL_INET_PTON     arg1=af (AF_INET; anything else fails) arg2=src
//                       (Amiga ptr to a NUL-terminated dotted-quad string)
//                       arg3=dst (Amiga ptr to a struct in_addr, written on
//                       success only)
//                       -> REG_RESULT = 1 (parsed, *dst written), 0
//                          (unparsable), or -1 (af not AF_INET)
//
//   CALL_IN_LOCALADDR  arg1=addr (in_addr_t, network byte order)
//                       -> REG_RESULT = 1 if addr falls inside one of this
//                          interface's own configured subnets (including
//                          127.0.0.0/8), 0 otherwise
//
//   CALL_IN_CANFORWARD arg1=addr (in_addr_t, network byte order)
//                       -> REG_RESULT = 1 if addr is a plausible unicast
//                          address eligible for forwarding (not class D/E,
//                          not net 0 or 127), 0 otherwise
//
//   CALL_SETSERVENT/CALL_SETPROTOENT/CALL_SETNETENT arg1=stay_open
//                       (accepted, not distinguished -- see
//                       do_setservent's own comment)
//                       -> REG_RESULT = 0 always; rewinds this task's own
//                          cursor into SERVICES/PROTOCOLS/NETWORKS to the
//                          start
//
//   CALL_ENDSERVENT/CALL_ENDPROTOENT/CALL_ENDNETENT (no args)
//                       -> REG_RESULT = 0 always; same rewind as the
//                          matching CALL_SET*ENT above
//
//   CALL_GETSERVENT arg1=bufaddr (LIB_SERVENTBUF)
//   CALL_GETPROTOENT arg1=bufaddr (LIB_PROTOENTBUF)
//   CALL_GETNETENT arg1=bufaddr (LIB_NETENTBUF)
//                       -> REG_RESULT = 0 (writes the table entry at this
//                          task's current cursor into bufaddr and advances
//                          it), or -1 once the cursor runs past the end of
//                          the table (real *ent() NULL-on-exhaustion,
//                          same 0/-1-then-bufaddr convention as
//                          CALL_GETSERVBYNAME and the rest of this family)
//
// Roadshow's own resolver-family extension (RFC 3493 getaddrinfo/
// getnameinfo, plus BSD-style reentrant gethostbyname_r/gethostbyaddr_r),
// past the AmiTCP-4.0-compatible tail above -- see
// crates/hostsocket-plugin/src/lib.rs's do_getaddrinfo and neighbors for
// the deliberate simplifications versus the full RFC 3493 contract.
// freeaddrinfo() needs no CALL_* at all: it's a guest-side no-op (see
// entry.s's own comment on why there is nothing to free here).
//
//   CALL_GAI_STRERROR  arg1=errnum (an EAI_* code, netdb.h) arg2=bufaddr
//                       (the guest's own LIB_GAIBUF scratch area)
//                       -> REG_RESULT = 0 always (writes a NUL-terminated
//                          message string to bufaddr; an unrecognized code
//                          still gets a real "Unknown error." string)
//
//   CALL_GETADDRINFO   arg1=hostname (Amiga ptr to a NUL-terminated string,
//                       or 0) arg2=servname (ditto, or 0) arg3=hints (Amiga
//                       ptr to a struct addrinfo used only for its
//                       ai_flags/ai_family/ai_socktype/ai_protocol fields,
//                       or 0) arg4=res (Amiga ptr to a struct addrinfo*,
//                       written on a definite return only) arg5=bufaddr
//                       (the guest's own LIB_ADDRINFOBUF scratch area)
//                       -> REG_RESULT = 0 (success; *res = bufaddr) or a
//                          negative EAI_* code (*res = 0), or RES_PENDING
//                          if `hostname` needs a real DNS round trip (same
//                          blocking shape as CALL_GETHOSTBYNAME)
//
//   CALL_GETNAMEINFO   arg1=sa (Amiga ptr to a sockaddr_in) arg2=salen
//                       arg3=host (Amiga ptr to the caller's own buffer, or
//                       0) arg4=hostlen arg5=serv (ditto, or 0) arg6=servlen
//                       arg7=flags (NI_* bits, netdb.h)
//                       -> REG_RESULT = 0 (host/serv written, truncated to
//                          fit) or a negative EAI_* code -- never
//                          RES_PENDING, see do_getnameinfo's own comment
//                          for why `host` is always numeric here
//
//   CALL_GETHOSTBYNAME_R arg1=name arg2=hp (Amiga ptr to the caller's own
//                       struct hostent shell) arg3=buf (Amiga ptr to the
//                       caller's own storage for the variable-length parts)
//                       arg4=buflen arg5=he (Amiga ptr to a LONG, written
//                       with an h_errno-style code on failure, 0 on
//                       success)
//                       -> REG_RESULT = 0 (hp filled in, trampoline
//                          returns hp) or -1 (NULL; ERANGE in *he if buf
//                          was too small), or RES_PENDING (same DNS shape
//                          as CALL_GETHOSTBYNAME)
//
//   CALL_GETHOSTBYADDR_R arg1=addr arg2=len arg3=type arg4=hp arg5=buf
//                       arg6=buflen arg7=he
//                       -> REG_RESULT = 0, -1, or RES_PENDING, same
//                          conventions as CALL_GETHOSTBYNAME_R but for a
//                          reverse (PTR) lookup -- shares the same single
//                          in-flight-PTR-query engine CALL_GETHOSTBYADDR
//                          uses (see PtrQuery::dest's own comment)
#define CALL_SOCKET        0
#define CALL_CONNECT       1
#define CALL_SEND          2
#define CALL_RECV          3
#define CALL_CLOSESOCKET   4
#define CALL_REGISTER_WAIT 5
#define CALL_IOCTLSOCKET   6
#define CALL_SETERRNOPTR   7
#define CALL_ERRNO         8
#define CALL_WAITSELECT    9
#define CALL_BIND          10
#define CALL_LISTEN        11
#define CALL_ACCEPT        12
#define CALL_SENDTO        13
#define CALL_RECVFROM      14
#define CALL_SHUTDOWN      15
#define CALL_SETSOCKOPT    16
#define CALL_GETSOCKOPT    17
#define CALL_GETSOCKNAME   18
#define CALL_GETPEERNAME   19
#define CALL_DUP2SOCKET    20
#define CALL_INET_NTOA     21
#define CALL_INET_ADDR     22
#define CALL_INET_LNAOF    23
#define CALL_INET_NETOF    24
#define CALL_INET_MAKEADDR 25
#define CALL_INET_NETWORK  26
#define CALL_GETDTABLESIZE 27
#define CALL_GETHOSTBYNAME 28
#define CALL_SOCKETBASETAGLIST 29
#define CALL_GETSOCKETEVENTS 30
#define CALL_GETHOSTNAME 31
#define CALL_GETHOSTID 32
#define CALL_SENDMSG 33
#define CALL_RECVMSG 34
#define CALL_GETHOSTBYADDR 35
#define CALL_OBTAINSOCKET 36
#define CALL_RELEASESOCKET 37
#define CALL_RELEASECOPYOFSOCKET 38
#define CALL_GETSERVBYNAME 39
#define CALL_GETSERVBYPORT 40
#define CALL_GETPROTOBYNAME 41
#define CALL_GETPROTOBYNUMBER 42
#define CALL_GETNETBYNAME 43
#define CALL_GETNETBYADDR 44
#define CALL_INET_ATON 45
#define CALL_INET_NTOP 46
#define CALL_INET_PTON 47
#define CALL_IN_LOCALADDR 48
#define CALL_IN_CANFORWARD 49
#define CALL_SETSERVENT 50
#define CALL_ENDSERVENT 51
#define CALL_GETSERVENT 52
#define CALL_SETPROTOENT 53
#define CALL_ENDPROTOENT 54
#define CALL_GETPROTOENT 55
#define CALL_SETNETENT 56
#define CALL_ENDNETENT 57
#define CALL_GETNETENT 58
#define CALL_GAI_STRERROR 59
#define CALL_GETADDRINFO 60
#define CALL_GETNAMEINFO 61
#define CALL_GETHOSTBYNAME_R 62
#define CALL_GETHOSTBYADDR_R 63

// REG_RESULT sentinel: the call hasn't completed yet (smoltcp needs more
// tick()s -- e.g. the TCP handshake hasn't finished, or no data is buffered
// yet); in blocking mode the trampoline registers a wait (CALL_REGISTER_WAIT)
// and re-issues the same call after waking, instead of Phase 1's spin. Not a
// valid fd/byte-count/error value (all real results are >= -1).
#define RES_PENDING (-2)

// The only IoctlSocket request Phase 2 implements. Computed from the real
// _IOW('f', 126, __LONG) macro (sys/ioccom.h), not guessed: IOC_IN
// (0x80000000) | (sizeof(LONG)=4 << 16) | ('f'=0x66 << 8) | 126 (0x7E).
#define FIONBIO 0x8004667E

#endif // HOSTSOCKET_BOARD_H
