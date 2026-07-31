// SPDX-License-Identifier: GPL-3.0-or-later
//
// Guest-side handler for Copperline's services board.
//
// Two entry points (see entry.s and copperline_board.h):
//
//  - mount_boards(): called at expansion init from the board's DiagArea with
//    the documented DiagPoint context (ExpansionBase, ConfigDev). For each
//    entry in the mount table the emulator wrote into the board window, it
//    builds a DeviceNode whose dn_SegList points back into this ROM and
//    adds it to the mount list. DOS mounts the nodes at boot; the handler
//    process is started on first reference.
//
// Kickstart 1.3 (V34) is supported for mounting: both entry points probe
// the library versions at runtime and fall back from the V36+ calls
// (AddBootNode, AddDosEntry/RemDosEntry/LockDosList) to their 1.3-era
// equivalents (AddDosNode, a Forbid-protected splice of the DosInfo
// device chain). Booting *from* a hostfs volume still needs 2.0+: the
// bootpri vote rides on the V36 BootNode strap.
//
//  - handler_main(): the DOS handler process. A pure packet pump: every
//    DosPacket is rung in through this unit's doorbell register in the
//    board window; the emulator implements the ACTION_* semantics against
//    the host filesystem and fills dp_Res1/dp_Res2 before the write
//    completes. Each unit has its own register bank, so handler processes
//    never synchronize with each other.
//
// The ROM must stay position-independent (compiled with -mpcrel) and free
// of data/bss sections; the Makefile fails the build if the linked
// executable contains relocations or data/bss hunks.

#include <exec/execbase.h>
#include <exec/memory.h>
#include <exec/ports.h>
#include <exec/types.h>

#include <dos/dos.h>
#include <dos/dosextens.h>
#include <dos/filehandler.h>

#include <libraries/configvars.h>
#include <libraries/expansion.h>

#define EXEC_BASE_NAME _sysbase
#define EXPANSION_BASE_NAME _expbase
#define DOS_BASE_NAME _dosbase
#include <inline/dos.h>
#include <inline/exec.h>
#include <inline/expansion.h>

#include "copperline_board.h"

// The pump itself needs well under 200 bytes; 2K leaves headroom for the
// OS calls and a future printf().
#define HANDLER_STACK 2048

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

    for (;;) {
        WaitPort(port);
        struct Message *msg;
        while ((msg = GetMsg(port)) != NULL) {
            struct DosPacket *pkt = (struct DosPacket *)msg->mn_Node.ln_Name;
            // The first packet is ACTION_STARTUP (== ACTION_NIL == 0): dp_Arg3
            // is our DeviceNode. dn_SegList points back into the board window
            // (board + 4), locating the board; dn_Startup's FileSysStartupMsg
            // holds our mount unit, selecting our register bank. Introduce
            // ourselves to the host by writing our MsgPort, once.
            if (regs == NULL) {
                struct DeviceNode *dn = BADDR(pkt->dp_Arg3);
                struct FileSysStartupMsg *fssm = BADDR(dn->dn_Startup);
                UBYTE *board = (UBYTE *)BADDR(dn->dn_SegList) - 4;
                regs = (struct HostRegs *)(board + REGS_OFFSET) +
                       fssm->fssm_Unit;
                regs->msgport = (ULONG)port;
            }
            // The doorbell: the host handles the packet within the write,
            // filling dp_Res1/dp_Res2 and latching result/arg.
            regs->dospkt = (ULONG)pkt;
            ULONG res = regs->result;
            struct DosList *vol = (struct DosList *)regs->arg;
            if (res != RES_NOREPLY) {
                struct MsgPort *reply = pkt->dp_Port;
                pkt->dp_Port = port;
                PutMsg(reply, pkt->dp_Link);
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

void mount_boards(UBYTE *board, struct Library *_expbase, struct ConfigDev *cd)
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
        dn->dn_Name = MKBADDR(bname);

        // Boot priority comes from the config via de_BootPri; the default
        // -128 mounts at DOS init but is never a boot candidate.
        // ADNF_STARTPROC: start the handler process at mount time rather
        // than on first reference, so problems surface at boot.
        // AddBootNode is V36+; on 1.3's V34 expansion.library the same
        // AddDosNode call (V33+) queues the node on eb_MountList, which
        // V34 dos.library binds at init -- mounted, but outside the boot
        // vote, so bootpri is inert under 1.3.
        struct FileSysStartupMsg *fssm = BADDR(dn->dn_Startup);
        struct DosEnvec *env = BADDR(fssm->fssm_Environ);
        if (_expbase->lib_Version >= 36)
            AddBootNode((BYTE)env->de_BootPri, ADNF_STARTPROC, dn, cd);
        else
            AddDosNode((BYTE)env->de_BootPri, ADNF_STARTPROC, dn);
    }
}
