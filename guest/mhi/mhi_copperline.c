/*
 * SPDX-FileCopyrightText: 2020-2026 Dimitris Panokostas
 * SPDX-FileCopyrightText: 2026 The Copperline project
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Ported from BlitterStudio/host-tools (github.com/BlitterStudio/host-tools),
 * commit c14cf8c1be881d7157a0a051e3f6f4ed695c57d3, drivers/mhi/src/mhiuae.c.
 * Every `UaeMHI*` trap call (uae.resource, Amiberry-side) is replaced by a
 * board.c call driving Copperline's own register protocol
 * (docs/internals/mhi.md); the single-decoder-at-a-time semaphore
 * discipline, the AllocVec'd per-decoder handle, and MHIQuery's
 * locally-answered static queries are otherwise unchanged from mhiuae.c's
 * own shape.
 */

#define __USE_SYSBASE

#include <exec/memory.h>
#include <proto/exec.h>

#include "board.h"
#include "mhi_copperline.h"

static const char decoder_version[] __attribute__((used)) = MHI_LIBRARY_NAME " " VERSION_STR " (" DATE_STR ")";

BOOL mhi_copperline_open_board(struct MHICopperlineBase *base)
{
    base->board = mhi_board_find();
    return base->board != NULL;
}

static struct MHIPlayer *valid_player(APTR handle)
{
    return (struct MHIPlayer *)handle;
}

/* Board STATUS (mhi.md's own STOPPED=0/PLAYING=1/PAUSED=2/OUT_OF_DATA=3) to
 * MHIF_* (mhi.h's PLAYING=0/STOPPED=1/OUT_OF_DATA=2/PAUSED=3) -- see
 * mhi.md "Status and control" for why these are deliberately not the same
 * numbering, and board.h's own note that board.c never performs this
 * translation itself. */
static UBYTE translate_status(UBYTE board_status)
{
    switch (board_status) {
        case MHI_STATUS_PLAYING:
            return MHIF_PLAYING;
        case MHI_STATUS_PAUSED:
            return MHIF_PAUSED;
        case MHI_STATUS_OUT_OF_DATA:
            return MHIF_OUT_OF_DATA;
        case MHI_STATUS_STOPPED:
        default:
            return MHIF_STOPPED;
    }
}

/* MHIP_* (mhi_abi.h) to this board's PARAM_SELECT index (mhi.md "Param
 * latches"). Returns TRUE and fills *index_out for the seven params this
 * board's table defines; FALSE for anything else (the 5/10-band EQ and its
 * aliases, MHIP_MIDBASS/MHIP_MIDHIGH) -- MHIQuery already answers
 * MHIF_UNSUPPORTED for all of those (see i_MHIQuery below), so
 * i_MHISetParam silently drops them rather than writing a board index the
 * protocol reserves as inert. */
static BOOL translate_param(UWORD param, UWORD *index_out)
{
    switch (param) {
        case MHIP_VOLUME:
            *index_out = MHI_PARAM_VOLUME;
            return TRUE;
        case MHIP_PANNING:
            *index_out = MHI_PARAM_PANNING;
            return TRUE;
        case MHIP_BASS:
            *index_out = MHI_PARAM_BASS;
            return TRUE;
        case MHIP_MID:
            *index_out = MHI_PARAM_MID;
            return TRUE;
        case MHIP_TREBLE:
            *index_out = MHI_PARAM_TREBLE;
            return TRUE;
        case MHIP_CROSSMIXING:
            *index_out = MHI_PARAM_CROSSMIXING;
            return TRUE;
        case MHIP_PREFACTOR:
            *index_out = MHI_PARAM_PREFACTOR;
            return TRUE;
        default:
            return FALSE;
    }
}

APTR i_MHIAllocDecoder(struct Task *task __asm("a0"), ULONG sigmask __asm("d0"), struct MHICopperlineBase *base __asm("a6"))
{
    struct MHIPlayer *player;

    ObtainSemaphore(&base->allocation_lock);
    if (base->allocated_decoders != 0) {
        ReleaseSemaphore(&base->allocation_lock);
        return NULL;
    }

    if (task == NULL) {
        task = FindTask(NULL);
    }

    player = AllocVec(sizeof(*player), MEMF_PUBLIC | MEMF_CLEAR);
    if (player == NULL) {
        ReleaseSemaphore(&base->allocation_lock);
        return NULL;
    }

    player->board = base->board;
    player->client_task = task;
    player->client_sigmask = sigmask;
    player->status = MHIF_STOPPED;

    if (!mhi_board_int_install(player)) {
        FreeVec(player);
        ReleaseSemaphore(&base->allocation_lock);
        return NULL;
    }

    base->allocated_decoders++;
    ReleaseSemaphore(&base->allocation_lock);
    return player;
}

void i_MHIFreeDecoder(APTR handle __asm("a3"), struct MHICopperlineBase *base __asm("a6"))
{
    struct MHIPlayer *player = valid_player(handle);

    if (player == NULL) {
        return;
    }

    ObtainSemaphore(&base->allocation_lock);
    /* Not documented as required by MHIFreeDecoder's own Autodoc, but
     * stopping first leaves the board in a clean, deterministic STOPPED
     * state (queue flushed, counters zeroed, mhi.md "Status and control")
     * rather than a freed handle that quietly leaves the board mid-play
     * with no client left to service its interrupts. Deliberate deviation
     * from mhiuae.c, which has no equivalent step (its host-side
     * UaeMHIFree presumably does the same on the Amiberry side). */
    mhi_control(player->board, MHI_CONTROL_STOP);
    mhi_board_int_remove(player);
    if (base->allocated_decoders > 0) {
        base->allocated_decoders--;
    }
    ReleaseSemaphore(&base->allocation_lock);
    FreeVec(player);
}

BOOL i_MHIQueueBuffer(APTR handle __asm("a3"), APTR buffer __asm("a0"), ULONG size __asm("d0"), struct MHICopperlineBase *base __asm("a6"))
{
    struct MHIPlayer *player = valid_player(handle);
    (void)base;
    if (player == NULL || buffer == NULL || size == 0) {
        return FALSE;
    }

    /* mhi.md "Descriptor queue and doorbell": poll room before ringing the
     * doorbell (a full queue silently drops the descriptor and only raises
     * a diagnostic INTREQ bit, which is not what MHIQueueBuffer's own
     * FALSE-return contract wants). Local FIFO room is checked too, purely
     * defensive -- MHI_QUEUE_MAX is sized well past the board's own
     * QUEUE_DEPTH so this should never be the limiting check in practice.
     */
    if (player->queue_count >= MHI_QUEUE_MAX) {
        return FALSE;
    }
    if (mhi_queue_count(player->board) >= mhi_queue_depth(player->board)) {
        return FALSE;
    }

    mhi_enqueue(player->board, (ULONG)buffer, size);

    player->queue[player->queue_tail] = buffer;
    player->queue_tail = (UWORD)((player->queue_tail + 1) % MHI_QUEUE_MAX);
    player->queue_count++;
    return TRUE;
}

APTR i_MHIGetEmpty(APTR handle __asm("a3"), struct MHICopperlineBase *base __asm("a6"))
{
    struct MHIPlayer *player = valid_player(handle);
    APTR buffer;
    UWORD now;
    UWORD delta;

    (void)base;
    if (player == NULL) {
        return NULL;
    }

    /* mhi.md "Completion and reclaim": wraparound-safe 16-bit delta against
     * the board's free-running COMPLETED_COUNT. Computed here rather than
     * in the INT2 handler (int_handler.s) so the client-side FIFO is only
     * ever touched from task context, never interrupt context -- avoids
     * needing to make queue_head/queue_tail/queue_count updates
     * interrupt-safe for no real benefit (MHIGetEmpty is documented as the
     * thing you call after a signal arrives, exactly like servicing a
     * message port, so the lag is at most one call). */
    now = mhi_completed_count(player->board);
    delta = (UWORD)(now - player->completed_seen);
    player->completed_seen = now;
    player->pending_completions = (UWORD)(player->pending_completions + delta);

    if (player->pending_completions == 0 || player->queue_count == 0) {
        return NULL;
    }

    buffer = player->queue[player->queue_head];
    player->queue_head = (UWORD)((player->queue_head + 1) % MHI_QUEUE_MAX);
    player->queue_count--;
    player->pending_completions--;
    return buffer;
}

UBYTE i_MHIGetStatus(APTR handle __asm("a3"), struct MHICopperlineBase *base __asm("a6"))
{
    struct MHIPlayer *player = valid_player(handle);

    (void)base;
    if (player == NULL) {
        return MHIF_STOPPED;
    }
    /* mhi.md: STATUS "may be polled freely at any time, from any context"
     * with no side effect -- always re-read the board rather than trust
     * player->status, which only exists as a convenience cache other entry
     * points update opportunistically. */
    player->status = translate_status(mhi_status(player->board));
    return player->status;
}

void i_MHIPlay(APTR handle __asm("a3"), struct MHICopperlineBase *base __asm("a6"))
{
    struct MHIPlayer *player = valid_player(handle);

    (void)base;
    if (player == NULL) {
        return;
    }
    mhi_control(player->board, MHI_CONTROL_PLAY);
    /* CONTROL takes effect immediately (mhi.md: "by the time the move.w
     * retires, STATUS already reflects the new state") -- including the
     * PLAY-from-empty-queue case that lands straight in OUT_OF_DATA, so
     * re-read rather than assume MHIF_PLAYING. */
    player->status = translate_status(mhi_status(player->board));
}

void i_MHIStop(APTR handle __asm("a3"), struct MHICopperlineBase *base __asm("a6"))
{
    struct MHIPlayer *player = valid_player(handle);

    (void)base;
    if (player == NULL) {
        return;
    }
    mhi_control(player->board, MHI_CONTROL_STOP);
    player->status = MHIF_STOPPED;

    /* mhi.md "Completion and reclaim": STOP resets QUEUE_COUNT and
     * COMPLETED_COUNT to 0 on the board, flushing every queued descriptor
     * whether or not it had started -- the client must resynchronize its
     * own local counter to 0 rather than compute a delta across the reset,
     * so the local FIFO and pending-completions count are dropped here
     * too, not drained through MHIGetEmpty. */
    player->queue_head = 0;
    player->queue_tail = 0;
    player->queue_count = 0;
    player->pending_completions = 0;
    player->completed_seen = 0;
}

void i_MHIPause(APTR handle __asm("a3"), struct MHICopperlineBase *base __asm("a6"))
{
    struct MHIPlayer *player = valid_player(handle);

    (void)base;
    if (player == NULL) {
        return;
    }
    mhi_control(player->board, MHI_CONTROL_PAUSE);
    player->status = translate_status(mhi_status(player->board));
}

ULONG i_MHIQuery(ULONG query __asm("d1"), struct MHICopperlineBase *base __asm("a6"))
{
    UWORD caps = base->board != NULL ? mhi_caps(base->board) : 0;

    switch (query) {
        case MHIQ_CAPABILITIES:
            return (ULONG)MHI_CAPABILITIES;
        case MHIQ_DECODER_NAME:
            return (ULONG)MHI_DECODER_NAME;
        case MHIQ_DECODER_VERSION:
            return (ULONG)decoder_version;
        case MHIQ_AUTHOR:
            return (ULONG)MHI_AUTHOR;

        /* Guest-library-side static answers (docs/internals/mhi.md "The
         * MHI-API/board split"): this is a real register-mailbox device
         * talked to over the Zorro bus, so IS_HARDWARE is true; it runs no
         * 68k/PPC code of its own, so both processor queries are false. */
        case MHIQ_IS_HARDWARE:
            return MHIF_SUPPORTED;
        case MHIQ_IS_68K:
        case MHIQ_IS_PPC:
            return MHIF_UNSUPPORTED;

        /* Genuinely board-reported (CAPS register, mhi.md "Capability/
         * version registers") -- a future board revision's decoder could
         * differ, unlike everything else in this switch. */
        case MHIQ_MPEG1:
            return (caps & MHI_CAPS_MPEG1) ? MHIF_SUPPORTED : MHIF_UNSUPPORTED;
        case MHIQ_MPEG2:
            return (caps & MHI_CAPS_MPEG2) ? MHIF_SUPPORTED : MHIF_UNSUPPORTED;
        case MHIQ_MPEG25:
            return (caps & MHI_CAPS_MPEG25) ? MHIF_SUPPORTED : MHIF_UNSUPPORTED;
        case MHIQ_LAYER3:
            return (caps & MHI_CAPS_LAYER3) ? MHIF_SUPPORTED : MHIF_UNSUPPORTED;
        case MHIQ_VARIABLE_BITRATE:
            return (caps & MHI_CAPS_VBR) ? MHIF_SUPPORTED : MHIF_UNSUPPORTED;

        /* MPEG-4 and Layer 1/2 have no CAPS bit in protocol VERSION 1
         * (mhi.md: "Layer I/II are not implemented, so bits for them do
         * not exist in this version") -- unconditionally unsupported. */
        case MHIQ_MPEG4:
        case MHIQ_LAYER1:
        case MHIQ_LAYER2:
            return MHIF_UNSUPPORTED;

        /* Inherent to any conforming Layer III decoder, not a distinct
         * board capability worth its own CAPS bit (mhi.md). */
        case MHIQ_JOINT_STEREO:
            return MHIF_SUPPORTED;

        /* The seven params this board's PARAM_SELECT table defines (mhi.md
         * "Param latches") always exist and round-trip, but whether
         * `MHISetParam` calls are actually *audible* depends on the board's
         * own CAPS bit 6 (mhi.md "M4: the DSP chain") -- reading it here
         * rather than hardcoding MHIF_SUPPORTED/UNSUPPORTED is what lets
         * this one library binary answer correctly against both an M1-M3
         * board (latches inert) and an M4-or-later board (latches audible)
         * with no rebuild needed either way (translate_param() above keeps
         * the same seven-entry list regardless, since the board still
         * needs to store the latch even when CAPS says it's inert). */
        case MHIQ_VOLUME_CONTROL:
        case MHIQ_PANNING_CONTROL:
        case MHIQ_BASS_CONTROL:
        case MHIQ_MID_CONTROL:
        case MHIQ_TREBLE_CONTROL:
        case MHIQ_CROSSMIXING:
        case MHIQ_PREFACTOR_CONTROL:
            return (caps & MHI_CAPS_PARAMS_APPLIED) ? MHIF_SUPPORTED : MHIF_UNSUPPORTED;

        /* Reserved param-index territory (mhi.md: indices 7-65535,
         * "unimplemented in this version") -- no 5/10-band EQ yet. */
        case MHIQ_5_BAND_EQ:
        case MHIQ_10_BAND_EQ:
            return MHIF_UNSUPPORTED;

        default:
            return MHIF_UNSUPPORTED;
    }
}

void i_MHISetParam(APTR handle __asm("a3"), UWORD param __asm("d0"), ULONG value __asm("d1"), struct MHICopperlineBase *base __asm("a6"))
{
    struct MHIPlayer *player = valid_player(handle);
    UWORD index;

    (void)base;
    if (player == NULL) {
        return;
    }
    if (!translate_param(param, &index)) {
        return;
    }
    /* mhi.md "Param latches": out-of-range values for a 0-100 parameter are
     * clamped by the board itself, not rejected here -- PARAM_VALUE is a
     * plain UWORD register, so a value wider than that is simply
     * truncated, matching how DESC_LEN/DESC_ADDR are handled (board.c
     * never range-checks either). */
    mhi_param_set(player->board, index, (UWORD)value);
}
