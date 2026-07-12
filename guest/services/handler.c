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
//  - handler_main(): the DOS handler process. A pure packet pump: every
//    DosPacket is handed to the emulator with one reserved A-line opcode
//    (TRAP_PACKET); the emulator implements the ACTION_* semantics against
//    the host filesystem and fills dp_Res1/dp_Res2 before the trap returns.
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

#define HANDLER_STACK 8192

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

// Hand the packet to the emulator, which fills dp_Res1/dp_Res2. Returns a
// TRAP_RES_* code; for TRAP_RES_ADDVOLUME the host also returns the volume
// DosList node it built in *vol (via A0).
static ULONG trap_packet(struct DosPacket *pkt, struct MsgPort *port,
                         struct DosList **vol)
{
    register ULONG res __asm("d0");
    register struct DosList *_vol __asm("a0");
    register struct DosPacket *_pkt __asm("d1") = pkt;
    register struct MsgPort *_port __asm("a1") = port;
    __asm volatile(".short 0xA402" // TRAP_PACKET
                   : "=r"(res), "=r"(_vol), "+r"(_pkt), "+r"(_port)
                   :
                   : "cc", "memory");
    *vol = _vol;
    return res;
}

void handler_main(void)
{
    struct ExecBase *_sysbase = sysbase();
    struct Library *_dosbase = OpenLibrary((STRPTR) "dos.library", 36);
    struct Process *me = (struct Process *)FindTask(NULL);
    struct MsgPort *port = &me->pr_MsgPort;

    for (;;) {
        WaitPort(port);
        struct Message *msg;
        while ((msg = GetMsg(port)) != NULL) {
            struct DosPacket *pkt = (struct DosPacket *)msg->mn_Node.ln_Name;
            struct DosList *vol;
            ULONG res = trap_packet(pkt, port, &vol);
            if (res != TRAP_RES_NOREPLY) {
                struct MsgPort *reply = pkt->dp_Port;
                pkt->dp_Port = port;
                PutMsg(reply, pkt->dp_Link);
            }
            // After replying, so DOS is not blocked on us while we take
            // the DosList semaphore.
            if (res == TRAP_RES_ADDVOLUME && _dosbase != NULL)
                AddDosEntry(vol);
            else if (res == TRAP_RES_DIE) {
                // ACTION_DIE: the emulator already cleared dn_Task and
                // dropped this unit's state. Take the volume off the
                // DosList (AddDosEntry locks internally, RemDosEntry
                // does not) and end the process.
                if (vol != NULL && _dosbase != NULL) {
                    LockDosList(LDF_VOLUMES | LDF_WRITE);
                    RemDosEntry(vol);
                    UnLockDosList(LDF_VOLUMES | LDF_WRITE);
                }
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
    UWORD count = *(UWORD *)(board + MOUNTS_OFFSET);

    if (count > MOUNT_MAX_COUNT)
        count = MOUNT_MAX_COUNT;
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
        struct FileSysStartupMsg *fssm = BADDR(dn->dn_Startup);
        struct DosEnvec *env = BADDR(fssm->fssm_Environ);
        AddBootNode((BYTE)env->de_BootPri, ADNF_STARTPROC, dn, cd);
    }
}
