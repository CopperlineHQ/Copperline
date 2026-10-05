// SPDX-License-Identifier: GPL-3.0-or-later
// Raw SCSI I/O into accelerator RAM, with byte checks and untouched guards.
// The host seeds a 4 MiB disk with byte i % 251. This probe never uses the
// disk as a filesystem; all writes target a private copy owned by the test.
#include <exec/types.h>
#include <exec/io.h>
#include <exec/memory.h>
#include <exec/execbase.h>
#include <clib/alib_protos.h>
#include <devices/scsidisk.h>
#include <dos/dos.h>
#define __NOLIBBASE__
#include <inline/exec.h>
#include <inline/dos.h>

struct ExecBase *SysBase;
struct Library *DOSBase;
#define LENGTH (320UL * 1024 + 512)
#define DISK_SIZE (4UL * 1024 * 1024)

static int transfer(struct IOStdReq *req, UWORD cmd, APTR data,
                    ULONG length, ULONG offset)
{
    req->io_Command = cmd;
    req->io_Data = data;
    req->io_Length = length;
    req->io_Offset = offset;
    DoIO((struct IORequest *)req);
    return req->io_Error == 0 && req->io_Actual == length;
}

static int pattern(UBYTE *data, ULONG size, ULONG offset)
{
    ULONG i;
    UBYTE expected = (UBYTE)(offset % 251);
    for (i = 0; i < size; i++) {
        if (data[i] != expected)
            return 0;
        if (++expected == 251)
            expected = 0;
    }
    return 1;
}

LONG entry(void)
{
    struct MsgPort *port = NULL;
    struct IOStdReq *req = NULL;
    UBYTE *allocation = NULL, *data = NULL;
    APTR hold = NULL;
    ULONG hold_size = 0, i, expected_mask = 511;
    ULONG report[8] = {0x41323039, 0, 0, 0, 0, 0, 0, 0};
    int opened = 0;
    BPTR output;

    __asm("move.l 4.w,%0" : "=r"(SysBase));
    DOSBase = OpenLibrary((STRPTR)"dos.library", 0);
    if (!DOSBase)
        return 20;
    /* Small-box Kickstarts do not probe CPU-slot RAM. The test config
     * always maps these 8 MiB; register that mapped bank with Exec. */
    if ((SysBase->AttnFlags & AFF_68020) && !TypeOfMem((APTR)0x08000000UL))
        AddMemList(8UL * 1024 * 1024, MEMF_PUBLIC | MEMF_FAST, 30,
                   (APTR)0x08000000UL, (STRPTR)"DMA test accelerator");
    port = CreatePort(NULL, 0);
    if (!port)
        goto done;
    req = AllocMem(sizeof(*req), MEMF_PUBLIC | MEMF_CLEAR);
    if (!req)
        goto done;
    req->io_Message.mn_ReplyPort = port;
    req->io_Message.mn_Length = sizeof(*req);
    if (OpenDevice((STRPTR)"scsi.device", 0, (struct IORequest *)req, 0) != 0)
        goto done;
    opened = 1;
    allocation = AllocMem(LENGTH + 16, MEMF_PUBLIC | MEMF_FAST);
    if (!allocation)
        allocation = AllocMem(LENGTH + 16, MEMF_PUBLIC | MEMF_CHIP);
    if (!allocation)
        goto done;
    data = allocation + 4;
    /* The asset-free 68000 fixture uses an odd low-memory destination;
     * Kickstart fixtures use accelerator RAM beyond the DMA ceiling. */
    if ((ULONG)data < 0x01000000UL)
        data++;
    report[2] = (ULONG)data;
    /* The scarcity fixture supplies only 512 KiB of Chip RAM and no Z2
     * RAM. Leave room for a small single bounce buffer and its descriptor. */
    if (AvailMem(MEMF_CHIP | MEMF_TOTAL) <= 512UL * 1024) {
        hold_size = AvailMem(MEMF_CHIP | MEMF_LARGEST);
        if (hold_size > 1536) {
            hold_size -= 1536;
            hold = AllocMem(hold_size, MEMF_CHIP);
        }
    }
    report[3] = AvailMem(MEMF_CHIP);
    report[4] = AvailMem(MEMF_FAST);
    for (i = 0; i < LENGTH + 16; i++)
        allocation[i] = 0xa5;
    if (!transfer(req, CMD_READ, data, LENGTH, 0) || !pattern(data, LENGTH, 0))
        goto done;
    report[1] |= 1;
    for (i = 0; i < 4; i++)
        if (data[(LONG)i - 4] != 0xa5 || data[LENGTH + i] != 0xa5)
            goto done;
    report[1] |= 2;
    /* Odd destination forces the byte-copy path without weakening DMA
     * alignment. A short tail also exercises the alternating buffer sizes. */
    if (!transfer(req, CMD_READ, data + 1, LENGTH - 512, 512) ||
        !pattern(data + 1, LENGTH - 512, 512))
        goto done;
    report[1] |= 4;
    for (i = 0; i < LENGTH; i++)
        data[i] = (UBYTE)(i * 13 + 7);
    if (!transfer(req, CMD_WRITE, data, LENGTH, 1024UL * 1024))
        goto done;
    for (i = 0; i < LENGTH; i++)
        data[i] = 0;
    if (!transfer(req, CMD_READ, data, LENGTH, 1024UL * 1024))
        goto done;
    for (i = 0; i < LENGTH; i++)
        if (data[i] != (UBYTE)(i * 13 + 7))
            goto done;
    report[1] |= 8;
    /* SCSI-direct uses the same Fast/Chip allocator, but arbitrary CDBs
     * must retain their original size and cannot be split by the driver. */
    {
        UBYTE cdb[10] = {0x28, 0, 0, 0, 0, 0, 0, 0, 128, 0};
        struct SCSICmd scmd = {0};
        scmd.scsi_Data = (UWORD *)data;
        scmd.scsi_Length = 64UL * 1024;
        scmd.scsi_Command = cdb;
        scmd.scsi_CmdLength = sizeof(cdb);
        scmd.scsi_Flags = SCSIF_READ;
        if (hold) {
            FreeMem(hold, hold_size);
            hold = NULL;
        }
        req->io_Command = HD_SCSICMD;
        req->io_Data = &scmd;
        req->io_Length = sizeof(scmd);
        DoIO((struct IORequest *)req);
        if (req->io_Error || scmd.scsi_Status ||
            scmd.scsi_Actual != scmd.scsi_Length || !pattern(data, 64UL * 1024, 0))
            goto done;
    }
    report[1] |= 16;
    /* A DMA-sized SCSI-direct buffer with a short INQUIRY response must
     * report the WD transfer-counter residual and copy only actual bytes. */
    {
        UBYTE cdb[6] = {0x12, 0, 0, 0, 36, 0};
        struct SCSICmd scmd = {0};
        for (i = 0; i < 1024; i++)
            data[i] = 0xa5;
        scmd.scsi_Data = (UWORD *)data;
        scmd.scsi_Length = 1024;
        scmd.scsi_Command = cdb;
        scmd.scsi_CmdLength = sizeof(cdb);
        scmd.scsi_Flags = SCSIF_READ;
        req->io_Command = HD_SCSICMD;
        req->io_Data = &scmd;
        req->io_Length = sizeof(scmd);
        DoIO((struct IORequest *)req);
        if (req->io_Error || scmd.scsi_Status || scmd.scsi_Actual != 36)
            goto done;
        for (i = 36; i < 1024; i++)
            if (data[i] != 0xa5)
                goto done;
    }
    report[1] |= 128;
    /* DMA-safe caller memory takes the direct path without bouncing. */
    {
        UBYTE *direct = AllocMem(512, MEMF_PUBLIC | MEMF_CHIP);
        int good;
        if (!direct)
            goto done;
        good = transfer(req, CMD_READ, direct, 512, 0) && pattern(direct, 512, 0);
        FreeMem(direct, 512);
        if (!good)
            goto done;
    }
    report[1] |= 256;
    /* Optional sparse-disk fixture straddles the 32-bit LBA boundary.
     * TD_READ64 supplies byte offset 0x1ff:ffff0000 (2 TiB - 64 KiB). */
    output = Open((STRPTR)"a2091-lba", MODE_OLDFILE);
    if (output) {
        Close(output);
        expected_mask |= 512;
        CloseDevice((struct IORequest *)req);
        opened = 0;
        if (OpenDevice((STRPTR)"scsi.device", 1, (struct IORequest *)req, 0))
            goto done;
        opened = 1;
        req->io_Command = 24; /* TD_READ64 */
        req->io_Data = data;
        req->io_Length = 128UL * 1024 + 512;
        req->io_Offset = 0xffff0000UL;
        req->io_Actual = 0x1ff;
        DoIO((struct IORequest *)req);
        if (req->io_Error || req->io_Actual != req->io_Length ||
            !pattern(data, req->io_Length, 0))
            goto done;
        report[1] |= 512;
        CloseDevice((struct IORequest *)req);
        opened = 0;
        if (OpenDevice((STRPTR)"scsi.device", 0, (struct IORequest *)req, 0))
            goto done;
        opened = 1;
    }
    /* A later chunk fails at end-of-media: preceding valid data must be
     * preserved, io_Actual must count only copied chunks, and the failed
     * chunk must leave the caller's sentinel untouched. */
    for (i = 0; i < LENGTH; i++)
        data[i] = 0xa5;
    transfer(req, CMD_READ, data, 128UL * 1024, DISK_SIZE - 64UL * 1024);
    report[5] = req->io_Actual;
    if (!req->io_Error || req->io_Actual != 64UL * 1024 ||
        !pattern(data, 64UL * 1024, DISK_SIZE - 64UL * 1024))
        goto done;
    for (i = 64UL * 1024; i < 128UL * 1024; i++)
        if (data[i] != 0xa5)
            goto done;
    report[1] |= 32;
    /* A subsequent good transfer proves the error stopped/drained DMA. */
    if (!transfer(req, CMD_READ, data, 512, 512) || !pattern(data, 512, 512))
        goto done;
    report[1] |= 64;
    report[6] = AvailMem(MEMF_CHIP);
    report[7] = AvailMem(MEMF_FAST);

done:
    if (hold)
        FreeMem(hold, hold_size);
    if (allocation)
        FreeMem(allocation, LENGTH + 16);
    if (opened)
        CloseDevice((struct IORequest *)req);
    if (req)
        FreeMem(req, sizeof(*req));
    if (port)
        DeletePort(port);
    output = Open((STRPTR)"a2091-result", MODE_NEWFILE);
    if (output) {
        Write(output, report, sizeof(report));
        Close(output);
    }
    CloseLibrary(DOSBase);
    return report[1] == expected_mask ? 0 : 20;
}
