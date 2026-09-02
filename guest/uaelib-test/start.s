| SPDX-License-Identifier: GPL-3.0-or-later
|
| Linked first: the CLI jumps to the start of the first hunk, and the
| compiler is free to place rodata (string literals) ahead of the C entry
| function, so the real entry must be reached through this stub. Named
| distinctly from _entry (defined in uaelibtest.c, not here): this file only
| references it, and .globl-ing a name this file doesn't define is a
| pure no-op, easy to misread as a definition.
	.text
	.globl	_start
_start:
	bra.w	_entry
