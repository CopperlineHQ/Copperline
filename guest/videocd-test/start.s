| SPDX-License-Identifier: GPL-3.0-or-later
|
| Keep the CLI entry at the first instruction even when GCC places constants
| before C functions in the first hunk.
	.text
	.globl	_start
_start:
	bra.w	_entry
