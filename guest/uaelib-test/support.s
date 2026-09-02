| SPDX-License-Identifier: GPL-3.0-or-later
|
| The two RawDoFmt character sinks the vscode-amiga-debug template's
| KPrintF uses (gcc8_a_support.s): PutChar stores into the buffer RawDoFmt
| carries in a3, KPutCharX hands the character to exec's RawPutChar
| (LVO -516), which is the serial-port path KPrintF falls back to when
| the uaelib trap is absent.
	.text
	.globl	_PutChar
_PutChar:
	move.b	d0,(a3)+
	rts

	.globl	_KPutCharX
_KPutCharX:
	move.l	a6,-(sp)
	move.l	4.w,a6
	jsr	-0x204(a6)
	move.l	(sp)+,a6
	rts

| The 68000 has no 32x32 multiply; GCC calls libgcc's __mulsi3 for the
| template's bitmap-size arithmetic. No libgcc is linked (-nostdlib), so
| this is libgcc's routine: x at 4(sp), y at 8(sp), product in d0.
	.globl	___mulsi3
___mulsi3:
	move.w	4(sp),d0
	mulu.w	10(sp),d0
	move.w	6(sp),d1
	mulu.w	8(sp),d1
	add.w	d1,d0
	swap	d0
	clr.w	d0
	move.w	6(sp),d1
	mulu.w	10(sp),d1
	add.l	d1,d0
	rts
