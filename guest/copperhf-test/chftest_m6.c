// SPDX-License-Identifier: GPL-3.0-or-later
//
// chftest_m6: guest-side end-to-end probe for copperhf.device's M3
// boot-ROM mounter's FSHD/LSEG path (guest/copperhf/mounter.c's
// chf_find_or_load_filesystem + chf_load_lseg_chain), exercised here for
// the first time by the M6 FFS-from-LSEG matrix axis
// (COPPERHF-DEVICE-PLAN.md). Unlike chftest/chftest_m4, this probe does
// not talk to copperhf.device's command protocol at all except to write
// its own markers -- everything it checks is the *result* of the ROM's
// own boot-time mount, already done by the time AROS reaches `--run` and
// hands this probe control: FileSystem.resource, the DOS device list,
// and the loaded seglist's own code.
//
// --- Image contract (read by the Rust-side worker building the fixture
//     unit0 image; guest/copperhf-test/gen_lsegfix.py generates the
//     hunk file this contract's LSEG chain must carry byte-for-byte) ---
//
//   Unit 0 is an ordinary RDB image (no host-side synthesized wrapper --
//   src/harddrive.rs only synthesizes an RDB around a *bare* partition;
//   an image that already opens with "RDSK" at block 0 passes through
//   unmodified, so the file's raw block numbers below are exactly what
//   the guest ROM reads):
//
//     block 0:  RDSK. rdb_PartitionList = 1, rdb_FileSysHeaderList = 2.
//     block 1:  PART. pb_DriveName = "CHFM6" (subtest 3 names it
//               explicitly, "CHFM6:", via Lock() -- any name works as far
//               as mounter.c is concerned, but the Rust worker
//               (tests/copperhf_m6.rs) and this file must agree
//               byte-for-byte). de_DosType (env[16]) =
//               TEST_DOSTYPE, PBFF_BOOTABLE CLEAR (this probe arrives via
//               ordinary --run staging, not autoboot -- see mounter.c's
//               AddBootNode call site for what PBFF_BOOTABLE does and
//               does not prevent), PBFF_NOMOUNT clear. Partition spans
//               cylinder 1 only (unused/empty -- no real filesystem
//               payload is needed since nothing ever reads through the
//               mounted handler beyond the packet exchange subtest 3
//               triggers; see the marker-block note below).
//     block 2:  FSHD. fhb_DosType = TEST_DOSTYPE, fhb_Version =
//               TEST_FSVERSION, fhb_PatchFlags = TEST_PATCHFLAGS,
//               fhb_GlobalVec = -1, fhb_SegListBlocks = 3, fhb_Next =
//               -1 (only FSHD on the list).
//     block 3:  LSEG. lsb_ID = 'LSEG', lsb_Next = -1 (chain is exactly
//               one block: gen_lsegfix.py's fixture is 160 bytes, well
//               under one 492-byte LoadSegBlock payload).
//               lsb_LoadData[0..40) = guest/copperhf-test/lsegfix's
//               bytes verbatim, zero-padded to the full 492-byte
//               payload.
//     blocks 4..511: unused (rest of the RDB cylinder).
//     block 512+: start of the partition's own cylinder -- carries no
//               filesystem at all in this fixture, so it is exactly
//               where this probe's own markers live (one block per
//               subtest, listed below); nothing the mounter or a real
//               filesystem driver ever touches collides with them.
//
//   TEST_DOSTYPE    0x54535401 ('TST\1')
//   TEST_FSVERSION  0x0001000A (1.10)
//   TEST_PATCHFLAGS 0x180 (FSE_PF_SEGLIST | FSE_PF_GLOBALVEC, i.e.
//                   "substitute SegList & GlobalVec" -- filesysres.doc's
//                   own canonical example value)
//   TEST_GLOBALVEC  -1 (not a BCPL program)
//   FIXTURE_HUNKS   3 (gen_lsegfix.py: CODE, DATA, BSS)
//   FIXTURE_MAGIC   0x88911FD5 (gen_lsegfix.py's MAGIC = CONST_A ^
//                   BSS_PATTERN ^ MARKER_VAL; regenerate both files
//                   together if this ever changes)
//
// Marker blocks (unit 0, one per subtest, same fill_marker/CMD_WRITE
// idiom as chftest/chftest_m4 -- 0xC5 fill with an ASCII tag prefix):
//
//   block 512  BLK_FSR       FileSystem.resource entry found, version
//                             and patchflags match.
//   block 513  BLK_SEGCHAIN  fse_SegList non-null, chain walks cleanly,
//                             segment count == FIXTURE_HUNKS.
//   block 514  BLK_EXEC      Lock("CHFM6:", ACCESS_READ) fails and
//                             IoErr() == FIXTURE_MAGIC -- the core
//                             relocation-correctness check, reached via
//                             an ordinary DOS packet exchange with the
//                             handler process AmigaDOS itself started
//                             from fse_SegList (see test_exec's own
//                             comment for why this isn't a direct call).
//   block 515  BLK_DEVNODE   a DeviceNode with dn_SegList == fse_SegList
//                             exists on the DOS device list, and
//                             dn_GlobalVec == TEST_GLOBALVEC (proof
//                             chf_apply_patch actually ran with
//                             PatchFlags 0x180).
//
// 68000-only, ordinary -noixemul relocatable hunk executable -- this is
// a plain application, not ROM code, so none of guest/copperhf/'s own
// no-data/bss/no-relocations discipline applies here (same reasoning as
// chftest_m4.c's own header comment).

#include <exec/types.h>
#include <exec/io.h>
#include <exec/errors.h>
#include <exec/memory.h>
#include <exec/nodes.h>
#include <dos/dos.h>
#include <dos/dosextens.h>
#include <resources/filesysres.h>
#include <proto/exec.h>
#include <proto/dos.h>
#include <clib/alib_protos.h>

#define UNIT_DATA 0
#define BLOCK_SIZE 512

#define TEST_DOSTYPE 0x54535401UL
#define TEST_FSVERSION 0x0001000AUL
#define TEST_PATCHFLAGS 0x180UL
#define TEST_GLOBALVEC (-1L)
#define FIXTURE_HUNKS 3
#define FIXTURE_MAGIC 0x88911FD5UL
#define FIXTURE_MAX_HUNKS 16 /* mirrors mounter.c's own CHF_MAX_HUNKS bound */

#define BLK_FSR 512
#define BLK_SEGCHAIN 513
#define BLK_EXEC 514
#define BLK_DEVNODE 515

static struct MsgPort *port0;
static struct IOStdReq *req0;
static UBYTE *writebuf;

static BPTR g_seglist = 0; /* handed from test_fsr() to the later subtests */

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

// Subtest 1: FileSystem.resource carries an entry for TEST_DOSTYPE with
// the version/patchflags the FSHD block declared -- proof
// chf_find_or_load_filesystem actually parsed the FSHD and built a
// FileSysEntry from it (rather than, say, silently finding nothing and
// this probe accidentally passing on an absent resource).
static BOOL test_fsr(void)
{
    struct FileSysResource *fsr;
    struct FileSysEntry *fse;

    fsr = (struct FileSysResource *)OpenResource((CONST_STRPTR)FSRNAME);
    if (fsr == NULL)
        return FALSE;

    for (fse = (struct FileSysEntry *)fsr->fsr_FileSysEntries.lh_Head;
         fse->fse_Node.ln_Succ != NULL; fse = (struct FileSysEntry *)fse->fse_Node.ln_Succ) {
        if (fse->fse_DosType == TEST_DOSTYPE) {
            if (fse->fse_Version != TEST_FSVERSION)
                return FALSE;
            if (fse->fse_PatchFlags != TEST_PATCHFLAGS)
                return FALSE;
            g_seglist = fse->fse_SegList;
            return TRUE;
        }
    }
    return FALSE;
}

// Subtest 2: the segment chain fse_SegList points at is walkable (BPTR
// links, terminated by 0) and has exactly FIXTURE_HUNKS segments --
// gen_lsegfix.py's CODE/DATA/BSS, one AllocMem block each
// (chf_load_lseg_chain's seglist layout: size long, next BPTR, then the
// hunk's own bytes).
static BOOL test_segchain(void)
{
    BPTR seg;
    int count;

    if (g_seglist == 0)
        return FALSE;

    // Each segment's link word (mounter.c's "next-BPTR link", the
    // longword sitting at BADDR(seg)) is itself already a BPTR (0 =
    // last) -- BADDR() converts a BPTR into a real address for the read,
    // and the value read back is the next segment's BPTR directly, no
    // further conversion needed.
    seg = g_seglist;
    count = 0;
    while (seg != 0) {
        ULONG *link = (ULONG *)BADDR(seg);
        count++;
        if (count > FIXTURE_MAX_HUNKS)
            return FALSE; /* cyclic/corrupt chain: bail rather than hang */
        seg = (BPTR)link[0];
    }
    return count == FIXTURE_HUNKS;
}

// Subtest 3: the core relocation-correctness check -- reached through an
// ordinary dos.library call, NOT by calling fse_SegList's entry point
// directly. gen_lsegfix.py's entry point is loaded as a DeviceNode's
// dn_SegList, so AmigaDOS itself starts it as a real (if trivial)
// filesystem-handler process the first time anything references the
// device (AmigaDOS Manual/rkrm-dos, "Starting a Handler": dol_GlobVec ==
// -1 means "the process is started from the first byte of dol_SegList",
// with the startup packet delivered to that process's own pr_MsgPort,
// not passed as a function argument) -- HARD-WON (M6): an earlier
// version of this probe called the entry directly as a plain function
// and read D0, which worked for THIS probe's own call, but the very
// same DeviceNode also gets automatically referenced by AmigaDOS during
// ordinary boot (independent of PBFF_BOOTABLE, ADNF_STARTPROC, or
// ConfigDev -- see mounter.c's own history comment at its AddBootNode
// call site for what was ruled out), which starts a SECOND, real
// process race against the same entry point; a bare "return a value"
// contract cannot answer both callers, so the fixture's entry now
// always behaves as a (permanently failing) real handler process, and
// this subtest talks to it exactly as any DOS client would: Lock() on
// the mounted device starts the handler if it is not already running --
// which first exchanges a private ACTION_STARTUP packet with
// GetDeviceProc() (gen_lsegfix.py's entry replies that one with
// DOSTRUE/0, or dos.library gives up with a generic
// ERROR_DEVICE_NOT_MOUNTED before ever forwarding anything else -- see
// that file's own header comment for the two failed attempts that
// found this) -- and only then sends the real ACTION_LOCATE_OBJECT
// packet Lock() itself waits on. Every packet after the first gets
// dp_Res1 = DOSFALSE, dp_Res2 = FIXTURE_MAGIC, so Lock() fails (returns
// 0, as expected) and IoErr() surfaces that dp_Res2 -- the very same
// relocation-derived value, reached the same way a real client's failed
// Lock() would see it.
static BOOL test_exec(void)
{
    BPTR lock;
    LONG err;

    if (g_seglist == 0)
        return FALSE;

    lock = Lock((CONST_STRPTR) "CHFM6:", ACCESS_READ);
    if (lock != 0) {
        UnLock(lock);
        return FALSE; /* a real filesystem would never live here */
    }
    err = IoErr();
    return (ULONG)err == FIXTURE_MAGIC;
}

// Subtest 4: the partition with TEST_DOSTYPE was actually mounted as a
// DeviceNode, and chf_apply_patch ran on it with PatchFlags 0x180 --
// found by matching dn_SegList against the same fse_SegList subtest 1
// already confirmed (robust regardless of whatever device name the
// mounter assigned; copperhf.device's own device_name), rather than by
// guessing a volume/device name this probe has no other way to learn.
static BOOL test_devnode(void)
{
    struct DosList *dl;
    BOOL found = FALSE;

    if (g_seglist == 0)
        return FALSE;

    dl = LockDosList(LDF_DEVICES | LDF_READ);
    for (; dl != NULL; dl = NextDosEntry(dl, LDF_DEVICES)) {
        if (dl->dol_misc.dol_handler.dol_SegList == g_seglist) {
            found = dl->dol_misc.dol_handler.dol_GlobVec == TEST_GLOBALVEC;
            break;
        }
    }
    UnLockDosList(LDF_DEVICES | LDF_READ);
    return found;
}

int main(void)
{
    BOOL opened0 = FALSE;
    BOOL all_ok = TRUE;
    struct Process *self = (struct Process *)FindTask(NULL);
    APTR old_window_ptr = self->pr_WindowPtr;

    // HARD-WON (M6): test_exec()'s Lock("CHFM6:", ...) fails because our
    // fixture's handler always replies its startup packet with
    // dp_Res1 = DOSFALSE -- exactly what a real hard-disk partition
    // whose filesystem can't identify the media does too, and AmigaDOS's
    // reaction to *that* is not a quiet failure but the standard
    // "Please insert volume CHFM6 in any drive" system requester
    // (AmigaDOS Manual/rkrm-dos, "Locating a Handler from a Path": "it
    // is then requested from the user if pr_WindowPtr allows to do so").
    // Nothing can click Retry/Cancel headless, so left alone this wedges
    // --run forever. pr_WindowPtr = -1 is the documented way a
    // non-interactive program disables every DOS system requester --
    // Lock() then just fails and sets IoErr(), which is exactly the path
    // test_exec() wants. Restored before returning, in case anything
    // else this process does later relies on the normal default.
    self->pr_WindowPtr = (APTR)-1L;

    port0 = CreateMsgPort();
    if (!port0)
        return 20;

    req0 = (struct IOStdReq *)CreateExtIO(port0, sizeof(struct IOStdReq));
    if (!req0) {
        DeleteMsgPort(port0);
        return 20;
    }

    if (OpenDevice((CONST_STRPTR) "copperhf.device", UNIT_DATA, (struct IORequest *)req0, 0) == 0)
        opened0 = TRUE;

    if (opened0) {
        writebuf = (UBYTE *)AllocMem(BLOCK_SIZE, MEMF_PUBLIC | MEMF_CLEAR);

        if (writebuf) {
            all_ok &= write_marker(BLK_FSR, test_fsr(), "M6-FSR-OK", "M6-FSR-BAD");
            all_ok &=
                write_marker(BLK_SEGCHAIN, test_segchain(), "M6-SEGCHAIN-OK", "M6-SEGCHAIN-BAD");
            all_ok &= write_marker(BLK_EXEC, test_exec(), "M6-EXEC-OK", "M6-EXEC-BAD");
            all_ok &= write_marker(BLK_DEVNODE, test_devnode(), "M6-DEVNODE-OK", "M6-DEVNODE-BAD");

            // Flush every marker to the backing image before the host
            // test reads the file.
            req0->io_Command = CMD_UPDATE;
            req0->io_Length = 0;
            req0->io_Offset = 0;
            DoIO((struct IORequest *)req0);
        } else {
            all_ok = FALSE;
        }

        if (writebuf)
            FreeMem(writebuf, BLOCK_SIZE);
    } else {
        all_ok = FALSE;
    }

    if (opened0)
        CloseDevice((struct IORequest *)req0);

    self->pr_WindowPtr = old_window_ptr;

    DeleteExtIO((struct IORequest *)req0);
    DeleteMsgPort(port0);

    return all_ok ? 0 : 20;
}
