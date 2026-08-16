| SPDX-License-Identifier: GPL-3.0-or-later
|
| INT2 server entry point for mhi_copperline.library, installed on
| INTB_PORTS via AddIntServer (board.c's mhi_board_int_install). Register
| convention verbatim from exec.doc's AddIntServer entry: D0/D1/A0/A1/A5/A6
| scratch (A1 = this server's is_Data, the struct MHIPlayer* passed to
| AddIntServer -- see board.c), every other register must be preserved.
| Must return with the 68k Z flag CLEAR if this interrupt was ours
| (INTB_PORTS is a shared chain -- real hardware shares it with CIA-A, so a
| real boot's keyboard/floppy handling depends on getting this right when
| there is nothing here for us to do), Z SET otherwise.
|
| A plain C function cannot reliably control the Z flag on return --
| exec.doc's own WARNING under AddIntServer says so explicitly ("Some
| compilers... may not have a mechanism for reliably setting the Z flag on
| exit"), and it's why guest/hostsocket/entry.s's own _int_handler is
| hand-written asm too, not C. This is the one piece of mhi_copperline's
| board-access layer that has to be assembly; board.c and board.h (which
| this file #includes, in MHI_ASSEMBLY mode, for the register offsets and
| MHI_OFF_*/MHI_LVO_SIGNAL constants) are ordinary C.
|
| is_Data (a1) points at struct MHIPlayer (mhi_copperline.h); MHI_OFF_BOARD/
| MHI_OFF_TASK/MHI_OFF_SIGMASK (board.h) are its first three fields, kept in
| that exact order by hand -- see board.h's own comment on those macros.

#define MHI_ASSEMBLY 1
#include "board.h"

	.text
	.globl	_mhi_int_handler

_mhi_int_handler:
	movea.l	MHI_OFF_BOARD(a1),a0	| a0 = board register window base
	move.w	MHI_REG_INTENA(a0),d0
	move.w	MHI_REG_INTREQ(a0),d1
	and.w	d0,d1			| d1 = pending & enabled (BUFFER_DONE /
					| OUT_OF_DATA bits this server cares
					| about -- QUEUE_OVERFLOW is never
					| enabled, see board.c's
					| mhi_board_int_install)
	beq.s	.Lnot_ours		| nothing pending for us: Z already
					| set by the and.w above, just rts

	move.w	d1,MHI_REG_INTREQ(a0)	| write-1-to-clear exactly the bits we
					| observed (mhi.md "Interrupts": ack
					| protocol is write-1-to-clear, and
					| the whole point of doing it here
					| rather than as a side effect of some
					| other register read is to never
					| lose a completion a client hasn't
					| consumed yet)

	move.l	MHI_OFF_SIGMASK(a1),d0	| d0 = signal mask (Signal()'s d0 input)
					| -- read before clobbering a1 below
	movea.l	MHI_OFF_TASK(a1),a1	| a1 = client task (Signal()'s a1
					| input) -- overwrites is_Data, which
					| nothing else here needs again
	move.l	4.w,a6
	jsr	MHI_LVO_SIGNAL(a6)	| Signal(a1=task, d0=mask)
	moveq	#-1,d0
	tst.l	d0			| force Z clear: this interrupt was
					| ours, regardless of what flags
					| Signal() itself left behind
	rts

.Lnot_ours:
	rts				| Z is already set from the `and.w`
					| above; nothing else has touched the
					| flags since
