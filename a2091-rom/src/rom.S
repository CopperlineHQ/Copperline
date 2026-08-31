**
** Open A2091/A590 autoboot ROM bootstrap.
**
** The DMAC supplies AutoConfig identity and points er_InitDiagVec at board
** offset $2000.  This image therefore begins with the DiagArea rather than
** carrying A4091-style nibble AutoConfig tables.
**

        INCLUDE "exec/types.i"
        INCLUDE "exec/nodes.i"
        INCLUDE "exec/resident.i"
        INCLUDE "lvo/exec_lib.i"
        INCLUDE "libraries/configvars.i"
        INCLUDE "version.i"

VERSION         EQU     DEVICE_VERSION

        CODE

RomStart
DiagStart
        ; This is the DiagArea structure whose relative offset from
        ; your board base appears as the Init Diag vector in your
        ; autoconfig ID information.  This structure is designed
        ; to use all relative pointers (no patching needed).
        dc.b    DAC_WORDWIDE+DAC_CONFIGTIME    ; da_Config
        dc.b    0                              ; da_Flags
        dc.w    EndCopy-DiagStart              ; da_Size
        dc.w    DiagEntry-DiagStart            ; da_DiagPoint
        dc.w    BootEntry-DiagStart            ; da_BootPoint
        dc.w    DevName-DiagStart              ; da_Name
        dc.w    0                              ; da_Reserved01
        dc.w    0                              ; da_Reserved02

*******  DiagEntry  **************************************************
**********************************************************************
*
*   success = DiagEntry(BoardBase,DiagCopy, configDev)
*   d0                  a0         a2       a3
*
*   Called by expansion architecture to relocate any pointers
*   in the copied diagnostic area.   We will patch the romtag.
*   If you have pre-coded your MakeDosNode packet, BootNode,
*   or device initialization structures, they would also need
*   to be within this copy area, and patched by this routine.
*
**********************************************************************
* These are the calling conventions for the Diag routine
*
* A7 -- points to at least 2K of stack
* A6 -- ExecBase
* A5 -- ExpansionBase
* A3 -- your board's ConfigDev structure
* A2 -- Base of diag/init area that was copied
* A0 -- Base of your board
*
* The Diag routine shall return a non-zero value in D0 for success.
* If this value is NULL, then the diag/init area that was copied
* will be returned to the free memory pool.
*

DiagEntry
        movem.l d2-d7/a2-a6,-(sp)

        ; Patch up Romtag Resident Structure in memory
        lea.l   Romtag(pc),a4                ; find Romtag in memory
        move.l  a4,RT_MATCHTAG(a4)           ; pointer to itself
        lea.l   EndCopy(pc),a1
        move.l  a1,RT_ENDSKIP(a4)
        lea.l   DevName(pc),a1
        move.l  a1,RT_NAME(a4)
        lea.l   IdString(pc),a1
        move.l  a1,RT_IDSTRING(a4)

        ; The CPU sees a board-linear payload beginning at board+$2000.
        ; Probe the TOC locations for the three supported payload spans,
        ; largest first.  A 64 KiB EPROM exposes 56 KiB because the first
        ; 8 KiB is shadowed by the board registers.
        lea.l   $2000(a0),a5
        move.l  a5,a0
        link    a4,#FP_SIZE
        moveq.l #0,d0
        bsr     InitHandle

        move.l  #(56*1024-4),d0
        bsr     RomFetch32At
        cmp.l   #$2f434448,d0
        bne.s   .try32
        move.l  #(56*1024-12),d0
        bsr     RomFetch32At
        tst.l   d0
        beq.s   .no_resident
        move.l  #(56*1024-16),d0
        bsr     RomFetch32At
        move.l  d0,d2
        bra.s   .gorelocate

.try32
        move.l  #(32*1024-4),d0
        bsr     RomFetch32At
        cmp.l   #$2f434448,d0
        bne.s   .try16
        move.l  #(32*1024-12),d0
        bsr     RomFetch32At
        tst.l   d0
        beq.s   .no_resident
        move.l  #(32*1024-16),d0
        bsr     RomFetch32At
        move.l  d0,d2
        bra.s   .gorelocate

.try16
        move.l  #(16*1024-4),d0
        bsr     RomFetch32At
        cmp.l   #$2f434448,d0
        bne.s   .no_resident
        move.l  #(16*1024-12),d0
        bsr     RomFetch32At
        tst.l   d0
        beq.s   .no_resident
        move.l  #(16*1024-16),d0
        bsr     RomFetch32At
        move.l  d0,d2
        bra.s   .gorelocate

.no_resident
        unlk    a4      ; deallocate ReadHandle
        ; destroy memory romtag
        lea.l   Romtag(pc),a4
        move.l  #0,(a4)

        bra .done

.gorelocate
        unlk    a4
        move.l  a5,a0
        move.l  d2,d0
        bsr     _relocate
        tst.l   d0
        bne.s   .ok

        *******  Show checkered purple failure screen **************************
        movem.l d2-d3,-(sp)
        move.w  #$0020,d2
.fc1
        move.w  #$ffff,d3
.fc2
        move.w  #$0000,$dff180               ; black
        move.w  #$0f0c,$dff180               ; purple
        dbra    d3,.fc2
        dbra    d2,.fc1
        movem.l (sp)+,d2-d3
        *******  End checkered purple failure screen **************************

        moveq.l #0,d0
        movem.l (sp)+,d2-d7/a2-a6
        rts
.ok
        lea.l   Romtag(pc),a4                ; find Romtag in memory
        move.l  d0,a2
        addq.l  #4,a2

        cmp.w   #$4afc,4(a2)
        beq.s   .new_style
.old_style
        move.l  a2,RT_INIT(a4)
        bra.s   .done
.new_style
        add.l   #4+RT_INIT,a2 ; skip _start and find _auto_init_tables
        move.l  (a2),RT_INIT(a4)

.done
        movem.l (sp)+,d2-d7/a2-a6

        moveq.l #1,d0           ; indicate "success"

        rts

*******  BootEntry  **************************************************
**********************************************************************

BootEntry
        lea     DosName(PC),a1          ; 'dos.library',0
        jsr     _LVOFindResident(a6)    ; find the DOS resident tag
        tst.l   d0
        beq.s   .BadBoot
        move.l  d0,a0                   ; in order to bootstrap
        move.l  RT_INIT(A0),a0          ; set vector to DOS INIT
        jsr     (a0)                    ; and initialize DOS
.BadBoot
        rts

*******  Resident Structure  *****************************************
**********************************************************************
Romtag
        dc.w    RTC_MATCHWORD      ; UWORD RT_MATCHWORD
        dc.l    0                  ; APTR  RT_MATCHTAG
        dc.l    0                  ; APTR  RT_ENDSKIP
        dc.b    RTF_COLDSTART      ; UBYTE RT_FLAGS
        dc.b    VERSION            ; UBYTE RT_VERSION
        dc.b    NT_DEVICE          ; UBYTE RT_TYPE
        dc.b    10                 ; BYTE  RT_PRI
        dc.l    0                  ; APTR  RT_NAME
        dc.l    0                  ; APTR  RT_IDSTRING
        dc.l    0                  ; APTR  RT_INIT

        ; After Diag patching, our romtag will point its Init pointer
        ; to the auto_init_tables of scsi.device so that it can be
        ; called at Resident initialization time.

*******  Strings referenced in Diag Copy area  **************************
*************************************************************************
DevName
        dc.b    'scsi.device',0                    ; Name string
IdString
        dc.b    'Copperline A2091 scsidisk '
        FULL_VERSION_STR
        dc.b    0                     ; ID string
DosName
        dc.b    'dos.library',0                    ; DOS library name
        align 1     ; word align

*******  Relocation Code  ***********************************************
*************************************************************************
ROM             EQU    1
        INCLUDE "reloc.S"
*
* End of the Diag copy area which is copied to RAM
*
EndCopy

        ; keep out of the Diag area copied to RAM automatically
        section DEVICE
        public  _device
_device

*************************************************************************
        END
