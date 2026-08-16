/*
 * SPDX-FileCopyrightText: 2020-2026 Dimitris Panokostas
 * SPDX-FileCopyrightText: 2026 The Copperline project
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Ported from BlitterStudio/host-tools (github.com/BlitterStudio/host-tools),
 * commit c14cf8c1be881d7157a0a051e3f6f4ed695c57d3, drivers/mhi/src/mhiuae.h.
 * Structural shape (library base + semaphore-guarded single decoder, the 10
 * i_MHI* entry points with the same __asm register bindings) kept
 * unchanged; `struct MHIUAEPlayer`'s single `host_handle` (an opaque
 * Amiberry uae.resource handle) is replaced by `struct MHIPlayer`'s real
 * board-access state (register window pointer, client-side completion
 * queue, installed interrupt server) -- see board.h/board.c and
 * docs/internals/mhi.md for the register protocol this now drives instead
 * of a `calltrap()` into the host.
 */

#ifndef MHI_COPPERLINE_H
#define MHI_COPPERLINE_H

#include <exec/interrupts.h>
#include <exec/libraries.h>
#include <exec/semaphores.h>
#include <exec/tasks.h>
#include <exec/types.h>

#include "mhi_abi.h"

#define MHI_LIBRARY_NAME "mhi_copperline.library"
#define MHI_DECODER_NAME "Copperline MHI"
#define MHI_AUTHOR       "Copperline"
#define MHI_CAPABILITIES "audio/mpeg{audio/mp3}"

/* How many descriptors this driver tracks client-side, for MHIGetEmpty's
 * FIFO-order completion accounting (docs/internals/mhi.md "Completion and
 * reclaim"). Sized well past the board's own QUEUE_DEPTH (16, mhi.md
 * "Descriptor queue and doorbell") rather than hardcoding that constant --
 * MHIQueueBuffer always checks the board's live QUEUE_DEPTH/QUEUE_COUNT
 * registers before enqueuing (never this array's size directly), so a
 * future board revision with a deeper queue works without a rebuild, per
 * mhi.md's own "a register, not a bare number" reasoning for QUEUE_DEPTH.
 */
#define MHI_QUEUE_MAX 32

struct MHICopperlineBase {
    struct Library lib;
    BPTR seg_list;
    struct ExecBase *sys_base;
    struct SignalSemaphore allocation_lock;
    ULONG allocated_decoders;

    /* The board's register window, found once at library-open time (see
     * startup.c's InitLib) via mhi_board_find(); NULL if no MHI board is
     * present, in which case InitLib fails the whole library open, mirroring
     * mhiuae's own "refuse to open without the host resource" behaviour. */
    APTR board;
};

struct MHIPlayer {
    /* Read directly by int_handler.s out of its is_Data pointer -- keep
     * these three fields first, in this order, in sync with board.h's
     * MHI_OFF_BOARD/MHI_OFF_TASK/MHI_OFF_SIGMASK. */
    APTR board;
    struct Task *client_task;
    ULONG client_sigmask;

    UBYTE status; /* cached MHIF_* value, see mhi_copperline.c's
                   * translate_status() -- MHIGetStatus always re-reads the
                   * board and refreshes this rather than trusting a stale
                   * cache, since mhi.md says STATUS "may be polled freely
                   * at any time, from any context". */

    /* Client-side FIFO mirroring MHIQueueBuffer's own call order, used to
     * turn the board's single free-running COMPLETED_COUNT counter (mhi.md
     * "Completion and reclaim") back into per-buffer MHIGetEmpty results.
     */
    APTR queue[MHI_QUEUE_MAX];
    UWORD queue_head;
    UWORD queue_tail;
    UWORD queue_count;         /* descriptors enqueued, not yet popped */
    UWORD pending_completions; /* popped from the wraparound-safe
                                 * COMPLETED_COUNT delta, not yet returned
                                 * via MHIGetEmpty */
    UWORD completed_seen;      /* last COMPLETED_COUNT value observed */

    struct Interrupt intserver;
};

BOOL mhi_copperline_open_board(struct MHICopperlineBase *base);

/* board.c: builds/installs (resp. disables/removes) the INTB_PORTS
 * interrupt server described in board.h's own comment on MHI_OFF_*; take
 * struct MHIPlayer* rather than living in board.h itself only because that
 * struct is defined here, not there -- board.h/board.c otherwise never
 * mention it (see board.h's header comment on staying MHI-vocabulary-free).
 */
BOOL mhi_board_int_install(struct MHIPlayer *player);
void mhi_board_int_remove(struct MHIPlayer *player);

APTR i_MHIAllocDecoder(struct Task *task __asm("a0"), ULONG sigmask __asm("d0"), struct MHICopperlineBase *base __asm("a6"));
void i_MHIFreeDecoder(APTR handle __asm("a3"), struct MHICopperlineBase *base __asm("a6"));
BOOL i_MHIQueueBuffer(APTR handle __asm("a3"), APTR buffer __asm("a0"), ULONG size __asm("d0"), struct MHICopperlineBase *base __asm("a6"));
APTR i_MHIGetEmpty(APTR handle __asm("a3"), struct MHICopperlineBase *base __asm("a6"));
UBYTE i_MHIGetStatus(APTR handle __asm("a3"), struct MHICopperlineBase *base __asm("a6"));
void i_MHIPlay(APTR handle __asm("a3"), struct MHICopperlineBase *base __asm("a6"));
void i_MHIStop(APTR handle __asm("a3"), struct MHICopperlineBase *base __asm("a6"));
void i_MHIPause(APTR handle __asm("a3"), struct MHICopperlineBase *base __asm("a6"));
ULONG i_MHIQuery(ULONG query __asm("d1"), struct MHICopperlineBase *base __asm("a6"));
void i_MHISetParam(APTR handle __asm("a3"), UWORD param __asm("d0"), ULONG value __asm("d1"), struct MHICopperlineBase *base __asm("a6"));

#endif
