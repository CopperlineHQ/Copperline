/* SPDX-License-Identifier: GPL-3.0-or-later */
#ifndef COPPERLINE_CD32MPEG_H
#define COPPERLINE_CD32MPEG_H

#include <exec/devices.h>
#include <exec/interrupts.h>
#include <exec/io.h>
#include <exec/tasks.h>

#define CD32MPEG_NAME "cd32mpeg.device"
#define CD32MPEG_VERSION 41

#define MPEGCMD_GETDEVINFO 15
#define MPEGCMD_SETVIDEOPARAMS 19
#define MPEGCMD_PLAYLSN 21

#define MPEG_DEVICE_INFO_SIZE 264
#define FMV_DEVICE_TYPE 0x006f0000UL
#define FMV_REQUEST_ABORTED 0x80

struct IOMPEGReq {
    struct IOStdReq iomr_Req;
    WORD iomr_MPEGError;
    UWORD iomr_StreamType;
    ULONG iomr_MPEGFlags;
    ULONG iomr_Arg1;
    ULONG iomr_Arg2;
    ULONG iomr_Arg3;
    ULONG iomr_Arg4;
    ULONG iomr_Arg5;
    ULONG iomr_Arg6;
    ULONG iomr_Arg7;
    UWORD iomr_Reserved;
};

struct MPEGDeviceInfo {
    ULONG mdi_Reserved;
    ULONG mdi_DeviceType;
    UBYTE mdi_Name[256];
};

struct CD32MPEGBase {
    struct Device device;
    struct ExecBase *sys_base;
    APTR board_addr;
    struct MsgPort *worker_port;
    struct Task *worker_task;
    struct Interrupt interrupt;
    BOOL initialized;
    BOOL interrupt_installed;
    UBYTE video_byte;
    UBYTE audio_byte;
    BOOL video_byte_pending;
    BOOL audio_byte_pending;
    volatile ULONG video_irqs;
    volatile ULONG audio_irqs;
};

BOOL fmv_open(struct CD32MPEGBase *base);
void fmv_queue_play(struct CD32MPEGBase *base, struct IOMPEGReq *request);
LONG fmv_abort(struct IOMPEGReq *request);

#endif
