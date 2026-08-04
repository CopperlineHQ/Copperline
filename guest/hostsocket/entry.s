| SPDX-License-Identifier: GPL-3.0-or-later
|
| Entry table, DiagArea, and bsdsocket.library LVO trampolines -- the whole
| guest-side ROM. Linked first (see Makefile), so this sits at ROM_OFFSET in
| the board window (see hostsocket_board.h). Everything stays PC-relative:
| the ROM runs at whatever base autoconfig assigns, and it is copied to RAM
| before da_DiagPoint runs (see the DiagArea comment below), so even
| da_DiagPoint's own code must not assume it runs from its original address.
|
| Adapted from guest/services/entry.s (same
| entry-table / DiagArea / PC-relative structure, same "no .long directly
| naming an external symbol under -mpcrel" discipline -- see that file's
| own "HARD-WON" comment: such a directive resolves PC-relative to the
| field's own address, not the intended target, silently producing garbage
| that is invisible until it corrupts a real boot). This board is simpler
| than the hostfs handler that reference file drives: bsdsocket.library is
| a plain NT_LIBRARY, not a DOS handler, so there is no BootNode / mount
| table / DosList surgery at all.
|
| da_DiagPoint does NOT build the library. An earlier version of this file
| called MakeLibrary/AddLibrary directly from da_DiagPoint, which installed
| the library fine but then silently corrupted the boot a few seconds
| later on real Kickstart 1.3 and 3.1 alike (found during Phase 4's
| end-to-end verification pass -- AROS tolerates it, real Kickstart does
| not). The reference file above hit the identical failure mode first and
| documents the fix in its own header comment: defer real library/resident
| construction out of da_DiagPoint entirely, into rt_Init, called by
| Kickstart's normal cold-start resident scan once every board has been
| DiagPoint-ed (RKRM Libraries, "Expansion Library", "Events At ROMTAG INIT
| Time" -- the same deferral the A590 SCSI boot ROM's own Romtag uses).
| da_DiagPoint here now only patches the Romtag's PC-relative fields in the
| RAM diag copy and returns D0 != 0 so Kickstart keeps that copy around for
| the resident scan to find; _resident_init (reached via rt_Init, near
| _install_int_server below) does the actual MakeLibrary + AddLibrary +
| interrupt-server install, re-deriving the board base itself via
| expansion.library's GetCurrentBinding() (see stub.c's hs_get_board_base)
| since none of da_DiagPoint's own registers are handed to rt_Init.
|
| Layout note (same constraint as the reference file): only the entry table
| and _diag_entry itself may precede the DiagArea's `.org 0x40` -- Kickstart
| copies exactly da_Size bytes starting at that fixed offset, so everything
| else lives *after* _diag_area_end instead. Ordinary PC-relative addressing
| (lea label(pc),...) still reaches all of it from _diag_entry regardless of
| the gap, since _diag_entry runs from its real ROM location (da_DiagPoint
| jsr's into it via the *original* board base in A0, not the RAM copy -- see
| _diag_point below). _diag_entry itself must also stay small enough to fit
| that same budget, which is why installing the interrupt server is a
| separate `bsr` out to `_install_int_server` (unconstrained, placed after
| the DiagArea) rather than inline.
|
| The Makefile forces C-preprocessor treatment of this file (-x
| assembler-with-cpp) so it can #include hostsocket_board.h instead of
| duplicating the register offsets and call numbers as bare numeric
| literals (a plain ".s" name isn't preprocessed by default, and this
| repo's filesystem conventions rule out the usual ".S" alternative).

#include "hostsocket_board.h"

| Extra fields appended after the standard 34-byte struct Library in our
| library's own data area (see MakeLibrary's dSize below).
| LIB_BOARDBASE: the Zorro board window's base address, stashed once at
|   library-build time (see _lib_init) by smuggling it through MakeLibrary's
|   segList parameter -- nothing DOS-related ever inspects that field here,
|   so it is free to repurpose as a plain APTR carrier.
| LIB_ARGBLK: the 8-LONG argument block the RPC trampolines below stage
|   their call arguments into, matching hostsocket_board.h's REG_ARGPTR
|   contract (arg0 is always the calling task's own pointer). Ordinary
|   Amiga RAM (the library base itself), so the plugin's dma_read reaches
|   it exactly like the hostfs handler's DosPacket.
| LIB_INTERRUPT: a struct Interrupt (exec/interrupts.h), built at runtime
|   (its is_Code field needs a real absolute address, which can't be a
|   compile-time ROM constant -- same PIC problem _lib_init already solves
|   for ln_Name/lib_IdString) and installed once via AddIntServer in
|   _install_int_server. is_Data is set to the board base, so the handler
|   (_int_handler) gets it for free in A1 on every invocation.
| LIB_INETBUF: a 20-byte scratch buffer for Inet_NtoA's returned string
|   (Phase 4) -- real Inet_NtoA has no caller-supplied buffer parameter, so
|   like every real bsdsocket.library it owns a fixed buffer of its own;
|   20 bytes comfortably fits the longest possible "255.255.255.255\0" (16
|   bytes) with room to spare. Genuine writable Amiga RAM (same as
|   LIB_ARGBLK), so the plugin's dma_write reaches it exactly like a
|   recv() buffer.
| LIB_HOSTENTBUF: a 124-byte scratch buffer for gethostbyname's returned
|   struct hostent -- same reasoning as LIB_INETBUF (real gethostbyname
|   has no caller-supplied buffer either, so bsdsocket.library owns one),
|   sized to match crates/hostsocket-plugin/src/lib.rs's HOSTENT_BUF_LEN exactly (kept in
|   sync by hand, like every other offset the guest ROM and plugin share).
| LIB_ERRNO_SLOT/LIB_HERRNO_SLOT: two 4-byte scratch LONGs backing
|   SocketBaseTags(SBTM_GETREF(SBTC_ERRNOLONGPTR/SBTC_HERRNOLONGPTR)) --
|   real bsdsocket.library owns its own errno/h_errno storage a caller
|   can ask for the *address* of (rather than always supplying one via
|   SET, like SetErrnoPtr/SBTM_SETVAL do), so this library needs a real,
|   valid guest RAM address to hand back too. `_hs_socketbasetaglist`
|   passes both addresses down on every call (crates/hostsocket-plugin/src/lib.rs's
|   do_socketbasetaglist decides whether to actually use LIB_ERRNO_SLOT --
|   only if the task hasn't already registered its own pointer via an
|   earlier SET).
| LIB_SERVENTBUF/LIB_PROTOENTBUF/LIB_NETENTBUF: scratch buffers for
|   getservbyname()/getservbyport()'s struct servent, getprotobyname()/
|   getprotobynumber()'s struct protoent, and getnetbyname()/
|   getnetbyaddr()'s struct netent -- same "library owns the returned
|   struct, caller has no buffer of their own to supply" reasoning as
|   LIB_HOSTENTBUF, sized to match crates/hostsocket-plugin/src/lib.rs's
|   SERVENT_BUF_LEN/PROTOENT_BUF_LEN/NETENT_BUF_LEN exactly (kept in sync
|   by hand, like every other offset the guest ROM and plugin share).
#define LIB_BOARDBASE 34
#define LIB_ARGBLK    38
#define LIB_INTERRUPT 70 /* LIB_ARGBLK + 8*4 */
#define LIB_INETBUF   92 /* LIB_INTERRUPT + 22 */
#define LIB_HOSTENTBUF 112 /* LIB_INETBUF + 20 */
#define LIB_ERRNO_SLOT 236 /* LIB_HOSTENTBUF + 124 */
#define LIB_HERRNO_SLOT 240 /* LIB_ERRNO_SLOT + 4 */
#define LIB_SERVENTBUF 244 /* LIB_HERRNO_SLOT + 4 */
#define LIB_PROTOENTBUF 304 /* LIB_SERVENTBUF + 60 */
#define LIB_NETENTBUF 336 /* LIB_PROTOENTBUF + 32 */
#define LIB_DATASIZE  372 /* sizeof(struct Library) [34] + BOARDBASE [4] + ARGBLK [32] + Interrupt [22] + InetBuf [20] + HostEntBuf [124] + ErrnoSlot [4] + HerrnoSlot [4] + ServEntBuf [60] + ProtoEntBuf [32] + NetEntBuf [36] */

#define LN_TYPE  8
#define LN_NAME  10
#define LIB_FLAGS 14
#define LIB_VERSION 20
#define LIB_REVISION 22
#define LIB_IDSTRING 24

| struct Interrupt (exec/interrupts.h), offsets relative to LIB_INTERRUPT:
|   is_Node (struct Node, 14 bytes: ln_Succ/ln_Pred/ln_Type/ln_Pri/ln_Name)
|   is_Data (APTR, 4 bytes)
|   is_Code (code ptr, 4 bytes)
#define IS_TYPE (LIB_INTERRUPT + 8)
#define IS_NAME (LIB_INTERRUPT + 10)
#define IS_DATA (LIB_INTERRUPT + 14)
#define IS_CODE (LIB_INTERRUPT + 18)

#define NT_LIBRARY 9
#define NT_INTERRUPT 2
#define LIBF_SUMUSED 4
#define INTB_PORTS 3 /* hardware/intbits.h -- I/O ports and timers */

#define LVO_MAKELIBRARY  -84
#define LVO_ADDLIBRARY   -396
#define LVO_ADDINTSERVER -168
#define LVO_FORBID       -132
#define LVO_PERMIT       -138
#define LVO_FINDTASK     -294
#define LVO_ALLOCSIGNAL  -330
#define LVO_FREESIGNAL   -336
#define LVO_WAIT         -318
#define LVO_SIGNAL       -324

| Every Exec LVO call from library-user context needs A6 swapped from our
| own SocketBase to ExecBase and back (the standard `move.l 4.w,a6` trick
| always works, regardless of what A6 held before) -- one macro instead of
| repeating the four-instruction dance at every call site.
	.macro	EXECLVO offset
	move.l	a6,-(sp)
	move.l	4.w,a6
	jsr	\offset(a6)
	movea.l	(sp)+,a6
	.endm

	.text
	.globl	_entry_table

_entry_table:
	| +0: process entry -- unused. bsdsocket.library is a plain
	| NT_LIBRARY, not a DOS handler, so no RunHandler ever jumps here;
	| the slot is kept only so the entry table shape matches
	| copperline_board.h's convention.
	rts
	nop

	| +4: rt_Init entry. Kickstart's cold-start resident scan jsr's here
	| (patched into the Romtag's rt_Init field by da_DiagPoint below,
	| board base + this offset + ROM_OFFSET -- the same bias _diag_point's
	| own jsr target uses) once expansion has finished DiagPoint-ing every
	| board. A local trampoline rather than branching straight to
	| _resident_init: this slot must stay at a fixed, small offset from
	| _entry_table (same layout-note budget da_DiagPoint's own target
	| does), while _resident_init itself is free to live anywhere else in
	| the file, same as _install_int_server.
_rt_init_entry:
	bra.w	_resident_init

	| +8: expansion-init entry. The DiagArea's DiagPoint jsr's here from
	| the diag copy with the documented DiagPoint registers still live:
	| A0 = board base, A2 = base of the RAM diag copy Kickstart just
	| made, A6 = ExecBase (see libraries/configregs.h's DiagArea
	| calling-convention block -- confirmed against the NDK autodocs, not
	| assumed). Patches the Romtag's PC-relative fields into the diag
	| copy and returns -- see this file's header comment for why building
	| the library itself must NOT happen here.
_diag_entry:
	move.l	a2,d0
	add.l	d0,(_rt_match-_diag_area)(a2)
	add.l	d0,(_rt_end-_diag_area)(a2)
	add.l	d0,(_rt_name-_diag_area)(a2)
	add.l	d0,(_rt_id-_diag_area)(a2)
	| rt_Init stays resident code (in the persistent board window, not
	| the diag copy Kickstart may discard), so it's patched with the
	| board base (a0) instead.
	move.l	a0,d0
	add.l	d0,(_rt_init-_diag_area)(a2)
	moveq	#1,d0			| keep the diag copy: Kickstart's
					| cold-start resident scan needs it to
					| find our patched Romtag once every
					| board has been DiagPoint-ed.
	rts

	| struct DiagArea (libraries/configvars.h/configregs.h), at the fixed
	| ROM offset DIAG_AREA_IN_ROM: er_InitDiagVec points here and
	| Kickstart copies da_Size bytes to RAM before calling da_DiagPoint.
	| All code/name offsets are relative to the copy, so da_DiagPoint
	| reaches the real ROM through A0 (the board base) rather than
	| branching within the copy -- a bsr/bra here would aim into the
	| copy, not the live ROM.
	.org	0x40		| errors out if the code above grows past this
_diag_area:
	.byte	0x90, 0x00		| da_Config = DAC_WORDWIDE | DAC_CONFIGTIME;
					| da_Flags. Matches the proven, working
					| hostfs pattern exactly (copperline_
					| board.h's entry.s uses the same 0x90) --
					| an earlier DAC_NEVER version of this
					| byte, on the theory that da_DiagPoint's
					| own invocation is unconditional, left
					| Kickstart reading exactly one byte of
					| this DiagArea and going no further, so
					| that theory was wrong in practice
					| (whatever the docs suggest, DAC_NEVER is
					| not what real Kickstart 3.x expects
					| here). DAC_CONFIGTIME needs a non-zero
					| da_BootPoint (see _boot_point below) --
					| the "hard-won" pitfall this project's
					| own header comments already flagged.
	.short	_diag_area_end-_diag_area	| da_Size
	.short	_diag_point-_diag_area		| da_DiagPoint
	.short	_boot_point-_diag_area		| da_BootPoint
	.short	_diag_name-_diag_area		| da_Name
	.short	0, 0				| da_Reserved01/02
_diag_point:
	jsr	(_diag_entry-_entry_table+8)(a0) | +8 = ROM_OFFSET
	rts
_boot_point:
	| Never a real boot candidate (bsdsocket.library has nothing to boot);
	| this only exists because DAC_CONFIGTIME requires a non-zero
	| da_BootPoint to be present at all. Decline immediately.
	moveq	#0,d0
	rts

	| struct Resident ("Romtag"; exec/resident.h), scanned for by
	| Kickstart's normal cold-start resident-module init once expansion
	| has finished DiagPoint-ing every board (the same pass that inits
	| dos.library itself) -- see this file's header comment and
	| _diag_entry above for why real library construction is deferred
	| here instead of happening directly from da_DiagPoint. rt_Init is
	| called with D0=0, A0=NULL segList, A6=ExecBase; _resident_init
	| (below, near _install_int_server) re-derives the board base itself
	| via expansion.library's GetCurrentBinding(), since none of
	| da_DiagPoint's own registers are handed to it directly.
_romtag:
	.short	0x4AFC				| rt_MatchWord (RTC_MATCHWORD)
_rt_match:
	.long	_romtag-_diag_area		| rt_MatchTag (patched: +diag copy)
_rt_end:
	.long	_diag_area_end-_diag_area	| rt_EndSkip (patched: +diag copy)
	.byte	1				| rt_Flags = RTF_COLDSTART
	.byte	0				| rt_Version
	.byte	NT_LIBRARY			| rt_Type
	.byte	20				| rt_Pri
_rt_name:
	.long	_diag_name-_diag_area		| rt_Name (patched: +diag copy)
_rt_id:
	.long	_diag_name-_diag_area		| rt_IdString (patched: +diag copy)
_rt_init:
	.long	_rt_init_entry-_entry_table+8	| rt_Init (patched: +board base)
_diag_name:
	.asciz	"HostSocket"
	.balign	2
_diag_area_end:

	| rt_Init: called by Kickstart's cold-start resident scan (D0=0,
	| A0=NULL segList, A6=ExecBase) once expansion has DiagPoint-ed every
	| board -- the documented, hardware-proven place to build a resident
	| library (RKRM Libraries, "Expansion Library", "Events At ROMTAG
	| INIT Time"). None of da_DiagPoint's own registers are handed to
	| rt_Init, so hs_get_board_base (stub.c) re-opens expansion.library
	| and calls GetCurrentBinding() for our ConfigDev instead -- the same
	| recipe Copperline's own guest/services/handler.c
	| resident_init() uses, confirmed safe there on real Kickstart
	| 1.3/2.0/3.1. Once the board base is back in hand, this is exactly
	| the MakeLibrary/AddLibrary/interrupt-server sequence that used to
	| run directly from da_DiagPoint (see this file's header comment for
	| why that was wrong on real hardware).
_resident_init:
	| rt_Init's documented contract only says D0=0/A0=NULL/A6=ExecBase on
	| entry -- it says nothing about being free to clobber every other
	| register, and real Kickstart 1.3's own cold-start Romtag-scan loop
	| turns out to keep live state in registers across the rt_Init call
	| (confirmed the hard way: an earlier version of this routine used a3
	| for the library base without saving it, and the scan loop's own a3
	| -- silently overwritten with our library base -- came back into use
	| moments later, making Kickstart JSR LVO -126 on what it still
	| believed was its own a3 value; see PROPOSAL.md's Phase 4
	| verification notes). Copperline's own guest/services/handler.c
	| never hit this: its resident_init() is plain C, and the compiler
	| generates this exact save/restore automatically. Preserve every
	| register this routine uses as scratch (d1-d7/a1-a5); d0/a0 are the
	| documented inputs (0/NULL), not values the caller needs back, and
	| a6/a7 are never repointed away from ExecBase/the stack here.
	movem.l	d1-d7/a1-a5,-(sp)
	bsr	_hs_get_board_base	| -> d0 = board base (APTR), or 0 on
					| failure (expansion.library wouldn't
					| open, or we have no current binding)
	tst.l	d0
	beq.s	1f
	movea.l	d0,a0			| a0 = board base, same register
					| _diag_entry used to receive it in
	move.l	a0,d1			| segList param to MakeLibrary, repurposed
					| to smuggle the board base through to
					| _lib_init (see below) -- nothing here
					| is really AmigaDOS, so the field is
					| free to carry whatever we want.
	lea	_func_table(pc),a0	| vectors
	suba.l	a1,a1			| structure = NULL (no InitStruct table)
	lea	_lib_init(pc),a2	| init
	move.l	#LIB_DATASIZE,d0	| dSize
	jsr	LVO_MAKELIBRARY(a6)
	tst.l	d0
	beq.s	1f
	movea.l	d0,a3			| a3 = libbase, held across AddLibrary
					| and the interrupt-server install below
					| (neither call is documented to
					| preserve a0/a1/d0/d1, so this handoff
					| deliberately uses a register outside
					| that scratch set instead).
	move.l	d0,a1
	jsr	LVO_ADDLIBRARY(a6)
	bsr	_install_int_server	| in: a3 = libbase, a6 = ExecBase (both
					| still true here -- neither has been
					| swapped yet in this routine)
1:
	movem.l	(sp)+,d1-d7/a1-a5
	rts

	| Builds and installs the interrupt server that turns int2() (raised
	| by the plugin whenever a blocked task's condition becomes ready --
	| see _ring_doorbell_blocking / crates/hostsocket-plugin/src/lib.rs's wake queue) into a
	| real Signal() to the waiting task(s). is_Code needs a real absolute
	| address, which is a runtime computation (lea label(pc)), not a
	| compile-time ROM constant -- exactly the same PIC constraint
	| _lib_init already works around for ln_Name/lib_IdString.
_install_int_server:
	| in: a3 = libbase, a6 = ExecBase
	move.b	#NT_INTERRUPT,IS_TYPE(a3)
	move.b	#0,IS_TYPE+1(a3)	| is_Node.ln_Pri = 0
	lea	_int_name(pc),a0
	move.l	a0,IS_NAME(a3)
	move.l	LIB_BOARDBASE(a3),IS_DATA(a3)	| is_Data = board base, so
					| _int_handler gets it for free in A1
	lea	_int_handler(pc),a0
	move.l	a0,IS_CODE(a3)
	moveq	#INTB_PORTS,d0
	lea	LIB_INTERRUPT(a3),a1
	jsr	LVO_ADDINTSERVER(a6)
	rts

	| Called by MakeLibrary before adding the library to the system.
	| Registers per the exec.doc SYNOPSIS (verified against the NDK
	| autodocs, not assumed): d0 = libAddr, a0 = segList (our smuggled
	| board base), a6 = ExecBase. Must return the library address in d0,
	| or 0 to fail (in which case we would have to free libAddr
	| ourselves -- not needed here, this never fails).
_lib_init:
	movea.l	d0,a1			| a1 = libAddr
	move.b	#NT_LIBRARY,LN_TYPE(a1)
	move.b	#0,LN_TYPE+1(a1)	| ln_Pri = 0
	lea	_lib_name(pc),a2
	move.l	a2,LN_NAME(a1)
	move.b	#LIBF_SUMUSED,LIB_FLAGS(a1)
	| 4, not 1: real bsdsocket.library implementations (AmiTCP, Roadshow,
	| Miami) all report version >= 3, and conformance tools check for it
	| -- bsdsocktest's own OpenLibrary("bsdsocket.library", 4) call
	| refuses to open anything reporting a lower version, discovered the
	| hard way running that suite against this project for the first
	| time (see PROPOSAL.md's Testing section).
	move.w	#4,LIB_VERSION(a1)
	move.w	#0,LIB_REVISION(a1)
	lea	_lib_idstring(pc),a2
	move.l	a2,LIB_IDSTRING(a1)
	move.l	a0,LIB_BOARDBASE(a1)	| stash the board base for the RPC
					| trampolines below (they read it back
					| via a6, the library base every LVO
					| call receives per the standard Amiga
					| library calling convention).
	rts				| d0 still holds libAddr -- untouched

_lib_name:
	.asciz	"bsdsocket.library"	| must match exactly: this is the name
					| real guest programs OpenLibrary() by
	.balign	2
_lib_idstring:
	.asciz	"$VER: bsdsocket.library 4.0 (2026-08-03)\r\n"
	.balign	2
_int_name:
	.asciz	"HostSocket wake"
	.balign	2

	| The function vector table MakeLibrary consumes (see its INPUTS):
	| first word -1 selects *displacement* mode (word offsets relative
	| to this table's own address), which is the only form that can be
	| a compile-time constant in PIC code -- an *absolute*-pointer table
	| would need a runtime relocation fixup this ROM discipline forbids
	| (see this file's header comment). MakeLibrary computes the real
	| jump-table addresses at runtime from these offsets plus its own
	| copy of this table's address, exactly like the `.short label-label`
	| idiom already used for the DiagArea fields above.
	|
	| Order matches the four standard LVOs (Open/Close/Expunge/Reserved)
	| followed by the real bsdsocket_lib.fd order starting at socket()
	| (LVO -30) through getprotobynumber (-252, the last name/number
	| database lookup -- see below), then on through GetSocketEvents
	| (-300, Phase 4's ceiling -- see PROPOSAL.md's Phase 4 scope
	| decisions); the vector array must be contiguous, so every LVO in
	| that range needs an entry even where the real function stays
	| unimplemented (`_hs_stub`, a real-but-"not implemented" LVO rather
	| than a jump off the end of this table into unrelated ROM bytes --
	| MakeLibrary's jump table has no bounds checking of its own). Only
	| SetSocketSignals and vsyslog are still on `_hs_stub` today (-1, the
	| correct BSD "error" convention for their plain-LONG return type) --
	| every other LVO in the table gets a real body, listed below by
	| group.
	|
	| gethostbyaddr (reverse/PTR DNS): smoltcp 0.13's wire::dns::Type has
	| no Ptr variant, and its dns::Socket API can't return a domain-name
	| answer at all, so this speaks DNS wire format directly over a plain
	| UDP socket instead of reusing that type -- see crates/hostsocket-plugin/src/lib.rs's
	| do_gethostbyaddr and parse_ptr_response for the full account.
	|
	| getservbyname/getservbyport/getprotobyname/getprotobynumber/
	| getnetbyname/getnetbyaddr: small static well-known-name tables (see
	| crates/hostsocket-plugin/src/lib.rs's SERVICES/PROTOCOLS/NETWORKS) -- this project is
	| used by general Amiga software, not just as a CI testing tool (see
	| README.md's own framing), so resolving common names for real is
	| worth doing rather than leaving these unimplemented. These, plus
	| gethostbyaddr above, all return a *pointer* (struct hostent/netent/
	| servent/protoent *), where real BSD "not found" means NULL (0), not
	| -1 -- `_hs_stub`'s own -1 would be treated as a non-null garbage
	| pointer by the caller instead, which used to make bsdsocktest's own
	| "not found" tests fail outright (a real, non-null struct full of
	| garbage fields) instead of skipping honestly or passing its explicit
	| "returns NULL" checks, back when these were still `_hs_stub`/
	| `_hs_stub_null` placeholders -- found running that suite's DNS
	| category for the first time with HOST set, since the loopback-tier
	| tests exercising these never checked field values (only "did it
	| crash", which -1-as-a-pointer never did either).
	|
	| Everything else already has a real body: Dup2Socket, getdtablesize,
	| the six Inet_*/inet_* address-conversion functions, gethostbyname
	| (forward DNS, A records only), gethostname (a fixed configurable
	| string, see crates/hostsocket-plugin/src/lib.rs's do_gethostname), gethostid (this
	| interface's own address, see do_gethostid), SocketBaseTagList
	| (SBTC_ERRNOLONGPTR, SBTC_HERRNOLONGPTR, SBTC_SIGEVENTMASK,
	| SBTC_BREAKMASK, and SBTC_DTABLESIZE -- see crates/hostsocket-plugin/src/lib.rs's
	| do_socketbasetaglist), GetSocketEvents (see
	| do_get_socket_events/process_socket_events -- returns a plain LONG
	| like SetSocketSignals/vsyslog, but unlike those two, -1 here isn't a
	| stub placeholder, it's the API's own documented "no events pending"
	| return value), sendmsg/recvmsg (TCP-only scatter/gather via a struct
	| msghdr's msg_iov/msg_iovlen -- msg_name/msg_control ignored, see
	| do_sendmsg/do_recvmsg's own comments for why), and ObtainSocket/
	| ReleaseSocket/ReleaseCopyOfSocket (the shared socket-pool transfer
	| mechanism, see do_obtain_socket/do_release_socket/
	| do_release_copy_of_socket).
_func_table:
	.short	-1
	.short	_lib_open          - _func_table
	.short	_lib_close         - _func_table
	.short	_lib_expunge       - _func_table
	.short	_lib_reserved      - _func_table
	.short	_hs_socket         - _func_table	| -30 socket
	.short	_hs_bind           - _func_table	| -36 bind
	.short	_hs_listen         - _func_table	| -42 listen
	.short	_hs_accept         - _func_table	| -48 accept
	.short	_hs_connect        - _func_table	| -54 connect
	.short	_hs_sendto         - _func_table	| -60 sendto
	.short	_hs_send           - _func_table	| -66 send
	.short	_hs_recvfrom       - _func_table	| -72 recvfrom
	.short	_hs_recv           - _func_table	| -78 recv
	.short	_hs_shutdown       - _func_table	| -84 shutdown
	.short	_hs_setsockopt     - _func_table	| -90 setsockopt
	.short	_hs_getsockopt     - _func_table	| -96 getsockopt
	.short	_hs_getsockname    - _func_table	| -102 getsockname
	.short	_hs_getpeername    - _func_table	| -108 getpeername
	.short	_hs_ioctl_socket   - _func_table	| -114 IoctlSocket
	.short	_hs_close_socket   - _func_table	| -120 CloseSocket
	.short	_hs_wait_select    - _func_table	| -126 WaitSelect
	.short	_hs_stub           - _func_table	| -132 SetSocketSignals
	.short	_hs_getdtablesize  - _func_table	| -138 getdtablesize
	.short	_hs_obtain_socket  - _func_table	| -144 ObtainSocket
	.short	_hs_release_socket - _func_table	| -150 ReleaseSocket
	.short	_hs_release_copy_of_socket - _func_table	| -156 ReleaseCopyOfSocket
	.short	_hs_errno          - _func_table	| -162 Errno
	.short	_hs_set_errno_ptr  - _func_table	| -168 SetErrnoPtr

	| -- Phase 4: Dup2Socket + Inet_*/inet_* utility functions -----------
	.short	_hs_inet_ntoa      - _func_table	| -174 Inet_NtoA
	.short	_hs_inet_addr      - _func_table	| -180 inet_addr
	.short	_hs_inet_lnaof     - _func_table	| -186 Inet_LnaOf
	.short	_hs_inet_netof     - _func_table	| -192 Inet_NetOf
	.short	_hs_inet_makeaddr  - _func_table	| -198 Inet_MakeAddr
	.short	_hs_inet_network   - _func_table	| -204 inet_network
	.short	_hs_gethostbyname  - _func_table	| -210 gethostbyname
	.short	_hs_gethostbyaddr  - _func_table	| -216 gethostbyaddr (reverse/PTR DNS)
	.short	_hs_getnetbyname   - _func_table	| -222 getnetbyname
	.short	_hs_getnetbyaddr   - _func_table	| -228 getnetbyaddr
	.short	_hs_getservbyname  - _func_table	| -234 getservbyname
	.short	_hs_getservbyport  - _func_table	| -240 getservbyport
	.short	_hs_getprotobyname - _func_table	| -246 getprotobyname
	.short	_hs_getprotobynumber - _func_table	| -252 getprotobynumber
	.short	_hs_stub           - _func_table	| -258 vsyslog
	.short	_hs_dup2socket     - _func_table	| -264 Dup2Socket
	.short	_hs_sendmsg        - _func_table	| -270 sendmsg
	.short	_hs_recvmsg        - _func_table	| -276 recvmsg
	.short	_hs_gethostname    - _func_table	| -282 gethostname
	.short	_hs_gethostid      - _func_table	| -288 gethostid
	.short	_hs_socketbasetaglist - _func_table	| -294 SocketBaseTagList
	.short	_hs_getsocketevents - _func_table	| -300 GetSocketEvents
	.short	-1

	| Standard library entry points. No per-open state and no real
	| expunge in Phase 1/2 (this library lives in the plugin's boot ROM
	| for the whole session) -- refusing to expunge is a common,
	| harmless simplification for a small resident library.
_lib_open:
	addq.w	#1,32(a6)		| lib_OpenCnt++
	bclr	#3,LIB_FLAGS(a6)	| clear LIBF_DELEXP
	move.l	a6,d0
	rts
_lib_close:
	subq.w	#1,32(a6)		| lib_OpenCnt--
	moveq	#0,d0			| never actually expunge in Phase 1/2
	rts
_lib_expunge:
	moveq	#0,d0			| refuse: no segList to hand back
	rts
_lib_reserved:
	moveq	#0,d0
	rts

	| -- Shared RPC helpers (Phase 2) --------------------------------
	|
	| _stage_task: fetches this task's own pointer (FindTask(NULL)) and
	| stores it as arg0 of the (already-staged-by-the-caller) argument
	| block. Callers stage their own call-specific args first (into
	| LIB_ARGBLK+4 onward) since this clobbers d0/a1 like any Exec call.
_stage_task:
	suba.l	a1,a1			| FindTask(NULL)
	EXECLVO	LVO_FINDTASK
	move.l	d0,LIB_ARGBLK(a6)	| arg0 = this task's pointer
	rts

	| _ring_doorbell: rings the REG_ARGPTR/REG_CALL doorbell for call
	| number `d0` (LIB_ARGBLK must already be fully staged, including
	| arg0 via _stage_task) and returns the result in `d0`. Bracketed in
	| Forbid()/Permit() -- a bare two-write doorbell sequence is not
	| atomic under real AmigaOS preemptive multitasking (a task switch
	| between the ARGPTR and CALL writes would let a second task's call
	| clobber the first task's arguments before it dispatches; this
	| project's Phase 1 never hit it because nothing exercised two tasks
	| calling concurrently, but Phase 2's per-task state makes that a
	| real scenario to get right).
	|
	| Only ever touches d0/d1/a0/a1 (the volatile set every LVO call is
	| free to clobber) -- d0 doubles as call-number input and result
	| output (the same shape AllocSignal's own SYNOPSIS uses), stashed on
	| the stack across the Forbid()/Permit() calls rather than trusted to
	| survive them, since the "d0/d1/a0/a1 may be clobbered by any Exec
	| call" convention applies to those too. A prior version of this used
	| d3/d5/d6/d7 as "convenient" scratch instead, which is exactly the
	| bug this fixes: those registers belong to the *caller* (real
	| bsdsocket.library callers keep long-lived state across LVO calls in
	| them, same as any other library), and clobbering them without
	| saving/restoring silently corrupted an unrelated register the
	| calling program was still using -- found via this project's own
	| Phase 2 test program, whose fd1 (kept in d7 across several LVO
	| calls) turned into garbage the moment an unrelated IoctlSocket call
	| went through the old, buggy version of this routine.
_ring_doorbell:
	move.l	d0,-(sp)
	EXECLVO	LVO_FORBID
	move.l	(sp)+,d0
	lea	LIB_ARGBLK(a6),a0
	move.l	LIB_BOARDBASE(a6),a1
	move.l	a0,REG_ARGPTR(a1)
	move.l	d0,REG_CALL(a1)
	move.l	REG_RESULT(a1),d0
	move.l	d0,-(sp)
	EXECLVO	LVO_PERMIT
	move.l	(sp)+,d0
	rts

	| _ring_doorbell_blocking: like _ring_doorbell (same d0 in/out
	| convention, same "only d0/d1/a0/a1" register discipline -- the
	| signal number and call number this loop needs across iterations
	| live on the stack instead of in registers for exactly the reason
	| explained above), but on RES_PENDING, parks the task with a real
	| Wait() instead of Phase 1's guest-side spin. `Wait()` is documented
	| to safely break a Forbid() for the sleep and re-arm on return
	| (exec.doc's own CAUTION on Wait) -- the standard AmigaOS idiom for
	| "check condition under Forbid, then Wait" without losing a signal
	| that arrives in between.
	|
	| The wait-registration call (CALL_REGISTER_WAIT) stages its signal
	| mask at arg7 specifically -- never used by any real call in this
	| table (WaitSelect, the greediest, only reaches arg6) -- so it can
	| never clobber the very call arguments this loop is about to retry.
	|
	| Stack layout while this runs: 0(sp) = signal number (-1 = none
	| allocated yet), 4(sp) = the real call number (reloaded into d0
	| before every _ring_doorbell call, which only promises to preserve
	| the stack, not d0 itself, across its own body).
_ring_doorbell_blocking:
	move.l	d0,-(sp)		| 0(sp) = call number
	moveq	#-1,d0
	move.l	d0,-(sp)		| 0(sp) = signal number, 4(sp) = call number
.Lblk_retry:
	move.l	4(sp),d0
	bsr	_ring_doorbell
	cmp.l	#RES_PENDING,d0
	bne.s	.Lblk_done
	tst.l	(sp)
	bpl.s	.Lblk_wait		| already holding a signal -- just wait again
	moveq	#-1,d0
	EXECLVO	LVO_ALLOCSIGNAL
	move.l	d0,(sp)
	bpl.s	.Lblk_register
		| No signal available (all 32 of this task's signal bits already
		| allocated -- rare, but architecturally possible). Used to
		| return RES_PENDING (-2) straight through to the public LVO
		| here: not a valid fd/byte-count/error value (every real
		| result is >= -1, see hostsocket_board.h's own comment on
		| RES_PENDING), so a caller checking `result == -1` rather than
		| `result < 0` would mishandle it, and it could be confused for
		| a genuine (if nonsensical) byte count by anything less
		| careful. Real errno can't be set from here: it's host-tracked
		| state this trampoline has no RPC round-trip in flight to
		| update (CALL_REGISTER_WAIT was never reached), and the guest
		| side has no cached errno-slot address of its own to write
		| into directly even if it wanted to (SetErrnoPtr's target is
		| forwarded straight to the host, never cached locally -- see
		| _hs_set_errno_ptr). A real, if imprecise, BSD-shaped error
		| return (whatever errno was already set to, from some earlier
		| call) is still strictly better than leaking an internal
		| sentinel a caller has no way to recognize as special.
	moveq	#-1,d0
	bra.s	.Lblk_done
.Lblk_register:
	moveq	#1,d1
	move.l	(sp),d0
	lsl.l	d0,d1			| d1 = 1 << signal
	move.l	d1,LIB_ARGBLK+28(a6)	| arg7 = signal mask
	moveq	#CALL_REGISTER_WAIT,d0
	bsr	_ring_doorbell
.Lblk_wait:
	moveq	#1,d1
	move.l	(sp),d0
	lsl.l	d0,d1
	move.l	d1,d0
	EXECLVO	LVO_WAIT
	bra.s	.Lblk_retry
.Lblk_done:
	move.l	d0,-(sp)		| 0(sp)=result 4(sp)=signal 8(sp)=call number
	tst.l	4(sp)
	bmi.s	.Lblk_nofree
	move.l	4(sp),d0
	EXECLVO	LVO_FREESIGNAL
.Lblk_nofree:
	move.l	(sp)+,d0		| pop result
	lea	8(sp),sp		| drop signal number + call number
	rts

	| -- bsdsocket.library RPC trampolines (PROPOSAL.md's "Call path") --
	|
	| Each stages its LVO's own arguments into LIB_ARGBLK first (arg1..),
	| then _stage_task (arg0), then rings the doorbell -- blocking via
	| _ring_doorbell_blocking for the calls whose real semantics can
	| legitimately need to wait (connect, send, recv, WaitSelect,
	| gethostbyname), plain _ring_doorbell for the rest. Non-blocking mode
	| is entirely the plugin's decision (per-fd flag in its own fd table):
	| a non-blocking socket's connect/send/recv never comes back
	| RES_PENDING at all, so there is no separate guest-side non-blocking
	| path to get wrong here.
_hs_socket:
	| in: d0=domain d1=type d2=protocol
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	d1,LIB_ARGBLK+8(a6)
	move.l	d2,LIB_ARGBLK+12(a6)
	bsr	_stage_task
	moveq	#CALL_SOCKET,d0
	bsr	_ring_doorbell
	rts

_hs_connect:
	| in: d0=sock a0=name (sockaddr_in ptr) d1=namelen
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	a0,LIB_ARGBLK+8(a6)
	move.l	d1,LIB_ARGBLK+12(a6)
	bsr	_stage_task
	moveq	#CALL_CONNECT,d0
	bsr	_ring_doorbell_blocking
	rts

_hs_send:
	| in: d0=sock a0=buf d1=len d2=flags
	| Blocking (_ring_doorbell_blocking, not plain _ring_doorbell): a real
	| blocking send() on a TCP socket queues as much as fits and then
	| waits for room to queue the rest, rather than returning a short
	| count the moment its own send buffer runs out -- see
	| crates/hostsocket-plugin/src/lib.rs's do_send for why (found hanging bsdsocktest's own
	| large-transfer test otherwise, once that suite finally got past its
	| own earlier deadlock at connect() -- see CALL_CONNECT's own retry
	| bug fix). Harmless on UDP fds too: do_sendto's own datagram path
	| never returns RES_PENDING, so this just falls straight through
	| .Lblk_done on its first pass there.
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	a0,LIB_ARGBLK+8(a6)
	move.l	d1,LIB_ARGBLK+12(a6)
	move.l	d2,LIB_ARGBLK+16(a6)
	bsr	_stage_task
	moveq	#CALL_SEND,d0
	bsr	_ring_doorbell_blocking
	rts

_hs_recv:
	| in: d0=sock a0=buf d1=len d2=flags
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	a0,LIB_ARGBLK+8(a6)
	move.l	d1,LIB_ARGBLK+12(a6)
	move.l	d2,LIB_ARGBLK+16(a6)
	bsr	_stage_task
	moveq	#CALL_RECV,d0
	bsr	_ring_doorbell_blocking
	rts

_hs_close_socket:
	| in: d0=sock
	move.l	d0,LIB_ARGBLK+4(a6)
	bsr	_stage_task
	moveq	#CALL_CLOSESOCKET,d0
	bsr	_ring_doorbell
	rts

_hs_ioctl_socket:
	| in: d0=sock d1=req a0=argp
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	d1,LIB_ARGBLK+8(a6)
	move.l	a0,LIB_ARGBLK+12(a6)
	bsr	_stage_task
	moveq	#CALL_IOCTLSOCKET,d0
	bsr	_ring_doorbell
	rts

_hs_set_errno_ptr:
	| in: a0=errno_ptr d0=size
	move.l	a0,LIB_ARGBLK+4(a6)
	move.l	d0,LIB_ARGBLK+8(a6)
	bsr	_stage_task
	moveq	#CALL_SETERRNOPTR,d0
	bsr	_ring_doorbell
	rts

_hs_socketbasetaglist:
	| in: a0=tags (TagItem array ptr)
	move.l	a0,LIB_ARGBLK+4(a6)
	lea	LIB_ERRNO_SLOT(a6),a0
	move.l	a0,LIB_ARGBLK+8(a6)
	lea	LIB_HERRNO_SLOT(a6),a0
	move.l	a0,LIB_ARGBLK+12(a6)
	bsr	_stage_task
	moveq	#CALL_SOCKETBASETAGLIST,d0
	bsr	_ring_doorbell
	rts

_hs_getsocketevents:
	| in: a0=event_ptr. Never blocks -- returns -1 (no events pending)
	| immediately rather than via CALL_REGISTER_WAIT/_ring_doorbell_blocking,
	| matching docs/AMITCP_API.md's own "poll, don't wait" semantics (real
	| callers WaitSelect()/Wait() on the SBTC_SIGEVENTMASK signal first,
	| then drain GetSocketEvents() in a loop -- see crates/hostsocket-plugin/src/lib.rs's
	| do_get_socket_events).
	move.l	a0,LIB_ARGBLK+4(a6)
	bsr	_stage_task
	moveq	#CALL_GETSOCKETEVENTS,d0
	bsr	_ring_doorbell
	rts

_hs_gethostname:
	| in: a0=name d0=namelen
	move.l	a0,LIB_ARGBLK+4(a6)
	move.l	d0,LIB_ARGBLK+8(a6)
	bsr	_stage_task
	moveq	#CALL_GETHOSTNAME,d0
	bsr	_ring_doorbell
	rts

_hs_gethostid:
	| in: (none)
	bsr	_stage_task
	moveq	#CALL_GETHOSTID,d0
	bsr	_ring_doorbell
	rts

_hs_sendmsg:
	| in: d0=sock a0=msg d1=flags
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	a0,LIB_ARGBLK+8(a6)
	move.l	d1,LIB_ARGBLK+12(a6)
	bsr	_stage_task
	moveq	#CALL_SENDMSG,d0
	bsr	_ring_doorbell_blocking
	rts

_hs_recvmsg:
	| in: d0=sock a0=msg d1=flags
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	a0,LIB_ARGBLK+8(a6)
	move.l	d1,LIB_ARGBLK+12(a6)
	bsr	_stage_task
	moveq	#CALL_RECVMSG,d0
	bsr	_ring_doorbell_blocking
	rts

_hs_errno:
	| in: (none)
	bsr	_stage_task
	moveq	#CALL_ERRNO,d0
	bsr	_ring_doorbell
	rts

_hs_wait_select:
	| in: d0=nfds a0=read_fds a1=write_fds a2=except_fds a3=timeout d1=signals
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	a0,LIB_ARGBLK+8(a6)
	move.l	a1,LIB_ARGBLK+12(a6)
	move.l	a2,LIB_ARGBLK+16(a6)
	move.l	a3,LIB_ARGBLK+20(a6)
	move.l	d1,LIB_ARGBLK+24(a6)
	bsr	_stage_task
	moveq	#CALL_WAITSELECT,d0
	bsr	_ring_doorbell_blocking
	rts

	| -- Phase 3: server-side path, UDP, remaining common LVOs -----------

_hs_bind:
	| in: d0=sock a0=name (sockaddr_in ptr) d1=namelen
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	a0,LIB_ARGBLK+8(a6)
	move.l	d1,LIB_ARGBLK+12(a6)
	bsr	_stage_task
	moveq	#CALL_BIND,d0
	bsr	_ring_doorbell
	rts

_hs_listen:
	| in: d0=sock d1=backlog
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	d1,LIB_ARGBLK+8(a6)
	bsr	_stage_task
	moveq	#CALL_LISTEN,d0
	bsr	_ring_doorbell
	rts

_hs_accept:
	| in: d0=sock a0=addr (sockaddr_in out ptr, or NULL) a1=addrlen
	| (ptr to a LONG, in/out, or NULL). Blocks like connect/recv until a
	| connection arrives.
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	a0,LIB_ARGBLK+8(a6)
	move.l	a1,LIB_ARGBLK+12(a6)
	bsr	_stage_task
	moveq	#CALL_ACCEPT,d0
	bsr	_ring_doorbell_blocking
	rts

_hs_sendto:
	| in: d0=sock a0=buf d1=len d2=flags a1=to (sockaddr_in ptr, or NULL
	| to use a peer already set by connect()) d3=tolen
	| Blocking (_ring_doorbell_blocking, not plain _ring_doorbell): the
	| host's do_sendto delegates a TCP fd straight to do_send (see
	| crates/hostsocket-plugin/src/lib.rs), which is a real blocking send
	| on a TCP socket -- it can genuinely return RES_PENDING the same way
	| _hs_send's own call does. An earlier version of this trampoline used
	| the plain non-blocking doorbell on the theory that "do_sendto's own
	| datagram path never returns RES_PENDING" (true for UDP, but sendto()
	| is also valid on a TCP fd): RES_PENDING (-2) would leak straight
	| through as this LVO's own result -- not a valid byte count/error a
	| caller could interpret -- and leave `send_progress` for this task
	| stale, so the *next* send()/sendto() on the same fd would resume
	| mid-buffer and silently drop its own leading bytes. Harmless on UDP
	| fds: do_sendto's datagram path still never returns RES_PENDING, so
	| this just falls straight through .Lblk_done on its first pass there,
	| same as _hs_send's own comment already documents for its call.
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	a0,LIB_ARGBLK+8(a6)
	move.l	d1,LIB_ARGBLK+12(a6)
	move.l	d2,LIB_ARGBLK+16(a6)
	move.l	a1,LIB_ARGBLK+20(a6)
	move.l	d3,LIB_ARGBLK+24(a6)
	bsr	_stage_task
	moveq	#CALL_SENDTO,d0
	bsr	_ring_doorbell_blocking
	rts

_hs_recvfrom:
	| in: d0=sock a0=buf d1=len d2=flags a1=addr (sockaddr_in out ptr, or
	| NULL) a2=addrlen (ptr to a LONG, in/out, or NULL)
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	a0,LIB_ARGBLK+8(a6)
	move.l	d1,LIB_ARGBLK+12(a6)
	move.l	d2,LIB_ARGBLK+16(a6)
	move.l	a1,LIB_ARGBLK+20(a6)
	move.l	a2,LIB_ARGBLK+24(a6)
	bsr	_stage_task
	moveq	#CALL_RECVFROM,d0
	bsr	_ring_doorbell_blocking
	rts

_hs_shutdown:
	| in: d0=sock d1=how
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	d1,LIB_ARGBLK+8(a6)
	bsr	_stage_task
	moveq	#CALL_SHUTDOWN,d0
	bsr	_ring_doorbell
	rts

_hs_setsockopt:
	| in: d0=sock d1=level d2=optname a0=optval d3=optlen
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	d1,LIB_ARGBLK+8(a6)
	move.l	d2,LIB_ARGBLK+12(a6)
	move.l	a0,LIB_ARGBLK+16(a6)
	move.l	d3,LIB_ARGBLK+20(a6)
	bsr	_stage_task
	moveq	#CALL_SETSOCKOPT,d0
	bsr	_ring_doorbell
	rts

_hs_getsockopt:
	| in: d0=sock d1=level d2=optname a0=optval (out ptr) a1=optlen (ptr
	| to a LONG, in/out)
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	d1,LIB_ARGBLK+8(a6)
	move.l	d2,LIB_ARGBLK+12(a6)
	move.l	a0,LIB_ARGBLK+16(a6)
	move.l	a1,LIB_ARGBLK+20(a6)
	bsr	_stage_task
	moveq	#CALL_GETSOCKOPT,d0
	bsr	_ring_doorbell
	rts

_hs_getsockname:
	| in: d0=sock a0=name (sockaddr_in out ptr) a1=namelen (ptr to a
	| LONG, in/out)
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	a0,LIB_ARGBLK+8(a6)
	move.l	a1,LIB_ARGBLK+12(a6)
	bsr	_stage_task
	moveq	#CALL_GETSOCKNAME,d0
	bsr	_ring_doorbell
	rts

_hs_getpeername:
	| in: d0=sock a0=name (sockaddr_in out ptr) a1=namelen (ptr to a
	| LONG, in/out)
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	a0,LIB_ARGBLK+8(a6)
	move.l	a1,LIB_ARGBLK+12(a6)
	bsr	_stage_task
	moveq	#CALL_GETPEERNAME,d0
	bsr	_ring_doorbell
	rts

	| -- Phase 4: Dup2Socket + Inet_*/inet_* utility functions -----------

_hs_getdtablesize:
	| in: (none). Returns MAX_FDS -- the plugin, not this stub, is the
	| single source of truth for the fd table size, so this stays a real
	| RPC round trip rather than a hand-duplicated constant that could
	| drift from crates/hostsocket-plugin/src/lib.rs's own MAX_FDS.
	bsr	_stage_task
	moveq	#CALL_GETDTABLESIZE,d0
	bsr	_ring_doorbell
	rts

_hs_dup2socket:
	| in: d0=sock d1=newfd (-1 = "any free fd")
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	d1,LIB_ARGBLK+8(a6)
	bsr	_stage_task
	moveq	#CALL_DUP2SOCKET,d0
	bsr	_ring_doorbell
	rts

_hs_obtain_socket:
	| in: d0=id d1=domain d2=type d3=protocol
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	d1,LIB_ARGBLK+8(a6)
	move.l	d2,LIB_ARGBLK+12(a6)
	move.l	d3,LIB_ARGBLK+16(a6)
	bsr	_stage_task
	moveq	#CALL_OBTAINSOCKET,d0
	bsr	_ring_doorbell
	rts

_hs_release_socket:
	| in: d0=sock d1=id
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	d1,LIB_ARGBLK+8(a6)
	bsr	_stage_task
	moveq	#CALL_RELEASESOCKET,d0
	bsr	_ring_doorbell
	rts

_hs_release_copy_of_socket:
	| in: d0=sock d1=id
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	d1,LIB_ARGBLK+8(a6)
	bsr	_stage_task
	moveq	#CALL_RELEASECOPYOFSOCKET,d0
	bsr	_ring_doorbell
	rts

_hs_inet_ntoa:
	| in: d0=addr (in_addr_t, network byte order). Returns a pointer to
	| this library's own LIB_INETBUF scratch buffer -- since we already
	| know that address here, there's no need to round-trip it back
	| through REG_RESULT; the plugin just DMA-writes the formatted string
	| into it and we return the address. Recomputed with `lea` again after
	| the doorbell call rather than held across it in a0: _ring_doorbell
	| clobbers a0/a1 (its own REG_ARGPTR/board-base scratch), same as any
	| Exec-convention call -- a6 is the only register this whole chain
	| promises to preserve.
	move.l	d0,LIB_ARGBLK+4(a6)
	lea	LIB_INETBUF(a6),a0
	move.l	a0,LIB_ARGBLK+8(a6)
	bsr	_stage_task
	moveq	#CALL_INET_NTOA,d0
	bsr	_ring_doorbell
	lea	LIB_INETBUF(a6),a0
	move.l	a0,d0
	rts

_hs_inet_addr:
	| in: a0=straddr (NUL-terminated dotted-quad string)
	move.l	a0,LIB_ARGBLK+4(a6)
	bsr	_stage_task
	moveq	#CALL_INET_ADDR,d0
	bsr	_ring_doorbell
	rts

_hs_inet_lnaof:
	| in: d0=addr (in_addr_t, network byte order)
	move.l	d0,LIB_ARGBLK+4(a6)
	bsr	_stage_task
	moveq	#CALL_INET_LNAOF,d0
	bsr	_ring_doorbell
	rts

_hs_inet_netof:
	| in: d0=addr (in_addr_t, network byte order)
	move.l	d0,LIB_ARGBLK+4(a6)
	bsr	_stage_task
	moveq	#CALL_INET_NETOF,d0
	bsr	_ring_doorbell
	rts

_hs_inet_makeaddr:
	| in: d0=net d1=host (both host byte order)
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	d1,LIB_ARGBLK+8(a6)
	bsr	_stage_task
	moveq	#CALL_INET_MAKEADDR,d0
	bsr	_ring_doorbell
	rts

_hs_inet_network:
	| in: a0=straddr (NUL-terminated dotted-quad string)
	move.l	a0,LIB_ARGBLK+4(a6)
	bsr	_stage_task
	moveq	#CALL_INET_NETWORK,d0
	bsr	_ring_doorbell
	rts

_hs_gethostbyname:
	| in: a0=name (NUL-terminated hostname string). Returns a pointer to
	| this library's own LIB_HOSTENTBUF scratch buffer on success (the
	| plugin DMA-writes a real struct hostent -- header, h_aliases/
	| h_addr_list arrays, address bytes, and the name string -- all into
	| it, same "trampoline already knows the address" trick Inet_NtoA
	| uses for LIB_INETBUF), or NULL on failure -- unlike Inet_NtoA this
	| can genuinely fail (unresolvable name, no network), so the RPC
	| result actually gets checked here rather than being ignored.
	| _ring_doorbell_blocking, not the plain doorbell: a DNS round trip
	| takes real network time, the same "start now, park on RES_PENDING,
	| retry once woken" shape connect()/accept() already use.
	move.l	a0,LIB_ARGBLK+4(a6)
	lea	LIB_HOSTENTBUF(a6),a0
	move.l	a0,LIB_ARGBLK+8(a6)
	bsr	_stage_task
	moveq	#CALL_GETHOSTBYNAME,d0
	bsr	_ring_doorbell_blocking
	tst.l	d0
	bne.s	1f
	lea	LIB_HOSTENTBUF(a6),a0	| success -- recomputed after the doorbell
					| call rather than held across it, same
					| reasoning as Inet_NtoA (a0/a1 are
					| _ring_doorbell's own scratch)
	move.l	a0,d0
	rts
1:	moveq	#0,d0			| failure -- NULL, no hostent
	rts

_hs_gethostbyaddr:
	| in: a0=addr (raw address bytes, a real struct in_addr -- not a
	| string, unlike gethostbyname's own hostname arg) d0=len d1=type.
	| Same LIB_HOSTENTBUF/RES_PENDING/NULL-on-failure shape as
	| _hs_gethostbyname just above (a reverse DNS lookup is still a real
	| network round trip) -- see crates/hostsocket-plugin/src/lib.rs's do_gethostbyaddr for
	| why this needs its own hand-rolled DNS client rather than reusing
	| gethostbyname's.
	move.l	a0,LIB_ARGBLK+4(a6)
	move.l	d0,LIB_ARGBLK+8(a6)
	move.l	d1,LIB_ARGBLK+12(a6)
	lea	LIB_HOSTENTBUF(a6),a0
	move.l	a0,LIB_ARGBLK+16(a6)
	bsr	_stage_task
	moveq	#CALL_GETHOSTBYADDR,d0
	bsr	_ring_doorbell_blocking
	tst.l	d0
	bne.s	1f
	lea	LIB_HOSTENTBUF(a6),a0
	move.l	a0,d0
	rts
1:	moveq	#0,d0
	rts

	| getservbyname/getservbyport/getprotobyname/getprotobynumber/
	| getnetbyname/getnetbyaddr: small static well-known-name tables (see
	| crates/hostsocket-plugin/src/lib.rs's SERVICES/PROTOCOLS/NETWORKS and do_getservbyname
	| and friends), all pure local lookups -- plain _ring_doorbell, not
	| _ring_doorbell_blocking, since none of these ever need a real network
	| round trip the way DNS does. Same NULL-vs-bufaddr split as
	| _hs_gethostbyname/_hs_gethostbyaddr above.
_hs_getservbyname:
	| in: a0=name a1=proto (STRPTR, may be NULL for "any protocol")
	move.l	a0,LIB_ARGBLK+4(a6)
	move.l	a1,LIB_ARGBLK+8(a6)
	lea	LIB_SERVENTBUF(a6),a0
	move.l	a0,LIB_ARGBLK+12(a6)
	bsr	_stage_task
	moveq	#CALL_GETSERVBYNAME,d0
	bsr	_ring_doorbell
	tst.l	d0
	bne.s	1f
	lea	LIB_SERVENTBUF(a6),a0
	move.l	a0,d0
	rts
1:	moveq	#0,d0
	rts

_hs_getservbyport:
	| in: d0=port a0=proto (STRPTR, may be NULL for "any protocol")
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	a0,LIB_ARGBLK+8(a6)
	lea	LIB_SERVENTBUF(a6),a0
	move.l	a0,LIB_ARGBLK+12(a6)
	bsr	_stage_task
	moveq	#CALL_GETSERVBYPORT,d0
	bsr	_ring_doorbell
	tst.l	d0
	bne.s	1f
	lea	LIB_SERVENTBUF(a6),a0
	move.l	a0,d0
	rts
1:	moveq	#0,d0
	rts

_hs_getprotobyname:
	| in: a0=name
	move.l	a0,LIB_ARGBLK+4(a6)
	lea	LIB_PROTOENTBUF(a6),a0
	move.l	a0,LIB_ARGBLK+8(a6)
	bsr	_stage_task
	moveq	#CALL_GETPROTOBYNAME,d0
	bsr	_ring_doorbell
	tst.l	d0
	bne.s	1f
	lea	LIB_PROTOENTBUF(a6),a0
	move.l	a0,d0
	rts
1:	moveq	#0,d0
	rts

_hs_getprotobynumber:
	| in: d0=proto
	move.l	d0,LIB_ARGBLK+4(a6)
	lea	LIB_PROTOENTBUF(a6),a0
	move.l	a0,LIB_ARGBLK+8(a6)
	bsr	_stage_task
	moveq	#CALL_GETPROTOBYNUMBER,d0
	bsr	_ring_doorbell
	tst.l	d0
	bne.s	1f
	lea	LIB_PROTOENTBUF(a6),a0
	move.l	a0,d0
	rts
1:	moveq	#0,d0
	rts

_hs_getnetbyname:
	| in: a0=name
	move.l	a0,LIB_ARGBLK+4(a6)
	lea	LIB_NETENTBUF(a6),a0
	move.l	a0,LIB_ARGBLK+8(a6)
	bsr	_stage_task
	moveq	#CALL_GETNETBYNAME,d0
	bsr	_ring_doorbell
	tst.l	d0
	bne.s	1f
	lea	LIB_NETENTBUF(a6),a0
	move.l	a0,d0
	rts
1:	moveq	#0,d0
	rts

_hs_getnetbyaddr:
	| in: d0=net d1=type
	move.l	d0,LIB_ARGBLK+4(a6)
	move.l	d1,LIB_ARGBLK+8(a6)
	lea	LIB_NETENTBUF(a6),a0
	move.l	a0,LIB_ARGBLK+12(a6)
	bsr	_stage_task
	moveq	#CALL_GETNETBYADDR,d0
	bsr	_ring_doorbell
	tst.l	d0
	bne.s	1f
	lea	LIB_NETENTBUF(a6),a0
	move.l	a0,d0
	rts
1:	moveq	#0,d0
	rts

	| Shared body for every still-unimplemented LVO that returns a plain
	| LONG (SetSocketSignals, vsyslog): no RPC round trip, just -1, the
	| correct BSD "error" convention for these.
_hs_stub:
	moveq	#-1,d0
	rts

	| -- Interrupt server (Phase 2) ----------------------------------
	|
	| Installed on Copperline's INTB_PORTS chain (see
	| _install_int_server). Entry convention, verbatim from exec.doc's
	| AddIntServer entry: D0/D1/A0/A1/A5/A6 scratch (A1 = this server's
	| is_Data, i.e. the board base -- see _install_int_server), all other
	| registers must be preserved. Must return with the 68000 Z flag
	| CLEAR if this interrupt was ours (stops the server chain), Z SET if
	| not (passes to the next server -- real hardware shares PORTS with
	| CIA-A, so a real boot's keyboard/floppy handling depends on getting
	| this right when there is nothing for us to do).
	|
	| Drains the plugin's whole wake queue in one invocation (REG_WAKE_
	| TASK/REG_WAKE_SIGNAL/REG_WAKE_ACK -- see hostsocket_board.h): more
	| than one task can become ready in the same tick(), and int2() stays
	| asserted (level-sensitive) for as long as the queue is non-empty,
	| so failing to drain it here would re-enter immediately in a tight
	| loop. Every register this touches is in the documented scratch set
	| except A7, so the running "how many did we signal" count and the
	| board-base pointer are kept on the stack across the nested Signal()
	| call instead of in a register Signal() might also use as scratch.
_int_handler:
	clr.l	-(sp)			| [sp] = handled count
.Lwake_loop:
	move.l	a1,-(sp)		| [sp]=board base, [sp+4]=handled count
	move.l	REG_WAKE_TASK(a1),d1
	move.l	REG_WAKE_SIGNAL(a1),d0
	tst.l	d1
	beq.s	.Lwake_done
	movea.l	d1,a1			| a1 = task (Signal()'s input)
	move.l	4.w,a6
	jsr	LVO_SIGNAL(a6)		| Signal(a1=task, d0=mask)
	movea.l	(sp),a1			| board base back (peek, not pop yet)
	move.l	#1,REG_WAKE_ACK(a1)	| pop the wake-queue entry we just handled
	addq.l	#4,sp			| drop the board-base copy
	addq.l	#1,(sp)			| handled count++
	bra.s	.Lwake_loop
.Lwake_done:
	addq.l	#4,sp			| drop the board-base copy
	move.l	(sp)+,d1		| pop handled count
	tst.l	d1			| Z clear if >0 (we signalled someone),
					| Z set if 0 (nothing was ours)
	rts
