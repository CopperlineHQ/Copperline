/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * hs_get_board_base(): entry.s's _resident_init (reached via rt_Init) is
 * called by Kickstart's cold-start resident scan with D0=0, A0=NULL
 * segList, A6=ExecBase -- none of da_DiagPoint's own registers (in
 * particular, the board base) are handed to it directly. This re-opens
 * expansion.library and calls GetCurrentBinding() for our ConfigDev
 * instead, exactly the recipe Copperline's own
 * guest/services/handler.c resident_init() uses (confirmed safe there on
 * real Kickstart 1.3/2.0/3.1) -- see entry.s's header comment for why
 * da_DiagPoint itself must not build the library synchronously.
 *
 * Plain C is safe here, unlike the bsdsocket.library LVO trampolines (see
 * this file's own original header comment, preserved below): rt_Init is a
 * completely different calling context with no per-call-number register
 * contract to get subtly wrong.
 *
 * -mpcrel/no-relocations/no-data-bss discipline (see entry.s's own header
 * comment): OpenLibrary/CloseLibrary/GetCurrentBinding come from this
 * toolchain's "inline" headers, which generate ordinary compiler
 * PC-relative code under -mpcrel -- the HARD-WON pitfall entry.s documents
 * is specific to hand-written `.long external_symbol` data directives in
 * assembly, not compiler-generated code, so it does not apply here.
 */

#include <exec/execbase.h>
#include <exec/types.h>
#include <libraries/configvars.h>
#include <libraries/expansion.h>
#include <libraries/expansionbase.h>

#define EXEC_BASE_NAME _sysbase
#define EXPANSION_BASE_NAME _expbase
#include <inline/exec.h>
#include <inline/expansion.h>

/* AbsExecBase -- see guest/services/
 * handler.c sysbase() for why this is asm rather than a plain
 * *(struct ExecBase **)4 dereference (GCC's array-bounds warning treats
 * any near-NULL pointer dereference as a bug; move.l 4.w is also the
 * canonical instruction for this anyway). */
static struct ExecBase *sysbase(void)
{
    struct ExecBase *base;
    __asm("move.l 4.w,%0" : "=r"(base));
    return base;
}

APTR hs_get_board_base(void)
{
    struct ExecBase *_sysbase = sysbase();
    struct Library *_expbase = OpenLibrary((STRPTR) "expansion.library", 0);
    if (_expbase == NULL)
        return NULL;

    struct CurrentBinding cb;
    GetCurrentBinding(&cb, sizeof(cb));
    APTR board = NULL;
    if (cb.cb_ConfigDev != NULL)
        board = cb.cb_ConfigDev->cd_BoardAddr;

    CloseLibrary(_expbase);
    return board;
}
