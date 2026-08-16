/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Board-register access layer implementation. See board.h's own header
 * comment: this file (plus board.h and int_handler.s) is the entire
 * hardware-specific surface of mhi_copperline.library -- everything else
 * (mhi_copperline.c, startup.c) only calls through these functions and
 * never pokes MHI_REG_* offsets directly.
 *
 * Ordinary C throughout (unlike guest/hostsocket's ROM stub): this board
 * has no boot ROM at all (docs/internals/mhi.md "Zorro identity": "no
 * autoboot ROM"), so mhi_copperline.library is a plain disk-loaded shared
 * library discovered by AmigaAMP under LIBS:mhi/ like any other -- normal
 * OS library calls (FindConfigDev, AddIntServer, ...) are available
 * directly, none of guest/hostsocket's or guest/services's -mpcrel/DiagArea
 * PIC discipline applies here.
 */

#define __USE_SYSBASE

#include <exec/execbase.h>
#include <exec/interrupts.h>
#include <exec/nodes.h>
#include <exec/types.h>
#include <hardware/intbits.h>
#include <libraries/configvars.h>
#include <libraries/expansion.h>

#define EXEC_BASE_NAME _sysbase
#define EXPANSION_BASE_NAME _expbase
#include <inline/exec.h>
#include <inline/expansion.h>

#include "board.h"
#include "mhi_copperline.h"

/* AbsExecBase -- see guest/hostfs-test/mkfile.c and guest/hostsocket/
 * stub.c's own sysbase(): asm sidesteps GCC's array-bounds warning about
 * dereferencing address 4 directly, and move.l 4.w,%0 is the canonical
 * instruction for it anyway. */
static struct ExecBase *sysbase(void)
{
    struct ExecBase *base;
    __asm("move.l 4.w,%0" : "=r"(base));
    return base;
}

APTR mhi_board_find(void)
{
    struct ExecBase *_sysbase = sysbase();
    struct Library *_expbase = OpenLibrary((STRPTR) "expansion.library", 0);
    if (_expbase == NULL) {
        return NULL;
    }

    APTR board = NULL;
    struct ConfigDev *cd = NULL;
    while ((cd = FindConfigDev(cd, MHI_BOARD_MANUFACTURER, MHI_BOARD_PRODUCT)) != NULL) {
        APTR candidate = cd->cd_BoardAddr;
        if (candidate == NULL) {
            continue;
        }
        UWORD version = mhi_reg_read(candidate, MHI_REG_VERSION);
        if (version >= MHI_PROTOCOL_VERSION) {
            board = candidate;
            break;
        }
        /* An older board than this driver understands: keep scanning in
         * case a second, newer board is also present, rather than giving
         * up on the first match (mirrors FindConfigDev's own documented
         * "search from the entry after oldConfigDev" idiom). */
    }

    CloseLibrary(_expbase);
    return board;
}

UWORD mhi_reg_read(APTR board, UWORD offset)
{
    volatile UWORD *regs = (volatile UWORD *)board;
    return regs[offset >> 1];
}

void mhi_reg_write(APTR board, UWORD offset, UWORD value)
{
    volatile UWORD *regs = (volatile UWORD *)board;
    regs[offset >> 1] = value;
}

UWORD mhi_caps(APTR board)
{
    return mhi_reg_read(board, MHI_REG_CAPS);
}

UBYTE mhi_status(APTR board)
{
    return (UBYTE)mhi_reg_read(board, MHI_REG_STATUS);
}

UWORD mhi_queue_depth(APTR board)
{
    return mhi_reg_read(board, MHI_REG_QUEUE_DEPTH);
}

UWORD mhi_queue_count(APTR board)
{
    return mhi_reg_read(board, MHI_REG_QUEUE_COUNT);
}

UWORD mhi_completed_count(APTR board)
{
    return mhi_reg_read(board, MHI_REG_COMPLETED_COUNT);
}

void mhi_control(APTR board, UWORD command)
{
    mhi_reg_write(board, MHI_REG_CONTROL, command);
}

void mhi_enqueue(APTR board, ULONG addr, ULONG len)
{
    /* Two independent word latches per 32-bit field (mhi.md "Descriptor
     * queue and doorbell"): hi then lo is the order shown in the spec,
     * but the spec explicitly says either order is fine since both are
     * independent latches read only when DOORBELL is written. */
    mhi_reg_write(board, MHI_REG_DESC_ADDR_HI, (UWORD)(addr >> 16));
    mhi_reg_write(board, MHI_REG_DESC_ADDR_LO, (UWORD)(addr & 0xFFFF));
    mhi_reg_write(board, MHI_REG_DESC_LEN_HI, (UWORD)(len >> 16));
    mhi_reg_write(board, MHI_REG_DESC_LEN_LO, (UWORD)(len & 0xFFFF));
    mhi_reg_write(board, MHI_REG_DOORBELL, 1);
}

UWORD mhi_intreq_read(APTR board)
{
    return mhi_reg_read(board, MHI_REG_INTREQ);
}

void mhi_intreq_ack(APTR board, UWORD bits)
{
    mhi_reg_write(board, MHI_REG_INTREQ, bits);
}

void mhi_intena_set(APTR board, UWORD bits)
{
    UWORD current = mhi_reg_read(board, MHI_REG_INTENA);
    mhi_reg_write(board, MHI_REG_INTENA, current | bits);
}

void mhi_intena_clear(APTR board, UWORD bits)
{
    UWORD current = mhi_reg_read(board, MHI_REG_INTENA);
    mhi_reg_write(board, MHI_REG_INTENA, current & ~bits);
}

void mhi_param_set(APTR board, UWORD index, UWORD value)
{
    mhi_reg_write(board, MHI_REG_PARAM_SELECT, index);
    mhi_reg_write(board, MHI_REG_PARAM_VALUE, value);
}

UWORD mhi_param_get(APTR board, UWORD index)
{
    mhi_reg_write(board, MHI_REG_PARAM_SELECT, index);
    return mhi_reg_read(board, MHI_REG_PARAM_VALUE);
}

/* The extern asm entry point AddIntServer needs (int_handler.s). Declared
 * here rather than in board.h so board.h stays includable from assembly
 * without a function-pointer-typed C declaration in its way. */
extern void mhi_int_handler(void);

BOOL mhi_board_int_install(struct MHIPlayer *player)
{
    player->intserver.is_Node.ln_Type = NT_INTERRUPT;
    player->intserver.is_Node.ln_Pri = 0;
    player->intserver.is_Node.ln_Name = (char *)"MHI Copperline";
    player->intserver.is_Data = (APTR)player;
    player->intserver.is_Code = (void (*)(void))mhi_int_handler;

    struct ExecBase *_sysbase = sysbase();
    AddIntServer(INTB_PORTS, &player->intserver);

    /* Enable exactly the two bits the INT2 handler acts on (mhi.md
     * "Interrupts"): BUFFER_DONE and OUT_OF_DATA. QUEUE_OVERFLOW is left
     * masked -- it is a diagnostic for a guest bug (racing the
     * QUEUE_COUNT check MHIQueueBuffer's own contract already avoids by
     * checking room before every DOORBELL write), not a condition MHI's
     * own API has any vocabulary for signalling. */
    mhi_intena_set(player->board, MHI_INT_BUFFER_DONE | MHI_INT_OUT_OF_DATA);
    return TRUE;
}

void mhi_board_int_remove(struct MHIPlayer *player)
{
    struct ExecBase *_sysbase = sysbase();
    mhi_intena_clear(player->board, MHI_INT_BUFFER_DONE | MHI_INT_OUT_OF_DATA);
    RemIntServer(INTB_PORTS, &player->intserver);
}
