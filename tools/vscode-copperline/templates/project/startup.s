/* SPDX-License-Identifier: CC0-1.0 */
	.text
	.globl _start
	.globl start
	.extern main
_start:
start:
	movem.l %d2-%d7/%a2-%a6,-(%sp)
	jsr main
	movem.l (%sp)+,%d2-%d7/%a2-%a6
	rts
