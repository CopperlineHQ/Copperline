| SPDX-License-Identifier: GPL-3.0-or-later
|
| Entry table and DiagArea of the handler ROM. Linked first, so this sits at
| ROM_OFFSET in the board window (see copperline_board.h). Everything must
| stay PC-relative: the ROM runs at whatever base autoconfig assigns.

	.text
	.globl	_entry_table
	.globl	_handler_main
	.globl	_resident_init

_entry_table:
	| +0: handler process entry (DOS RunHandler jumps here via dn_SegList)
	bra.w	_handler_main

	| +4: expansion-init entry. The DiagArea's DiagPoint jsr's here from
	| the diag copy with the documented DiagPoint registers still live:
	| A0 = board base, A2 = base of the RAM diag copy Kickstart just made.
	| Ring the board's DIAG_DOORBELL with the base as the value (the host
	| captures it and resets per-boot state), patch our Romtag in the diag
	| copy (see _romtag below -- rt_Init must NOT run yet: DOS-list surgery
	| this early corrupts Kickstart 1.3's boot under real hardware and here
	| alike, per the A590 boot ROM's own Romtag-deferred recipe), and
	| return D0 != 0 so Kickstart keeps the diag copy: strap calls
	| da_BootPoint from it if one of our mounts wins the boot vote, and
	| Kickstart's cold-start resident scan calls rt_Init once dos-list
	| mounting is actually safe.
	|
	| KNOWN ISSUE: this avoids the DiagPoint-time corruption, but the
	| resident-scan timing empirically runs too late for the boot vote
	| on every Kickstart version tested (1.3, 2.0, 3.1) -- see the
	| resident_init() comment in handler.c.
_diag_entry:
	move.l	a0,0x7E00(a0)	| DIAG_DOORBELL = board base
	| rt_Match/rt_End/rt_Name/rt_Id were coded as DiagArea-relative offsets
	| (assembler constants); add the RAM diag copy's base (a2, via d0 --
	| ADD's memory destination form takes a data register source only) to
	| turn each into a real pointer, matching the documented DiagEntry
	| patch recipe.
	move.l	a2,d0
	add.l	d0,(_rt_match-_diag_area)(a2)
	add.l	d0,(_rt_end-_diag_area)(a2)
	add.l	d0,(_rt_name-_diag_area)(a2)
	add.l	d0,(_rt_id-_diag_area)(a2)
	| rt_Init stays resident code (in the persistent board window, not the
	| diag copy Kickstart may discard), so it's patched with the board base
	| (a0) instead.
	move.l	a0,d0
	add.l	d0,(_rt_init-_diag_area)(a2)
	moveq	#1,d0
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
	| Called by strap (with A6 = ExecBase) when one of our BootNodes has
	| the highest boot priority -- on 2.0+ via AddBootNode, on 1.3 via
	| the BootNode mount_boards enqueues by hand. The standard autoboot
	| boot code, same as real autoboot ROMs and both strap generations:
	| fire up dos.library, whose init then mounts the highest-priority
	| BootNode -- ours -- as SYS:. Returns (boot failed, strap tries the
	| next candidate) only if dos.library is missing.
	lea	_dos_name(pc),a1
	jsr	-96(a6)		| FindResident("dos.library")
	tst.l	d0
	beq.s	1f
	move.l	d0,a0
	move.l	22(a0),d0	| rt_Init
	beq.s	1f
	move.l	d0,a0
	jsr	(a0)		| boots DOS; does not return on success
1:	moveq	#0,d0
	rts
_dos_name:
	.asciz	"dos.library"

	| struct Resident ("Romtag"; exec/resident.h), scanned for by Kickstart's
	| normal cold-start resident-module init once expansion has finished
	| DiagPoint-ing every board (the same pass that inits dos.library
	| itself): the documented, hardware-proven place to do DOS-list surgery
	| for an autoboot driver (RKRM Libraries, "Expansion Library" chapter,
	| "Events At ROMTAG INIT Time"; confirmed against the A590 SCSI boot
	| ROM's own DiagPoint, which is this tiny and defers identically).
	| rt_Init is called with D0=0, A0=NULL segList, A6=ExecBase; it re-opens
	| expansion.library and calls GetCurrentBinding() for its ConfigDev,
	| since none of DiagPoint's registers are handed to it directly.
_romtag:
	.short	0x4AFC				| rt_MatchWord (RTC_MATCHWORD)
_rt_match:
	.long	_romtag-_diag_area		| rt_MatchTag (patched: +diag copy)
_rt_end:
	.long	_romtag_end-_diag_area		| rt_EndSkip (patched: +diag copy)
	.byte	1				| rt_Flags = RTF_COLDSTART
	.byte	0				| rt_Version
	.byte	3				| rt_Type = NT_DEVICE
	.byte	20				| rt_Pri
_rt_name:
	.long	_diag_name-_diag_area		| rt_Name (patched: +diag copy)
_rt_id:
	.long	_diag_name-_diag_area		| rt_IdString (patched: +diag copy)
_rt_init:
	.long	_resident_init-_entry_table+8	| rt_Init (patched: +board base)
_romtag_end:
_diag_name:
	.asciz	"Copperline"
	.balign	2
_diag_area_end:
