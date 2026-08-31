| SPDX-License-Identifier: GPL-3.0-or-later
|
| INT2 server entry point for copperhf.device, installed on INTB_PORTS via
| AddIntServer (device.c's resident_init). Register convention verbatim
| from exec.doc's AddIntServer entry: D0/D1/A0/A1/A5/A6 scratch (A1 = this
| server's is_Data, the board base -- see device.c), every other register
| must be preserved. Must return with the 68000 Z flag CLEAR if this
| interrupt was ours (INTB_PORTS is a shared chain -- real hardware shares
| it with CIA-A, so a real boot's keyboard/floppy handling depends on
| getting this right when there is nothing here for us to do), Z SET
| otherwise -- exactly the same contract guest/mhi/int_handler.s documents
| and implements, which this file mirrors move-for-move against
| copperhf_board.h's protocol instead of MHI's.
|
| A plain C function cannot reliably control the Z flag on return --
| exec.doc's own WARNING under AddIntServer says so explicitly ("Some
| compilers... may not have a mechanism for reliably setting the Z flag on
| exit"), which is why this is hand-written asm and device.c's own header
| comment defers to this file instead.
|
| copperhf_board.h's protocol (see its own header comment): CHF_IRQ_STATUS
| bit 0 is the only defined bit, so a plain move.w's own Z flag already
| answers "pending or not" with no extra btst needed. CHF_COMPLETE_GET is
| idempotent (does not pop by itself), so the drain loop must write
| CHF_COMPLETE_ACK after every ReplyMsg to actually advance the queue --
| skipping that would re-deliver the same completion forever.

#include "copperhf_board.h"

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
	| in: a1 = is_Data = board base. Moved to a5: the one register that
	| is BOTH in the server chain's scratch set (D0/D1/A0/A1/A5/A6 -- see
	| the header comment; anything else, a2 included, must come back to
	| the dispatcher untouched, and exec's server loop really does keep
	| its list iterator live across this call) AND preserved across the
	| ReplyMsg library call below (library callees may only trash
	| D0/D1/A0/A1), so the board base survives the drain loop with no
	| save/restore. An earlier version parked it in a2 instead and
	| Guru'd every boot: the dispatcher advanced ln_Succ from our
	| leftover a2, read all-ones from the board window's unmapped first
	| longword, and address-errored on the next node dereference.
	movea.l	a1,a5
	move.w	CHF_IRQ_STATUS(a5),d0
	beq.s	.Lnot_ours		| Z set by the move.w itself if the
					| completion-queue-non-empty bit (the
					| register's only defined bit) is
					| clear: nothing pending for us.

.Ldrain:
	move.l	CHF_COMPLETE_GET(a5),d0	| 32-bit, idempotent snapshot of the
					| queue head (0 = empty)
	tst.l	d0
	beq.s	.Ldone
	movea.l	d0,a1			| a1 = completed IORequest pointer
	move.l	4.w,a6
	jsr	LVO_REPLYMSG(a6)	| ReplyMsg(a1=msg)
	move.w	#0,CHF_COMPLETE_ACK(a5)	| any write pops the oldest completion
	bra.s	.Ldrain

.Ldone:
	moveq	#-1,d0
	tst.l	d0			| force Z clear: this interrupt was
					| ours, regardless of what flags
					| ReplyMsg itself left behind
	rts

.Lnot_ours:
	rts				| Z is already set from the move.w
					| above; nothing else has touched the
					| flags since
