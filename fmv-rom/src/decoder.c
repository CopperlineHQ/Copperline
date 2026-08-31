/*
 * SPDX-FileCopyrightText: 2026 The Copperline project
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Clean-room CD32 FMV hardware and CDXL streamer, implemented from the public
 * cd.device/cd32mpeg.device ABIs, decoder datasheets, and the behavior notes
 * in ../NOTES-api.md. No proprietary cartridge code or microcode is present.
 */

#define __USE_SYSBASE

#include <stdint.h>

#include <devices/cd.h>
#include <clib/alib_protos.h>
#include <exec/errors.h>
#include <exec/execbase.h>
#include <exec/memory.h>
#include <hardware/intbits.h>
#include <libraries/configvars.h>
#include <proto/exec.h>
#include <proto/expansion.h>
#include <utility/tagitem.h>

#include "cd32mpeg.h"

extern struct ExecBase *SysBase;

#define FMV_MANUFACTURER 514
#define FMV_PRODUCT 106

#define FMV_IO_OFFSET 0x040000UL
#define FMV_AUDIO_OFFSET 0x050000UL
#define FMV_VIDEO_DATA_OFFSET 0x060000UL
#define FMV_VIDEO_REG_OFFSET 0x070000UL
#define FMV_RAM_OFFSET 0x080000UL

#define L64111_CONTROL1 1
#define L64111_CONTROL2 2
#define L64111_CONTROL3 3
#define L64111_INT1 4
#define L64111_INT2 5
#define L64111_TCR 6
#define L64111_TORH 7
#define L64111_TORL 8

#define CL450_CMEM_CONTROL 0x80
#define CL450_CMEM_DMACTRL 0x84
#define CL450_CPU_CONTROL 0x20
#define CL450_CPU_PC 0x22
#define CL450_CPU_IADDR 0x3e
#define CL450_CPU_IMEM 0x42
#define CL450_CPU_TADDR 0x38
#define CL450_CPU_TMEM 0x46
#define CL450_DRAM_REFCNT 0xac
#define CL450_HOST_CONTROL 0x90
#define CL450_HOST_NEW_CMD 0x56
#define CL450_HOST_RADDR 0x88
#define CL450_HOST_RDATA 0x8c
#define CL450_HOST_SCR2 0x96
#define CL450_VID_CONTROL 0xec
#define CL450_VID_REGDATA 0xee

#define CL450_CMD_SET_THRESHOLD 0x0103
#define CL450_CMD_SET_INTERRUPT_MASK 0x0104
#define CL450_CMD_SET_VIDEO_FORMAT 0x0105
#define CL450_CMD_SET_BORDER 0x0407
#define CL450_CMD_SET_BLANK 0x030f
#define CL450_CMD_PLAY 0x000d

#define FMV_IO_VIDEO_READY 0x0800
#define FMV_IO_VIDEO_IRQ 0x8000
#define FMV_IO_AUDIO_IRQ 0x4000
#define FMV_SECTOR_SIZE 2328
#define CL450_INTERRUPT_MASK 0x0d03
#define L64111_INTERRUPT2_MASK 0x0042

#define CL450_FW_HEADER_OFFSET 0x761c
#define CL450_FW_DATA_OFFSET 0x7680
#define CL450_FW_MAGIC 0xc3c301fdUL
#define FMV_RAM_DRIVER_RESERVE 0x1000UL

static struct CD32MPEGBase *worker_start_base;
static struct Task *worker_start_parent;

static UWORD read_le16(const volatile UBYTE *p)
{
    return (UWORD)p[0] | ((UWORD)p[1] << 8);
}

static ULONG read_le32(const volatile UBYTE *p)
{
    return (ULONG)p[0] | ((ULONG)p[1] << 8) |
        ((ULONG)p[2] << 16) | ((ULONG)p[3] << 24);
}

static UWORD read_be16(const UBYTE *p)
{
    return ((UWORD)p[0] << 8) | p[1];
}

static ULONG read_be32(const volatile UBYTE *p)
{
    return ((ULONG)p[0] << 24) | ((ULONG)p[1] << 16) |
        ((ULONG)p[2] << 8) | p[3];
}

static struct ConfigDev *find_board(void)
{
    struct ExpansionBase *ExpansionBase;
    struct ConfigDev *board = NULL;

    ExpansionBase = (struct ExpansionBase *)OpenLibrary(
        (CONST_STRPTR)"expansion.library", 0);
    if (ExpansionBase) {
        board = FindConfigDev(NULL, FMV_MANUFACTURER, FMV_PRODUCT);
        CloseLibrary((struct Library *)ExpansionBase);
    }
    return board;
}

static void fmv_interrupt(void)
{
    register struct CD32MPEGBase *base __asm("a1");
    volatile UBYTE *board;
    volatile UWORD *io;
    volatile UWORD *audio;
    volatile UWORD *video;
    UWORD status;

    __asm__ volatile ("" : "=r"(base));
    board = base->board_addr;
    if (!board)
        return;
    io = (volatile UWORD *)(board + FMV_IO_OFFSET);
    audio = (volatile UWORD *)(board + FMV_AUDIO_OFFSET);
    video = (volatile UWORD *)(board + FMV_VIDEO_REG_OFFSET);
    status = *io;
    if ((status & FMV_IO_VIDEO_IRQ) == 0) {
        video[CL450_HOST_CONTROL >> 1] |= 0x0080;
        base->video_irqs++;
    }
    if ((status & FMV_IO_AUDIO_IRQ) == 0) {
        (void)audio[L64111_INT1];
        (void)audio[L64111_INT2];
        base->audio_irqs++;
    }
    if (base->worker_task)
        Signal(base->worker_task, SIGF_SINGLE);
}

static void cl450_command(
    volatile UWORD *regs, UWORD command, const UWORD *args, UWORD count)
{
    UWORD i;

    while (regs[CL450_HOST_NEW_CMD >> 1] != 0)
        ;
    regs[CL450_HOST_RADDR >> 1] = 0;
    regs[CL450_HOST_RDATA >> 1] = command;
    for (i = 0; i < count; i++)
        regs[CL450_HOST_RDATA >> 1] = args[i];
    regs[CL450_HOST_NEW_CMD >> 1] = 1;
    while (regs[CL450_HOST_NEW_CMD >> 1] != 0)
        ;
}

static BOOL load_open_firmware(struct CD32MPEGBase *base)
{
    volatile UBYTE *rom = base->board_addr;
    volatile UWORD *regs = (volatile UWORD *)(rom + FMV_VIDEO_REG_OFFSET);
    volatile UWORD *ram = (volatile UWORD *)(rom + FMV_RAM_OFFSET);
    const volatile UBYTE *header = rom + CL450_FW_HEADER_OFFSET;
    const volatile UBYTE *src = rom + CL450_FW_DATA_OFFSET;
    ULONG firmware_base;
    UWORD entry;
    UWORD chunks;
    UWORD chunk;
    ULONG i;

    if (read_be32(header) != CL450_FW_MAGIC)
        return FALSE;
    entry = read_le16(rom + 0x7622);
    firmware_base = read_le16(rom + 0x7624);
    chunks = read_le16(rom + 0x762a);

    regs[CL450_CMEM_CONTROL >> 1] = 0x0043;
    regs[CL450_CMEM_CONTROL >> 1] = 0;
    regs[CL450_CMEM_DMACTRL >> 1] = 0;
    regs[CL450_HOST_SCR2 >> 1] = 0x1de0;
    regs[CL450_DRAM_REFCNT >> 1] = 0x0136;
    regs[CL450_VID_CONTROL >> 1] = 0x000e;
    regs[CL450_VID_REGDATA >> 1] = 1;
    regs[CL450_HOST_RADDR >> 1] = 10;
    regs[CL450_HOST_RDATA >> 1] = 1;

    regs[CL450_CPU_IADDR >> 1] = 0;
    for (i = 0; i < 1024; i++)
        regs[CL450_CPU_IMEM >> 1] = 0;
    regs[CL450_CPU_TADDR >> 1] = 0;
    for (i = 0; i < 128; i++)
        regs[CL450_CPU_TMEM >> 1] = 0;
    for (i = 0; i < ((0x80000 - FMV_RAM_DRIVER_RESERVE) / sizeof(UWORD)); i++)
        ram[i] = 0;

    for (chunk = 0; chunk < chunks; chunk++) {
        ULONG length = read_le32(src);
        ULONG start = read_le32(src + 4);
        ULONG end = start + length;
        BOOL instruction_chunk;

        src += 8;
        if ((start & 3) != 0 || end > 0x80000 - FMV_RAM_DRIVER_RESERVE ||
            (length & 1) != 0)
            return FALSE;
        instruction_chunk = start >= firmware_base &&
            end < firmware_base + 2048;
        if (instruction_chunk)
            regs[CL450_CPU_IADDR >> 1] =
                ((start - firmware_base) >> 1) & 0x03fe;
        while (start < end) {
            UWORD value = ((UWORD)src[0] << 8) | src[1];
            src += 2;
            ram[start >> 1] = value;
            if (instruction_chunk)
                regs[CL450_CPU_IMEM >> 1] = value;
            start += 2;
        }
    }

    regs[CL450_CPU_PC >> 1] = entry & 0x01ff;
    regs[CL450_HOST_RADDR >> 1] = 15;
    regs[CL450_HOST_RDATA >> 1] = 0xffff;
    regs[CL450_CPU_CONTROL >> 1] = 1;
    regs[CL450_HOST_RADDR >> 1] = 15;
    while (regs[CL450_HOST_RDATA >> 1] != 0)
        ;
    regs[CL450_HOST_CONTROL >> 1] = 0x0081;
    regs[CL450_CMEM_DMACTRL >> 1] = 4;
    return TRUE;
}

static BOOL initialize_decoders(struct CD32MPEGBase *base)
{
    volatile UBYTE *board = base->board_addr;
    volatile UWORD *io = (volatile UWORD *)(board + FMV_IO_OFFSET);
    volatile UWORD *audio = (volatile UWORD *)(board + FMV_AUDIO_OFFSET);
    volatile UWORD *video = (volatile UWORD *)(board + FMV_VIDEO_REG_OFFSET);
    UWORD args[4];

    if (!load_open_firmware(base))
        return FALSE;
    args[0] = 0x2000;
    cl450_command(video, CL450_CMD_SET_THRESHOLD, args, 1);
    args[0] = 0;
    args[1] = 0;
    args[2] = 0x0011;
    args[3] = 0x1111;
    cl450_command(video, CL450_CMD_SET_BORDER, args, 4);
    args[0] = 3;
    cl450_command(video, CL450_CMD_SET_VIDEO_FORMAT, args, 1);

    audio[L64111_CONTROL1] = 0x0086;
    audio[L64111_CONTROL1] = 0;
    audio[L64111_CONTROL2] = 0x0011;
    audio[L64111_CONTROL3] = 0;
    audio[L64111_INT1] = 0;
    audio[L64111_INT2] = 0;
    audio[L64111_TCR] = 4;
    audio[L64111_TORH] = 0;
    audio[L64111_TORL] = 0;
    *io = 0x7000;
    audio[L64111_INT1] = 0;
    audio[L64111_INT2] = L64111_INTERRUPT2_MASK;
    audio[L64111_CONTROL2] = 0x0012;
    audio[L64111_CONTROL1] = 1;

    args[0] = CL450_INTERRUPT_MASK;
    cl450_command(video, CL450_CMD_SET_INTERRUPT_MASK, args, 1);
    args[0] = 0;
    cl450_command(video, CL450_CMD_SET_BLANK, args, 1);
    cl450_command(video, CL450_CMD_PLAY, NULL, 0);
    video[CL450_HOST_CONTROL >> 1] = 0x0081;
    *io = 0x7000;
    return TRUE;
}

void fmv_set_visible(struct CD32MPEGBase *base, BOOL visible)
{
    volatile UBYTE *board;
    volatile UWORD *io;
    volatile UWORD *video;
    UWORD blank;

    if (!base->initialized || !base->board_addr)
        return;
    board = base->board_addr;
    io = (volatile UWORD *)(board + FMV_IO_OFFSET);
    video = (volatile UWORD *)(board + FMV_VIDEO_REG_OFFSET);
    blank = visible ? 0 : 1;
    cl450_command(video, CL450_CMD_SET_BLANK, &blank, 1);
    *io = visible ? 0x7000 : 0x3200;
}

static BOOL feed_decoder(
    struct CD32MPEGBase *base,
    struct IORequest *request,
    ULONG port_offset,
    UWORD ready_mask,
    const UBYTE *data,
    ULONG length)
{
    volatile UWORD *status = (volatile UWORD *)
        ((UBYTE *)base->board_addr + FMV_IO_OFFSET);
    volatile UWORD *port = (volatile UWORD *)
        ((UBYTE *)base->board_addr + port_offset);
    UBYTE *pending_byte;
    BOOL *byte_pending;

    if (port_offset == FMV_VIDEO_DATA_OFFSET) {
        pending_byte = &base->video_byte;
        byte_pending = &base->video_byte_pending;
    } else {
        pending_byte = &base->audio_byte;
        byte_pending = &base->audio_byte_pending;
    }
    if (*byte_pending && length != 0) {
        while (ready_mask != 0 && (*status & ready_mask) == 0) {
            if (request->io_Flags & FMV_REQUEST_ABORTED)
                return FALSE;
        }
        *port = ((UWORD)*pending_byte << 8) | *data++;
        length--;
        *byte_pending = FALSE;
    }
    while (length >= 2) {
        UWORD value;
        while (ready_mask != 0 && (*status & ready_mask) == 0) {
            if (request->io_Flags & FMV_REQUEST_ABORTED)
                return FALSE;
        }
        value = (UWORD)*data++ << 8;
        value |= *data++;
        length -= 2;
        *port = value;
    }
    if (length != 0) {
        *pending_byte = *data;
        *byte_pending = TRUE;
    }
    return TRUE;
}

static const UBYTE *pes_payload(const UBYTE *packet, const UBYTE *end)
{
    const UBYTE *p = packet + 6;
    while (p < end && *p == 0xff)
        p++;
    if (p < end && (*p & 0xc0) == 0x40)
        p += 2;
    if (p < end && (*p & 0xf0) == 0x20)
        p += 5;
    else if (p < end && (*p & 0xf0) == 0x30)
        p += 10;
    else if (p < end && *p == 0x0f)
        p++;
    return p < end ? p : end;
}

static BOOL feed_sector(
    struct CD32MPEGBase *base,
    struct IOMPEGReq *mpeg,
    const UBYTE *data,
    ULONG length)
{
    const UBYTE *end = data + length;
    const UBYTE *p;

    for (p = data; p + 6 <= end; p++) {
        UBYTE stream;
        const UBYTE *packet_end;
        if (p[0] != 0 || p[1] != 0 || p[2] != 1)
            continue;
        stream = p[3];
        if (!((stream >= 0xc0 && stream <= 0xdf) ||
              (stream >= 0xe0 && stream <= 0xef)))
            continue;
        packet_end = p + 6 + read_be16(p + 4);
        if (packet_end > end)
            packet_end = end;
        if (stream >= 0xc0 && stream <= 0xdf &&
            (mpeg->iomr_StreamType & 1) != 0) {
            if (!feed_decoder(base, (struct IORequest *)mpeg,
                    FMV_AUDIO_OFFSET, 0, data, packet_end - data))
                return FALSE;
        } else if (stream >= 0xe0 && stream <= 0xef &&
                   (mpeg->iomr_StreamType & 2) != 0) {
            const UBYTE *payload = pes_payload(p, packet_end);
            if (!feed_decoder(base, (struct IORequest *)mpeg,
                    FMV_VIDEO_DATA_OFFSET, FMV_IO_VIDEO_READY,
                    payload, packet_end - payload))
                return FALSE;
        }
        break;
    }
    return TRUE;
}

static void restore_cd_config(struct IOStdReq *cdio, const struct CDInfo *info)
{
    struct TagItem tags[] = {
        { TAGCD_READXLSPEED, info->ReadXLSpeed },
        { TAGCD_SECTORSIZE, info->SectorSize },
        { TAGCD_XLECC, info->XLECC },
        { TAG_DONE, 0 },
    };
    cdio->io_Command = CD_CONFIG;
    cdio->io_Data = tags;
    cdio->io_Length = 0;
    DoIO((struct IORequest *)cdio);
}

static void play_lsn(struct CD32MPEGBase *base, struct IOMPEGReq *mpeg)
{
    struct MsgPort *port = NULL;
    struct IOStdReq *cdio = NULL;
    struct IOStdReq *readio = NULL;
    struct CDInfo saved_info;
    UBYTE *buffers = NULL;
    BOOL cd_open = FALSE;
    BOOL restore_config = FALSE;
    LONG end;
    ULONG current_lsn;
    ULONG sector_count;
    ULONG sector;

    mpeg->iomr_Req.io_Error = 0;
    mpeg->iomr_Req.io_Actual = 0;
    mpeg->iomr_MPEGError = 0;
    base->video_byte_pending = FALSE;
    base->audio_byte_pending = FALSE;

    port = CreateMsgPort();
    if (port)
        cdio = (struct IOStdReq *)CreateIORequest(port, sizeof(*cdio));
    if (port)
        readio = (struct IOStdReq *)CreateIORequest(port, sizeof(*readio));
    if (!cdio || !readio ||
        OpenDevice((CONST_STRPTR)"cd.device", 0,
            (struct IORequest *)cdio, 0) != 0) {
        mpeg->iomr_Req.io_Error = IOERR_OPENFAIL;
        goto out;
    }
    cd_open = TRUE;
    readio->io_Device = cdio->io_Device;
    readio->io_Unit = cdio->io_Unit;

    buffers = AllocMem(FMV_SECTOR_SIZE, MEMF_PUBLIC);
    if (!buffers) {
        mpeg->iomr_Req.io_Error = IOERR_OPENFAIL;
        goto free_buffers;
    }

    cdio->io_Command = CD_INFO;
    cdio->io_Data = &saved_info;
    cdio->io_Length = sizeof(saved_info);
    DoIO((struct IORequest *)cdio);
    if (cdio->io_Error != 0 || cdio->io_Actual < sizeof(saved_info)) {
        mpeg->iomr_Req.io_Error = cdio->io_Error != 0 ?
            cdio->io_Error : IOERR_BADLENGTH;
        goto free_buffers;
    }
    {
        struct TagItem tags[] = {
            { TAGCD_READXLSPEED, 75 },
            { TAGCD_SECTORSIZE, FMV_SECTOR_SIZE },
            { TAGCD_XLECC, 0 },
            { TAG_DONE, 0 },
        };
        cdio->io_Command = CD_CONFIG;
        cdio->io_Data = tags;
        cdio->io_Length = 0;
        restore_config = TRUE;
        DoIO((struct IORequest *)cdio);
        if (cdio->io_Error != 0) {
            mpeg->iomr_Req.io_Error = cdio->io_Error;
            goto restore_cd;
        }
    }

    end = (LONG)(mpeg->iomr_Req.io_Offset + mpeg->iomr_Req.io_Length);
    if (end <= (LONG)mpeg->iomr_Req.io_Offset) {
        union CDTOC toc;
        cdio->io_Command = CD_TOCLSN;
        cdio->io_Data = &toc;
        cdio->io_Length = 1;
        cdio->io_Offset = 0;
        DoIO((struct IORequest *)cdio);
        if (cdio->io_Error != 0 || cdio->io_Actual == 0) {
            mpeg->iomr_Req.io_Error = cdio->io_Error != 0 ?
                cdio->io_Error : IOERR_BADLENGTH;
            goto restore_cd;
        }
        end = toc.Summary.LeadOut.LSN;
    }
    current_lsn = mpeg->iomr_Req.io_Offset;
    sector_count = (ULONG)end - current_lsn;

    for (sector = 0; sector < sector_count; sector++) {
        if (mpeg->iomr_Req.io_Flags & FMV_REQUEST_ABORTED) {
            mpeg->iomr_Req.io_Error = IOERR_ABORTED;
            break;
        }
        readio->io_Command = CD_READ;
        readio->io_Data = buffers;
        readio->io_Length = FMV_SECTOR_SIZE;
        readio->io_Offset = (current_lsn + sector) * FMV_SECTOR_SIZE;
        DoIO((struct IORequest *)readio);
        if (readio->io_Error != 0 ||
            readio->io_Actual != FMV_SECTOR_SIZE) {
            mpeg->iomr_Req.io_Error = readio->io_Error != 0 ?
                readio->io_Error : IOERR_BADLENGTH;
            break;
        }
        if (!feed_sector(base, mpeg, buffers, FMV_SECTOR_SIZE - 4)) {
            mpeg->iomr_Req.io_Error = IOERR_ABORTED;
            break;
        }
        mpeg->iomr_Req.io_Actual++;
    }

restore_cd:
    if (restore_config)
        restore_cd_config(cdio, &saved_info);
free_buffers:
    if (buffers)
        FreeMem(buffers, FMV_SECTOR_SIZE);
    if (cd_open)
        CloseDevice((struct IORequest *)cdio);
out:
    if (readio)
        DeleteIORequest((struct IORequest *)readio);
    if (cdio)
        DeleteIORequest((struct IORequest *)cdio);
    if (port)
        DeleteMsgPort(port);
}

static void fmv_worker(void)
{
    struct CD32MPEGBase *base = worker_start_base;
    struct MsgPort *port;
    struct IOMPEGReq *request;

    base->worker_task = FindTask(NULL);
    port = CreateMsgPort();
    base->worker_port = port ? port : (struct MsgPort *)-1;
    Signal(worker_start_parent, SIGF_SINGLE);
    if (!port)
        return;
    for (;;) {
        WaitPort(port);
        while ((request = (struct IOMPEGReq *)GetMsg(port)) != NULL) {
            if (request->iomr_Req.io_Command == MPEGCMD_PLAYLSN)
                play_lsn(base, request);
            else
                request->iomr_Req.io_Error = IOERR_NOCMD;
            ReplyMsg(&request->iomr_Req.io_Message);
        }
    }
}

static BOOL start_worker(struct CD32MPEGBase *base)
{
    struct Task *task;

    if (base->worker_task && base->worker_port &&
        base->worker_port != (struct MsgPort *)-1)
        return TRUE;
    worker_start_base = base;
    worker_start_parent = FindTask(NULL);
    base->worker_port = NULL;
    task = CreateTask((CONST_STRPTR)CD32MPEG_NAME, 10,
        (APTR)fmv_worker, 8192);
    if (!task)
        return FALSE;
    base->worker_task = task;
    while (!base->worker_port)
        Wait(SIGF_SINGLE);
    return base->worker_port != (struct MsgPort *)-1;
}

BOOL fmv_open(struct CD32MPEGBase *base)
{
    struct ConfigDev *board;

    if (!base->board_addr) {
        board = find_board();
        if (!board)
            return FALSE;
        base->board_addr = board->cd_BoardAddr;
    }
    if (!base->initialized) {
        if (!initialize_decoders(base))
            return FALSE;
        base->initialized = TRUE;
    }
    if (!base->interrupt_installed) {
        base->interrupt.is_Node.ln_Type = NT_INTERRUPT;
        base->interrupt.is_Node.ln_Pri = 0;
        base->interrupt.is_Node.ln_Name = CD32MPEG_NAME;
        base->interrupt.is_Data = base;
        base->interrupt.is_Code = (void (*)(void))fmv_interrupt;
        AddIntServer(INTB_PORTS, &base->interrupt);
        base->interrupt_installed = TRUE;
    }
    return start_worker(base);
}

void fmv_queue_play(struct CD32MPEGBase *base, struct IOMPEGReq *request)
{
    fmv_set_visible(base, TRUE);
    request->iomr_Req.io_Flags &= ~(IOF_QUICK | FMV_REQUEST_ABORTED);
    PutMsg(base->worker_port, &request->iomr_Req.io_Message);
}

LONG fmv_abort(struct IOMPEGReq *request)
{
    Forbid();
    request->iomr_Req.io_Flags |= FMV_REQUEST_ABORTED;
    Permit();
    return 0;
}
