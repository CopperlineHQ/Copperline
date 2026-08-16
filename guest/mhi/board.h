/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Board-register access layer for Copperline's virtual MHI decoder board.
 * This header is the single place that knows the wire protocol from
 * docs/internals/mhi.md (THE CONTRACT) -- every offset, bit, and command
 * value below is a direct transcription of that document's register map,
 * kept in one small file per MHI-PLAN.md WP4's "published-spec portability
 * story": if a future emulator ports mhi_copperline.library against its own
 * board, board.h/board.c are the only files it needs to replace.
 *
 * Deliberately free of any MHI-API vocabulary (MHIF_*, MHIP_*, MHIQ_*, a
 * decoder handle, a signal mask) -- see mhi.md's "The MHI-API/board split".
 * All of that lives in mhi_copperline.c/.h instead.
 *
 * #include-able from both C (board.c, mhi_copperline.c) and assembly
 * (int_handler.s, via -x assembler-with-cpp) -- see int_handler.s's own
 * header comment for why the INT2 handler itself is hand-written asm.
 */

#ifndef MHI_BOARD_H
#define MHI_BOARD_H

/* Zorro identity (docs/internals/mhi.md "Zorro identity"). */
#define MHI_BOARD_MANUFACTURER 5192   /* 0x1448 -- Copperline/dec0de Consulting */
#define MHI_BOARD_PRODUCT      7

/* The register-protocol version this driver was written against. A board
 * reporting a lower VERSION is refused (mhi.md "Versioning": we don't
 * understand it); a higher VERSION is accepted (new fields land at
 * offsets this driver already treats as reserved-inert). */
#define MHI_PROTOCOL_VERSION 1

/* Register map (docs/internals/mhi.md "Register map") -- word offsets
 * within the board's autoconfigured window. */
#define MHI_REG_VERSION         0x00 /* RO */
#define MHI_REG_CAPS            0x02 /* RO */
#define MHI_REG_STATUS          0x04 /* RO */
#define MHI_REG_CONTROL         0x06 /* WO */
#define MHI_REG_INTREQ          0x08 /* RW, write-1-to-clear */
#define MHI_REG_INTENA          0x0A /* RW */
#define MHI_REG_QUEUE_DEPTH     0x0C /* RO */
#define MHI_REG_QUEUE_COUNT     0x0E /* RO */
#define MHI_REG_DESC_ADDR_HI    0x10 /* WO */
#define MHI_REG_DESC_ADDR_LO    0x12 /* WO */
#define MHI_REG_DESC_LEN_HI     0x14 /* WO */
#define MHI_REG_DESC_LEN_LO     0x16 /* WO */
#define MHI_REG_DOORBELL        0x18 /* WO */
#define MHI_REG_COMPLETED_COUNT 0x1A /* RO */
#define MHI_REG_PARAM_SELECT    0x1C /* RW */
#define MHI_REG_PARAM_VALUE     0x1E /* RW */

/* CAPS bits (mhi.md "Capability/version registers"). */
#define MHI_CAPS_MPEG1  (1U << 0)
#define MHI_CAPS_MPEG2  (1U << 1)
#define MHI_CAPS_MPEG25 (1U << 2)
#define MHI_CAPS_LAYER3 (1U << 3)
#define MHI_CAPS_CBR    (1U << 4)
#define MHI_CAPS_VBR    (1U << 5)

/* STATUS values (mhi.md "Status and control") -- the board's OWN state
 * codes, deliberately numbered differently from MHIF_* (see mhi.md's own
 * note on why: it makes a guest driver that forgets to translate fail
 * loudly). mhi_copperline.c translates these, board.c never does. */
#define MHI_STATUS_STOPPED     0
#define MHI_STATUS_PLAYING     1
#define MHI_STATUS_PAUSED      2
#define MHI_STATUS_OUT_OF_DATA 3

/* CONTROL commands (mhi.md "Status and control"). */
#define MHI_CONTROL_NOP   0
#define MHI_CONTROL_PLAY  1
#define MHI_CONTROL_PAUSE 2
#define MHI_CONTROL_STOP  3

/* INTREQ/INTENA bit layout (mhi.md "Interrupts"). */
#define MHI_INT_BUFFER_DONE    (1U << 0)
#define MHI_INT_OUT_OF_DATA    (1U << 1)
#define MHI_INT_QUEUE_OVERFLOW (1U << 2)

/* PARAM_SELECT indices (mhi.md "Param latches"). Indices 7-65535 are
 * reserved by the protocol; mhi_copperline.c never writes them. */
#define MHI_PARAM_VOLUME      0
#define MHI_PARAM_PANNING     1
#define MHI_PARAM_BASS        2
#define MHI_PARAM_MID         3
#define MHI_PARAM_TREBLE      4
#define MHI_PARAM_CROSSMIXING 5
#define MHI_PARAM_PREFACTOR   6

/* struct MHIPlayer field offsets (mhi_copperline.h) that int_handler.s
 * reads directly out of its is_Data pointer -- kept in sync by hand with
 * that struct's field order (its first three fields, in this order,
 * exactly like guest/hostsocket/entry.s's LIB_* offsets are kept in sync
 * with its own C-side layout). All three fields are APTR/pointer/ULONG,
 * 4 bytes each on m68k, so there is no padding to account for.
 *   OFF_BOARD    -- volatile UWORD *board        (register window base)
 *   OFF_TASK     -- struct Task    *client_task
 *   OFF_SIGMASK  -- ULONG           client_sigmask
 */
#define MHI_OFF_BOARD   0
#define MHI_OFF_TASK    4
#define MHI_OFF_SIGMASK 8

/* Exec LVOs the assembly INT2 handler needs (int_handler.s only --
 * everything else in this library reaches Exec through the normal
 * <inline/exec.h> stubs). Value from hardware/intbits.h /
 * clib/exec_protos.h, verified against the NDK autodocs, matching the
 * convention already established by guest/hostsocket/entry.s. */
#define MHI_LVO_SIGNAL -324

#ifndef MHI_ASSEMBLY

#include <exec/types.h>

/* Opaque board handle: the register window's base address, exactly what
 * ConfigDev.cd_BoardAddr gives us. A plain APTR, not a struct -- board.c's
 * whole job is turning that address plus the offsets above into the
 * operations below. */

/* Locate the board via FindConfigDev(NULL, MHI_BOARD_MANUFACTURER,
 * MHI_BOARD_PRODUCT) and validate VERSION. Returns the board's register
 * window base on success, or NULL if no matching board is present or its
 * VERSION is older than MHI_PROTOCOL_VERSION (mhi.md "Versioning": an
 * unrecognized *higher* VERSION is fine -- only "older than what we were
 * written against" is refused, since new versions only add inert reserved
 * offsets we already ignore, never repurpose an existing one). */
APTR mhi_board_find(void);

/* Raw register access -- word (UWORD) accesses only, per mhi.md "Access
 * size and alignment": this protocol has no move.l-sized registers. */
UWORD mhi_reg_read(APTR board, UWORD offset);
void mhi_reg_write(APTR board, UWORD offset, UWORD value);

/* CAPS/STATUS/QUEUE_DEPTH/QUEUE_COUNT/COMPLETED_COUNT are read-only and
 * side-effect-free at any time (mhi.md), so these are thin, freely
 * pollable wrappers over mhi_reg_read. */
UWORD mhi_caps(APTR board);
UBYTE mhi_status(APTR board);
UWORD mhi_queue_depth(APTR board);
UWORD mhi_queue_count(APTR board);
UWORD mhi_completed_count(APTR board);

/* CONTROL is one-shot and WO -- see mhi.md's own note that STATUS already
 * reflects the new state by the time the write retires. */
void mhi_control(APTR board, UWORD command);

/* Enqueue one descriptor: stages DESC_ADDR_HI/LO and DESC_LEN_HI/LO, then
 * rings DOORBELL. Callers must have already checked room via
 * mhi_queue_count(board) < mhi_queue_depth(board) -- this function does
 * not itself poll or retry (MHIQueueBuffer's contract is "return FALSE
 * once, let the caller retry later", not "block here"). */
void mhi_enqueue(APTR board, ULONG addr, ULONG len);

/* INTREQ/INTENA. mhi_intreq_ack clears exactly the bits set in `bits`
 * (write-1-to-clear, mhi.md "Interrupts"); mhi_intena_set/clear sets or
 * clears bits in the enable mask via read-modify-write (INTENA is RW, so
 * this is a genuine read before write, not a blind overwrite -- safe here
 * because only one client (one struct MHIPlayer) ever touches INTENA at a
 * time under this library's single-decoder-at-a-time model). */
UWORD mhi_intreq_read(APTR board);
void mhi_intreq_ack(APTR board, UWORD bits);
void mhi_intena_set(APTR board, UWORD bits);
void mhi_intena_clear(APTR board, UWORD bits);

/* PARAM_SELECT/PARAM_VALUE mailbox. `index` is a board index (see
 * MHI_PARAM_* above), never an MHIP_* constant -- mhi_copperline.c
 * translates before calling these. */
void mhi_param_set(APTR board, UWORD index, UWORD value);
UWORD mhi_param_get(APTR board, UWORD index);

#endif /* !MHI_ASSEMBLY */

#endif /* MHI_BOARD_H */
