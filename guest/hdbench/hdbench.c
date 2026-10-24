// SPDX-License-Identifier: GPL-3.0-or-later
// Hard-drive timing benchmark: sequential reads at four transfer sizes,
// random 4 KiB reads and a sequential write against a raw disk device,
// timed with the E-clock. The host test names the device and unit in
// `hdbench-config` ("scsi.device 0") and reads `hdbench-result` back.
#include <exec/types.h>
#include <exec/io.h>
#include <exec/memory.h>
#include <devices/timer.h>
#include <clib/alib_protos.h>
#include <dos/dos.h>
#define __NOLIBBASE__
#include <inline/exec.h>
#include <inline/dos.h>
#include <inline/timer.h>

struct ExecBase *SysBase;
struct Library *DOSBase;
struct Device *TimerBase;

#define MAGIC 0x4844424EUL /* 'HDBN' */
#define SPAN (1024UL * 1024)
#define BUFFER (256UL * 1024)
#define RANDOM_READS 32
#define WRITE_OFFSET (8UL * 1024 * 1024)

/* report[0] magic, [1] completed-step mask, [2] E-clock Hz, [3] disk bytes
 * probed, then E-clock ticks for: [4] 1 MiB in 512 B reads, [5] in 4 KiB,
 * [6] in 32 KiB, [7] in 256 KiB, [8] 32 random 4 KiB reads, [9] a 256 KiB
 * write in 32 KiB requests. */
static ULONG report[10];

static ULONG now(void)
{
    struct EClockVal ev;
    ReadEClock(&ev);
    return ev.ev_lo;
}

static int transfer(struct IOStdReq *req, UWORD cmd, APTR data, ULONG length, ULONG offset)
{
    req->io_Command = cmd;
    req->io_Data = data;
    req->io_Length = length;
    req->io_Offset = offset;
    DoIO((struct IORequest *)req);
    return req->io_Error == 0 && req->io_Actual == length;
}

static int sequential(struct IOStdReq *req, UBYTE *data, ULONG chunk, ULONG *ticks)
{
    ULONG offset, start = now();
    for (offset = 0; offset < SPAN; offset += chunk)
        if (!transfer(req, CMD_READ, data, chunk, offset))
            return 0;
    *ticks = now() - start;
    return 1;
}

LONG entry(void)
{
    struct MsgPort *port = NULL;
    struct IOStdReq *req = NULL, *timer = NULL;
    UBYTE *data = NULL;
    char config[64];
    char *name, *p;
    ULONG unit = 0, seed = 12345, i, start;
    struct EClockVal ev;
    int opened = 0, timer_open = 0;
    BPTR file;
    LONG got;

    __asm("move.l 4.w,%0" : "=r"(SysBase));
    DOSBase = OpenLibrary((STRPTR)"dos.library", 0);
    if (!DOSBase)
        return 20;
    report[0] = MAGIC;
    file = Open((STRPTR)"hdbench-config", MODE_OLDFILE);
    if (!file)
        goto done;
    got = Read(file, config, sizeof(config) - 1);
    Close(file);
    if (got <= 0)
        goto done;
    config[got] = 0;
    name = config;
    for (p = config; *p && *p != ' '; p++)
        ;
    if (*p)
        *p++ = 0;
    while (*p >= '0' && *p <= '9')
        unit = unit * 10 + (ULONG)(*p++ - '0');

    port = CreatePort(NULL, 0);
    if (!port)
        goto done;
    timer = AllocMem(sizeof(*timer), MEMF_PUBLIC | MEMF_CLEAR);
    req = AllocMem(sizeof(*req), MEMF_PUBLIC | MEMF_CLEAR);
    if (!timer || !req)
        goto done;
    if (OpenDevice((STRPTR)TIMERNAME, UNIT_ECLOCK, (struct IORequest *)timer, 0) != 0)
        goto done;
    timer_open = 1;
    TimerBase = timer->io_Device;
    report[2] = ReadEClock(&ev);
    req->io_Message.mn_ReplyPort = port;
    req->io_Message.mn_Length = sizeof(*req);
    if (OpenDevice((STRPTR)name, unit, (struct IORequest *)req, 0) != 0)
        goto done;
    opened = 1;
    data = AllocMem(BUFFER, MEMF_PUBLIC | MEMF_FAST);
    if (!data)
        data = AllocMem(BUFFER, MEMF_PUBLIC | MEMF_CHIP);
    if (!data)
        goto done;
    report[1] |= 1;
    /* Settle: the first read pays any head movement from the boot. */
    if (!transfer(req, CMD_READ, data, 512, 0))
        goto done;
    if (!sequential(req, data, 512, &report[4]))
        goto done;
    report[1] |= 2;
    if (!sequential(req, data, 4096, &report[5]))
        goto done;
    report[1] |= 4;
    if (!sequential(req, data, 32768, &report[6]))
        goto done;
    report[1] |= 8;
    if (!sequential(req, data, 262144, &report[7]))
        goto done;
    report[1] |= 16;
    /* Random 4 KiB reads over the first 16 MiB, from a fixed sequence. */
    report[3] = 16UL * 1024 * 1024;
    start = now();
    for (i = 0; i < RANDOM_READS; i++) {
        seed = seed * 1103515245UL + 12345UL;
        if (!transfer(req, CMD_READ, data, 4096, ((seed >> 8) % 4096UL) * 4096UL))
            goto done;
    }
    report[8] = now() - start;
    report[1] |= 32;
    for (i = 0; i < 32768; i++)
        data[i] = (UBYTE)(i * 7 + 3);
    start = now();
    for (i = 0; i < 8; i++)
        if (!transfer(req, CMD_WRITE, data, 32768, WRITE_OFFSET + i * 32768))
            goto done;
    report[9] = now() - start;
    report[1] |= 64;

done:
    if (data)
        FreeMem(data, BUFFER);
    if (opened)
        CloseDevice((struct IORequest *)req);
    if (timer_open)
        CloseDevice((struct IORequest *)timer);
    if (req)
        FreeMem(req, sizeof(*req));
    if (timer)
        FreeMem(timer, sizeof(*timer));
    if (port)
        DeletePort(port);
    file = Open((STRPTR)"hdbench-result", MODE_NEWFILE);
    if (file) {
        Write(file, report, sizeof(report));
        Close(file);
    }
    CloseLibrary(DOSBase);
    return report[1] == 127 ? 0 : 20;
}
