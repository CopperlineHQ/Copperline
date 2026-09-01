#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Generates lsegfix, a hand-crafted m68k HUNK EXECUTABLE used as M6's
# LSEG-chain stress fixture (COPPERHF-DEVICE-PLAN.md M6, guest/copperhf/
# mounter.c's chf_load_lseg_chain). This is NOT built with the project's
# usual dockerized gcc: the point of this fixture is to pin every byte
# -- including a HUNK_HEADER memory size that legally exceeds its hunk's
# body length (the trailing-zero-truncation case) and a genuine
# HUNK_RELOC32SHORT record with odd word-count padding -- neither of
# which a normal compile reliably reproduces on demand. Run this script
# to regenerate `lsegfix`; the committed binary and this generator must
# always match (CI does not regenerate it).
#
# Layout (three hunks, matching an ordinary LoadSeg-able file -- exactly
# what an RDB FSHD's LSEG chain carries):
#
#   hunk 0, CODE (40 bytes, header size == body size, no truncation):
#     offset 0x00: entry point. Plain 68000 code, no OS calls, JSR-safe.
#       Follows a relocated pointer chain code -> data -> bss -> data ->
#       code and folds three values into D0:
#         D0  = *(data+8)                                   (plain const)
#         D0 ^= readback of a pattern this code just wrote  (data->bss
#                                                              pointer,
#                                                              round trip)
#         D0 ^= *(data->code pointer)                       (data->code
#                                                              pointer,
#                                                              reads this
#                                                              hunk's own
#                                                              marker back
#                                                              through a
#                                                              relocated
#                                                              pointer
#                                                              instead of
#                                                              PC-relative)
#       RTS with the folded value in D0.
#     offset 0x20: CODE_FIELD1 -- long, relocated to point at DATA+0x0
#                  (code->data pointer; patched by a HUNK_RELOC32 record
#                  in this hunk's trailer)
#     offset 0x24: CODE_FIELD_MARKER -- long, plain constant MARKER_VAL
#                  (the value a DATA->CODE pointer reads back)
#
#   hunk 1, DATA (12 bytes written; HUNK_HEADER declares 16 bytes, so the
#            loader must allocate at the header's memory size and
#            zero-fill the trailing 4 bytes -- the truncation case):
#     offset 0x0: long, relocated to point at BSS+0x0 (data->bss pointer)
#     offset 0x4: long, relocated to point at CODE+0x24 (data->code
#                 pointer, i.e. CODE_FIELD_MARKER above)
#     offset 0x8: long CONST_A, plain constant
#     [offset 0xC..0xF: not written -- must come back zero via
#      MEMF_CLEAR, never garbage]
#     Relocated with a single HUNK_RELOC32SHORT record carrying two
#     groups (one per target hunk) plus its zero terminator -- 7 words
#     total, so the record's own odd-word pad is exercised too.
#
#   hunk 2, BSS (4 bytes, zero-filled by the loader, never has its own
#           body or trailer): the code writes BSS_PATTERN through the
#           data->bss pointer and reads it back, proving that pointer
#           was relocated (not left as a raw hunk-local offset) and that
#           the allocation is actually writable.
#
# MAGIC (the value the fixture's entry point must return in D0) =
# CONST_A ^ BSS_PATTERN ^ MARKER_VAL. Any relocation bug -- wrong base,
# wrong target hunk, an unpatched (still hunk-relative) pointer, or the
# truncated DATA tail coming back as garbage instead of zero (which does
# not feed the magic directly here, but would show as a corrupted
# CONST_A/pointer read if the allocation size were wrong) -- changes at
# least one of the three XOR operands, so the probe can never mistake a
# broken loader for a working one by accident.

import struct
import sys

HUNK_CODE = 1001
HUNK_DATA = 1002
HUNK_BSS = 1003
HUNK_RELOC32 = 1004
HUNK_SYMBOL = 1008  # noqa: F841 (documented, unused by this fixture)
HUNK_DEBUG = 1009  # noqa: F841
HUNK_END = 1010
HUNK_HEADER = 1011
HUNK_RELOC32SHORT = 1020

CONST_A = 0x1234ABCD
BSS_PATTERN = 0x5A5A5A5A
MARKER_VAL = 0xC0FFEE42
MAGIC = CONST_A ^ BSS_PATTERN ^ MARKER_VAL


def u32(v):
    return struct.pack(">I", v & 0xFFFFFFFF)


def u16(v):
    return struct.pack(">H", v & 0xFFFF)


# ---------------------------------------------------------------------
# hunk 0: CODE -- hand-assembled 68000, PC-relative addressing only for
# this hunk's own fields (no reloc needed for those reads), indirection
# through AllocMem'd pointers for every cross-hunk step.
# ---------------------------------------------------------------------

code = b""
code += u16(0x41FA) + u16(0x0074)  # lea field1(pc),a0     (0x76 - 2 = 0x74)
code += u16(0x2250)  # movea.l (a0),a1        ; a1 = &DATA (code->data)
code += u16(0x2029) + u16(0x0008)  # move.l 8(a1),d0        ; d0 = CONST_A
code += u16(0x2451)  # movea.l (a1),a2        ; a2 = DATA->bss ptr
code += u16(0x24BC) + u32(BSS_PATTERN)  # move.l #BSS_PATTERN,(a2)
code += u16(0x2212)  # move.l (a2),d1         ; readback through same ptr
code += u16(0x2669) + u16(0x0004)  # movea.l 4(a1),a3       ; a3 = DATA->code ptr
code += u16(0x2413)  # move.l (a3),d2         ; d2 = MARKER_VAL via reloc
code += u16(0xB380)  # eor.l d1,d0
code += u16(0xB580)  # eor.l d2,d0
# MAGIC is now in D0. This entry point is loaded as a DeviceNode's
# dn_SegList and started by AmigaDOS as an ordinary AmigaDOS process
# (dol_GlobVec == -1, "C or assembler handler": AmigaDOS Manual/rkrm-dos,
# "Starting a Handler" -- the process begins at the first byte of
# dol_SegList with no packet in any register; it must WaitPort/GetMsg its
# own pr_MsgPort for the startup packet). Every register/struct offset
# below (pr_MsgPort=92, Message.mn_Node.ln_Name=10, DosPacket
# dp_Link/dp_Port/dp_Res1/dp_Res2=0/4/12/16, exec.library LVOs
# FindTask=-294/WaitPort=-384/GetMsg=-372/PutMsg=-366) was verified by
# compiling the equivalent C against this project's own cross-toolchain
# and reading the emitted offsets/opcodes back out of objdump, not typed
# from memory.
#
# HARD-WON (M6), three iterations to land on this behaviour:
#   1. Just RTS immediately: leaves the startup packet unanswered.
#      AmigaDOS's own process-exit path reports that as a "Recoverable
#      Alert: unexpected DOS packet received" -- fatal headless, nothing
#      can click Continue.
#   2. Reply the startup packet with dp_Res1=DOSFALSE/dp_Res2=MAGIC, then
#      RTS (terminate) -- matches the AmigaDOS Manual's own handler
#      sketch ("if startup failed... release resources and terminate"),
#      but chftest_m6.c's Lock("TST0:", ...) sends a *second*, separate
#      ACTION_LOCATE_OBJECT packet after the (now-dead) startup exchange,
#      which never gets a reply -- Lock() sees the port vanish and fails
#      with a generic error, never MAGIC.
#   3. Reply the startup packet with dp_Res1=DOSFALSE and loop forever
#      replying every later packet the same way: now the *second* packet
#      does get MAGIC back, but a *failed* startup reply makes
#      GetDeviceProc() itself give up with ERROR_DEVICE_NOT_MOUNTED
#      before ever sending that second packet at all -- dos.library
#      never exposes a failed startup's dp_Res2 to the caller.
# The fix: reply the startup packet (the first one received) with
# dp_Res1=DOSTRUE/dp_Res2=0 (this "filesystem" claims to start up fine),
# then reply every *subsequent* packet -- including the
# ACTION_LOCATE_OBJECT that chftest_m6.c's Lock() call actually
# triggers -- with dp_Res1=DOSFALSE/dp_Res2=MAGIC, forever. D6 tracks
# "have I already replied the startup packet" (0 = not yet).
code += u16(0x2E00)  # move.l d0,d7           ; stash MAGIC (D2-D7/A2-A6 are callee-saved)
code += u16(0x2C78) + u16(0x0004)  # movea.l 4.w,a6         ; a6 = SysBase
code += u16(0x93C9)  # suba.l a1,a1           ; a1 = NULL (FindTask(NULL))
code += u16(0x4EAE) + u16(0xFEDA)  # jsr -294(a6)           ; FindTask
code += u16(0x2440)  # movea.l d0,a2          ; a2 = proc
code += u16(0x45EA) + u16(0x005C)  # lea 92(a2),a2          ; a2 = &proc->pr_MsgPort
code += u16(0x7C00)  # moveq #0,d6            ; d6 = "startup packet already answered?"
LOOP_TOP_OFF = len(code)
code += u16(0x2C78) + u16(0x0004)  # movea.l 4.w,a6
code += u16(0x204A)  # movea.l a2,a0          ; a0 = &pr_MsgPort
code += u16(0x4EAE) + u16(0xFE80)  # jsr -384(a6)           ; WaitPort
code += u16(0x2C78) + u16(0x0004)  # movea.l 4.w,a6
code += u16(0x204A)  # movea.l a2,a0
code += u16(0x4EAE) + u16(0xFE8C)  # jsr -372(a6)           ; GetMsg
code += u16(0x2040)  # movea.l d0,a0          ; a0 = msg
code += u16(0x2268) + u16(0x000A)  # movea.l 10(a0),a1      ; a1 = pkt (Message.mn_Node.ln_Name)
code += u16(0x4A86)  # tst.l d6
FAIL_REPLY_BRANCH_AT = len(code)
code += u16(0x6600)  # bne.s fail_reply       ; (disp patched below)
code += u16(0x70FF)  # moveq #-1,d0           ; DOSTRUE
code += u16(0x2340) + u16(0x000C)  # move.l d0,12(a1)       ; pkt->dp_Res1 = DOSTRUE
code += u16(0x42A9) + u16(0x0010)  # clr.l 16(a1)           ; pkt->dp_Res2 = 0
code += u16(0x7C01)  # moveq #1,d6            ; startup packet answered
DO_REPLY_BRANCH_AT = len(code)
code += u16(0x6000)  # bra.s do_reply         ; (disp patched below)
FAIL_REPLY_OFF = len(code)
code += u16(0x42A9) + u16(0x000C)  # clr.l 12(a1)           ; pkt->dp_Res1 = DOSFALSE
code += u16(0x2347) + u16(0x0010)  # move.l d7,16(a1)       ; pkt->dp_Res2 = MAGIC
DO_REPLY_OFF = len(code)
code += u16(0x2C78) + u16(0x0004)  # movea.l 4.w,a6
code += u16(0x2069) + u16(0x0004)  # movea.l 4(a1),a0       ; a0 = pkt->dp_Port
code += u16(0x2251)  # movea.l (a1),a1        ; a1 = pkt->dp_Link
code += u16(0x4EAE) + u16(0xFE92)  # jsr -366(a6)           ; PutMsg (reply)
# bra.s loop_top: 8-bit signed displacement, relative to the address right
# after this 2-byte instruction (opcode byte 0x60, then the displacement
# byte itself). Same for the two forward .s branches above, patched here
# now that every offset is known.
bra_disp = LOOP_TOP_OFF - (len(code) + 2)
code += b"\x60" + struct.pack(">b", bra_disp)


def patch_s_branch(buf, at, opcode_hi, target):
    disp = target - (at + 2)
    buf[at] = opcode_hi
    buf[at + 1] = disp & 0xFF
    return buf


code = bytearray(code)
patch_s_branch(code, FAIL_REPLY_BRANCH_AT, 0x66, FAIL_REPLY_OFF)  # bne.s fail_reply
patch_s_branch(code, DO_REPLY_BRANCH_AT, 0x60, DO_REPLY_OFF)  # bra.s do_reply
code = bytes(code)

assert len(code) == 0x76, f"code length drifted: 0x{len(code):x}"

CODE_FIELD1_OFF = 0x76
CODE_MARKER_OFF = 0x7A
code += u32(0)  # CODE_FIELD1: patched by HUNK_RELOC32 below (-> DATA+0)
code += u32(MARKER_VAL)  # CODE_FIELD_MARKER: plain constant

assert len(code) == 0x7E
code += b"\x00\x00"  # pad to a longword boundary (HUNK_CODE sizes are in longs)
assert len(code) == 0x80
CODE_SIZE_LONGS = len(code) // 4  # 32, header == body: no truncation here

code_trailer = u32(HUNK_RELOC32)
code_trailer += u32(1) + u32(1) + u32(CODE_FIELD1_OFF)  # count=1, hunk#1 (DATA)
code_trailer += u32(0)  # terminator
code_trailer += u32(HUNK_END)

# ---------------------------------------------------------------------
# hunk 1: DATA -- 12 bytes written, declared 16 in the header (truncation
# stress: the loader must allocate 16 and zero the last 4).
# ---------------------------------------------------------------------

data = u32(0)  # +0x0: patched -> BSS+0
data += u32(CODE_MARKER_OFF)  # +0x4: patched -> CODE+0x24
data += u32(CONST_A)  # +0x8: plain constant
assert len(data) == 0xC
DATA_BODY_LONGS = len(data) // 4  # 3
DATA_HEADER_LONGS = DATA_BODY_LONGS + 1  # 4 (16 bytes) -- the truncation gap

BSS_HUNK_IDX = 2
CODE_HUNK_IDX = 0

# HUNK_RELOC32SHORT: two groups (one per target hunk) + zero terminator,
# word-counted (7 words -> odd -> one pad word required).
data_reloc_words = []
data_reloc_words += [1, BSS_HUNK_IDX, 0x0]  # DATA+0x0 -> BSS+0x0
data_reloc_words += [1, CODE_HUNK_IDX, 0x4]  # DATA+0x4 -> CODE+0x24
data_reloc_words += [0]  # terminator
assert len(data_reloc_words) == 7  # odd: exercises the pad path
data_trailer = u32(HUNK_RELOC32SHORT)
for w in data_reloc_words:
    data_trailer += u16(w)
data_trailer += u16(0)  # pad to a longword boundary
data_trailer += u32(HUNK_END)

# ---------------------------------------------------------------------
# hunk 2: BSS -- 4 bytes, no body, no trailer relocs (nothing to relocate
# out of a hunk with no bytes).
# ---------------------------------------------------------------------

BSS_HEADER_LONGS = 1  # 4 bytes


def build():
    out = b""
    out += u32(HUNK_HEADER)
    out += u32(0)  # resident-library name list terminator (none)
    out += u32(3)  # table_size: 3 hunks
    out += u32(0)  # first_hunk
    out += u32(2)  # last_hunk
    out += u32(CODE_SIZE_LONGS)  # hunk 0 memory size (longs)
    out += u32(DATA_HEADER_LONGS)  # hunk 1 memory size (longs) -- truncated
    out += u32(BSS_HEADER_LONGS)  # hunk 2 memory size (longs)

    out += u32(HUNK_CODE)
    out += u32(CODE_SIZE_LONGS)
    out += code
    out += code_trailer

    out += u32(HUNK_DATA)
    out += u32(DATA_BODY_LONGS)  # body shorter than the header's memsize
    out += data
    out += data_trailer

    out += u32(HUNK_BSS)
    out += u32(BSS_HEADER_LONGS)
    out += u32(HUNK_END)

    return out


if __name__ == "__main__":
    blob = build()
    out_path = sys.argv[1] if len(sys.argv) > 1 else "lsegfix"
    with open(out_path, "wb") as f:
        f.write(blob)
    sys.stderr.write(
        f"lsegfix: {len(blob)} bytes, MAGIC=0x{MAGIC:08X} "
        f"(CONST_A=0x{CONST_A:08X} ^ BSS_PATTERN=0x{BSS_PATTERN:08X} "
        f"^ MARKER_VAL=0x{MARKER_VAL:08X})\n"
    )
