// SPDX-License-Identifier: GPL-3.0-or-later
//
// chftest_m4: guest-side end-to-end probe for copperhf.device's M4 command
// coverage (tests/copperhf_m4_guest.rs). A second probe rather than an
// extension of chftest.c (M2) so the M2 round trip -- and the Rust test
// that already pins its exact marker layout -- stays untouched.
//
// Talks to copperhf.device directly (OpenDevice/DoIO/SendIO against a
// 2-unit config), exactly as any real client would, exercising every
// command COPPERHF-DEVICE-PLAN.md's M4 section adds:
//
//   - NSCMD_DEVICEQUERY: struct NSDeviceQueryResult sanity (format/type/
//     the guest-ROM-resident supported-command table containing TD_READ64)
//   - TD_CHANGENUM / TD_CHANGESTATE / TD_PROTSTATUS on unit 0
//   - TD_READ64 (unit 0, block 0, offset < 4 GiB -- the host worker's own
//     unit tests cover the >4 GiB cases this probe cannot reach without a
//     multi-gigabyte fixture)
//   - HD_SCSICMD INQUIRY and READ CAPACITY(10) on unit 0
//   - the change-interrupt story on unit 1: TD_ADDCHANGEINT (SendIO, never
//     completes on its own), TD_EJECT (io_Length 1), poll the Interrupt's
//     is_Code flag, TD_REMCHANGEINT (which both completes the removal and
//     ReplyMsg's the held ADDCHANGEINT)
//
// One CMD_WRITE marker per subtest into unit 0, one block apart, so the
// Rust test can report exactly which check failed rather than a single
// pass/fail bit -- the same "marker-in-image" trick M2's chftest.c and M3's
// mounter test use for output with no hostfs plumbing.
//
// 68000-only: no 68020+ addressing tricks, matching every other guest
// binary in this project. Ordinary relocatable hunk executable, standard
// -noixemul startup (this is a plain application, not the ROM -- the
// no-data/bss/no-relocations discipline in guest/copperhf/'s own Makefile
// does not apply here).

#include <exec/types.h>
#include <exec/io.h>
#include <exec/errors.h>
#include <exec/memory.h>
#include <exec/interrupts.h>
#include <exec/nodes.h>
#include <devices/newstyle.h>
#include <devices/scsidisk.h>
#include <proto/exec.h>
#include <clib/alib_protos.h>
#include <stddef.h>

// ABI tripwire (devsoak's -X HD_SCSICMD phase caught the original bug):
// src/copperhf.rs mirrors these struct SCSICmd offsets as hand-kept
// constants, and an earlier version had the three sense fields +2 (it
// assumed 4-byte pointer alignment; the classic m68k ABI aligns to 2, so
// scsi_SenseData directly follows scsi_Status with NO padding and the
// whole struct is 30 bytes, not 32). The Rust side's own unit tests
// could not catch that -- they read and wrote through the same wrong
// constants -- so the REAL m68k compiler against the REAL NDK header is
// the authority: if any of these fire, fix src/copperhf.rs's SCSI_*
// constants, never these asserts.
_Static_assert(offsetof(struct SCSICmd, scsi_Data) == 0, "scsi_Data @0");
_Static_assert(offsetof(struct SCSICmd, scsi_Length) == 4, "scsi_Length @4");
_Static_assert(offsetof(struct SCSICmd, scsi_Actual) == 8, "scsi_Actual @8");
_Static_assert(offsetof(struct SCSICmd, scsi_Command) == 12, "scsi_Command @12");
_Static_assert(offsetof(struct SCSICmd, scsi_CmdLength) == 16, "scsi_CmdLength @16");
_Static_assert(offsetof(struct SCSICmd, scsi_CmdActual) == 18, "scsi_CmdActual @18");
_Static_assert(offsetof(struct SCSICmd, scsi_Flags) == 20, "scsi_Flags @20");
_Static_assert(offsetof(struct SCSICmd, scsi_Status) == 21, "scsi_Status @21");
_Static_assert(offsetof(struct SCSICmd, scsi_SenseData) == 22, "scsi_SenseData @22");
_Static_assert(offsetof(struct SCSICmd, scsi_SenseLength) == 26, "scsi_SenseLength @26");
_Static_assert(offsetof(struct SCSICmd, scsi_SenseActual) == 28, "scsi_SenseActual @28");
_Static_assert(sizeof(struct SCSICmd) == 30, "sizeof(SCSICmd) == 30");

// copperhf.device's M4 command numbers (guest/copperhf/copperhf_board.h is
// this project's shared source of truth for the full register/command
// protocol -- kept in sync there, not `#include`d here: the dockerized
// toolchain's bind mount covers only this directory, per
// ../toolchain.mk's own comment, so a cross-directory #include cannot
// reach it). Only the command numbers used by this probe.
#define CHF_CMD_TD_CHANGENUM 13
#define CHF_CMD_TD_CHANGESTATE 14
#define CHF_CMD_TD_PROTSTATUS 15
#define CHF_CMD_TD_ADDCHANGEINT 20
#define CHF_CMD_TD_REMCHANGEINT 21
#define CHF_CMD_TD_EJECT 23
#define CHF_CMD_TD_READ64 24

#define BLOCK_SIZE 512
#define UNIT_DATA 0 /* seeded pattern + read-only-ish query unit */
#define UNIT_CHANGE 1 /* change-interrupt story: ejected, never read */

// Marker blocks in unit 0, one per subtest (block 0 is the seeded i % 251
// pattern TD_READ64 verifies against).
#define BLK_NSDQ 1
#define BLK_CHANGENUM 2
#define BLK_CHANGESTATE 3
#define BLK_PROTSTATUS 4
#define BLK_READ64 5
#define BLK_SCSI_INQUIRY 6
#define BLK_SCSI_READCAP 7
#define BLK_CHANGEINT_STORY 8
#define BLK_SCSI_SENSE 9

static struct MsgPort *port0;
static struct IOStdReq *req0; /* unit 0: DoIO round trips */
static struct MsgPort *port1;
static struct IOStdReq *req1; /* unit 1: DoIO round trips (EJECT, REMCHANGEINT) */
static struct MsgPort *addport;
static struct IOStdReq *addreq; /* unit 1: the SendIO'd TD_ADDCHANGEINT */
static UBYTE *readbuf;
static UBYTE *writebuf;

// The change-interrupt handler: a software interrupt Cause()d by
// int_handler.s's chf_drain_changes() (device.c). No Z-flag contract
// applies here (unlike the AddIntServer chain BeginIO/int_handler.s's own
// header comments document) -- Cause() invokes exactly one Interrupt
// directly, not a shared chain, so an ordinary C function is fine; it only
// needs to be safe to call at interrupt time, and setting one flag byte is.
static volatile LONG change_fired = 0;

static void change_int_handler(void)
{
    change_fired = 1;
}

static struct Interrupt change_interrupt;

static void fill_marker(UBYTE *buf, const char *marker)
{
    long i;
    for (i = 0; i < BLOCK_SIZE; i++)
        buf[i] = 0xC5;
    for (i = 0; marker[i] != '\0'; i++)
        buf[i] = (UBYTE)marker[i];
}

static BOOL write_marker(long block, BOOL ok, const char *okmsg, const char *badmsg)
{
    fill_marker(writebuf, ok ? okmsg : badmsg);
    req0->io_Command = CMD_WRITE;
    req0->io_Length = BLOCK_SIZE;
    req0->io_Data = writebuf;
    req0->io_Offset = (ULONG)block * BLOCK_SIZE;
    DoIO((struct IORequest *)req0);
    return ok && req0->io_Error == 0;
}

// NSCMD_DEVICEQUERY: format 0, the documented common fields, and the
// ROM-resident supported-command table containing TD_READ64 -- proof the
// guest stub filled it in rather than leaving the buffer untouched.
static BOOL test_devicequery(void)
{
    struct NSDeviceQueryResult r;
    UWORD *cmd;
    BOOL found_read64 = FALSE;
    long i;

    for (i = 0; i < (long)sizeof(r); i++)
        ((UBYTE *)&r)[i] = 0xAA;

    req0->io_Command = NSCMD_DEVICEQUERY;
    req0->io_Length = sizeof(r);
    req0->io_Data = &r;
    DoIO((struct IORequest *)req0);

    if (req0->io_Error != 0 || req0->io_Actual != sizeof(r))
        return FALSE;
    if (r.nsdqr_DevQueryFormat != 0)
        return FALSE;
    if (r.nsdqr_SizeAvailable != sizeof(r))
        return FALSE;
    if (r.nsdqr_DeviceType != NSDEVTYPE_TRACKDISK)
        return FALSE;
    if (r.nsdqr_SupportedCommands == NULL)
        return FALSE;

    for (cmd = r.nsdqr_SupportedCommands, i = 0; *cmd != 0 && i < 64; cmd++, i++) {
        if (*cmd == CHF_CMD_TD_READ64)
            found_read64 = TRUE;
    }
    return found_read64;
}

// TD_CHANGENUM: unit 0 has never been ejected, so the change counter must
// read back 0.
static BOOL test_changenum(void)
{
    req0->io_Command = CHF_CMD_TD_CHANGENUM;
    req0->io_Length = 0;
    DoIO((struct IORequest *)req0);
    return req0->io_Error == 0 && req0->io_Actual == 0;
}

// TD_CHANGESTATE: unit 0 has media attached, so this must report "present"
// (io_Actual == 0, trackdisk.device convention: 0 = disk in drive).
static BOOL test_changestate(void)
{
    req0->io_Command = CHF_CMD_TD_CHANGESTATE;
    req0->io_Length = 0;
    DoIO((struct IORequest *)req0);
    return req0->io_Error == 0 && req0->io_Actual == 0;
}

// TD_PROTSTATUS: the host test's unit 0 image is an ordinary writable file,
// so this must report "writable" (io_Actual == 0).
static BOOL test_protstatus(void)
{
    req0->io_Command = CHF_CMD_TD_PROTSTATUS;
    req0->io_Length = 0;
    DoIO((struct IORequest *)req0);
    return req0->io_Error == 0 && req0->io_Actual == 0;
}

// TD_READ64: block 0, offset 0 (upper 32 bits, io_HighOffset/io_Actual on
// entry, also 0 -- well under 4 GiB), verified against the same i % 251
// pattern the host test seeds the image with (M2's chftest.c convention).
static BOOL test_read64(void)
{
    long i;

    for (i = 0; i < BLOCK_SIZE; i++)
        readbuf[i] = 0;

    req0->io_Command = CHF_CMD_TD_READ64;
    req0->io_Length = BLOCK_SIZE;
    req0->io_Data = readbuf;
    req0->io_Offset = 0;
    req0->io_Actual = 0; /* io_HighOffset: upper 32 bits of the byte offset */
    DoIO((struct IORequest *)req0);

    if (req0->io_Error != 0 || req0->io_Actual != BLOCK_SIZE)
        return FALSE;
    for (i = 0; i < BLOCK_SIZE; i++) {
        if (readbuf[i] != (UBYTE)(i % 251))
            return FALSE;
    }
    return TRUE;
}

// HD_SCSICMD INQUIRY: standard 6-byte CDB, 36-byte allocation. Checks the
// peripheral device type (0 = direct access) and copperhf's vendor ID
// (src/scsi.rs::ScsiDisk::inquiry_data, "COPPERLN").
static BOOL test_scsi_inquiry(void)
{
    UBYTE cdb[6];
    struct SCSICmd cmd;
    long i;

    for (i = 0; i < (long)sizeof(cdb); i++)
        cdb[i] = 0;
    cdb[0] = 0x12; /* INQUIRY */
    cdb[4] = 36;   /* allocation length */

    for (i = 0; i < BLOCK_SIZE; i++)
        readbuf[i] = 0xAA;

    cmd.scsi_Data = (UWORD *)readbuf;
    cmd.scsi_Length = 36;
    cmd.scsi_Actual = 0;
    cmd.scsi_Command = cdb;
    cmd.scsi_CmdLength = sizeof(cdb);
    cmd.scsi_CmdActual = 0;
    cmd.scsi_Flags = SCSIF_READ;
    cmd.scsi_Status = 0xFF;
    cmd.scsi_SenseData = NULL;
    cmd.scsi_SenseLength = 0;
    cmd.scsi_SenseActual = 0;

    req0->io_Command = HD_SCSICMD;
    req0->io_Length = sizeof(cmd);
    req0->io_Data = &cmd;
    DoIO((struct IORequest *)req0);

    if (req0->io_Error != 0 || cmd.scsi_Status != 0 /* GOOD */)
        return FALSE;
    if (readbuf[0] != 0x00) /* peripheral device type: direct access */
        return FALSE;
    for (i = 0; i < 8; i++) {
        static const char vendor[] = "COPPERLN"; /* 8 chars + NUL */
        if (readbuf[8 + i] != (UBYTE)vendor[i])
            return FALSE;
    }
    return TRUE;
}

// HD_SCSICMD autosense on an unsupported opcode (0xFF): the target must
// answer CHECK CONDITION, the device must report HFERR_BadStatus in
// io_Error, and -- the part the original M4 implementation got wrong,
// found by devsoak -- SCSIF_AUTOSENSE must deliver the sense bytes into
// scsi_SenseData at the struct's REAL (2-byte-aligned) sense-field
// offsets, with scsi_SenseActual reporting how many. A wrong-offset
// implementation reads a mangled sense pointer, writes the sense to a
// wrong guest address, and leaves the real scsi_SenseActual untouched at
// 0 -- exactly what this subtest distinguishes from success.
static BOOL test_scsi_autosense(void)
{
    UBYTE cdb[6];
    UBYTE sense[32];
    struct SCSICmd cmd;
    long i;

    for (i = 0; i < (long)sizeof(cdb); i++)
        cdb[i] = 0;
    cdb[0] = 0xFF; /* not a SCSI opcode this target implements */

    for (i = 0; i < (long)sizeof(sense); i++)
        sense[i] = 0xA5; /* devsoak-style prefill: proves what got written */

    cmd.scsi_Data = NULL;
    cmd.scsi_Length = 0;
    cmd.scsi_Actual = 0;
    cmd.scsi_Command = cdb;
    cmd.scsi_CmdLength = sizeof(cdb);
    cmd.scsi_CmdActual = 0;
    cmd.scsi_Flags = SCSIF_READ | SCSIF_AUTOSENSE;
    cmd.scsi_Status = 0;
    cmd.scsi_SenseData = sense;
    cmd.scsi_SenseLength = 18;
    cmd.scsi_SenseActual = 0;

    req0->io_Command = HD_SCSICMD;
    req0->io_Length = sizeof(cmd);
    req0->io_Data = &cmd;
    DoIO((struct IORequest *)req0);

    if (req0->io_Error != HFERR_BadStatus)
        return FALSE;
    if (cmd.scsi_Status != 0x02) /* CHECK CONDITION */
        return FALSE;
    if (cmd.scsi_SenseActual != 18)
        return FALSE;
    if (sense[0] != 0x70) /* current-error response code */
        return FALSE;
    if ((sense[2] & 0x0F) != 0x05) /* ILLEGAL REQUEST */
        return FALSE;
    if (sense[12] != 0x20) /* ASC: invalid command operation code */
        return FALSE;
    if (sense[18] != 0xA5) /* nothing written past scsi_SenseLength */
        return FALSE;
    return TRUE;
}

// HD_SCSICMD READ CAPACITY(10): standard 10-byte CDB, 8-byte response
// (last LBA, block length). Only checks the block length -- 512, this
// device's only sector size -- since the exact last LBA depends on the
// host test's chosen image size, which this probe does not need to know.
static BOOL test_scsi_readcap(void)
{
    UBYTE cdb[10];
    struct SCSICmd cmd;
    long i;
    ULONG block_len;

    for (i = 0; i < (long)sizeof(cdb); i++)
        cdb[i] = 0;
    cdb[0] = 0x25; /* READ CAPACITY(10) */

    for (i = 0; i < BLOCK_SIZE; i++)
        readbuf[i] = 0;

    cmd.scsi_Data = (UWORD *)readbuf;
    cmd.scsi_Length = 8;
    cmd.scsi_Actual = 0;
    cmd.scsi_Command = cdb;
    cmd.scsi_CmdLength = sizeof(cdb);
    cmd.scsi_CmdActual = 0;
    cmd.scsi_Flags = SCSIF_READ;
    cmd.scsi_Status = 0xFF;
    cmd.scsi_SenseData = NULL;
    cmd.scsi_SenseLength = 0;
    cmd.scsi_SenseActual = 0;

    req0->io_Command = HD_SCSICMD;
    req0->io_Length = sizeof(cmd);
    req0->io_Data = &cmd;
    DoIO((struct IORequest *)req0);

    if (req0->io_Error != 0 || cmd.scsi_Status != 0)
        return FALSE;

    block_len = ((ULONG)readbuf[4] << 24) | ((ULONG)readbuf[5] << 16) |
                ((ULONG)readbuf[6] << 8) | (ULONG)readbuf[7];
    return block_len == BLOCK_SIZE;
}

// The change-interrupt story, on unit 1: TD_ADDCHANGEINT queues our
// Interrupt and blocks (SendIO -- trackdisk.doc: "this command only
// returns when the handler is removed"); TD_EJECT (io_Length 1) then bumps
// unit 1's change counter and sets CHF_CHANGED_MASK, which int_handler.s's
// chf_drain_changes() (device.c) drains on the next INT2 and Cause()s our
// Interrupt; TD_REMCHANGEINT removes it (matching by io_Data, per
// device.c's chf_do_remchangeint) and ReplyMsg's the held ADDCHANGEINT.
static BOOL test_change_interrupt_story(void)
{
    ULONG spins;
    BOOL fired;
    struct Message *reply;

    change_interrupt.is_Node.ln_Type = NT_INTERRUPT;
    change_interrupt.is_Node.ln_Pri = 0;
    change_interrupt.is_Node.ln_Name = (char *)"chftest_m4.change";
    change_interrupt.is_Data = NULL;
    change_interrupt.is_Code = change_int_handler;

    addreq->io_Command = CHF_CMD_TD_ADDCHANGEINT;
    addreq->io_Flags = 0;
    addreq->io_Length = sizeof(struct Interrupt);
    addreq->io_Data = &change_interrupt;
    SendIO((struct IORequest *)addreq); /* never completes until REMCHANGEINT */

    req1->io_Command = CHF_CMD_TD_EJECT;
    req1->io_Length = 1; /* non-zero: eject (0 would be an "insert" no-op) */
    DoIO((struct IORequest *)req1);
    if (req1->io_Error != 0)
        return FALSE;

    fired = FALSE;
    for (spins = 0; spins < 2000000UL; spins++) {
        if (change_fired) {
            fired = TRUE;
            break;
        }
    }
    if (!fired)
        return FALSE;

    req1->io_Command = CHF_CMD_TD_REMCHANGEINT;
    req1->io_Length = sizeof(struct Interrupt);
    req1->io_Data = &change_interrupt; /* matches addreq->io_Data */
    DoIO((struct IORequest *)req1);
    if (req1->io_Error != 0)
        return FALSE;

    // The removed ADDCHANGEINT was ReplyMsg'd synchronously inside that
    // DoIO (device.c's chf_do_remchangeint) -- reap it so DeleteMsgPort
    // below never sees an outstanding message.
    reply = GetMsg(addport);
    if (reply == NULL)
        return FALSE;

    // Confirm the eject actually took effect: unit 1 now reports no media.
    req1->io_Command = CHF_CMD_TD_CHANGESTATE;
    req1->io_Length = 0;
    DoIO((struct IORequest *)req1);
    return req1->io_Error == 0 && req1->io_Actual == 1;
}

int main(void)
{
    BOOL opened0 = FALSE, opened1 = FALSE, openedadd = FALSE;
    BOOL all_ok = TRUE;

    port0 = CreateMsgPort();
    port1 = CreateMsgPort();
    addport = CreateMsgPort();
    if (!port0 || !port1 || !addport)
        return 20;

    req0 = (struct IOStdReq *)CreateExtIO(port0, sizeof(struct IOStdReq));
    req1 = (struct IOStdReq *)CreateExtIO(port1, sizeof(struct IOStdReq));
    addreq = (struct IOStdReq *)CreateExtIO(addport, sizeof(struct IOStdReq));
    if (!req0 || !req1 || !addreq)
        return 20;

    if (OpenDevice((CONST_STRPTR) "copperhf.device", UNIT_DATA, (struct IORequest *)req0, 0) == 0)
        opened0 = TRUE;
    if (OpenDevice((CONST_STRPTR) "copperhf.device", UNIT_CHANGE, (struct IORequest *)req1, 0) == 0)
        opened1 = TRUE;
    if (opened1 &&
        OpenDevice((CONST_STRPTR) "copperhf.device", UNIT_CHANGE, (struct IORequest *)addreq, 0) ==
            0)
        openedadd = TRUE;

    if (opened0 && opened1 && openedadd) {
        readbuf = (UBYTE *)AllocMem(BLOCK_SIZE, MEMF_PUBLIC | MEMF_CLEAR);
        writebuf = (UBYTE *)AllocMem(BLOCK_SIZE, MEMF_PUBLIC | MEMF_CLEAR);

        if (readbuf && writebuf) {
            all_ok &= write_marker(BLK_NSDQ, test_devicequery(), "M4-NSDQ-OK", "M4-NSDQ-BAD");
            all_ok &= write_marker(BLK_CHANGENUM, test_changenum(), "M4-CHGNUM-OK",
                                    "M4-CHGNUM-BAD");
            all_ok &= write_marker(BLK_CHANGESTATE, test_changestate(), "M4-CHGSTATE-OK",
                                    "M4-CHGSTATE-BAD");
            all_ok &= write_marker(BLK_PROTSTATUS, test_protstatus(), "M4-PROTSTAT-OK",
                                    "M4-PROTSTAT-BAD");
            all_ok &= write_marker(BLK_READ64, test_read64(), "M4-READ64-OK", "M4-READ64-BAD");
            all_ok &= write_marker(BLK_SCSI_INQUIRY, test_scsi_inquiry(), "M4-INQUIRY-OK",
                                    "M4-INQUIRY-BAD");
            all_ok &= write_marker(BLK_SCSI_READCAP, test_scsi_readcap(), "M4-READCAP-OK",
                                    "M4-READCAP-BAD");
            all_ok &= write_marker(BLK_CHANGEINT_STORY, test_change_interrupt_story(),
                                    "M4-CHGINT-OK", "M4-CHGINT-BAD");
            all_ok &= write_marker(BLK_SCSI_SENSE, test_scsi_autosense(), "M4-SENSE-OK",
                                    "M4-SENSE-BAD");

            // Flush every marker to the backing image before the host test
            // reads the file.
            req0->io_Command = CMD_UPDATE;
            req0->io_Length = 0;
            req0->io_Offset = 0;
            DoIO((struct IORequest *)req0);
        } else {
            all_ok = FALSE;
        }

        if (readbuf)
            FreeMem(readbuf, BLOCK_SIZE);
        if (writebuf)
            FreeMem(writebuf, BLOCK_SIZE);
    } else {
        all_ok = FALSE;
    }

    if (openedadd)
        CloseDevice((struct IORequest *)addreq);
    if (opened1)
        CloseDevice((struct IORequest *)req1);
    if (opened0)
        CloseDevice((struct IORequest *)req0);

    DeleteExtIO((struct IORequest *)addreq);
    DeleteExtIO((struct IORequest *)req1);
    DeleteExtIO((struct IORequest *)req0);
    DeleteMsgPort(addport);
    DeleteMsgPort(port1);
    DeleteMsgPort(port0);

    return all_ok ? 0 : 20;
}
