/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * copperhf.device: the M2 exec-device stub. Builds the device (MakeLibrary +
 * AddDevice, deferred to rt_Init the same way guest/services/handler.c and
 * guest/hostsocket/stub.c defer their own resident construction -- see
 * entry.s's header comment) and implements Open/Close/Expunge/BeginIO/
 * AbortIO against the doorbell/completion-queue protocol in
 * copperhf_board.h. INT2 completion draining is int_handler.s, not here:
 * exec.doc's own AddIntServer WARNING says a plain C function cannot
 * reliably control the 68000 Z flag on return, which the interrupt-server
 * chain contract depends on (see that file's header comment).
 *
 * -mpcrel/no-relocations/no-data-bss discipline (see entry.s's header
 * comment): every function here is reached only via a `label-label`
 * distance in entry.s's _func_table, or via MakeLibrary's own `init`
 * register convention (never a hand `.long external_symbol`), and nothing
 * here is a static/global variable -- all device state lives in the
 * MakeLibrary-allocated device base (real Amiga RAM, not this ROM's own
 * data section), read back through the `dev` parameter every call.
 * Library calls (OpenLibrary/CloseLibrary/GetCurrentBinding/MakeLibrary/
 * AddDevice/AddIntServer) come from the toolchain's inline headers, which
 * generate ordinary compiler PC-relative code under -mpcrel -- the
 * HARD-WON pitfall is specific to hand-written assembly data directives,
 * not compiler-generated code (guest/hostsocket/stub.c's own header
 * comment makes the same point for its GetCurrentBinding use).
 */

#include <exec/types.h>
#include <exec/nodes.h>
#include <exec/lists.h>
#include <exec/libraries.h>
#include <exec/devices.h>
#include <exec/errors.h>
#include <exec/io.h>
#include <exec/interrupts.h>
#include <exec/execbase.h>
#include <libraries/configvars.h>
#include <libraries/expansion.h>
#include <libraries/expansionbase.h>

#define EXEC_BASE_NAME _sysbase
#define EXPANSION_BASE_NAME _expbase
#include <inline/exec.h>
#include <inline/expansion.h>

#include "copperhf_board.h"

#define INTB_PORTS 3 /* hardware/intbits.h -- I/O ports and timers */

/* copperhf.device's extension of struct Library/struct Device: the board
 * base (every register access is board-base-relative) and the INT2
 * interrupt-server node int_handler.s drains completions through. Both
 * live in the device base MakeLibrary allocates -- real writable Amiga
 * RAM, not ROM -- so there is nothing here that needs relocation fixups. */
struct CopperhfDevice {
    struct Library dev_Lib;
    APTR dev_BoardBase;
    struct Interrupt dev_Interrupt;
};

/* Defined in entry.s: the vector table MakeLibrary consumes, and the
 * shared "copperhf.device" name/id string (also this device's ln_Name). */
extern APTR func_table[];
extern char device_name[];

/* int_handler.s -- see this file's header comment for why the INT2
 * server itself cannot be C. */
extern void chf_int_handler(void);

/* mounter.c (M3) -- narrow interface, see that file's own header comment.
 * Ordinary extern C function, unlike the four exec vectors above: nothing
 * about this call needs to satisfy exec's register-based dispatch
 * contract, so it is reached with a plain compiler-generated call. */
extern void chf_mount_all(struct ExecBase *sysbase, APTR board, struct ConfigDev *cd);

/* AbsExecBase -- same reasoning as guest/hostsocket/stub.c's sysbase():
 * GCC's array-bounds warning treats any near-NULL dereference as a bug,
 * and move.l 4.w is the canonical instruction for this anyway. */
static struct ExecBase *sysbase(void)
{
    struct ExecBase *base;
    __asm("move.l 4.w,%0" : "=r"(base));
    return base;
}

/* Re-derives the board base (and, for the mounter, the ConfigDev itself)
 * via expansion.library's GetCurrentBinding(), exactly the recipe
 * guest/services/handler.c's resident_init() and guest/hostsocket/stub.c's
 * hs_get_board_base() both use (confirmed safe there on real Kickstart
 * 1.3/2.0/3.1) -- none of da_DiagPoint's own registers (in particular, the
 * board base) are handed to rt_Init. `cd_out` may be NULL. */
static APTR chf_get_board_base(struct ConfigDev **cd_out)
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
    if (cd_out != NULL)
        *cd_out = cb.cb_ConfigDev;

    CloseLibrary(_expbase);
    return board;
}

/* MakeLibrary's `init` entry point (exec.doc/MakeLibrary INPUTS): called
 * before the device is added to the system, with d0 = libAddr, a0 =
 * segList, a6 = ExecBase. entry.s's _resident_init smuggles the board
 * base through the segList parameter (a0) -- nothing here is really
 * AmigaDOS, so that field is free to carry whatever this device wants,
 * same trick guest/hostsocket/entry.s's own _lib_init uses. Must return
 * the device address, or NULL for failure (in which case the caller would
 * have to free it itself -- never needed here, this never fails). */
struct CopperhfDevice *dev_init(struct ExecBase *sysbase __asm("a6"),
                                 APTR boardbase __asm("a0"),
                                 struct CopperhfDevice *dev __asm("d0"))
{
    (void)sysbase;
    dev->dev_Lib.lib_Node.ln_Type = NT_DEVICE;
    dev->dev_Lib.lib_Node.ln_Pri = 0;
    dev->dev_Lib.lib_Node.ln_Name = (char *)device_name;
    dev->dev_Lib.lib_Flags = LIBF_SUMUSED;
    dev->dev_Lib.lib_Version = 1;
    dev->dev_Lib.lib_Revision = 0;
    dev->dev_Lib.lib_IdString = (char *)device_name;
    dev->dev_BoardBase = boardbase;
    return dev;
}

/* rt_Init: called by Kickstart's cold-start resident scan (D0=0, A0=NULL
 * segList, A6=ExecBase) once expansion has DiagPoint-ed every board --
 * the documented, hardware-proven place to build a resident device (RKRM
 * Libraries, "Expansion Library", "Events At ROMTAG INIT Time"; see
 * entry.s's header comment for why real construction can't happen from
 * da_DiagPoint itself). */
void resident_init(struct ExecBase *_sysbase __asm("a6"))
{
    struct ConfigDev *cd = NULL;
    APTR board = chf_get_board_base(&cd);
    if (board == NULL)
        return;

    struct CopperhfDevice *dev = (struct CopperhfDevice *)MakeLibrary(
        (APTR)func_table, NULL, (APTR)dev_init,
        sizeof(struct CopperhfDevice), (BPTR)board);
    if (dev == NULL)
        return;

    AddDevice((struct Device *)dev);

    /* Mount every present unit's partitions (M3) strictly before the INT2
     * server goes live: the mounter's own I/O is polled (mounter.c's
     * chf_do_io spins CHF_COMPLETE_GET/ACK itself) and its IORequests have
     * no MsgPort at all, so int_handler.s's ReplyMsg must never see one of
     * them -- see mounter.c's header comment for the full reasoning. */
    chf_mount_all(_sysbase, board, cd);

    dev->dev_Interrupt.is_Node.ln_Type = NT_INTERRUPT;
    dev->dev_Interrupt.is_Node.ln_Pri = 0;
    dev->dev_Interrupt.is_Node.ln_Name = (char *)device_name;
    dev->dev_Interrupt.is_Data = board;
    dev->dev_Interrupt.is_Code = (void (*)(void))chf_int_handler;
    AddIntServer(INTB_PORTS, &dev->dev_Interrupt);

    *(volatile UWORD *)((UBYTE *)board + CHF_IRQ_ENABLE) = 1;
}

/* Open(dev, ioreq, unitNumber, flags) -- register convention verbatim
 * from exec.doc's device-vector contract (A6=devbase, A1=ioreq, D0=unit,
 * D1=flags). Fails with IOERR_OPENFAIL unless unit < CHF_UNITS and the
 * matching CHF_UNIT_PRESENT bit is set (COPPERHF-DEVICE-PLAN.md's M2
 * spec); on success io_Unit is left holding the raw unit NUMBER, per this
 * device's io_Unit-is-not-a-pointer convention (copperhf_board.h). */
void dev_open(struct IOStdReq *ioreq __asm("a1"), ULONG unit __asm("d0"),
              ULONG flags __asm("d1"), struct CopperhfDevice *dev __asm("a6"))
{
    (void)flags;
    UBYTE *board = (UBYTE *)dev->dev_BoardBase;
    UWORD units = *(volatile UWORD *)(board + CHF_UNITS);
    UWORD present = *(volatile UWORD *)(board + CHF_UNIT_PRESENT);

    if (unit >= units || !(present & (1U << unit))) {
        ioreq->io_Error = CHF_IOERR_OPENFAIL;
        ioreq->io_Device = NULL;
        return;
    }

    dev->dev_Lib.lib_OpenCnt++;
    dev->dev_Lib.lib_Flags &= ~LIBF_DELEXP;
    ioreq->io_Device = (struct Device *)dev;
    ioreq->io_Unit = (struct Unit *)unit;
    ioreq->io_Error = 0;
}

/* Close(dev, ioreq) -- A6=devbase, A1=ioreq. Returns the seglist to
 * expunge, or 0. copperhf.device is ROM-resident and never auto-expunges
 * (COPPERHF-DEVICE-PLAN.md's M2 spec), so this always returns 0 even once
 * the open count reaches zero. */
BPTR dev_close(struct IOStdReq *ioreq __asm("a1"), struct CopperhfDevice *dev __asm("a6"))
{
    if (dev->dev_Lib.lib_OpenCnt > 0)
        dev->dev_Lib.lib_OpenCnt--;
    ioreq->io_Device = NULL;
    ioreq->io_Unit = NULL;
    return 0;
}

/* Expunge(dev) -- A6=devbase. A ROM-resident device always refuses: there
 * is no segment list to hand back, and nothing to free. */
BPTR dev_expunge(struct CopperhfDevice *dev __asm("a6"))
{
    (void)dev;
    return 0;
}

/* ExtFunc/reserved (LVO -24): never called by any real client; every
 * other device's own ExtFunc vector is likewise an unconditional 0. */
ULONG dev_extfunc(void)
{
    return 0;
}

/* BeginIO(dev, ioreq) -- A6=devbase, A1=ioreq. Clears IOF_QUICK (the M1
 * board always executes synchronously host-side, but the guest is never
 * told so -- it must wait for the INT2 completion drain like real
 * asynchronous hardware, never inspect the request again until then) and
 * rings CHF_DOORBELL with the request pointer as a single 32-bit write
 * (copperhf_board.h: "a single 32-bit write commits immediately"). Never
 * calls ReplyMsg here -- that is int_handler.s's job once the completion
 * actually shows up on CHF_COMPLETE_GET. */
void dev_beginio(struct IOStdReq *ioreq __asm("a1"), struct CopperhfDevice *dev __asm("a6"))
{
    ioreq->io_Flags &= ~IOF_QUICK;
    ioreq->io_Error = 0;
    *(volatile ULONG *)((UBYTE *)dev->dev_BoardBase + CHF_DOORBELL) = (ULONG)ioreq;
}

/* AbortIO(dev, ioreq) -- A6=devbase, A1=ioreq. The M1/M2 protocol executes
 * every request synchronously host-side and completes it before
 * CHF_DOORBELL's write even returns, so by the time a client could call
 * AbortIO the request is already done (or about to be, moments away from
 * INT2) -- there is never anything left to actually cancel. Report
 * IOERR_NOCMD ("not aborted") rather than pretending success. */
LONG dev_abortio(struct IOStdReq *ioreq __asm("a1"), struct CopperhfDevice *dev __asm("a6"))
{
    (void)ioreq;
    (void)dev;
    return CHF_IOERR_NOCMD;
}
