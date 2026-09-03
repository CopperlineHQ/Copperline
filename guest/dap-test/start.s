| SPDX-License-Identifier: GPL-3.0-or-later
|
| Linked first: the CLI jumps to the start of the first hunk, and the
| compiler is free to place rodata ahead of the C entry function, so
| the real entry is reached through this stub (see guest/hostfs-test).
| The three library calls the probe makes are wrapped here rather than
| through the NDK inline headers, so the fixture builds with any
| m68k-amigaos-gcc and no NDK; the wrappers also give the adapter's
| call-stack tests frames without debug information.
	.text
	.globl	_start
_start:
	bra.w	_entry

| struct Library *OpenLibrary(const char *name, ULONG version)
	.globl	_OpenLibrary
_OpenLibrary:
	move.l	%a6,-(%sp)
	move.l	8(%sp),%a1
	move.l	12(%sp),%d0
	move.l	4.w,%a6
	jsr	-552(%a6)
	move.l	(%sp)+,%a6
	rts

| void CloseLibrary(struct Library *lib)
	.globl	_CloseLibrary
_CloseLibrary:
	move.l	%a6,-(%sp)
	move.l	8(%sp),%a1
	move.l	4.w,%a6
	jsr	-414(%a6)
	move.l	(%sp)+,%a6
	rts

| void RawPutStr(const char *str) -- exec RawPutChar per byte, so the
| text reaches the serial port (where a debugger sees it) as KPrintF's
| output does.
	.globl	_RawPutStr
_RawPutStr:
	movem.l	%a2/%a6,-(%sp)
	move.l	12(%sp),%a2
	move.l	4.w,%a6
1:	moveq	#0,%d0
	move.b	(%a2)+,%d0
	beq.s	2f
	jsr	-516(%a6)
	bra.s	1b
2:	movem.l	(%sp)+,%a2/%a6
	rts

| LONG PutStr(const char *str)  -- through _dosbase
	.globl	_PutStr
_PutStr:
	move.l	%a6,-(%sp)
	move.l	8(%sp),%d1
	move.l	__dosbase,%a6
	jsr	-948(%a6)
	move.l	(%sp)+,%a6
	rts
