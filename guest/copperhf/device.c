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
#include <devices/newstyle.h>
#include <stddef.h>

#define EXEC_BASE_NAME _sysbase
#define EXPANSION_BASE_NAME _expbase
#include <inline/exec.h>
#include <inline/expansion.h>

#include "copperhf_board.h"
#include "device_layout.h"

#define INTB_PORTS 3 /* hardware/intbits.h -- I/O ports and timers */

/* copperhf.device's extension of struct Library/struct Device: the board
 * base (every register access is board-base-relative), the INT2
 * interrupt-server node int_handler.s drains completions through, and (M4)
 * the list of pending TD_ADDCHANGEINT requests int_handler.s's
 * chf_drain_changes() walks on a CHF_CHANGED_MASK interrupt. All three
 * live in the device base MakeLibrary allocates -- real writable Amiga
 * RAM, not ROM -- so there is nothing here that needs relocation fixups.
 *
 * dev_BoardBase's offset must stay device_layout.h's
 * CHF_DEV_BOARDBASE_OFFSET -- see that file's header comment for why
 * int_handler.s needs the number at all (is_Data is now this whole struct,
 * not just the board pointer) and the _Static_assert below for how it's
 * kept honest. */
struct CopperhfDevice {
    struct Library dev_Lib;
    APTR dev_BoardBase;
    struct Interrupt dev_Interrupt;
    struct List dev_ChangeInts; /* M4: pending TD_ADDCHANGEINT requests */
};

_Static_assert(offsetof(struct CopperhfDevice, dev_BoardBase) == CHF_DEV_BOARDBASE_OFFSET,
               "device_layout.h's CHF_DEV_BOARDBASE_OFFSET is out of sync with "
               "struct CopperhfDevice -- update it (and re-derive the byte count in "
               "that file's header comment) if this struct's layout ever changes");

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
    /* Manual empty-list init (the idiom exec/lists.h's own NewList macro
     * expands to -- mounter.c's chf_get_or_create_fsr uses the same
     * pattern for fsr_FileSysEntries), not a NewList() library call: this
     * runs inside MakeLibrary's own init before the device (and thus
     * exec.library's normal calling context for this task) is fully
     * established. */
    dev->dev_ChangeInts.lh_Head = (struct Node *)&dev->dev_ChangeInts.lh_Tail;
    dev->dev_ChangeInts.lh_Tail = NULL;
    dev->dev_ChangeInts.lh_TailPred = (struct Node *)&dev->dev_ChangeInts.lh_Head;
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
    /* M4: is_Data is the whole device (not just the board pointer, M1-M3)
     * so int_handler.s's CHF_CHANGED_MASK path can reach dev_ChangeInts;
     * the asm recovers the board pointer itself via
     * device_layout.h's CHF_DEV_BOARDBASE_OFFSET. */
    dev->dev_Interrupt.is_Data = dev;
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

/* ROM-resident, 0-terminated NSD supported-command table
 * (NSCMD_DEVICEQUERY's nsdqr_SupportedCommands, devices/newstyle.h):
 * every command copperhf.device answers, guest-side stub commands and
 * doorbell-forwarded commands alike. `static const`, so this is compiler-
 * generated PC-relative rodata referenced only from this same file --
 * exactly the "ordinary compiler-generated PC-relative code" case entry.s's
 * header comment carves out as safe, never a hand `.long`. */
static const UWORD chf_nsd_commands[] = {
    CHF_CMD_READ,
    CHF_CMD_WRITE,
    CHF_CMD_UPDATE,
    CHF_CMD_CLEAR,
    CHF_CMD_TD_MOTOR,
    CHF_CMD_TD_FORMAT,
    CHF_CMD_TD_GETGEOMETRY,
    CHF_CMD_TD_CHANGENUM,
    CHF_CMD_TD_CHANGESTATE,
    CHF_CMD_TD_PROTSTATUS,
    CHF_CMD_TD_ADDCHANGEINT,
    CHF_CMD_TD_REMCHANGEINT,
    CHF_CMD_TD_EJECT,
    CHF_CMD_TD_READ64,
    CHF_CMD_TD_WRITE64,
    CHF_CMD_TD_SEEK64,
    CHF_CMD_TD_FORMAT64,
    CHF_CMD_HD_SCSICMD,
    CHF_NSCMD_DEVICEQUERY,
    CHF_NSCMD_TD_READ64,
    CHF_NSCMD_TD_WRITE64,
    CHF_NSCMD_TD_SEEK64,
    CHF_NSCMD_TD_FORMAT64,
    0
};

/* Completes a guest-answered request per BeginIO's IOF_QUICK contract
 * (exec.doc/BeginIO): if the caller allowed quick I/O, leave IOF_QUICK set
 * and return -- the caller polls io_Error/io_Actual itself and there is no
 * ReplyMsg. Otherwise clear it (already clear on this path, but that is
 * exactly what "answering a quick-eligible request the slow way" means)
 * and ReplyMsg from here -- BeginIO always runs in the caller's own task
 * context, so this is safe unlike int_handler.s's own interrupt-context
 * ReplyMsg. */
static void chf_complete(struct IOStdReq *ioreq, struct ExecBase *_sysbase)
{
    if (ioreq->io_Flags & IOF_QUICK)
        return;
    ReplyMsg(&ioreq->io_Message);
}

/* NSCMD_DEVICEQUERY (guest-side, copperhf_board.h): fills the
 * NSDeviceQueryResult at io_Data from chf_nsd_commands above.
 * nsdqr_SizeAvailable is always this device's full result-struct size (the
 * NSD contract: report it regardless of how much the caller's buffer can
 * hold), but the struct itself is only written -- and io_Actual only set
 * -- if io_Length says the caller's buffer is big enough to hold it. */
static void chf_do_devicequery(struct IOStdReq *ioreq, struct ExecBase *_sysbase)
{
    struct NSDeviceQueryResult *r;

    if (ioreq->io_Length < (ULONG)sizeof(struct NSDeviceQueryResult)) {
        ioreq->io_Error = CHF_IOERR_BADLENGTH;
        chf_complete(ioreq, _sysbase);
        return;
    }

    r = (struct NSDeviceQueryResult *)ioreq->io_Data;
    r->nsdqr_DevQueryFormat = 0;
    r->nsdqr_SizeAvailable = sizeof(struct NSDeviceQueryResult);
    r->nsdqr_DeviceType = NSDEVTYPE_TRACKDISK;
    r->nsdqr_DeviceSubType = 0;
    r->nsdqr_SupportedCommands = (UWORD *)chf_nsd_commands;

    ioreq->io_Actual = sizeof(struct NSDeviceQueryResult);
    ioreq->io_Error = 0;
    chf_complete(ioreq, _sysbase);
}

/* TD_ADDCHANGEINT (guest-side, copperhf_board.h): queues io_Data (a
 * struct Interrupt*, trackdisk.doc's TD_ADDCHANGEINT contract) on the
 * per-device pending list under Disable()/Enable() -- int_handler.s's
 * chf_drain_changes() walks this same list at interrupt time on a
 * CHF_CHANGED_MASK IRQ. Never rings the doorbell and never replies: per
 * trackdisk.doc, "this command only returns when the handler is removed"
 * (TD_REMCHANGEINT), so it is always effectively asynchronous regardless
 * of what IOF_QUICK the caller passed in -- callers are required to use
 * SendIO(), never DoIO(), with this command. */
static void chf_do_addchangeint(struct IOStdReq *ioreq, struct CopperhfDevice *dev,
                                 struct ExecBase *_sysbase)
{
    ioreq->io_Error = 0;
    ioreq->io_Flags &= ~IOF_QUICK;
    Disable();
    AddTail(&dev->dev_ChangeInts, (struct Node *)ioreq);
    Enable();
}

/* TD_REMCHANGEINT (guest-side, copperhf_board.h): finds the pending
 * TD_ADDCHANGEINT request this call is removing. trackdisk.doc's own
 * IO REQUEST INPUT for TD_REMCHANGEINT says "the same IO request used for
 * TD_ADDCHANGEINT", but since that original request is still held by the
 * device (never replied until this call), a caller cannot literally reuse
 * the same struct for both DoIO(REMCHANGEINT) and the still-pending
 * SendIO(ADDCHANGEINT) at once -- so match by io_Data (the Interrupt*
 * pointer, unique per registration and the field the documented "same IO
 * request" phrasing is really about identifying) first, and fall back to
 * matching the request pointer itself in case a caller does literally
 * reuse the block after its ADDCHANGEINT was already removed some other
 * way. Removes the match under Disable()/Enable() and ReplyMsg's it --
 * that is the only place the held ADDCHANGEINT ever completes. The
 * REMCHANGEINT request itself then completes normally, honoring its own
 * IOF_QUICK. */
static void chf_do_remchangeint(struct IOStdReq *ioreq, struct CopperhfDevice *dev,
                                 struct ExecBase *_sysbase)
{
    struct Node *node;
    struct IOStdReq *pending = NULL;

    Disable();
    for (node = dev->dev_ChangeInts.lh_Head; node->ln_Succ != NULL; node = node->ln_Succ) {
        struct IOStdReq *cand = (struct IOStdReq *)node;
        if (cand->io_Data == ioreq->io_Data || cand == ioreq) {
            pending = cand;
            break;
        }
    }
    if (pending != NULL)
        Remove((struct Node *)pending);
    Enable();

    /* pending == ioreq means the caller reused the still-held ADDCHANGEINT
     * block itself as the REMCHANGEINT request -- then the one completion
     * below already covers it, and a separate ReplyMsg here would reply
     * the same message twice (double-linking it on the reply port). */
    if (pending != NULL && pending != ioreq)
        ReplyMsg(&pending->io_Message);

    ioreq->io_Error = 0;
    chf_complete(ioreq, _sysbase);
}

/* BeginIO(dev, ioreq) -- A6=devbase, A1=ioreq. Three commands are answered
 * entirely from the guest stub (copperhf_board.h: "the guest-side ones
 * never reach the doorbell at all") because they need guest pointers (the
 * NSD command table) or guest state (the pending change-interrupt list)
 * the host cannot provide: NSCMD_DEVICEQUERY, TD_ADDCHANGEINT,
 * TD_REMCHANGEINT. Every other command keeps the M1-M3 behaviour
 * unchanged -- clears IOF_QUICK (the board always executes synchronously
 * host-side, but the guest is never told so -- it must wait for the INT2
 * completion drain like real asynchronous hardware, never inspect the
 * request again until then) and rings CHF_DOORBELL with the request
 * pointer as a single 32-bit write (copperhf_board.h: "a single 32-bit
 * write commits immediately"). Never calls ReplyMsg on that path -- that
 * is int_handler.s's job once the completion actually shows up on
 * CHF_COMPLETE_GET. */
void dev_beginio(struct IOStdReq *ioreq __asm("a1"), struct CopperhfDevice *dev __asm("a6"))
{
    switch (ioreq->io_Command) {
    case CHF_NSCMD_DEVICEQUERY:
        chf_do_devicequery(ioreq, sysbase());
        return;
    case CHF_CMD_TD_ADDCHANGEINT:
        chf_do_addchangeint(ioreq, dev, sysbase());
        return;
    case CHF_CMD_TD_REMCHANGEINT:
        chf_do_remchangeint(ioreq, dev, sysbase());
        return;
    default:
        break;
    }

    ioreq->io_Flags &= ~IOF_QUICK;
    ioreq->io_Error = 0;
    *(volatile ULONG *)((UBYTE *)dev->dev_BoardBase + CHF_DOORBELL) = (ULONG)ioreq;
}

/* int_handler.s (M4): CHF_IRQ_STATUS bit 1 handler, reached by an ordinary
 * cross-object `bsr.w` (not a hand `.long` -- see entry.s's header
 * comment), with `dev` passed as an ORDINARY STACK ARGUMENT -- unlike the
 * four exec vectors above (dev_open/dev_close/dev_beginio/dev_abortio),
 * this is NOT a `__asm("a5")`-bound register parameter, and int_handler.s
 * pushes it accordingly (`move.l a5,-(sp)` before the `bsr.w`, cleaned up
 * by the caller after return, matching this exact toolchain's own
 * plain-C-call convention -- confirmed against chf_mount_all's call site
 * in resident_init, which pushes its three arguments the same way).
 *
 * HARD-WON: an earlier version declared this function as
 * `chf_drain_changes(struct CopperhfDevice *dev __asm("a5"))`, matching
 * the four exec vectors' own register-bound-parameter idiom, and had
 * int_handler.s simply `bsr.w` with dev already sitting in a5 (no stack
 * push) on the theory that it was "the same explicit-register-parameter
 * convention". That theory only holds for the four exec vectors because
 * they are EXEC's own ABI entry points (MakeLibrary's function table /
 * BeginIO's documented A6=devbase,A1=ioreq contract) with no
 * GCC-generated *caller* at all -- GCC only ever generates the callee side
 * for those, and honors the register binding at the function's own entry.
 * chf_drain_changes has no such special status: it is an ordinary internal
 * function reached by hand-written asm, and this exact GCC target does NOT
 * reliably honor `__asm("a5")` as an input-parameter binding for a
 * CALLEE-SAVED register (A5, unlike the four vectors' A0/A1/D0/D1/A6,
 * which are all caller-scratch registers) -- confirmed via objdump: the
 * compiled prologue (`movem.l d2/a2/a5-a6,-(sp)`) preserves the CALLER's
 * incoming A5 as any callee-saved register would, then reads the
 * "register-bound" parameter back from `50(sp)` -- i.e. GCC silently fell
 * back to ordinary stack-passing for this parameter, ignoring the `a5`
 * annotation, because using a callee-saved register as a fixed parameter
 * slot conflicts with the function's own need to preserve the caller's A5.
 * Since int_handler.s's `bsr.w` (no push) never put anything useful at
 * that stack slot, `dev` inside chf_drain_changes read whatever garbage
 * happened to be there -- explaining the M4 guest test's exact failure
 * signature (`board` computed from that garbage `dev` pointed nowhere
 * real, so CHF_CHANGED_ACK's write never reached the actual board,
 * CHF_CHANGED_MASK never cleared, and INT2 re-fired continuously --
 * confirmed by a temporary diagnostic build that counted 6193+
 * chf_drain_changes entries in 13 emulated seconds, all completing
 * normally, all reading the same bogus non-board address). Standard
 * C-callee register discipline applies otherwise (this function, like any
 * ordinary library call, may trash D0/D1/A0/A1; D2-D7/A2/A5/A6 are
 * GCC-callee-preserved on this target, so int_handler.s's own board-base
 * register in A0 -- recomputed after this call anyway -- and A5 itself
 * both survive).
 *
 * Reads CHF_CHANGED_MASK once and immediately acks exactly what it saw
 * (copperhf_board.h: CHF_CHANGED_ACK "write a mask; clears those
 * CHF_CHANGED_MASK bits") so a change that lands between the read and the
 * ack still shows up as pending on the next interrupt rather than being
 * silently swallowed, then Cause()s (exec LVO -180) every queued
 * TD_ADDCHANGEINT Interrupt whose io_Unit (the raw unit number, this
 * device's io_Unit convention -- copperhf_board.h) names a unit the mask
 * has set. One pass over the pending list rather than a mask-bit outer
 * loop: every pending node is checked against the whole mask once, which
 * is the same amount of work and simpler. */
void chf_drain_changes(struct CopperhfDevice *dev)
{
    UBYTE *board = (UBYTE *)dev->dev_BoardBase;
    UWORD mask = *(volatile UWORD *)(board + CHF_CHANGED_MASK);
    struct ExecBase *_sysbase;
    struct Node *node;

    if (mask == 0)
        return;
    *(volatile UWORD *)(board + CHF_CHANGED_ACK) = mask;

    _sysbase = sysbase();
    for (node = dev->dev_ChangeInts.lh_Head; node->ln_Succ != NULL; node = node->ln_Succ) {
        struct IOStdReq *req = (struct IOStdReq *)node;
        UWORD unit = (UWORD)(ULONG)req->io_Unit;
        if (unit < CHF_NUM_UNITS && (mask & (1U << unit)))
            Cause((struct Interrupt *)req->io_Data);
    }
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
