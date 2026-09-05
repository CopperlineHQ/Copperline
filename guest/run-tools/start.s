| SPDX-License-Identifier: GPL-3.0-or-later
| CLI entry: a0 points at the argument bytes, d0 gives their length.
| Preserve the shell's callee-saved registers and pass a normal C ABI.
	.text
	.globl _start
_start:
	lea 4(sp),a1
	movem.l d2-d7/a2-a6,-(sp)
	move.l a1,-(sp)
	move.l d0,-(sp)
	move.l a0,-(sp)
	jsr _entry
	lea 12(sp),sp
	movem.l (sp)+,d2-d7/a2-a6
	rts
