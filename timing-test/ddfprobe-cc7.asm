; VERBATIM gen-x mosaic copper probe.
;
; The blob (cc7blob.bin, generated from a chip-RAM dump of the running demo at
; 140s) is the demo's own memory from $34400 to $3F400: the mosaic frame
; header, the one repeated bitmap row (BPLxMOD=-32), and all per-cell-row
; copper blocks with their self-referencing COP2LC pokes. Only display CONTENT
; is patched: plane 1 row = $1111 grid / planes 2-4 zero, COLOR01/COLOR17
; writes = white, COLOR00 train writes = alternating red and green with the
; tail write dark blue. Instruction structure, registers, addresses and slot
; costs are byte-identical to the demo.
;
; Entry mirrors the demo: a minimal bootstrap copper list sets the demo's own
; display state (from its frame header at $35464: DIW/DDF, BPL1-4PT, mods
; $FFE0, manual BPL5DAT/BPL6DAT pokes), points COP2LC at the first block's
; anchor WAIT ($3A408, v=$27) and strobes COPJMP2. The block chain then
; self-runs exactly as in the demo. Compare the first raced cell against the
; grid on Copperline vs vAmiga vs FS-UAE/real: the demo ground truth (first
; cell ~1 lo-res px NARROWER than body) implies the train lands one copper
; slot later than Copperline's current model.
CUST   equ $dff000
BLOB   equ $34400
BLOBLEN equ 45056
CLIST  equ $33000                 ; small bootstrap copper list (below blob)

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)

        ; copy the blob to its original addresses. The source (inside this
        ; binary at $30000+) overlaps the $34400.. destination, and dest >
        ; src, so copy BACKWARD from the end.
        lea blob_data(pc),a0
        adda.l #BLOBLEN,a0
        lea BLOB+BLOBLEN,a1
        move.l #BLOBLEN/4-1,d0
.cp:    move.l -(a0),-(a1)
        dbra d0,.cp

        ; ---- bootstrap copper list: demo frame-header state, then chain ----
        lea CLIST,a1
        move.l #$008e2881,(a1)+   ; DIWSTRT (demo mosaic header)
        move.l #$009030c1,(a1)+   ; DIWSTOP
        move.l #$00920048,(a1)+   ; DDFSTRT
        move.l #$009400c0,(a1)+   ; DDFSTOP
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$0108ffe0,(a1)+   ; BPL1MOD -32 (repeat the single bitmap row)
        move.l #$010affe0,(a1)+   ; BPL2MOD -32
        move.l #$00e00003,(a1)+   ; BPL1PT $34B50 (demo values)
        move.l #$00e24b50,(a1)+
        move.l #$00e40003,(a1)+
        move.l #$00e64b70,(a1)+
        move.l #$00e80003,(a1)+
        move.l #$00ea4b90,(a1)+
        move.l #$00ec0003,(a1)+
        move.l #$00ee4bb0,(a1)+
        move.l #$0118111f,(a1)+   ; BPL5DAT manual poke (demo header)
        move.l #$011a0000,(a1)+   ; BPL6DAT
        move.l #$01800002,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white (grid)
        move.l #$01a20fff,(a1)+   ; COLOR17 white (grid + plane5 mirror)
        move.l #$01000200,(a1)+   ; BPLCON0 off; first block enables it
        move.l #$00840003,(a1)+   ; COP2LC -> first block anchor $3A408
        move.l #$0086a408,(a1)+
        move.l #$008a0000,(a1)+   ; COPJMP2: enter the demo's block chain
        move.l #$fffffffe,(a1)+   ; (not reached)

        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l

blob_data:
        incbin "cc7blob.bin"
