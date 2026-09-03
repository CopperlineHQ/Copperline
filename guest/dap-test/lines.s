; A vasm probe carrying LINE debug hunks (-linedebug) for the DAP
; adapter's assembly-source tests. Returns 0 to the shell after a
; short call chain the adapter can step through by source line.

        section code,code
start:
        moveq   #0,d0
        moveq   #3,d1
        bsr.s   twice
        add.l   d1,d0
        moveq   #0,d0
        rts

twice:
        add.l   d1,d1
        move.l  d1,value
        rts

        section data,data
value:
        dc.l    0
