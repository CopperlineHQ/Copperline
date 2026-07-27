; Probe server: a resident serial-driven loader for the hardware reference rig.
;
; Loaded by boot.asm to $30000 like any other probe, this program immediately
; relocates itself to $70000 and then serves the serial port forever: the host
; uploads an assembled probe binary, tells it to run, and reads the probe's own
; serial output back. That turns "run a probe on real hardware" into a couple of
; seconds over a wire instead of writing a floppy image per iteration.
;
; The point of relocating is that the whole normal probe working area stays
; free, so the committed probes (test.bin, ddfprobe-*.bin, clxprobe.bin, ...)
; upload and run byte-for-byte unmodified: they still load at $30000, still use
; SCREEN $40000 / RESULTS $48000 / scratch $60000.
;
; Memory contract
;   $00000-$2FFFF  free for probes (vector table at $0 is not maintained here)
;   $30000-$6FFFF  free for probes -- the conventional load area
;   $70000-$7DFFF  RESERVED: server code
;   $7E000-$7E0FF  RESERVED: server variables
;   $7E800         RESERVED: supervisor stack top (grows down)
;   $7F000         RESERVED: user stack top (grows down)
; A probe that writes into $70000-$7FFFF destroys the server; recovery is a
; hardware reset from the control MCU. That is expected and is what the host
; watchdog is for.
;
; Wire protocol: line-oriented ASCII, all numbers hex without a 0x prefix.
; Commands should be terminated with a bare LF -- see the note in getline about
; why a trailing CRLF corrupts the first byte of a LOAD payload. Full
; documentation in tools/hwrig/README.md.
;
;   -> ID                       <- BANNER ...
;   -> PING                     <- READY
;   -> LOAD <addr> <len> <crc>  <- LOADRDY, then raw bytes, then LOADOK|LOADERR
;   -> RUN <addr>               <- BEGIN, then whatever the probe emits
;
; A probe that returns via RTS lands back in the command loop and the server
; prints READY. The committed probes do NOT return -- they end in an infinite
; display loop -- so for those the host collects output and then resets. Both
; are normal.

CUSTOM   equ    $dff000

SRVDEST  equ    $70000          ; where the server relocates itself to
VARS     equ    $7e000          ; server variables
STACKTOP equ    $7f000          ; user stack, grows down from here
SSPTOP   equ    $7e800          ; supervisor stack, relocated out of the ROM's
PROBESTK equ    $2f000          ; stack handed to a probe: clear of the server

; --- variables (in VARS)
V_AGNUS  equ    VARS+$00        ; word: VPOSR bits 14-8 (Agnus/Alice ID field)
V_DENISE equ    VARS+$02        ; word: DENISEID ($07C) raw; floats on OCS
V_CPU    equ    VARS+$04        ; word: 0 = 68000, 1 = 68010 or later
V_CHIPKB equ    VARS+$06        ; word: chip RAM in KB (512 / 1024 / 2048)
V_LINES  equ    VARS+$08        ; word: raster lines per frame (PAL 313, NTSC 263)
V_REACHKB equ   VARS+$0a        ; word: chip-window mirror boundary in KB (Agnus reach)
V_SAVESP equ    VARS+$0c        ; long: server SP parked across a probe run
V_ARG1   equ    VARS+$10        ; long: parsed command arguments
V_ARG2   equ    VARS+$14
V_ARG3   equ    VARS+$18
LINEBUF  equ    VARS+$20        ; 96 bytes of command line
LINEMAX  equ    80

; SERPER divisor. baud = clock / (SERPER+1); PAL clock 3546895, NTSC 3579545.
;   19200 -> 183 ($B7)   PAL +0.4%, NTSC +1.3%   (default: safe on both)
;   38400 ->  91 ($5B)   PAL +0.4%, NTSC +1.3%
;   115200 -> 30 ($1E)   PAL -0.7%, NTSC +0.2%
; Both ends must agree; the value in use is reported in the banner.
SERPER_V equ    183

;-------------------------------------------------------------- entry at $30000
; boot.asm jumps here. Take the machine, then get out of the probe load area.
;
; The boot block is entered UNPRIVILEGED (see the same note in test.asm), so a
; direct "move #$2700,sr" here is a privilege violation on the first
; instruction. The server stays unprivileged for the same reason test.asm does,
; and for one more: a probe must inherit exactly the CPU state a boot block
; gets. Handing it supervisor mode with IPL 7 silently disables the interrupt
; rows -- test.asm rows 19/20 read 0000 instead of 003D/0147, because no
; interrupt can ever reach a CPU masked at level 7.
;
; What the server does need is a supervisor stack it owns, because the ROM's SSP
; can sit anywhere -- including inside $70000-$7FFFF, which the relocation below
; is about to claim. An exception after that (a probe fault, or the deliberate
; one in cputype) would push straight into the server's own code. So relocate
; the supervisor stack once, and return to user mode.
start:
        lea     CUSTOM,a6
        bsr     quiesce                 ; INTENA/INTREQ/DMACON off
        lea     t0ssp(pc),a0
        move.l  a0,$80.w                ; TRAP #0 vector
        trap    #0                      ; returns unprivileged, on a new SSP
        lea     STACKTOP,sp
        move.w  #$0f00,$180(a6)         ; "alive" background until the banner

        ; Relocate to SRVDEST. The body is position independent (every code
        ; reference is PC-relative), so a straight block copy is enough. This is
        ; also the point where $70000-$7FFFF stops belonging to the ROM, which is
        ; why supervisor and the new stack are established first.
        lea     start(pc),a0
        lea     SRVDEST,a1
        move.w  #(endcode-start+3)/4-1,d0
.rel    move.l  (a0)+,(a1)+
        dbra    d0,.rel
        jmp     SRVDEST+(main-start)

; TRAP #0, entered in supervisor: move the exception frame to the server's own
; supervisor stack and RTE from there, which leaves SSP relocated and returns
; the caller to its original unprivileged mode. Eight bytes are copied so the
; 68010+ format word travels too; on a 68000 RTE consumes only the first six and
; the spare word is simply never reclaimed.
t0ssp:  move.l  sp,a0
        lea     SSPTOP,sp
        move.w  6(a0),-(sp)             ; format/vector word (68010+)
        move.l  2(a0),-(sp)             ; PC
        move.w  (a0),-(sp)              ; SR (unchanged: still user mode)
        rte

;--------------------------------------------------------- quiesce the chipset
; Interrupts and DMA off, requests cleared. Run at entry and again after every
; probe returns, so each probe starts from the same machine state regardless of
; what the previous one left behind.
quiesce:
        move.w  #$7fff,$09a(a6)         ; INTENA: disable all
        move.w  #$7fff,$09c(a6)         ; INTREQ: clear all
        move.w  #$7fff,$096(a6)         ; DMACON: all DMA off
        rts

;------------------------------------------------------------------- main body
main:
        lea     CUSTOM,a6
        lea     STACKTOP,sp
        move.w  #SERPER_V,$032(a6)      ; SERPER before anything can fail
        bsr     ident                   ; identify the silicon
        bsr     banner
        move.w  #$0080,$180(a6)         ; settled: serving (green)

cmdloop:
        lea     STACKTOP,sp             ; resynchronise after any probe
        lea     CUSTOM,a6
        bsr     getline
        lea     LINEBUF,a0

        lea     s_id(pc),a1
        bsr     match
        beq     do_id
        lea     s_ping(pc),a1
        bsr     match
        beq     do_ping
        lea     s_load(pc),a1
        bsr     match
        beq     do_load
        lea     s_run(pc),a1
        bsr     match
        beq     do_run

        lea     m_err(pc),a0
        bsr     putstr
        bra     cmdloop

;--------------------------------------------------------------- ID
do_id:  bsr     banner
        bra     cmdloop

;--------------------------------------------------------------- PING
do_ping:
        lea     m_ready(pc),a0
        bsr     putstr
        bra     cmdloop

;--------------------------------------------------------------- LOAD a len crc
; Read len raw bytes to a, checking CRC-16/XMODEM. The payload is raw: CR and LF
; inside it are data, not terminators. A byte that never arrives times out so a
; half-sent upload cannot wedge the server.
do_load:
        bsr     args3
        bne     .bad
        lea     m_loadrdy(pc),a0
        bsr     putstr

        move.l  V_ARG1,a2               ; destination
        move.l  V_ARG2,d4               ; length
        moveq   #0,d5                   ; running CRC
.rx     tst.l   d4
        beq.s   .done
        bsr     getbt
        tst.l   d1                      ; d1 != 0 -> timed out
        bne.s   .bad
        move.b  d0,(a2)+
        bsr     crcbyte
        subq.l  #1,d4
        bra.s   .rx
.done
        move.l  V_ARG3,d0
        cmp.w   d5,d0
        bne.s   .badcrc
        lea     m_loadok(pc),a0
        bsr     putstr
        bra     cmdloop
; Report the CRC actually computed. A value that is wrong but stable across
; retries means the two implementations disagree; a value that changes from run
; to run means bytes are being lost on the link.
.badcrc lea     m_loadcrc(pc),a0
        bsr     putstr
        move.w  d5,d0
        bsr     puthexw
        bsr     putnl
        bra     cmdloop
.bad    lea     m_loaderr(pc),a0
        bsr     putstr
        bra     cmdloop

;--------------------------------------------------------------- RUN addr
; Hand the machine to the probe. If it returns, re-quiesce, restore our serial
; rate (probes set SERPER themselves) and go back to the command loop.
do_run:
        bsr     args1
        bne     .bad
        lea     m_begin(pc),a0
        bsr     putstr

        move.l  V_ARG1,a0
        move.l  sp,V_SAVESP
        lea     PROBESTK,sp             ; a deep probe stack cannot reach the server
        lea     CUSTOM,a6
        bsr     syncbeam
        jsr     (a0)

        move.l  V_SAVESP,sp
        lea     CUSTOM,a6
        bsr     quiesce
        move.w  #SERPER_V,$032(a6)
        lea     m_ready(pc),a0
        bsr     putstr
        bra     cmdloop
.bad    lea     m_err(pc),a0
        bsr     putstr
        bra     cmdloop

;----------------------------------------------------------- identify the machine
; Everything here is reported raw and interpreted on the host: the mapping from
; ID fields to part numbers belongs in one place, and it is not here.
ident:
        move.w  $004(a6),d0             ; VPOSR
        and.w   #$7f00,d0
        lsr.w   #8,d0
        move.w  d0,V_AGNUS

        move.w  $07c(a6),d0             ; DENISEID: ECS/AGA only, floats on OCS
        move.w  d0,V_DENISE

        bsr     chipsize
        bsr     framelines
        bsr     cputype
        rts

;----------------------------------------------------------- chip RAM probe
; Two different facts, both worth recording, and easy to conflate:
;   reachkb -- where the chip window starts mirroring, i.e. how far Agnus
;              decodes: 512K on the OCS 8361/8367, 1M on the ECS 8372A, 2M on
;              the 8375 and Alice.
;   chipkb  -- how much RAM is actually fitted. NOT the same number: a 512K A500
;              fitted with an 8372A mirrors at 1M but only answers at 512K, so a
;              mirror test alone reports 1024 for a 512K machine.
; A high address that is neither mirrored nor backed by RAM floats. A floating
; bus cannot reproduce two different words, so each candidate is probed with a
; longword holding two distinct halves -- the technique test.asm uses to decide
; whether slow RAM is fitted.
LOWCELL  equ    $000100                 ; unused vector-table slot, restored after

chipsize:
        move.w  #512,V_CHIPKB
        move.w  #512,V_REACHKB
        lea     $080000+LOWCELL,a0      ; the mirror of this IS the low witness
        bsr     probecell
        tst.w   d0
        beq.s   .done                   ; mirrors at 512K: reach and size agree
        move.w  #1024,V_REACHKB
        cmp.w   #1,d0
        bne.s   .hi                     ; floating: reach is wider than the RAM
        move.w  #1024,V_CHIPKB
.hi     lea     $100000+LOWCELL,a0
        bsr     probecell
        tst.w   d0
        beq.s   .done                   ; mirrors at 1M
        move.w  #2048,V_REACHKB
        cmp.w   #1,d0
        bne.s   .done
        move.w  #2048,V_CHIPKB
.done   rts

; a0 = candidate address. Returns d0 = 0 mirrored / 1 real RAM / 2 floating.
; Both cells are restored, so this is safe to run over live memory.
probecell:
        movem.l d1-d2/a1,-(sp)
        lea     LOWCELL,a1
        move.l  (a1),d1                 ; save the low witness
        move.l  (a0),d2                 ; save the candidate (garbage if floating)
        move.l  #$a5a55a5a,(a1)
        move.l  #$5a5aa5a5,(a0)
        cmp.l   #$5a5aa5a5,(a1)
        beq.s   .mirror                 ; the high write landed on the low cell
        cmp.l   #$5a5aa5a5,(a0)
        beq.s   .real
        moveq   #2,d0                   ; held neither pattern: floating bus
        bra.s   .out
.mirror moveq   #0,d0
        bra.s   .out
.real   moveq   #1,d0
.out    move.l  d2,(a0)
        move.l  d1,(a1)
        movem.l (sp)+,d1-d2/a1
        rts

;----------------------------------------------------------- lines per frame
; Counts to the vpos wrap: PAL 313, NTSC 263 (long frames). Distinguishes PAL
; from NTSC without trusting any ROM or config.
; VPOSR and VHPOSR are read as one longword but still land in two bus cycles, so
; a sample taken across the vpos 255->256 carry can catch VPOSR before the carry
; and VHPOSR after it, reading a transient 0. Ending the scan on the first
; backwards step therefore reports exactly 256 lines on a PAL machine, depending
; only on what phase the preceding code left the beam in. Line 0 lasts a whole
; scanline and the poll loop is far shorter than that, so a real wrap reads 0
; repeatedly and the carry glitch reads it exactly once: confirm before
; believing it.
framelines:
.top    bsr     getvp                   ; wait for the top of a frame
        tst.w   d0
        bne.s   .top
.leave  bsr     getvp                   ; and then leave line 0, or the scan
        tst.w   d0                      ; below would take it for the wrap at once
        beq.s   .leave
        move.w  d0,d1                   ; running max
.tick   bsr     getvp
        tst.w   d0
        beq.s   .maybe
        cmp.w   d1,d0
        bls.s   .tick                   ; transient dip: ignore, keep the max
        move.w  d0,d1
        bra.s   .tick
.maybe  bsr     getvp                   ; confirm the zero on a second sample
        tst.w   d0
        bne.s   .tick
        addq.w  #1,d1
        move.w  d1,V_LINES
        rts

; Park the beam at the top of a frame before handing over to a probe. Without
; this the phase a probe starts at depends on host scheduling during the upload,
; and 8 of test.bin's 32 timing rows move by a tick or two between otherwise
; identical runs. The poll resolution is a few colour clocks, so this narrows
; the spread rather than removing it: wire-driven runs are never bit-identical
; to a native boot, and results are still meant to be repeated and pooled.
syncbeam:
        bsr     getvp
        tst.w   d0
        beq.s   syncbeam                ; already in line 0: let it pass first
.wait   bsr     getvp
        tst.w   d0
        bne.s   .wait
        bsr     getvp                   ; confirm past the 255->256 carry glitch
        tst.w   d0
        bne.s   .wait
        rts

; live beam vpos -> d0. Reads VPOSR:VHPOSR as one longword so the two halves
; cannot straddle a line boundary.
getvp:  move.l  $004(a6),d0
        lsr.l   #8,d0
        and.l   #$1ff,d0
        rts

;----------------------------------------------------------- CPU generation
; Identify by which exception MOVEC VBR raises, running unprivileged:
;   68000    -- the opcode does not exist at all      -> illegal instruction (vector 4)
;   68010+   -- it exists but is privileged           -> privilege violation (vector 8)
; Both handlers step the stacked PC past the 4-byte MOVEC and RTE, so neither
; needs to touch a stack pointer. Anything past the 68010 also answers "1"; the
; host config names the exact part.
cputype:
        move.l  $10.w,d2                ; save vector 4  (illegal instruction)
        move.l  $20.w,d3                ; save vector 8  (privilege violation)
        lea     .ill(pc),a0
        move.l  a0,$10.w
        lea     .priv(pc),a0
        move.l  a0,$20.w
        moveq   #1,d7
        dc.w    $4e7a,$0801             ; movec vbr,d0
.back   move.l  d2,$10.w
        move.l  d3,$20.w
        move.w  d7,V_CPU
        rts
.ill    moveq   #0,d7                   ; 68000
        addq.l  #4,2(sp)
        rte
.priv   moveq   #1,d7                   ; 68010 or later
        addq.l  #4,2(sp)
        rte

;----------------------------------------------------------------- banner
banner:
        lea     m_banner(pc),a0
        bsr     putstr
        lea     s_agnus(pc),a0
        bsr     putstr
        move.w  V_AGNUS,d0
        bsr     puthexw
        lea     s_denise(pc),a0
        bsr     putstr
        move.w  V_DENISE,d0
        bsr     puthexw
        lea     s_cpu(pc),a0
        bsr     putstr
        move.w  V_CPU,d0
        bsr     puthexw
        lea     s_chipkb(pc),a0
        bsr     putstr
        move.w  V_CHIPKB,d0
        bsr     puthexw
        lea     s_reachkb(pc),a0
        bsr     putstr
        move.w  V_REACHKB,d0
        bsr     puthexw
        lea     s_lines(pc),a0
        bsr     putstr
        move.w  V_LINES,d0
        bsr     puthexw
        lea     s_serper(pc),a0
        bsr     putstr
        move.w  #SERPER_V,d0
        bsr     puthexw
        bsr     putnl
        rts

;------------------------------------------------------------- command parsing
; match: a0 = line, a1 = keyword. Returns Z set on match with a0 advanced past
; the keyword and any following spaces.
match:
        move.l  a0,-(sp)
.cmp    move.b  (a1)+,d0
        beq.s   .end
        move.b  (a0)+,d1
        cmp.b   #'a',d1                 ; fold case so "run" works too
        bcs.s   .nof
        cmp.b   #'z',d1
        bhi.s   .nof
        sub.b   #32,d1
.nof    cmp.b   d0,d1
        bne.s   .no
        bra.s   .cmp
.end                                    ; keyword consumed; must end on space/NUL
        move.b  (a0),d1
        beq.s   .yes
        cmp.b   #' ',d1
        bne.s   .no
.yes    addq.l  #4,sp                   ; keep the advanced a0
.skip   cmp.b   #' ',(a0)
        bne.s   .yok
        addq.l  #1,a0
        bra.s   .skip
.yok    moveq   #0,d0                   ; Z set
        rts
.no     move.l  (sp)+,a0
        moveq   #1,d0                   ; Z clear
        rts

; args1/args3: parse 1 or 3 hex arguments from a0 into V_ARG1..3.
; Returns Z set on success (so "bne" after the call means "malformed").
args1:  bsr     hexarg
        bne.s   .bad
        move.l  d0,V_ARG1
        moveq   #0,d0
        rts
.bad    moveq   #1,d0
        rts

args3:  bsr     hexarg
        bne.s   .bad
        move.l  d0,V_ARG1
        bsr     hexarg
        bne.s   .bad
        move.l  d0,V_ARG2
        bsr     hexarg
        bne.s   .bad
        move.l  d0,V_ARG3
        moveq   #0,d0
        rts
.bad    moveq   #1,d0
        rts

; hexarg: parse one hex number at (a0) -> d0, advancing a0 past trailing spaces.
; Z set on success, clear if there was no digit at all.
hexarg:
        moveq   #0,d0
        moveq   #0,d2                   ; digit count
.dig    move.b  (a0),d1
        beq.s   .end
        cmp.b   #' ',d1
        beq.s   .end
        sub.b   #'0',d1
        bcs.s   .bad
        cmp.b   #9,d1
        bls.s   .ok
        and.b   #$df,d1                 ; fold 'a'-'f' to 'A'-'F'
        sub.b   #7,d1
        cmp.b   #10,d1
        bcs.s   .bad
        cmp.b   #15,d1
        bhi.s   .bad
.ok     lsl.l   #4,d0
        and.w   #$f,d1
        or.b    d1,d0
        addq.l  #1,a0
        addq.w  #1,d2
        bra.s   .dig
.end    tst.w   d2
        beq.s   .bad
.skip   cmp.b   #' ',(a0)
        bne.s   .good
        addq.l  #1,a0
        bra.s   .skip
.good   moveq   #0,d1                   ; Z set
        rts
.bad    moveq   #1,d1
        and.b   #$ff,d1                 ; Z clear
        rts

;------------------------------------------------------------- CRC-16/XMODEM
; d5 = running CRC (init 0), d0.b = byte. Poly $1021, no reflection, no final
; xor -- matches crc16() in tools/hwrig/hwrig.py. Check value for "123456789"
; is 0x31C3.
crcbyte:
        movem.l d0/d2/d3,-(sp)
        and.w   #$ff,d0
        lsl.w   #8,d0
        eor.w   d0,d5
        moveq   #8-1,d3
.bit    add.w   d5,d5
        bcc.s   .no
        eor.w   #$1021,d5
.no     dbra    d3,.bit
        movem.l (sp)+,d0/d2/d3
        rts

;----------------------------------------------------------------- serial I/O
; send one char (d0.b): wait for SERDATR TBE (bit 13), write SERDAT with the
; framing stop bit (bit 8). Same routine as test.asm; no DMA, no interrupts.
sendb:
.tbe    move.w  $018(a6),d1
        btst    #13,d1
        beq.s   .tbe
        and.w   #$ff,d0
        or.w    #$100,d0
        move.w  d0,$030(a6)             ; SERDAT
        rts

; blocking receive -> d0.b. RBF is SERDATR bit 14, mirroring INTREQR bit 11, and
; is acknowledged by clearing INTREQ bit 11. Waits forever: the command loop has
; nothing better to do, and the host owns the real timeout.
getb:
.w      move.w  $018(a6),d0
        btst    #14,d0
        beq.s   .w
        move.w  #$0800,$09c(a6)         ; clear INTREQ RBF (also clears OVRUN)
        and.w   #$ff,d0
        rts

; receive with timeout -> d0.b, d1 = 0 on success / 1 on timeout. Used only for
; upload payload bytes, so a truncated upload cannot hang the server. The count
; is a plain loop, so the wall-clock timeout is shorter on a faster CPU; it only
; has to be long enough that it never fires on a healthy link.
getbt:
        move.l  #4000000,d2
.w      move.w  $018(a6),d0
        btst    #14,d0
        bne.s   .got
        subq.l  #1,d2
        bne.s   .w
        moveq   #1,d1                   ; timed out
        rts
.got    move.w  #$0800,$09c(a6)
        and.w   #$ff,d0
        moveq   #0,d1
        rts

; read one CR/LF-terminated line into LINEBUF, NUL-terminated. Overlong lines
; are truncated, not allowed to run off the buffer.
getline:
        lea     LINEBUF,a2
        moveq   #0,d3
.ch     bsr     getb
        cmp.b   #13,d0
        beq.s   .end
        cmp.b   #10,d0
        beq.s   .end
        cmp.w   #LINEMAX,d3
        bhs.s   .ch                     ; at the limit: drop, keep draining
        move.b  d0,(a2)+
        addq.w  #1,d3
        bra.s   .ch
.end    tst.w   d3
        beq.s   getline                 ; ignore empty lines (bare LF after CR)
        clr.b   (a2)
        ; A CR-terminated line leaves the LF of a CRLF pair sitting in the
        ; receiver. LOAD then reads its payload as RAW bytes, so that stray LF
        ; becomes the first byte of the upload and shifts the whole blob by one
        ; -- which surfaces as a CRC failure that looks exactly like line noise.
        ; Peek at the next byte and consume it only if it is itself a line
        ; ending: SERDATR holds the byte until RBF is cleared, so anything else
        ; is left untouched for the next read. Bounded, so an LF-only host (the
        ; normal case) does not stall here.
        move.l  #5000,d2
.peek   move.w  $018(a6),d0
        btst    #14,d0
        beq.s   .none
        and.w   #$ff,d0
        cmp.b   #10,d0
        beq.s   .drop
        cmp.b   #13,d0
        bne.s   .out                    ; real data: leave it for getb
.drop   move.w  #$0800,$09c(a6)
        bra.s   .out
.none   subq.l  #1,d2
        bne.s   .peek
.out    rts

putstr:                                 ; a0 = NUL-terminated string
        move.l  a0,-(sp)
        move.l  a0,a1
.c      move.b  (a1)+,d0
        beq.s   .done
        bsr     sendb
        bra.s   .c
.done   move.l  (sp)+,a0
        rts

putnl:  moveq   #13,d0
        bsr     sendb
        moveq   #10,d0
        bsr     sendb
        rts

puthexw:                                ; d0.w as 4 hex digits
        move.l  d0,-(sp)
        move.l  d0,d3
        moveq   #4-1,d6
.h      rol.w   #4,d3
        move.w  d3,d0
        and.w   #$f,d0
        add.w   #'0',d0
        cmp.w   #'9',d0
        ble.s   .ok
        addq.w  #7,d0
.ok     bsr     sendb
        dbra    d6,.h
        move.l  (sp)+,d0
        rts

;------------------------------------------------------------------- strings
s_id     dc.b   "ID",0
s_ping   dc.b   "PING",0
s_load   dc.b   "LOAD",0
s_run    dc.b   "RUN",0

m_banner dc.b   "BANNER cl-probe 1",0
s_agnus  dc.b   " agnus=",0
s_denise dc.b   " denise=",0
s_cpu    dc.b   " cpu=",0
s_chipkb dc.b   " chipkb=",0
s_reachkb dc.b  " reachkb=",0
s_lines  dc.b   " lines=",0
s_serper dc.b   " serper=",0

m_ready   dc.b  "READY",13,10,0
m_err     dc.b  "ERR",13,10,0
m_loadrdy dc.b  "LOADRDY",13,10,0
m_loadok  dc.b  "LOADOK",13,10,0
m_loaderr dc.b  "LOADERR",13,10,0
m_loadcrc dc.b  "LOADERR crc=",0
m_begin   dc.b  "BEGIN",13,10,0

        even
endcode:
