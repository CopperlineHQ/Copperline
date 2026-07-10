| SPDX-License-Identifier: GPL-3.0-or-later
|
| Entry table of the handler ROM. Linked first, so these instructions sit at
| ROM_OFFSET in the board window (see copperline_board.h). Everything must stay
| PC-relative: the ROM runs at whatever base autoconfig assigns the board.

	.text
	.globl	_entry_table
	.globl	_handler_main
	.globl	_mount_boards

_entry_table:
	| +0: handler process entry (DOS RunHandler jumps here via dn_SegList)
	bra.w	_handler_main
	| +4: mount entry. The DiagArea shim has already pushed the C arguments
	| (board, ExpansionBase, ConfigDev) and jsr'd here, so the stack is
	| exactly a C call frame: tail-branch into the C function.
	bra.w	_mount_boards
