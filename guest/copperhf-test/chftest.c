// SPDX-License-Identifier: GPL-3.0-or-later
//
// chftest: guest-side end-to-end probe for copperhf.device
// (tests/copperhf_device.rs). Talks to the M2 device stub directly --
// OpenDevice/DoIO against unit 0, exactly the way any real client would --
// rather than through a mounted filesystem, since M2 has no partition
// mounter yet (COPPERHF-DEVICE-PLAN.md).
//
// CMD_READs block 0 (byte offset 0) and checks it against the i % 251
// pattern the host test seeds the drive image with, then CMD_WRITEs a
// pass/fail marker into block 1 (byte offset 512) for the host test to read
// back directly from the image file -- no hostfs output plumbing needed,
// unlike guest/modem-test's transcript-file idiom, since the marker itself
// lives in the very image the host test already has open.
//
// Ordinary relocatable hunk executable, standard -noixemul startup -- same
// style as guest/modem-test/modemtest.c and guest/zz9kprobe, verified
// against both AROS and Kickstart 3.1.

#include <exec/types.h>
#include <exec/io.h>
#include <exec/errors.h>
#include <exec/memory.h>
#include <proto/exec.h>
#include <clib/alib_protos.h>

#define CHFTEST_UNIT 0
#define BLOCK_SIZE 512

static struct MsgPort *port;
static struct IOStdReq *req;
static UBYTE *readbuf;
static UBYTE *writebuf;

static void fill_marker(UBYTE *buf, const char *marker)
{
    long i;
    for (i = 0; i < BLOCK_SIZE; i++)
        buf[i] = 0xC5;
    for (i = 0; marker[i] != '\0'; i++)
        buf[i] = (UBYTE)marker[i];
}

int main(void)
{
    BOOL opened = FALSE;
    BOOL read_ok = FALSE;
    BOOL all_ok = FALSE;

    port = CreateMsgPort();
    if (!port)
        return 20;

    req = (struct IOStdReq *)CreateExtIO(port, sizeof(struct IOStdReq));
    if (!req) {
        DeleteMsgPort(port);
        return 20;
    }

    if (OpenDevice((CONST_STRPTR) "copperhf.device", CHFTEST_UNIT,
                    (struct IORequest *)req, 0) == 0) {
        opened = TRUE;
    }

    if (opened) {
        readbuf = (UBYTE *)AllocMem(BLOCK_SIZE, MEMF_PUBLIC | MEMF_CLEAR);
        writebuf = (UBYTE *)AllocMem(BLOCK_SIZE, MEMF_PUBLIC | MEMF_CLEAR);

        if (readbuf && writebuf) {
            long i;

            // CMD_READ block 0 (byte offset 0) and verify the i % 251
            // pattern the host test seeded the image with.
            req->io_Command = CMD_READ;
            req->io_Length = BLOCK_SIZE;
            req->io_Data = readbuf;
            req->io_Offset = 0;
            DoIO((struct IORequest *)req);

            if (req->io_Error == 0 && req->io_Actual == BLOCK_SIZE) {
                read_ok = TRUE;
                for (i = 0; i < BLOCK_SIZE; i++) {
                    if (readbuf[i] != (UBYTE)(i % 251)) {
                        read_ok = FALSE;
                        break;
                    }
                }
            }

            // CMD_WRITE the pass/fail marker into block 1 (byte offset
            // 512) for the host test to read back from the image file.
            fill_marker(writebuf, read_ok ? "COPPERHF-TEST-OK" : "COPPERHF-TEST-BAD");
            req->io_Command = CMD_WRITE;
            req->io_Length = BLOCK_SIZE;
            req->io_Data = writebuf;
            req->io_Offset = BLOCK_SIZE;
            DoIO((struct IORequest *)req);
            all_ok = read_ok && req->io_Error == 0;

            // CMD_UPDATE: flush the marker to the backing image before the
            // host test reads the file.
            req->io_Command = CMD_UPDATE;
            req->io_Length = 0;
            req->io_Offset = 0;
            DoIO((struct IORequest *)req);
        }

        if (readbuf)
            FreeMem(readbuf, BLOCK_SIZE);
        if (writebuf)
            FreeMem(writebuf, BLOCK_SIZE);

        CloseDevice((struct IORequest *)req);
    }

    DeleteExtIO((struct IORequest *)req);
    DeleteMsgPort(port);

    return all_ok ? 0 : 20;
}
