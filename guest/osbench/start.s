| SPDX-License-Identifier: GPL-3.0-or-later
| CLI entry: a0 points at the argument bytes, d0 gives their length.
| The library-call stubs keep the program free of NDK headers.
	.text
	.globl _start
_start:
	movem.l d2-d7/a2-a6,-(sp)
	move.l d0,-(sp)
	move.l a0,-(sp)
	jsr _entry
	addq.l #8,sp
	movem.l (sp)+,d2-d7/a2-a6
	rts

| long dos4(long lvo, long d1, long d2, long d3, long d4)
	.globl _dos4
_dos4:
	movem.l d2-d4/a6,-(sp)
	move.l _DOSBase,a6
	move.l 20(sp),d0
	move.l 24(sp),d1
	move.l 28(sp),d2
	move.l 32(sp),d3
	move.l 36(sp),d4
	jsr 0(a6,d0.l)
	movem.l (sp)+,d2-d4/a6
	rts

| long exec4(long lvo, long d0, long d1, long a0, long a1)
	.globl _exec4
_exec4:
	movem.l d2/a6,-(sp)
	move.l 4.w,a6
	move.l 12(sp),d2
	move.l 16(sp),d0
	move.l 20(sp),d1
	move.l 24(sp),a0
	move.l 28(sp),a1
	jsr 0(a6,d2.l)
	movem.l (sp)+,d2/a6
	rts
