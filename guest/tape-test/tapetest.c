// SPDX-License-Identifier: GPL-3.0-or-later
// SCSI Direct exercise of a tape drive on scsi.device unit 4. Returns 0
// when every step behaved, 100 + the step number otherwise. The host test
// supplies an empty cartridge directory and checks what is left in it.
#include <exec/types.h>
#include <exec/io.h>
#include <exec/memory.h>
#include <exec/execbase.h>
#include <clib/alib_protos.h>
#include <devices/scsidisk.h>
#include <dos/dos.h>
#define __NOLIBBASE__
#include <inline/exec.h>

struct ExecBase *SysBase;

#define TAPE_UNIT 4
#define BLOCK 512
#define VARIABLE_LEN 300

#define SENSE_FILEMARK 0x80
#define SENSE_EOM 0x40
#define SENSE_ILI 0x20

static struct IOStdReq *req;
static struct SCSICmd cmd;
static UBYTE sense[18];
static UBYTE *buffer;

// Send one CDB. Returns the SCSI status byte, or 0xFF if the request
// itself failed without one.
static UBYTE scsi(const UBYTE *cdb, UWORD cdb_len, APTR data, ULONG len, UBYTE direction)
{
    ULONG i;
    for (i = 0; i < sizeof(sense); i++)
        sense[i] = 0;
    cmd.scsi_Data = (UWORD *)data;
    cmd.scsi_Length = len;
    cmd.scsi_Actual = 0;
    cmd.scsi_Command = (UBYTE *)cdb;
    cmd.scsi_CmdLength = cdb_len;
    cmd.scsi_CmdActual = 0;
    cmd.scsi_Flags = direction | SCSIF_AUTOSENSE;
    cmd.scsi_Status = 0;
    cmd.scsi_SenseData = sense;
    cmd.scsi_SenseLength = sizeof(sense);
    cmd.scsi_SenseActual = 0;
    req->io_Command = HD_SCSICMD;
    req->io_Data = &cmd;
    req->io_Length = sizeof(cmd);
    DoIO((struct IORequest *)req);
    if (req->io_Error != 0 && cmd.scsi_Status == 0)
        return 0xFF;
    return cmd.scsi_Status;
}

static UBYTE simple(UBYTE op, UBYTE b1, ULONG count24, UBYTE b4)
{
    UBYTE cdb[6];
    cdb[0] = op;
    cdb[1] = b1;
    cdb[2] = (UBYTE)(count24 >> 16);
    cdb[3] = (UBYTE)(count24 >> 8);
    cdb[4] = b4 ? b4 : (UBYTE)count24;
    cdb[5] = 0;
    return scsi(cdb, 6, NULL, 0, SCSIF_READ);
}

static UBYTE rw(UBYTE op, UBYTE flags, ULONG count, ULONG bytes)
{
    UBYTE cdb[6];
    cdb[0] = op;
    cdb[1] = flags;
    cdb[2] = (UBYTE)(count >> 16);
    cdb[3] = (UBYTE)(count >> 8);
    cdb[4] = (UBYTE)count;
    cdb[5] = 0;
    return scsi(cdb, 6, buffer, bytes, op == 0x0A ? SCSIF_WRITE : SCSIF_READ);
}

static UBYTE set_block_length(ULONG len)
{
    static UBYTE list[12];
    UBYTE cdb[6] = {0x15, 0x10, 0, 0, 12, 0};
    list[0] = 0;
    list[1] = 0;
    list[2] = 0x10; /* buffered mode */
    list[3] = 8;
    list[4] = list[5] = list[6] = list[7] = list[8] = 0;
    list[9] = (UBYTE)(len >> 16);
    list[10] = (UBYTE)(len >> 8);
    list[11] = (UBYTE)len;
    return scsi(cdb, 6, list, sizeof(list), SCSIF_WRITE);
}

static UBYTE sense_key(void)
{
    return sense[2] & 0x0F;
}

static UBYTE sense_flags(void)
{
    return sense[2] & 0xE0;
}

static LONG sense_info(void)
{
    return (LONG)(((ULONG)sense[3] << 24) | ((ULONG)sense[4] << 16) | ((ULONG)sense[5] << 8) |
                  sense[6]);
}

static void fill(UBYTE seed, ULONG len)
{
    ULONG i;
    for (i = 0; i < len; i++)
        buffer[i] = (UBYTE)(seed + i * 7);
}

static int matches(UBYTE seed, ULONG len)
{
    ULONG i;
    for (i = 0; i < len; i++) {
        if (buffer[i] != (UBYTE)(seed + i * 7))
            return 0;
    }
    return 1;
}

static void clear(ULONG len)
{
    ULONG i;
    for (i = 0; i < len; i++)
        buffer[i] = 0xEE;
}

#define STEP(n, condition)                                                                        \
    do {                                                                                          \
        if (!(condition))                                                                         \
            return 100 + (n);                                                                     \
    } while (0)

static LONG run(void)
{
    static const UBYTE inquiry[6] = {0x12, 0, 0, 0, 36, 0};
    static const UBYTE limits[6] = {0x05, 0, 0, 0, 0, 0};
    static const UBYTE mode_sense[6] = {0x1A, 0, 0, 0, 12, 0};
    UBYTE status;

    // 1: a removable sequential-access device.
    status = scsi(inquiry, 6, buffer, 36, SCSIF_READ);
    STEP(1, status == 0 && (buffer[0] & 0x1F) == 0x01 && (buffer[1] & 0x80));
    // 2: ready. A drive may answer the first command after power-on with
    // a unit attention; ask twice.
    status = simple(0x00, 0, 0, 0);
    if (status != 0)
        status = simple(0x00, 0, 0, 0);
    STEP(2, status == 0);
    // 3: block limits 1 .. 0xFFFFFF.
    status = scsi(limits, 6, buffer, 6, SCSIF_READ);
    STEP(3, status == 0 && buffer[1] == 0xFF && buffer[2] == 0xFF && buffer[3] == 0xFF &&
                buffer[4] == 0 && buffer[5] == 1);
    // 4: fixed 512-byte blocks, and MODE SENSE says so (writable tape).
    STEP(4, set_block_length(BLOCK) == 0);
    status = scsi(mode_sense, 6, buffer, 12, SCSIF_READ);
    STEP(5, status == 0 && buffer[3] == 8 && buffer[9] == 0 && buffer[10] == 2 &&
                buffer[11] == 0 && !(buffer[2] & 0x80));
    STEP(6, simple(0x01, 0, 0, 0) == 0); /* REWIND */

    // 7-8: tape file 0, two fixed blocks, closed by a filemark.
    fill(0x11, 2 * BLOCK);
    STEP(7, rw(0x0A, 0x01, 2, 2 * BLOCK) == 0);
    STEP(8, simple(0x10, 0, 1, 0) == 0); /* WRITE FILEMARKS 1 */
    // 9-11: tape file 1, one variable block of 300 bytes, and its filemark.
    STEP(9, set_block_length(0) == 0);
    fill(0x33, VARIABLE_LEN);
    STEP(10, rw(0x0A, 0x00, VARIABLE_LEN, VARIABLE_LEN) == 0);
    STEP(11, simple(0x10, 0, 1, 0) == 0);

    // 12-13: back to the start; the two fixed blocks read back.
    STEP(12, simple(0x01, 0, 0, 0) == 0 && set_block_length(BLOCK) == 0);
    clear(2 * BLOCK);
    STEP(13, rw(0x08, 0x01, 2, 2 * BLOCK) == 0 && matches(0x11, 2 * BLOCK));
    // 14: the next fixed read runs into the filemark: nothing read, one
    // block of residue.
    status = rw(0x08, 0x01, 1, BLOCK);
    STEP(14, status == 2 && (sense[0] & 0x80) && sense_key() == 0 &&
                 sense_flags() == SENSE_FILEMARK && sense_info() == 1);
    // 15: tape file 1 with a 512-byte variable read: the 300-byte block
    // arrives and is reported as short by 212.
    STEP(15, set_block_length(0) == 0);
    clear(BLOCK);
    status = rw(0x08, 0x00, BLOCK, BLOCK);
    STEP(16, status == 2 && sense_key() == 0 && sense_flags() == SENSE_ILI &&
                 sense_info() == BLOCK - VARIABLE_LEN && matches(0x33, VARIABLE_LEN) &&
                 buffer[VARIABLE_LEN] == 0xEE);
    // 17: its filemark, then end-of-data (BLANK CHECK).
    status = rw(0x08, 0x00, BLOCK, BLOCK);
    STEP(17, status == 2 && sense_flags() == SENSE_FILEMARK && sense_info() == BLOCK);
    status = rw(0x08, 0x00, BLOCK, BLOCK);
    STEP(18, status == 2 && sense_key() == 0x08 && sense_flags() == 0);

    // 19: SPACE back over both filemarks: the end of tape file 0.
    STEP(19, simple(0x11, 0x01, 0xFFFFFE, 0xFE) == 0);
    STEP(20, set_block_length(BLOCK) == 0);
    // 21: back two blocks is the beginning of the tape; one more runs
    // into it: EOM, residue -1.
    STEP(21, simple(0x11, 0x00, 0xFFFFFE, 0xFE) == 0);
    status = simple(0x11, 0x00, 0xFFFFFF, 0xFF);
    STEP(22, status == 2 && sense_key() == 0 && sense_flags() == SENSE_EOM && sense_info() == -1);
    // 23: forward over one filemark, and tape file 1 is there again.
    STEP(23, simple(0x11, 0x01, 1, 0) == 0 && set_block_length(0) == 0);
    clear(BLOCK);
    status = rw(0x08, 0x02, BLOCK, BLOCK); /* SILI: the short block is not an error */
    STEP(24, status == 0 && matches(0x33, VARIABLE_LEN));

    // 25-26: UNLOAD makes the unit not ready (LOAD required); LOAD
    // brings it back at the beginning of the tape.
    STEP(25, simple(0x1B, 0, 0, 0) == 0);
    status = simple(0x00, 0, 0, 0);
    STEP(26, status == 2 && sense_key() == 0x02 && sense[12] == 0x04 && sense[13] == 0x02);
    {
        static const UBYTE load[6] = {0x1B, 0, 0, 0, 0x01, 0};
        STEP(27, scsi(load, 6, NULL, 0, SCSIF_READ) == 0);
    }
    STEP(28, simple(0x00, 0, 0, 0) == 0 && set_block_length(BLOCK) == 0);
    clear(BLOCK);
    STEP(29, rw(0x08, 0x01, 1, BLOCK) == 0 && matches(0x11, BLOCK));

    // 30-32: a third tape file at end-of-data, then a long ERASE from its
    // start takes it away again.
    STEP(30, simple(0x11, 0x03, 0, 0) == 0);
    fill(0x55, BLOCK);
    STEP(31, rw(0x0A, 0x01, 1, BLOCK) == 0);
    STEP(32, simple(0x01, 0, 0, 0) == 0 && simple(0x11, 0x01, 2, 0) == 0);
    {
        static const UBYTE erase[6] = {0x19, 0x01, 0, 0, 0, 0};
        STEP(33, scsi(erase, 6, NULL, 0, SCSIF_READ) == 0);
    }
    status = rw(0x08, 0x01, 1, BLOCK);
    STEP(34, status == 2 && sense_key() == 0x08);
    return 0;
}

LONG entry(void)
{
    struct MsgPort *port;
    LONG result = 99;

    __asm("move.l 4.w,%0" : "=r"(SysBase));
    port = CreatePort(NULL, 0);
    if (!port)
        return 98;
    req = AllocMem(sizeof(*req), MEMF_PUBLIC | MEMF_CLEAR);
    buffer = AllocMem(2 * BLOCK, MEMF_PUBLIC | MEMF_CLEAR);
    if (req && buffer) {
        req->io_Message.mn_ReplyPort = port;
        req->io_Message.mn_Length = sizeof(*req);
        if (OpenDevice((STRPTR)"scsi.device", TAPE_UNIT, (struct IORequest *)req, 0) == 0) {
            result = run();
            CloseDevice((struct IORequest *)req);
        } else {
            result = 97;
        }
    }
    if (buffer)
        FreeMem(buffer, 2 * BLOCK);
    if (req)
        FreeMem(req, sizeof(*req));
    DeletePort(port);
    return result;
}
