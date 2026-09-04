/* SPDX-License-Identifier: CC0-1.0 */
	.text
	.globl _CopperlineFormatPutChar
_CopperlineFormatPutChar:
	move.b %d0,(%a3)+
	rts
