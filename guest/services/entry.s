| SPDX-License-Identifier: GPL-3.0-or-later
|
| Entry table and DiagArea of the handler ROM. Linked first, so this sits at
| ROM_OFFSET in the board window (see copperline_board.h). Everything must
| stay PC-relative: the ROM runs at whatever base autoconfig assigns.

	.text
	.globl	_entry_table
	.globl	_handler_main
	.globl	_mount_boards

_entry_table:
	| +0: handler process entry (DOS RunHandler jumps here via dn_SegList)
	bra.w	_handler_main

	| +4: expansion-init entry. The DiagArea's DiagPoint jsr's here from
	| the diag copy with the documented DiagPoint registers still live:
	| A0 = board base, A3 = ConfigDev, A5 = ExpansionBase. Trap to the
	| host (which captures the board base), mount the configured volumes,
	| and return D0 = 0 so Kickstart frees the diag copy -- nothing
	| references it afterwards.
_diag_entry:
	.short	0xA400		| TRAP_DIAG_ENTRY
	move.l	a3,-(sp)	| ConfigDev
	move.l	a5,-(sp)	| ExpansionBase
	move.l	a0,-(sp)	| board base
	bsr.w	_mount_boards	| mount_boards(board, ExpansionBase, ConfigDev)
	lea	12(sp),sp
	moveq	#0,d0
	rts

	| struct DiagArea (libraries/configregs.h), at the fixed ROM offset
	| DIAG_AREA_IN_ROM: er_InitDiagVec points here and Kickstart copies
	| da_Size bytes to RAM before calling da_DiagPoint. All code offsets
	| are relative to the copy, so the DiagPoint stub reaches the ROM
	| through A0 (the board base) -- a bsr would aim into the copy.
	| Hard-won Kickstart 3.x gotchas: da_Config needs a DAC_BOOTTIME bit
	| or the area is abandoned after one read, and DAC_CONFIGTIME
	| requires a non-zero da_BootPoint.
	.org	0x40		| errors out if the code above grows past this
_diag_area:
	.byte	0x90, 0x00			| da_Config = DAC_WORDWIDE
					|   | DAC_CONFIGTIME; da_Flags
	.short	_diag_area_end-_diag_area	| da_Size
	.short	_diag_point-_diag_area		| da_DiagPoint
	.short	_boot_point-_diag_area		| da_BootPoint
	.short	_diag_name-_diag_area		| da_Name
	.short	0, 0				| da_Reserved01/02
_diag_point:
	jsr	(_diag_entry-_entry_table+8)(a0) | +8 = ROM_OFFSET
	rts
_boot_point:
	| Never usefully called (see da_Config note), but must exist.
	moveq	#0,d0
	rts
_diag_name:
	.asciz	"Copperline"
	.balign	2
_diag_area_end:
