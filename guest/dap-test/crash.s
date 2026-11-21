; A vasm probe carrying LINE debug hunks (-linedebug) for the DAP
; adapter's exception unwinding. fetch reads a word at an odd address,
; which raises an address error on the 68000: the adapter stops on the
; catch at the operating system's handler and must show the faulting
; line, with the call in start below it.

        section code,code
start:
        lea     buffer+1,a0
        bsr.s   fetch
        moveq   #0,d0
        rts

fetch:
        move.w  (a0),d0
        rts

        section data,data
buffer:
        dc.l    0
