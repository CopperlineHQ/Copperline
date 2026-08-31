/*
 * SPDX-FileCopyrightText: 2026 The Copperline project
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Open CD32 FMV cartridge resident and public device dispatch.
 */

#define __USE_SYSBASE

#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include <exec/devices.h>
#include <exec/errors.h>
#include <exec/execbase.h>
#include <exec/io.h>
#include <exec/libraries.h>
#include <exec/memory.h>
#include <exec/resident.h>
#include <proto/exec.h>

#include "cd32mpeg.h"
#include "player.h"
#include "videocd.h"

struct ExecBase *SysBase;

extern APTR FuncTab[];
static const char device_name[] = CD32MPEG_NAME;
static const char device_id[] = CD32MPEG_NAME " 41.0 (30.08.2026)";
static const char ver_string[] __attribute__((used)) =
    "\0$VER: " CD32MPEG_NAME " 41.0 (30.08.2026)";

static struct CD32MPEGBase *InitDevice(
    struct ExecBase *sysbase __asm("a6"),
    BPTR seglist __asm("a0"),
    struct CD32MPEGBase *base __asm("d0"));
static struct CD32MPEGBase *OpenDeviceVector(
    struct CD32MPEGBase *base __asm("a6"),
    struct IOMPEGReq *request __asm("a1"),
    ULONG unit __asm("d0"),
    ULONG flags __asm("d1"));
static BPTR CloseDeviceVector(
    struct CD32MPEGBase *base __asm("a6"),
    struct IORequest *request __asm("a1"));
static BPTR ExpungeDeviceVector(struct CD32MPEGBase *base __asm("a6"));
static ULONG ExtFuncDeviceVector(void);
static void BeginIOVector(
    struct CD32MPEGBase *base __asm("a6"),
    struct IORequest *request __asm("a1"));
static LONG AbortIOVector(
    struct CD32MPEGBase *base __asm("a6"),
    struct IORequest *request __asm("a1"));

struct InitTable {
    ULONG base_size;
    APTR *function_table;
    APTR data_table;
    APTR init_function;
};

static struct InitTable InitTab = {
    sizeof(struct CD32MPEGBase),
    FuncTab,
    NULL,
    InitDevice,
};

APTR FuncTab[] = {
    OpenDeviceVector,
    CloseDeviceVector,
    ExpungeDeviceVector,
    ExtFuncDeviceVector,
    BeginIOVector,
    AbortIOVector,
    (APTR)-1,
};

struct Resident ROMTag __attribute__((used)) = {
    RTC_MATCHWORD,
    &ROMTag,
    (APTR)(&ROMTag + 1),
    RTF_AUTOINIT,
    CD32MPEG_VERSION,
    NT_DEVICE,
    4,
    (char *)device_name,
    (char *)device_id,
    &InitTab,
};

/* Called through the six-byte JMP in the expansion DiagArea. */
LONG RomDiagEntry(struct ExecBase *sysbase __asm("a6"))
{
    APTR *list;
    APTR *node;
    struct Resident *resident;
    ULONG value;

    SysBase = sysbase;
    if (InitResident(&ROMTag, 0) == NULL)
        return -1;
    if (InitResident(&VideoCDROMTag, 0) == NULL)
        return -1;

    /*
     * Match the classic expansion-ROM splice used by the original module.
     * Replacing the lower-version extended-ROM cdstrap in place is important:
     * InitCode may already be walking this list while expansion diagnostics
     * run. If no same-name resident exists, insert by priority through a
     * three-slot RESLIST_NEXT node so that the in-progress traversal sees it.
     */
    list = (APTR *)sysbase->ResModules;
    while (list && (value = (ULONG)(uintptr_t)*list) != 0) {
        if (value & RESLIST_NEXT) {
            list = (APTR *)(uintptr_t)(value & ~RESLIST_NEXT);
            continue;
        }
        resident = (struct Resident *)(uintptr_t)value;
        if (strcmp(resident->rt_Name, CDStrapROMTag.rt_Name) == 0) {
            if (CDStrapROMTag.rt_Version > resident->rt_Version ||
                (CDStrapROMTag.rt_Version == resident->rt_Version &&
                 CDStrapROMTag.rt_Pri >= resident->rt_Pri)) {
                player_set_fallback(resident->rt_Init);
                *list = &CDStrapROMTag;
            }
            sysbase->KickCheckSum = (APTR)(uintptr_t)SumKickData();
            return 0;
        }
        list++;
    }

    list = (APTR *)sysbase->ResModules;
    while (list) {
        value = (ULONG)(uintptr_t)*list;
        if (value & RESLIST_NEXT) {
            list = (APTR *)(uintptr_t)(value & ~RESLIST_NEXT);
            continue;
        }
        if (value != 0) {
            resident = (struct Resident *)(uintptr_t)value;
            if (CDStrapROMTag.rt_Pri <= resident->rt_Pri) {
                list++;
                continue;
            }
        }
        node = AllocMem(3 * sizeof(*node),
            MEMF_PUBLIC | MEMF_CLEAR | MEMF_REVERSE);
        if (!node)
            return -1;
        node[0] = &CDStrapROMTag;
        node[1] = (APTR)(uintptr_t)value;
        node[2] = (APTR)((ULONG)(uintptr_t)(list + 1) | RESLIST_NEXT);
        *list = (APTR)((ULONG)(uintptr_t)node | RESLIST_NEXT);
        sysbase->KickCheckSum = (APTR)(uintptr_t)SumKickData();
        return 0;
    }
    return -1;
}

static struct CD32MPEGBase *InitDevice(
    struct ExecBase *sysbase __asm("a6"),
    BPTR seglist __asm("a0"),
    struct CD32MPEGBase *base __asm("d0"))
{
    (void)seglist;
    SysBase = sysbase;
    memset((UBYTE *)base + sizeof(base->device), 0,
        sizeof(*base) - sizeof(base->device));
    base->sys_base = sysbase;
    return base;
}

static struct CD32MPEGBase *OpenDeviceVector(
    struct CD32MPEGBase *base __asm("a6"),
    struct IOMPEGReq *request __asm("a1"),
    ULONG unit __asm("d0"),
    ULONG flags __asm("d1"))
{
    (void)flags;
    if (unit != 0 || !fmv_open(base)) {
        request->iomr_Req.io_Error = IOERR_OPENFAIL;
        return NULL;
    }
    base->device.dd_Library.lib_OpenCnt++;
    request->iomr_Req.io_Error = 0;
    request->iomr_Req.io_Unit = (struct Unit *)base;
    return base;
}

static BPTR CloseDeviceVector(
    struct CD32MPEGBase *base __asm("a6"),
    struct IORequest *request __asm("a1"))
{
    if (base->device.dd_Library.lib_OpenCnt != 0)
        base->device.dd_Library.lib_OpenCnt--;
    request->io_Unit = NULL;
    request->io_Device = NULL;
    return 0;
}

static BPTR ExpungeDeviceVector(struct CD32MPEGBase *base __asm("a6"))
{
    (void)base;
    return 0;
}

static ULONG ExtFuncDeviceVector(void)
{
    return 0;
}

static void complete_request(struct IORequest *request)
{
    if (!(request->io_Flags & IOF_QUICK))
        ReplyMsg(&request->io_Message);
}

static void BeginIOVector(
    struct CD32MPEGBase *base __asm("a6"),
    struct IORequest *request __asm("a1"))
{
    struct IOMPEGReq *mpeg = (struct IOMPEGReq *)request;
    struct IOStdReq *io = &mpeg->iomr_Req;

    request->io_Error = 0;
    io->io_Actual = 0;
    mpeg->iomr_MPEGError = 0;
    switch (request->io_Command) {
    case MPEGCMD_GETDEVINFO:
        if (io->io_Data && io->io_Length >= MPEG_DEVICE_INFO_SIZE) {
            struct MPEGDeviceInfo *info = io->io_Data;
            memset(info, 0, sizeof(*info));
            info->mdi_DeviceType = FMV_DEVICE_TYPE;
            strcpy((char *)info->mdi_Name, "CD32 MPEG Module");
            io->io_Actual = MPEG_DEVICE_INFO_SIZE;
        } else {
            request->io_Error = IOERR_BADLENGTH;
        }
        break;
    case MPEGCMD_SETVIDEOPARAMS:
        if (!io->io_Data || io->io_Length < sizeof(struct MPEGVideoParamsSet)) {
            request->io_Error = IOERR_BADLENGTH;
        } else {
            const struct MPEGVideoParamsSet *parameters = io->io_Data;
            fmv_set_visible(base, parameters->mvp_Fade != 0);
        }
        break;
    case MPEGCMD_PLAYLSN:
        fmv_queue_play(base, mpeg);
        return;
    default:
        request->io_Error = IOERR_NOCMD;
        break;
    }

    complete_request(request);
}

static LONG AbortIOVector(
    struct CD32MPEGBase *base __asm("a6"),
    struct IORequest *request __asm("a1"))
{
    (void)base;
    return fmv_abort((struct IOMPEGReq *)request);
}
