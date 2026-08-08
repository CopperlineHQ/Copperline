; Minimal WHDLoad slave used as a project-owned regression "game".
;
; When WHDLoad passes control to ws_GameLoader the OS is gone; the slave
; paints the display a solid colour by writing COLOR00 and parks. With
; bitplane DMA off the whole frame (display window and border) shows
; COLOR00, so a screenshot is a single flat colour: deterministic and
; trivially checkable. The colour is $0B4 (teal green), unlikely to appear
; as a full frame by accident.
;
; Assemble: vasmm68k_mot -Fhunkexe -nosym -o Test.Slave testgame.asm
;
; Slave structure (whdload.i), ws_Version 10: fields through ws_info.

        org     0                       ; RPTRs are relative to _base

_base:
        dc.l    $70FF4E75               ; ws_Security: moveq #-1,d0 / rts
        dc.b    "WHDLOADS"              ; ws_ID
        dc.w    10                      ; ws_Version
        dc.w    0                       ; ws_Flags
        dc.l    $80000                  ; ws_BaseMemSize (512 KiB chip)
        dc.l    0                       ; ws_ExecInstall (must be 0)
        dc.w    _start-_base            ; ws_GameLoader
        dc.w    0                       ; ws_CurrentDir (data files in root)
        dc.w    0                       ; ws_DontCache
        dc.b    0                       ; ws_keydebug (none)
        dc.b    $45                     ; ws_keyexit (Esc)
        dc.l    0                       ; ws_ExpMem
        dc.w    _name-_base             ; ws_name
        dc.w    _copy-_base             ; ws_copy
        dc.w    _info-_base             ; ws_info

_start: ; a0 = resload base (unused: this slave never returns or loads)
        lea     $dff000,a1
        move.w  #$7fff,$96(a1)          ; DMACON: all DMA off
        move.w  #$7fff,$9a(a1)          ; INTENA: all interrupts off
        move.w  #$0b4,$180(a1)          ; COLOR00: flat teal frame
.park:  bra.s   .park

_name:  dc.b    "Copperline WHDLoad probe",0
_copy:  dc.b    "2026 Copperline",0
_info:  dc.b    "paints COLOR00 $0B4 and parks",0
