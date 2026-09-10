| SPDX-License-Identifier: GPL-3.0-or-later
|
| Entry table and DiagArea of the handler ROM. Linked first, so this sits at
| ROM_OFFSET in the board window (see copperline_board.h). Everything must
| stay PC-relative: the ROM runs at whatever base autoconfig assigns.
|
| HARD-WON: never reference a symbol from another object file (handler.c)
| in a data directive here. Under -mpcrel the toolchain resolves
| `.long _external_sym-...` PC-relative to the field's own address, not as
| the intended section offset, silently storing a pointer that lands ~0x92
| bytes short of the target. That bug shipped in this ROM's Romtag rt_Init:
| on every Kickstart (1.3, 2.0, 3.1), InitResident faithfully jumped into
| the middle of handler_main's packet loop, corrupting the boot -- a Guru
| on 1.3, a reset loop on 3.1 -- while looking exactly like "the resident
| scan never calls us". External code is only ever reached through real
| PC-relative branches (bra.w below); data directives only ever encode
| differences of entry.s-local labels, which are plain assembly-time
| constants.

	.text
	.globl	_entry_table
	.globl	_handler_main
	.globl	_resident_init

_entry_table:
	| +0: handler process entry (DOS RunHandler jumps here via dn_SegList)
	bra.w	_handler_main
	| +4: rt_Init entry: the Romtag's rt_Init points here (patched with
	| the board base by _diag_entry). A local trampoline, per the header
	| comment -- rt_Init must not name _resident_init in a .long directly.
_rt_init_entry:
	bra.w	_resident_init

	| Expansion-init entry. The DiagArea's DiagPoint jsr's here from the
	| diag copy with the documented DiagPoint registers still live:
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
	.long	_diag_area_end-_diag_area	| rt_EndSkip (patched: +diag copy)
	.byte	1				| rt_Flags = RTF_COLDSTART
	.byte	0				| rt_Version
	.byte	3				| rt_Type = NT_DEVICE
	.byte	20				| rt_Pri
_rt_name:
	.long	_diag_name-_diag_area		| rt_Name (patched: +diag copy)
_rt_id:
	.long	_diag_name-_diag_area		| rt_IdString (patched: +diag copy)
_rt_init:
	.long	_rt_init_entry-_entry_table+8	| rt_Init (patched: +board base)
_diag_name:
	.asciz	"Copperline"
	.balign	2
_diag_area_end:

	| Clipboard bridge callbacks (see clipboard_main in handler.c). Both
	| are handed a struct ClipShared through their data pointer:
	|   +0  regs        APTR, the clipboard register bank in the window
	|   +4  task        APTR, the bridge process
	|   +8  irq_sigmask ULONG, signalled by the INT2 server
	|   +12 hook_sigmask ULONG, signalled by the clipboard hook
	|   +16 hook_clip_id LONG, chm_ClipID of the newest hook message
	| Assembly rather than C: each runs in a foreign context (an interrupt
	| server, clipboard.device's own task) with its own register contract,
	| which the C ABI cannot express. Both only touch D0/D1/A0/A1 (plus a
	| saved A6), so they preserve everything either contract demands.
	.globl	_clip_int_server
	.globl	_clip_hook

	| INTB_PORTS server: A1 = is_Data (ClipShared), A6 = ExecBase. If the
	| board is holding INT2 for us (CLIP_ST_IRQ), acknowledge it -- which
	| drops the line -- and Signal() the bridge process, which reads the
	| registers from a proper task context. Returns Z set so the shared
	| level-2 chain carries on to the other PORTS servers.
_clip_int_server:
	move.l	(a1),a0			| regs
	move.l	0x80(a0),d0		| CLIP_REG_STATUS
	btst	#0,d0			| CLIP_ST_IRQ
	beq.s	1f
	move.l	#3,0x40(a0)		| CLIP_REG_CTRL = CLIP_CTRL_IRQACK
	move.l	8(a1),d0		| irq_sigmask
	move.l	4(a1),a1		| task
	jsr	-324(a6)		| Signal()
1:	moveq	#0,d0
	rts

	| CBD_CHANGEHOOK hook: A0 = struct Hook (h_Data at +16 = ClipShared),
	| A1 = struct ClipHookMsg (chm_ClipID at +8), A2 = the clipboard unit.
	| Runs in clipboard.device's context: record the clip ID of the change
	| and Signal() the bridge, which decides whether it was its own write.
_clip_hook:
	move.l	16(a0),a0		| h_Data: ClipShared
	move.l	8(a1),16(a0)		| hook_clip_id = chm_ClipID
	move.l	a6,-(sp)
	move.l	4.w,a6
	move.l	12(a0),d0		| hook_sigmask
	move.l	4(a0),a1		| task
	jsr	-324(a6)		| Signal()
	move.l	(sp)+,a6
	moveq	#0,d0
	rts
