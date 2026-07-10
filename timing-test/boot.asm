; Boot block: a minimal loader. The system ROM reads this block, then calls it with
; A1 = the boot device's I/O request (trackdisk, already open) and A6 = SysBase.
; We reuse that request to read the main program (test.asm, assembled separately
; and placed from sector 2 onward) into chip RAM at $30000, then jump to it.
;
; This keeps the main program off the 1024-byte boot-block size limit.

MAIN    equ     $30000          ; load address for the main program (chip RAM)
SECTORS equ     96              ; sectors to load (48 KB; cc7 embeds a 45 KB blob)

CMD_READ  equ   2
IO_COMMAND equ  $1c             ; IOStdReq.io_Command  (word)
IO_LENGTH  equ  $24             ; IOStdReq.io_Length   (long)
IO_DATA    equ  $28             ; IOStdReq.io_Data     (long)
IO_OFFSET  equ  $2c             ; IOStdReq.io_Offset   (long)
LVODoIO    equ  -456

;-------------------------------------------------------------- boot block header
        dc.b    "DOS",0
        dc.l    0               ; checksum (patched by build script)
        dc.l    880             ; rootblock

;----------------------------------------------------- entry (a1=bootio a6=sys)
        move.w  #CMD_READ,IO_COMMAND(a1)
        move.l  #SECTORS*512,IO_LENGTH(a1)
        move.l  #MAIN,IO_DATA(a1)
        move.l  #512*2,IO_OFFSET(a1)    ; byte offset of sector 2
        jsr     LVODoIO(a6)             ; DoIO -- synchronous read
        jmp     MAIN
