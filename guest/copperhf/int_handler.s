| SPDX-License-Identifier: GPL-3.0-or-later
|
| INT2 server entry point for copperhf.device, installed on INTB_PORTS via
| AddIntServer (device.c's resident_init). Register convention verbatim
| from exec.doc's AddIntServer entry: D0/D1/A0/A1/A5/A6 scratch (A1 = this
| server's is_Data -- M4: the device pointer, struct CopperhfDevice*, not
| just the board base as in M1-M3, see device_layout.h's header comment),
| every other register must be preserved. Must return with the 68000 Z flag
| CLEAR if this interrupt was ours (INTB_PORTS is a shared chain -- real
| hardware shares it with CIA-A, so a real boot's keyboard/floppy handling
| depends on getting this right when there is nothing here for us to do),
| Z SET otherwise -- exactly the same contract guest/mhi/int_handler.s
| documents and implements, which this file mirrors move-for-move against
| copperhf_board.h's protocol instead of MHI's.
|
| A plain C function cannot reliably control the Z flag on return --
| exec.doc's own WARNING under AddIntServer says so explicitly ("Some
| compilers... may not have a mechanism for reliably setting the Z flag on
| exit"), which is why this is hand-written asm and device.c's own header
| comment defers to this file instead.
|
| copperhf_board.h's protocol (see its own header comment): CHF_IRQ_STATUS
| bit 0 = the completion queue is non-empty, bit 1 (M4) = CHF_CHANGED_MASK
| is non-zero. CHF_COMPLETE_GET is idempotent (does not pop by itself), so
| the drain loop must write CHF_COMPLETE_ACK after every ReplyMsg to
| actually advance the queue -- skipping that would re-deliver the same
| completion forever. The changed-mask path (M4) is handled by jsr'ing into
| device.c's chf_drain_changes(), an ordinary C function reached by a
| same-object-shape cross-object call (see that function's own header
| comment in device.c) -- unlike the completion drain, CHF_CHANGED_MASK's
| own ack (CHF_CHANGED_ACK) and the pending-list walk it drives both need
| exec.library calls (Cause()) beyond what this file's hand register
| discipline is worth hand-rolling in asm for.
|
| HARD-WON (M4): a5 holds the DEVICE pointer (dev) for the entire function,
| never the board pointer. An earlier version loaded a5 = *(dev +
| CHF_DEV_BOARDBASE_OFFSET) (the board pointer's VALUE, dereferenced via
| movea.l) and later tried to recover dev by computing
| `board_value - CHF_DEV_BOARDBASE_OFFSET` -- but that arithmetic is
| nonsense: the board lives in Zorro autoconfig space (~0x00EA0000) and dev
| is a MakeLibrary-allocated address in chip/fast RAM, two completely
| unrelated addresses. Subtracting the struct offset from the board VALUE
| does not undo anything -- `dev + OFFSET` is the *address* of the
| dev_BoardBase field, but `*(dev + OFFSET)` (what movea.l loads) is that
| field's *contents*, and only the address form is invertible by
| subtraction. The bug was invisible for the completion-drain path (which
| never needed dev back) but broke the changed-mask path outright:
| chf_drain_changes() read/acked CHF_CHANGED_MASK/ACK at a garbage
| "board" address instead of the real one, so the real CHF_CHANGED_MASK
| bit never actually cleared and INT2 re-fired forever (confirmed via the
| host-side worker's own instrumentation: CHF_CHANGED_MASK observed stuck
| at 0x0002 across 160k+ interrupt entries). The fix below re-derives the
| board pointer into scratch register a0 -- `movea.l
| CHF_DEV_BOARDBASE_OFFSET(a5),a0` -- every time it is needed instead of
| ever repurposing a5, so a5 = dev is always trivially available with no
| arithmetic to get wrong. m68k GCC callees on this target preserve
| D2-D7/A2-A6 across a call, trashing only D0/D1/A0/A1 (the same
| "library-call" discipline this file already assumes for LVO calls), so
| a5 (dev) survives every ReplyMsg/chf_drain_changes call below untouched;
| a0 (board) is cheaply recomputed after each such call since it is exactly
| as disposable as d0/d1/a1.

#include "copperhf_board.h"
#include "device_layout.h"

// exec.library/ReplyMsg (exec_lib.i FUNCDEF order, cross-checked against
// this project's own confirmed anchors: AddIntServer -168, FindTask -294,
// Wait -318, Signal -324, AllocSignal -330, FreeSignal -336).
//
// HARD-WON: this comment must be a C-style `//`/`/* */` comment that the C
// preprocessor itself strips, never a same-line trailing `|`-style Amiga-asm
// comment on the #define line -- cpp does not recognise `|` as a comment
// introducer, so a trailing `| ...` on a #define's own line becomes part of
// the macro's *replacement text*. Expanding `jsr LVO_REPLYMSG(a6)` then
// produced `jsr -378 | exec.library/ReplyMsg (exec_lib.i FUNCDEF(a6)`, and
// the assembler's own `|`-comment stripping ate the trailing `(a6)` along
// with the rest of the line -- silently assembling `jsr -378` as an
// absolute-short JSR to 0xFFFFFE86 instead of `jsr -378(a6)` (register
// indirect with displacement), with a6 never consulted at all. Confirmed
// against this exact toolchain's objdump: `4eb8 fe86` (JSR (xxx).W) instead
// of the intended `4eae fe86` (JSR d16(A6)). The result guru'd on the very
// first INTB_PORTS interrupt after AddIntServer -- an F-line "Emulator/
// Coprocessor error" at PC=0xFFFFFE86, since that unmapped high address
// reads back as the 0xFFFF "nothing here" pattern every other unmapped
// offset in this project uses, which happens to decode as an F-line opcode.
#define LVO_REPLYMSG -378

	.text
	.globl	_chf_int_handler

_chf_int_handler:
	| in: a1 = is_Data = device pointer (struct CopperhfDevice*, M4).
	| Copied to a5 and kept there for the whole function -- see the
	| header comment's HARD-WON note. a5 is BOTH in the server chain's
	| scratch set (D0/D1/A0/A1/A5/A6 -- anything else, a2 included, must
	| come back to the dispatcher untouched, and exec's server loop
	| really does keep its list iterator live across this call) AND
	| preserved across the ReplyMsg/chf_drain_changes library-discipline
	| calls below (callees may only trash D0/D1/A0/A1), so dev survives
	| the whole function with no save/restore. An earlier version parked
	| the board pointer in a2 instead (a distinct bug from the one this
	| file's header comment documents) and Guru'd every boot: the
	| dispatcher advanced ln_Succ from our leftover a2, read all-ones
	| from the board window's unmapped first longword, and
	| address-errored on the next node dereference.
	movea.l	a1,a5
	movea.l	CHF_DEV_BOARDBASE_OFFSET(a5),a0	| a0 = board (scratch;
							| re-derive after any jsr)
	move.w	CHF_IRQ_STATUS(a0),d0
	beq.s	.Lnot_ours		| Z set by the move.w itself if neither
					| defined bit (completion-queue
					| non-empty, or M4's changed-mask
					| non-empty) is set: nothing pending
					| for us.

	btst	#0,d0
	beq.s	.Lcheck_changed		| bit 0 clear: nothing to drain, go
					| straight to the M4 changed-mask
					| check below.

.Ldrain:
	move.l	CHF_COMPLETE_GET(a0),d0	| 32-bit, idempotent snapshot of the
					| queue head (0 = empty)
	tst.l	d0
	beq.s	.Lcheck_changed
	| DMA cache maintenance before the completion becomes visible: the
	| host wrote io_Actual/io_Error (and, for reads, the data payload)
	| straight into guest memory, so a copyback data cache (real 68040,
	| or the emulator's `[cpu] dcache` model) still holds stale lines
	| over them. chf_post_dma (device.c) CachePostDMA's the request and
	| its buffers -- called with the same ordinary stack-argument
	| convention as chf_drain_changes below (caller pushes, caller
	| cleans up); the pop doubles as the cleanup and lands the pointer
	| in a1 exactly where ReplyMsg wants it. The C call may trash
	| d0/d1/a0/a1 (a5 = dev survives; a0 is re-derived after ReplyMsg
	| anyway).
	move.l	d0,-(sp)		| push ioreq: chf_post_dma's argument
	bsr.w	_chf_post_dma		| chf_post_dma(ioreq)
	movea.l	(sp)+,a1		| pop the argument: a1 = IORequest
	move.l	4.w,a6
	jsr	LVO_REPLYMSG(a6)	| ReplyMsg(a1=msg); trashes d0/d1/a0/a1
					| (a5 = dev survives)
	movea.l	CHF_DEV_BOARDBASE_OFFSET(a5),a0	| re-derive board: the
							| call above trashed a0
	move.w	#0,CHF_COMPLETE_ACK(a0)	| any write pops the oldest completion
	bra.s	.Ldrain

.Lcheck_changed:
	| Re-derive board and re-read CHF_IRQ_STATUS rather than reuse the
	| d0/a0 snapshot from entry: the drain loop's own ReplyMsg calls are
	| library calls (may trash d0/d1/a0/a1), and bit 1 is independent of
	| bit 0 anyway (a change can be pending with the completion queue
	| empty, or vice versa), so a fresh read is both necessary and
	| correct.
	movea.l	CHF_DEV_BOARDBASE_OFFSET(a5),a0
	move.w	CHF_IRQ_STATUS(a0),d0
	btst	#1,d0
	beq.s	.Ldone

	| HARD-WON: chf_drain_changes takes dev as an ORDINARY STACK ARGUMENT,
	| not a register-bound parameter -- see device.c's own header comment
	| on chf_drain_changes for the full story (a `__asm("a5")` parameter
	| binding was tried first and silently ignored by this GCC target,
	| since A5 is callee-saved; the compiled callee read its "parameter"
	| off the stack regardless of the declaration, and a plain
	| `bsr.w` with no push left it reading garbage there). Push dev (a5),
	| call, then pop the argument back off -- this toolchain's own
	| plain-C-call convention (caller pushes, caller cleans up), matching
	| the ordinary `chf_mount_all(sysbase, board, cd)` call site in
	| device.c's resident_init (three `move.l reg,-(sp)` pushes, then
	| `lea 12(sp),sp` to clean up, confirmed via objdump).
	move.l	a5,-(sp)		| push dev
	bsr.w	_chf_drain_changes	| chf_drain_changes(dev); may trash
					| d0/d1/a0/a1 (ordinary C-callee
					| discipline); a5 itself is
					| GCC-callee-preserved, so dev
					| survives too, though nothing below
					| needs it again.
	addq.l	#4,sp			| caller cleans up the one stack argument

.Ldone:
	moveq	#-1,d0
	tst.l	d0			| force Z clear: this interrupt was
					| ours, regardless of what flags any
					| library/C call above left behind
	rts

.Lnot_ours:
	rts				| Z is already set from the move.w
					| above; nothing else has touched the
					| flags since
