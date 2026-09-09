// SPDX-License-Identifier: GPL-3.0-or-later
//
// Guest-side handler for Copperline's services board.
//
// Three entry points (see entry.s and copperline_board.h):
//
//  - resident_init(): rt_Init of the Romtag entry.s's DiagPoint patches into
//    the diag copy. Kickstart's cold-start resident scan calls this once
//    expansion has DiagPoint-ed every board -- the documented, hardware-
//    proven place for an autoboot driver to touch the DOS list (RKRM
//    Libraries, "Expansion Library", "Events At ROMTAG INIT Time"; the same
//    deferral the A590 SCSI boot ROM's own Romtag uses). Doing this from
//    raw DiagPoint context instead -- this ROM's original approach -- mounts
//    fine but corrupts Kickstart 1.3's own boot shortly after, since DOS and
//    much of exec's cold-start state is not yet ready that early.
//
//    The Romtag deferral long appeared not to work at all -- on Kickstart
//    1.3, 2.0, and 3.1 alike, resident_init() never executed and the boot
//    crashed or reset-looped, which read as "the resident scan never picks
//    up a DiagPoint-patched Romtag". The actual cause (found with
//    Copperline's headless CPU trace, then confirmed statically in the
//    shipped ROM bytes) was the -mpcrel external-symbol pitfall documented
//    at the top of entry.s: rt_Init was stored 0x92 bytes short, so every
//    Kickstart's InitResident faithfully jumped into the middle of
//    handler_main() and wrecked the boot from there. The scan mechanism
//    itself works on every version, exactly as the RKRM describes; rt_Init
//    now routes through a local trampoline in entry.s instead of naming
//    this function in a data directive.
//
//    resident_init() re-opens expansion.library and calls
//    GetCurrentBinding() for its ConfigDev (system-set as the current
//    binding for this call, per the RKRM), since none of DiagPoint's
//    registers are handed to a Romtag's rt_Init. It then hands
//    (board, ExpansionBase, ConfigDev) to mount_boards(), unchanged.
//
//  - mount_boards(): for each entry in the mount table the emulator wrote
//    into the board window, builds a DeviceNode whose dn_SegList points
//    back into this ROM and adds it to the mount list. DOS mounts the
//    nodes at resident-init time; the handler process is started on first
//    reference.
//
// Kickstart 1.3 (V34) is supported: both entry points probe the library
// versions at runtime and fall back from the V36+ calls (AddBootNode,
// AddDosEntry/RemDosEntry/LockDosList) to their 1.3-era equivalents. On
// V34 a non-boot mount (bootpri -128) uses AddDosNode; a bootable mount
// hand-builds the BootNode that V36's AddBootNode would have built and
// Enqueue()s it on eb_MountList with ln_Name pointing at the ConfigDev --
// the linkage V34 AddDosNode cannot make (it takes no ConfigDev), and the
// one strap needs to find the DiagArea whose da_BootPoint boots us.
//
//  - handler_main(): the DOS handler process. A pure packet pump: every
//    DosPacket is rung in through this unit's doorbell register in the
//    board window; the emulator implements the ACTION_* semantics against
//    the host filesystem and fills dp_Res1/dp_Res2 before the write
//    completes. Each unit has its own register bank, so handler processes
//    never synchronize with each other.
//
//  - clipboard_main(): the host <-> guest clipboard bridge, run by the
//    handler process of the mount-table entry of kind MOUNT_KIND_CLIPBOARD
//    (the HOSTCLIP DOS device, which exists only to have DOS start this
//    process at mount time). It opens clipboard.device unit 0, registers a
//    CBD_CHANGEHOOK hook (V36+; polled by clip ID on V34) to learn of
//    guest clips and pushes their raw IFF stream to the host through the
//    G2H window, and installs an INTB_PORTS server so the board's doorbell
//    interrupt can wake it when the host stages new text, which it writes
//    into clipboard.device as an IFF FTXT stream straight out of the H2G
//    window. Generation counters and clip IDs keep the two directions from
//    echoing each other. clipboard.device is disk-based on Kickstart 1.3
//    (and 3.1), so the open is retried on a backing-off timer; if it never
//    appears the process idles harmlessly on its (unused) packet port.
//
// The ROM must stay position-independent (compiled with -mpcrel) and free
// of data/bss sections; the Makefile fails the build if the linked
// executable contains relocations or data/bss hunks.

#include <exec/execbase.h>
#include <exec/interrupts.h>
#include <exec/io.h>
#include <exec/memory.h>
#include <exec/ports.h>
#include <exec/types.h>

#include <devices/clipboard.h>
#include <devices/timer.h>

#include <hardware/intbits.h>

#include <utility/hooks.h>

#include <dos/dos.h>
#include <dos/dosextens.h>
#include <dos/filehandler.h>

#include <libraries/configvars.h>
#include <libraries/expansion.h>
#include <libraries/expansionbase.h>

// __NOLIBBASE__: the sfdc 1.12 inline headers (amiga-gcc 16.2 image)
// define the ...Tags varargs wrappers as file-scope static functions that
// name DOS_BASE_NAME directly, which cannot resolve to a function-local
// base; with __NOLIBBASE__ they take the base as an explicit argument at
// the call site instead, where the local is in scope.
#define __NOLIBBASE__
#define EXEC_BASE_NAME _sysbase
#define EXPANSION_BASE_NAME _expbase
#define DOS_BASE_NAME _dosbase
#include <inline/dos.h>
#include <inline/exec.h>
#include <inline/expansion.h>

#include "copperline_board.h"

// The pump itself needs well under 200 bytes, but this is dn_StackSize for
// the whole handler process, and V34 dos.library burns ~1500 bytes of BCPL
// environment at the stack bottom per DOS call -- the 1.3 *boot*-time
// process bootstrap runs enough BCPL frames that a 2K stack overflows
// before the handler's first instruction, crashing through a wild pointer.
// 6000 is the WinUAE boot ROM's proven value for C-style handlers booting
// under 1.3, and it was enough under AROS only while AROS's dos.library
// floored every process at 16 KB: once AROS (master from 2026-09-01,
// upstream commit e9c4ecde99) started honouring the requested size, a
// 6000-byte handler process overflowed during the staged --run boot (the
// program never started, the boot process later warm-rebooted). AROS's
// C dos.library needs far more per call than 1.3's BCPL frames, so ask for
// 16 KB outright: the memory is per mount, from any RAM, and cheap.
#define HANDLER_STACK 16384

// AbsExecBase. A plain *(struct ExecBase **)4 works too (a constant address
// needs no relocation) but trips GCC's array-bounds warning, which treats any
// dereference near address 0 as a null-pointer bug; the asm hides it and
// move.l 4.w is the canonical instruction anyway.
static struct ExecBase *sysbase(void)
{
    struct ExecBase *base;
    __asm("move.l 4.w,%0" : "=r"(base));
    return base;
}

// This unit's register bank in the board window. All registers are
// longwords; volatile, because writes have host side effects and reads
// return what the host latched.
struct HostRegs {
    volatile ULONG dospkt;  // +0x00 write: DosPacket APTR (the doorbell)
    ULONG pad0[3];
    volatile ULONG msgport; // +0x10 write: our MsgPort (0 = exiting)
    ULONG pad1[3];
    volatile ULONG result;  // +0x20 read: RES_* verb
    ULONG pad2[3];
    volatile ULONG arg;     // +0x30 read: volume node for the verb
    ULONG pad3[3];
};
_Static_assert(sizeof(struct HostRegs) == REG_BANK_SIZE,
               "HostRegs must cover exactly one register bank");

// The clipboard unit's bank: the pump registers, then the bridge's own.
struct ClipRegs {
    struct HostRegs pump;    // +0x00 .. +0x3F
    volatile ULONG ctrl;     // +0x40 write: CLIP_CTRL_* verb
    ULONG pad4[3];
    volatile ULONG offset;   // +0x50 write: FETCH/PUSH byte offset
    ULONG pad5[3];
    volatile ULONG len;      // +0x60 FETCH: read chunk length; PUSH: write
    ULONG pad6[3];
    volatile ULONG total;    // +0x70 read: staged host text length
    ULONG pad7[3];
    volatile ULONG status;   // +0x80 read: CLIP_ST_* bits
    ULONG pad8[3];
    volatile ULONG hostgen;  // +0x90 read: newest host text generation
    ULONG pad9[3];
    volatile ULONG guestgen; // +0xA0 write: generation written to the clip
    ULONG padA[3];
    volatile ULONG stagedgen; // +0xB0 read: generation staged by FETCH 0
    ULONG padB[3];
};
_Static_assert(__builtin_offsetof(struct ClipRegs, ctrl) == CLIP_REG_CTRL &&
                   __builtin_offsetof(struct ClipRegs, offset) == CLIP_REG_OFFSET &&
                   __builtin_offsetof(struct ClipRegs, len) == CLIP_REG_LEN &&
                   __builtin_offsetof(struct ClipRegs, total) == CLIP_REG_TOTAL &&
                   __builtin_offsetof(struct ClipRegs, status) == CLIP_REG_STATUS &&
                   __builtin_offsetof(struct ClipRegs, hostgen) == CLIP_REG_HOSTGEN &&
                   __builtin_offsetof(struct ClipRegs, guestgen) == CLIP_REG_GUESTGEN &&
                   __builtin_offsetof(struct ClipRegs, stagedgen) == CLIP_REG_STAGEDGEN,
               "ClipRegs must match the CLIP_REG_* layout");

// Shared with the assembly callbacks in entry.s (their offsets are fixed
// there): the INT2 server and the clipboard hook only ever touch this.
struct ClipShared {
    struct ClipRegs *regs;  // +0
    struct Task *task;      // +4
    ULONG irq_sigmask;      // +8
    ULONG hook_sigmask;     // +12
    LONG hook_clip_id;      // +16: chm_ClipID of the newest change
};
_Static_assert(__builtin_offsetof(struct ClipShared, hook_clip_id) == 16,
               "entry.s reads ClipShared at fixed offsets");

// entry.s callbacks, reached through PC-relative code references only.
extern void clip_int_server(void);
extern void clip_hook(void);

// The bridge's whole state, in one MEMF_PUBLIC allocation: the ROM has no
// data section, and the hook and interrupt server read it from foreign
// contexts.
struct ClipState {
    struct ClipShared shared;
    struct Hook hook;
    struct Interrupt server;
    struct MsgPort cport; // clipboard.device replies
    struct MsgPort tport; // timer.device replies
    struct IOClipReq creq;
    struct timerequest treq;
    UBYTE cb_open;    // clipboard.device is open
    UBYTE has_hook;   // CBD_CHANGEHOOK installed (else poll by clip ID)
    UBYTE open_tries; // OpenDevice attempts so far
    UBYTE pad;
    LONG own_id;    // io_ClipID of the bridge's own last write
    LONG seen_id;   // newest clip ID pushed to the host
    ULONG seen_gen; // newest host generation written into the clip
};

#define IFF_ID(a, b, c, d) \
    (((ULONG)(a) << 24) | ((ULONG)(b) << 16) | ((ULONG)(c) << 8) | (ULONG)(d))

// Give up opening clipboard.device after this many attempts (2, 4, 8, ...
// 64 s apart): on a disk-based Kickstart each attempt walks DEVS: on the
// boot volume, which on a floppy system is a drive access.
#define CLIP_OPEN_TRIES 8

// The 1.3 DosList: dos.library V34 has no AddDosEntry/RemDosEntry, and its
// list has no semaphore -- the convention is Forbid() around a splice of
// the BPTR-linked di_DevInfo chain hanging off the RootNode.
static LONG *devinfo_head(struct Library *dosbase)
{
    struct RootNode *root = ((struct DosLibrary *)dosbase)->dl_Root;
    struct DosInfo *info = BADDR(root->rn_Info);
    return (LONG *)&info->di_DevInfo;
}

static void add_dos_entry_v34(struct ExecBase *_sysbase,
                              struct Library *dosbase, struct DosList *vol)
{
    LONG *head = devinfo_head(dosbase);
    Forbid();
    vol->dol_Next = *head;
    *head = MKBADDR(vol);
    Permit();
}

static void rem_dos_entry_v34(struct ExecBase *_sysbase,
                              struct Library *dosbase, struct DosList *vol)
{
    LONG *prev = devinfo_head(dosbase);
    Forbid();
    while (*prev != 0) {
        struct DosList *node = BADDR(*prev);
        if (node == vol) {
            *prev = node->dol_Next;
            break;
        }
        prev = (LONG *)&node->dol_Next;
    }
    Permit();
}

// Ring `pkt` in through `regs`' doorbell and reply it unless the host
// keeps it; returns the RES_* verb, with the verb's node in `*vol`.
static ULONG pump_packet(struct ExecBase *_sysbase, struct HostRegs *regs,
                         struct MsgPort *port, struct DosPacket *pkt,
                         struct DosList **vol)
{
    // The doorbell: the host handles the packet within the write,
    // filling dp_Res1/dp_Res2 and latching result/arg.
    regs->dospkt = (ULONG)pkt;
    ULONG res = regs->result;
    *vol = (struct DosList *)regs->arg;
    if (res != RES_NOREPLY) {
        struct MsgPort *reply = pkt->dp_Port;
        pkt->dp_Port = port;
        PutMsg(reply, pkt->dp_Link);
    }
    return res;
}

static void clipboard_main(struct ExecBase *_sysbase, UBYTE *board,
                           struct ClipRegs *regs, struct MsgPort *port);

void handler_main(void)
{
    struct ExecBase *_sysbase = sysbase();
    // V34 (Kickstart 1.3) is the supported floor: below 36 the volume
    // add/remove falls back to the V34 conventions, and 1.2's V33 never
    // reaches us anyway (no expansion diag-ROM hook to mount with).
    struct Library *_dosbase = OpenLibrary((STRPTR) "dos.library", 34);
    struct Process *me = (struct Process *)FindTask(NULL);
    struct MsgPort *port = &me->pr_MsgPort;
    struct HostRegs *regs = NULL;
    UBYTE *board = NULL;
    UBYTE kind = MOUNT_KIND_FILESYS;

    for (;;) {
        WaitPort(port);
        struct Message *msg;
        while ((msg = GetMsg(port)) != NULL) {
            struct DosPacket *pkt = (struct DosPacket *)msg->mn_Node.ln_Name;
            // The first packet is the startup packet. On V36+ and on every
            // deferred (first-reference) start it is ACTION_STARTUP
            // (== ACTION_NIL == 0) with dp_Arg3 = our DeviceNode: dn_SegList
            // points back into the board window (board + 4), locating the
            // board; dn_Startup's FileSysStartupMsg holds our mount unit,
            // selecting our register bank. Kickstart 1.3's *boot*-path
            // startup is different: V34 dos init reuses the packet for its
            // BCPL process parameters (dp_Type = our seglist, dp_Res1 = the
            // stack size), dp_Arg1 is garbage and dp_Arg3 is NULL -- only
            // dp_Arg2, the FileSysStartupMsg, "works as documented" (the
            // same shape WinUAE's filesys handles, with the same words).
            // The FSSM lives inside the board window at a fixed offset, so
            // it alone locates both the board and the unit. Introduce
            // ourselves to the host by writing our MsgPort, once.
            if (regs == NULL) {
                struct DeviceNode *dn = BADDR(pkt->dp_Arg3);
                struct FileSysStartupMsg *fssm;
                if (dn != NULL) {
                    fssm = BADDR(dn->dn_Startup);
                    board = (UBYTE *)BADDR(dn->dn_SegList) - 4;
                } else {
                    fssm = BADDR(pkt->dp_Arg2);
                    board = (UBYTE *)fssm - FSSM_OFFSET -
                            fssm->fssm_Unit * FSSM_SLOT_SIZE;
                }
                // The mount table entry's kind byte says which bank this
                // unit talks to: a HOSTFS mount owns the bank of its unit
                // number; the clipboard entry has a bank of its own.
                kind = board[MOUNTS_OFFSET + 2 +
                             fssm->fssm_Unit * MOUNT_ENTRY_SIZE +
                             MOUNT_KIND_OFFSET];
                if (kind == MOUNT_KIND_CLIPBOARD)
                    regs = (struct HostRegs *)(board + CLIP_REGS_OFFSET);
                else
                    regs = (struct HostRegs *)(board + REGS_OFFSET) +
                           fssm->fssm_Unit;
                regs->msgport = (ULONG)port;
            }
            struct DosList *vol;
            ULONG res = pump_packet(_sysbase, regs, port, pkt, &vol);
            if (kind == MOUNT_KIND_CLIPBOARD) {
                // The startup packet is answered; the rest of this
                // process's life is the clipboard bridge (never returns).
                if (_dosbase != NULL)
                    CloseLibrary(_dosbase);
                clipboard_main(_sysbase, board, (struct ClipRegs *)regs, port);
            }
            // After replying, so DOS is not blocked on us while we take
            // the DosList semaphore (V36+) or Forbid (V34).
            if (res == RES_ADDVOLUME && _dosbase != NULL) {
                if (_dosbase->lib_Version >= 36)
                    AddDosEntry(vol);
                else
                    add_dos_entry_v34(_sysbase, _dosbase, vol);
            } else if (res == RES_DIE) {
                // ACTION_DIE: the emulator already cleared dn_Task and
                // dropped this unit's state. Take the volume off the
                // DosList (AddDosEntry locks internally, RemDosEntry
                // does not), tell the host the unit is going dark, and
                // end the process.
                if (vol != NULL && _dosbase != NULL) {
                    if (_dosbase->lib_Version >= 36) {
                        LockDosList(LDF_VOLUMES | LDF_WRITE);
                        RemDosEntry(vol);
                        UnLockDosList(LDF_VOLUMES | LDF_WRITE);
                    } else {
                        rem_dos_entry_v34(_sysbase, _dosbase, vol);
                    }
                }
                regs->msgport = 0;
                if (_dosbase != NULL)
                    CloseLibrary(_dosbase);
                return;
            }
        }
    }
}

// ---- Clipboard bridge -------------------------------------------------

// A MsgPort by hand: CreateMsgPort() is V36+, and the ROM links no
// amiga.lib for CreatePort().
static void init_port(struct MsgPort *port, struct Task *task, BYTE sigbit)
{
    port->mp_Node.ln_Type = NT_MSGPORT;
    port->mp_Flags = PA_SIGNAL;
    port->mp_SigBit = sigbit;
    port->mp_SigTask = task;
    port->mp_MsgList.lh_Head = (struct Node *)&port->mp_MsgList.lh_Tail;
    port->mp_MsgList.lh_Tail = NULL;
    port->mp_MsgList.lh_TailPred = (struct Node *)&port->mp_MsgList.lh_Head;
}

static void start_timer(struct ExecBase *_sysbase, struct ClipState *st,
                        ULONG secs)
{
    st->treq.tr_node.io_Command = TR_ADDREQUEST;
    st->treq.tr_time.tv_secs = secs;
    st->treq.tr_time.tv_micro = 0;
    SendIO((struct IORequest *)&st->treq);
}

static BYTE clip_write(struct ExecBase *_sysbase, struct ClipState *st,
                       APTR data, ULONG len)
{
    st->creq.io_Command = CMD_WRITE;
    st->creq.io_Data = data;
    st->creq.io_Length = len;
    return DoIO((struct IORequest *)&st->creq);
}

// Host -> guest: for every host generation not yet written, pull the
// staged text through the H2G window and write it into clipboard.device
// as FORM FTXT { CHRS }, chunk by chunk (the device advances io_Offset by
// io_Actual after each CMD_WRITE, and the window itself is honest memory,
// so io_Data points straight into it). The write's own clip ID is
// remembered so the resulting change hook is not echoed back to the host.
static void fetch_host_text(struct ExecBase *_sysbase, struct ClipState *st,
                            UBYTE *board)
{
    struct ClipRegs *regs = st->shared.regs;
    struct IOClipReq *req = &st->creq;
    if (!st->cb_open)
        return;
    while (regs->hostgen != st->seen_gen) {
        regs->offset = 0;
        regs->ctrl = CLIP_CTRL_FETCH;
        ULONG gen = regs->stagedgen;
        ULONG total = regs->total;
        ULONG len = regs->len;
        // Marked seen up front so a failing device cannot spin this loop.
        st->seen_gen = gen;
        if (total == 0 || len == 0) {
            regs->guestgen = gen;
            continue;
        }
        ULONG hdr[5];
        hdr[0] = IFF_ID('F', 'O', 'R', 'M');
        hdr[1] = 4 + 8 + total + (total & 1);
        hdr[2] = IFF_ID('F', 'T', 'X', 'T');
        hdr[3] = IFF_ID('C', 'H', 'R', 'S');
        hdr[4] = total;
        req->io_Offset = 0;
        req->io_ClipID = 0;
        req->io_Error = 0;
        if (clip_write(_sysbase, st, hdr, sizeof(hdr)) != 0)
            continue;
        st->own_id = req->io_ClipID;
        ULONG off = 0;
        BOOL ok = TRUE;
        for (;;) {
            if (clip_write(_sysbase, st, board + CLIP_H2G_OFFSET, len) != 0) {
                ok = FALSE;
                break;
            }
            off += len;
            if (off >= total)
                break;
            regs->offset = off;
            regs->ctrl = CLIP_CTRL_FETCH;
            len = regs->len;
            if (len == 0)
                break;
        }
        if (ok && (total & 1)) {
            UBYTE padbyte = 0;
            clip_write(_sysbase, st, &padbyte, 1);
        }
        req->io_Command = CMD_UPDATE;
        DoIO((struct IORequest *)req);
        st->own_id = req->io_ClipID;
        regs->guestgen = gen;
    }
}

// Guest -> host: read the current clip in window-sized chunks straight
// into the G2H window and PUSH each to the host; the read cycle ends
// (releasing the clip) when a read returns io_Actual == 0, and only then
// is the stream COMMITted -- the host parses the IFF and ignores anything
// that is not FTXT. The first read reveals the clip's ID: one already
// pushed, or the bridge's own write, is drained without pushing (a read
// cycle once begun must be completed, or writers block on the clip). A
// push abandoned mid-way is simply overwritten by the next one, which
// restarts at offset 0.
static void push_guest_clip(struct ExecBase *_sysbase, struct ClipState *st,
                            UBYTE *board)
{
    struct ClipRegs *regs = st->shared.regs;
    struct IOClipReq *req = &st->creq;
    req->io_Offset = 0;
    req->io_ClipID = 0;
    req->io_Error = 0;
    ULONG off = 0;
    BOOL skip = FALSE;
    for (;;) {
        req->io_Command = CMD_READ;
        req->io_Data = (STRPTR)(board + CLIP_G2H_OFFSET);
        req->io_Length = CLIP_CHUNK_SIZE;
        if (DoIO((struct IORequest *)req) != 0)
            return;
        if (off == 0) {
            LONG id = req->io_ClipID;
            skip = id == 0 || id == st->seen_id || id == st->own_id;
        }
        ULONG n = req->io_Actual;
        if (n == 0)
            break;
        if (!skip) {
            regs->offset = off;
            regs->len = n;
            regs->ctrl = CLIP_CTRL_PUSH;
        }
        off += n;
    }
    if (skip)
        return;
    st->seen_id = req->io_ClipID;
    if (off != 0)
        regs->ctrl = CLIP_CTRL_COMMIT;
}

static void try_open_clipboard(struct ExecBase *_sysbase, struct ClipState *st,
                               UBYTE *board)
{
    st->open_tries++;
    if (OpenDevice((STRPTR) "clipboard.device", PRIMARY_CLIP,
                   (struct IORequest *)&st->creq, 0) != 0)
        return;
    st->cb_open = 1;
    if (st->creq.io_Device->dd_Library.lib_Version >= 36) {
        st->hook.h_Entry = (ULONG(*)())clip_hook;
        st->hook.h_Data = &st->shared;
        st->creq.io_Command = CBD_CHANGEHOOK;
        st->creq.io_Data = (STRPTR)&st->hook;
        st->creq.io_Length = 1; // install
        if (DoIO((struct IORequest *)&st->creq) == 0)
            st->has_hook = 1;
    }
    st->server.is_Node.ln_Type = NT_INTERRUPT;
    st->server.is_Node.ln_Pri = 0;
    st->server.is_Node.ln_Name = (char *)"Copperline clipboard";
    st->server.is_Data = &st->shared;
    st->server.is_Code = clip_int_server;
    AddIntServer(INTB_PORTS, &st->server);
    st->shared.regs->ctrl = CLIP_CTRL_ENABLE;
    // Text the host staged before the device came up is still waiting,
    // and so may be a clip the guest posted before the hook existed.
    fetch_host_text(_sysbase, st, board);
    push_guest_clip(_sysbase, st, board);
}

// The clipboard bridge process: see the header comment. Runs forever
// (the HOSTCLIP device is never ACTION_DIEd), pumping any DosPacket that
// does reach it so a stray reference to HOSTCLIP: gets an error rather
// than a hang.
static void clipboard_main(struct ExecBase *_sysbase, UBYTE *board,
                           struct ClipRegs *regs, struct MsgPort *port)
{
    struct Process *me = (struct Process *)FindTask(NULL);
    // No "please insert volume" requester if DEVS: points somewhere
    // unmounted while clipboard.device is looked for.
    me->pr_WindowPtr = (APTR)-1;

    struct ClipState *st = AllocMem(sizeof(*st), MEMF_PUBLIC | MEMF_CLEAR);
    ULONG timer_mask = 0, bridge_mask = 0;
    if (st != NULL) {
        st->shared.regs = regs;
        st->shared.task = &me->pr_Task;
        BYTE irq_sig = AllocSignal(-1);
        BYTE hook_sig = AllocSignal(-1);
        BYTE c_sig = AllocSignal(-1);
        BYTE t_sig = AllocSignal(-1);
        if (irq_sig >= 0 && hook_sig >= 0 && c_sig >= 0 && t_sig >= 0) {
            st->shared.irq_sigmask = 1UL << irq_sig;
            st->shared.hook_sigmask = 1UL << hook_sig;
            init_port(&st->cport, &me->pr_Task, c_sig);
            init_port(&st->tport, &me->pr_Task, t_sig);
            st->creq.io_Message.mn_ReplyPort = &st->cport;
            st->creq.io_Message.mn_Length = sizeof(st->creq);
            st->treq.tr_node.io_Message.mn_ReplyPort = &st->tport;
            st->treq.tr_node.io_Message.mn_Length = sizeof(st->treq);
            if (OpenDevice((STRPTR) "timer.device", UNIT_VBLANK,
                           (struct IORequest *)&st->treq, 0) == 0) {
                timer_mask = 1UL << t_sig;
                bridge_mask = st->shared.irq_sigmask | st->shared.hook_sigmask;
                start_timer(_sysbase, st, 2);
            }
        }
    }

    ULONG port_mask = 1UL << port->mp_SigBit;
    for (;;) {
        ULONG sigs = Wait(port_mask | timer_mask | bridge_mask);
        if (sigs & port_mask) {
            struct Message *msg;
            while ((msg = GetMsg(port)) != NULL) {
                struct DosList *vol;
                pump_packet(_sysbase, &regs->pump, port,
                            (struct DosPacket *)msg->mn_Node.ln_Name, &vol);
            }
        }
        if (sigs & timer_mask) {
            while (GetMsg(&st->tport) != NULL)
                ;
            if (st->cb_open) {
                // V34 (no change hook): look for a new clip by reading it
                // -- a full read cycle, which push_guest_clip drains
                // without pushing when the clip is one it has seen. With
                // a hook installed the timer has nothing left to do.
                if (!st->has_hook) {
                    push_guest_clip(_sysbase, st, board);
                    start_timer(_sysbase, st, 2);
                }
            } else if (st->open_tries < CLIP_OPEN_TRIES) {
                try_open_clipboard(_sysbase, st, board);
                if (st->cb_open)
                    start_timer(_sysbase, st, 1);
                else
                    start_timer(_sysbase, st,
                                st->open_tries < 6 ? 2UL << st->open_tries
                                                   : 64);
            }
        }
        if (sigs & bridge_mask) {
            if (sigs & st->shared.irq_sigmask)
                fetch_host_text(_sysbase, st, board);
            if ((sigs & st->shared.hook_sigmask) &&
                st->shared.hook_clip_id != st->own_id)
                push_guest_clip(_sysbase, st, board);
        }
    }
}

static void mount_boards(UBYTE *board, struct Library *_expbase,
                         struct ConfigDev *cd)
{
    struct ExecBase *_sysbase = sysbase();
    // The host writes count into the mount table and bounds it (board_image).
    UWORD count = *(UWORD *)(board + MOUNTS_OFFSET);

    for (UWORD i = 0; i < count; i++) {
        const UBYTE *name =
            board + MOUNTS_OFFSET + 2 + (ULONG)i * MOUNT_ENTRY_SIZE;
        ULONG len = 0;
        while (name[len] != '\0' && len < MOUNT_ENTRY_SIZE - 1)
            len++;

        // DeviceNode and its BSTR name in one public allocation.
        struct DeviceNode *dn = AllocMem(sizeof(*dn) + 1 + len + 1,
                                         MEMF_PUBLIC | MEMF_CLEAR);
        if (dn == NULL)
            break;
        UBYTE *bname = (UBYTE *)(dn + 1);
        bname[0] = len;
        for (ULONG c = 0; c < len; c++)
            bname[1 + c] = name[c];

        dn->dn_Type = DLT_DEVICE;
        dn->dn_StackSize = HANDLER_STACK;
        dn->dn_Priority = 10;
        // The emulator prepared one FileSysStartupMsg (with a per-unit
        // DosEnvec) per mount at expansion init; the boot menu displays it,
        // and the emulator reads the unit back from fssm_Unit at
        // ACTION_STARTUP.
        dn->dn_Startup = MKBADDR(board + FSSM_OFFSET + i * FSSM_SLOT_SIZE);
        dn->dn_SegList = MKBADDR(board + 4);
        dn->dn_GlobalVec = -1; // C handler: no BCPL global vector
        // A valid BSTR handler name: normally cosmetic (the handler is
        // seglist-resident, nothing LoadSegs this), but WinUAE's boot ROM
        // sets it for its Kickstart-1.3-bootable virtual drives, and 1.3's
        // BCPL boot init dereferences DeviceNode fields it finds along the
        // way -- a NULL BPTR walks address 0.
        dn->dn_Handler = MKBADDR(board + FSSM_DEVNAME_OFFSET);
        dn->dn_Name = MKBADDR(bname);

        // Boot priority comes from the config via de_BootPri; the default
        // -128 mounts at DOS init but is never a boot candidate.
        // ADNF_STARTPROC: start the handler process at mount time rather
        // than on first reference, so problems surface at boot.
        struct FileSysStartupMsg *fssm = BADDR(dn->dn_Startup);
        struct DosEnvec *env = BADDR(fssm->fssm_Environ);
        BYTE pri = (BYTE)env->de_BootPri;
        if (_expbase->lib_Version >= 36) {
            AddBootNode(pri, ADNF_STARTPROC, dn, cd);
        } else if (pri == -128) {
            // AddBootNode is V36+. V34's AddDosNode (V33+) queues the
            // node on eb_MountList too, so V34 dos.library mounts it at
            // init, but takes no ConfigDev -- strap cannot trace such a
            // node back to a boot ROM, which is exactly right for the
            // never-boot priority.
            AddDosNode(pri, ADNF_STARTPROC, dn);
        } else {
            // Bootable mount on V34: hand-build the BootNode AddBootNode
            // would have built (the documented 1.3 autoboot recipe, per
            // the A2091's V34 ROM). ln_Name carries the ConfigDev; strap
            // takes the highest-priority valid BootNode, follows the
            // ConfigDev to our DiagArea, and calls its da_BootPoint
            // (entry.s), which fires dos.library's init; DOS then mounts
            // the eb_MountList nodes and roots SYS: on the boot winner.
            // A bootable floppy competes as a strap-enqueued BootNode at
            // priority 5, so bootpri composes with DF0: as on V36+.
            struct BootNode *bn =
                AllocMem(sizeof(*bn), MEMF_PUBLIC | MEMF_CLEAR);
            if (bn == NULL) {
                AddDosNode(pri, ADNF_STARTPROC, dn);
                continue;
            }
            bn->bn_Node.ln_Type = NT_BOOTNODE;
            bn->bn_Node.ln_Pri = pri;
            bn->bn_Node.ln_Name = (char *)cd;
            // bn_Flags stays 0 (MEMF_CLEAR): the RKRM documents it as
            // unused and expected NULL, and both UAE's equivalent
            // hand-built BootNode (filesys.asm) and the A590 SCSI boot
            // ROM's real firmware leave it untouched too.
            bn->bn_DeviceNode = dn;
            Forbid();
            Enqueue(&((struct ExpansionBase *)_expbase)->MountList,
                    &bn->bn_Node);
            Permit();
        }
    }
}

// rt_Init of the Romtag entry.s's DiagPoint patches into the diag copy,
// reached through entry.s's local trampoline (see the -mpcrel note there).
// Kickstart's cold-start resident scan calls this with D0=0, A0=NULL
// segList, A6=ExecBase -- none of DiagPoint's own registers, so the
// ConfigDev has to be re-obtained via GetCurrentBinding(), which the
// resident-scan machinery sets as the current binding for exactly this
// call (RKRM Libraries, "Events At ROMTAG INIT Time").
void resident_init(void)
{
    struct ExecBase *_sysbase = sysbase();
    struct Library *_expbase = OpenLibrary((STRPTR) "expansion.library", 0);
    if (_expbase == NULL)
        return;

    struct CurrentBinding cb;
    GetCurrentBinding(&cb, sizeof(cb));
    struct ConfigDev *cd = cb.cb_ConfigDev;
    if (cd != NULL)
        mount_boards((UBYTE *)cd->cd_BoardAddr, _expbase, cd);

    CloseLibrary(_expbase);
}
