| SPDX-License-Identifier: GPL-3.0-or-later
|
| Linked first: the CLI jumps to the start of the first hunk, and the
| compiler is free to place rodata (string literals) ahead of the C entry
| function, so the real entry must be reached through this stub -- same
| convention as guest/hostfs-test/start.s. A0/D0 are left untouched: the
| Shell's own CLI calling convention already hands the command-line tail in
| A0 (pointer) and D0 (length), which mhitest.c's entry() picks up via its
| own __asm("a0")/__asm("d0") parameter bindings.
	.text
	.globl	_start
_start:
	bra.w	_entry
