| SPDX-License-Identifier: GPL-3.0-or-later
|
| Linked first: the CLI jumps to the start of the first hunk, and the
| compiler is free to place rodata (string literals) ahead of the C entry
| function, so the real entry must be reached through this stub.

	.text
	.globl	_entry
	bra.w	_entry
