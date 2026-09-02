| SPDX-License-Identifier: GPL-3.0-or-later
|
| Entry table and DiagArea of copperhf.device's boot ROM. Adapted from
| guest/services/entry.s and guest/hostsocket/entry.s -- same PC-relative
| discipline, same DiagArea/Romtag layout, same deferred-rt_Init recipe.
| Read those files' own header comments for the two hard-won traps this
| file must not repeat:
|
|   - Never `.long` an external symbol directly under -mpcrel: it resolves
|     PC-relative to the *field's own address*, not the intended target,
|     silently landing short of it (guest/services/entry.s's own header
|     tells the corrupted-boot story). Only `.short label-label` distances
|     between assembly-time-resolved symbols, or ordinary compiler-
|     generated PC-relative code (`lea sym(pc),reg`), are safe -- which is
|     why the four device vectors and dev_init below are written in C
|     (device.c) but only ever *referenced* from here via `label-label`
|     word displacements in `_func_table`, never `.long`'d directly.
|   - `da_Config` needs DAC_CONFIGTIME, which requires a non-zero
|     `da_BootPoint` -- `_boot_point` boots dos.library on V34 the same way
|     guest/services/entry.s's own da_BootPoint does (M3: mounter.c's V34
|     fallback path Enqueue's a hand-built BootNode on eb_MountList for
|     strap to find; V36+ never calls da_BootPoint at all, AddBootNode
|     handles it).
|
| This board is a device, not a library or DOS handler: `rt_Type` is
| NT_DEVICE, and `rt_Init` (patched by `_diag_entry` below, deferred to
| Kickstart's normal cold-start resident scan for the same reason
| documented in guest/services/entry.s and guest/hostsocket/entry.s -- DOS
| surgery this early corrupts 1.3's boot) lands in `_resident_init`
| (device.c), which builds the device with MakeLibrary (the same call used
| for libraries -- see its own autodoc: "the same call is used to make
| devices") and AddDevice()s it.
|
| The ROM is served at window offset ROM_OFFSET (0x08, matching the
| filesys/hostsocket convention of leaving the first 8 bytes of the window
| free even though this board's window is otherwise pure registers above
| 0x4000); DIAG_OFFSET (src/copperhf.rs) is ROM_OFFSET + 0x40 to match.

	.text
	.globl	_entry_table
	.globl	_func_table
	.globl	_device_name

_entry_table:
	| +0: process entry -- unused. copperhf.device is a plain NT_DEVICE,
	| never RunHandler'd; kept only so the entry table shape matches this
	| project's other ROMs.
	rts
	nop
	| +4: rt_Init entry: the Romtag's rt_Init points here (patched with the
	| board base by _diag_entry). A local trampoline, per the header
	| comment above -- rt_Init must not name _resident_init in a .long
	| directly.
_rt_init_entry:
	bra.w	_resident_init
	| +8 = ROM_OFFSET: expansion-init entry. The DiagArea's DiagPoint
	| jsr's here from the diag copy with the documented DiagPoint
	| registers still live: A0 = board base, A2 = base of the RAM diag
	| copy Kickstart just made. Patches our Romtag's PC-relative fields
	| into the diag copy and returns D0 != 0 so Kickstart keeps the
	| copy around for the cold-start resident scan to find; real device
	| construction is deferred to rt_Init (see this file's header
	| comment for why).
_diag_entry:
	move.l	a2,d0
	add.l	d0,(_rt_match-_diag_area)(a2)
	add.l	d0,(_rt_end-_diag_area)(a2)
	add.l	d0,(_rt_name-_diag_area)(a2)
	add.l	d0,(_rt_id-_diag_area)(a2)
	| rt_Init stays resident code (in the persistent board window, not
	| the diag copy Kickstart may discard), so it's patched with the
	| board base (a0) instead.
	move.l	a0,d0
	add.l	d0,(_rt_init-_diag_area)(a2)
	moveq	#1,d0
	rts

	| struct DiagArea (libraries/configregs.h), at the fixed ROM offset
	| DIAG_AREA_IN_ROM: er_InitDiagVec points here and Kickstart copies
	| da_Size bytes to RAM before calling da_DiagPoint. Hard-won
	| Kickstart 3.x gotchas (see guest/services/entry.s): da_Config
	| needs DAC_CONFIGTIME, which needs a non-zero da_BootPoint.
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
	| Called by strap (A6 = ExecBase) when one of our BootNodes -- V34
	| only; V36+ boots through AddBootNode's own strap integration, which
	| never calls da_BootPoint at all -- has the highest boot priority.
	| Mirrors guest/services/entry.s's own da_BootPoint move-for-move:
	| fire up dos.library, whose init mounts the highest-priority
	| BootNode (mounter.c's chf_add_boot_node_v34 built and Enqueue'd it
	| on eb_MountList) as SYS:. Returns (boot failed, strap tries the
	| next candidate) only if dos.library is missing. Not exercised by
	| the AROS CI gate (it boots V36+ only); correctness here rests on
	| this being byte-for-byte the same recipe as the already
	| hardware-proven guest/services ROM.
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
	.balign	2

	| struct Resident ("Romtag"; exec/resident.h). rt_Init is called
	| with D0=0, A0=NULL segList, A6=ExecBase once expansion has
	| DiagPoint-ed every board; _resident_init (device.c) re-derives the
	| board base itself via expansion.library's GetCurrentBinding(),
	| since none of DiagPoint's own registers are handed to rt_Init.
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
	.asciz	"copperhf.device"
	.balign	2
_diag_area_end:

	| Shared device-name string: also the device's ln_Name (device.c's
	| dev_init sets it from here) -- OpenDevice("copperhf.device", ...)
	| must match this exactly.
_device_name = _diag_name

	| The function vector table MakeLibrary consumes (see its autodoc
	| INPUTS): first word -1 selects *displacement* mode (word offsets
	| relative to this table's own address), the only form that can be a
	| compile-time constant in PIC ROM code -- an absolute-pointer table
	| would need a runtime relocation fixup this ROM discipline forbids
	| (see this file's header comment). The four standard LVOs (Open/
	| Close/Expunge/Reserved, offsets -6/-12/-18/-24) come first, exactly
	| like every Amiga library/device; BeginIO (-30) and AbortIO (-36)
	| are this device's own two vectors, matching
	| COPPERHF-DEVICE-PLAN.md's M2 spec.
	|
	| HARD-WON: every entry here must be a label defined in *this file*,
	| never a `label-label` distance where either label lives in another
	| object file (device.c). Unlike a `bra.w`/`jsr` branch (an ordinary
	| PC-relative relocation this toolchain's linker resolves correctly
	| across object files -- see _rt_init_entry above and every LVO call
	| in device.c/int_handler.s), a `.short symA-symB` *difference*
	| relocation spanning two object files is silently miscomputed by
	| this toolchain's linker: an earlier version of this table read
	| `.short _dev_open - _func_table` etc. directly, and every entry
	| came out wrong (confirmed against `nm`'s own symbol values for
	| _func_table/_dev_open) -- MakeLibrary read those bogus displacements
	| as this device's Open/Close/Expunge/BeginIO/AbortIO addresses and
	| jumped through a random one live during boot, Guru-ing at whatever
	| garbage byte pattern happened to follow in ROM. The fix: each entry
	| below is a same-file trampoline (`.short trampoline-_func_table`,
	| a same-object distance, always a compile-time constant) that
	| `bra.w`s to the real C function in device.c -- an ordinary branch,
	| not a difference, so it links correctly.
_func_table:
	.short	-1
	.short	_open_tramp    - _func_table	| -6  Open
	.short	_close_tramp   - _func_table	| -12 Close
	.short	_expunge_tramp - _func_table	| -18 Expunge
	.short	_extfunc_tramp - _func_table	| -24 ExtFunc (reserved)
	.short	_beginio_tramp - _func_table	| -30 BeginIO
	.short	_abortio_tramp - _func_table	| -36 AbortIO
	| Terminator: exec.doc's own MakeLibrary INPUTS spells this out --
	| "The vector list is terminated by a -1 (of the same size as the
	| pointers)" -- word-sized here, matching the displacement-mode
	| header.
	.short	-1

_open_tramp:
	bra.w	_dev_open
_close_tramp:
	bra.w	_dev_close
_expunge_tramp:
	bra.w	_dev_expunge
_extfunc_tramp:
	bra.w	_dev_extfunc
_beginio_tramp:
	bra.w	_dev_beginio
_abortio_tramp:
	bra.w	_dev_abortio
