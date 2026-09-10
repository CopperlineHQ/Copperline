// SPDX-License-Identifier: GPL-3.0-or-later
//
// cliptest: guest-side probe for the clipboard integration test
// (tests/clipboard.rs). Posts "Hello from the guest" to clipboard.device
// unit 0 as an IFF FTXT clip -- which the services ROM's bridge should push
// to the host -- then re-reads the clip twice a second for up to a minute
// until one appears that is not its own, which the host side stages
// through `clipboard.set`. The text of that clip is written to
// clip-out.txt in the current directory (RunProg:, the host directory the
// program was run from), so the host test can read it back. Returns 0 on
// success, 10 on timeout, 20 on any failure.
//
// clipboard.device is disk-based (the bundled AROS ROM has none, nor do
// Kickstart 1.3 and 3.1), and the --run boot volume carries no Devs
// drawer: when a clipboard.device sits next to the probe (PROGDIR:), it
// is installed as DEVS:clipboard.device first -- creating SYS:Devs and
// the DEVS: assign if the boot volume lacks them -- so both the probe and
// the ROM bridge, which retries its open on a timer, can load it.
//
// Built standalone (no startup code): start.s, linked first, branches to
// entry() -- the CLI enters at the start of the hunk, which the compiler
// may fill with rodata rather than the first function.

#include <exec/io.h>
#include <exec/memory.h>
#include <exec/ports.h>
#include <exec/types.h>

#include <devices/clipboard.h>

#include <dos/dos.h>

#define __NOLIBBASE__
#define EXEC_BASE_NAME _sysbase
#define DOS_BASE_NAME _dosbase
#include <inline/dos.h>
#include <inline/exec.h>

#define IFF_ID(a, b, c, d) \
    (((ULONG)(a) << 24) | ((ULONG)(b) << 16) | ((ULONG)(c) << 8) | (ULONG)(d))

#define BUF_SIZE 4096

static struct ExecBase *sysbase(void);

static void init_port(struct MsgPort *port, struct Task *task, BYTE sigbit)
{
    port->mp_Node.ln_Type = NT_MSGPORT;
    port->mp_Flags = PA_SIGNAL;
    port->mp_SigBit = sigbit;
    port->mp_SigTask = task;
    port->mp_MsgList.lh_Head = (struct Node *)&port->mp_MsgList.lh_Tail;
    port->mp_MsgList.lh_Tail = NULL;
    port->mp_MsgList.lh_TailPred = (struct Node *)&port->mp_MsgList.lh_Head;
}

static BYTE clip_io(struct ExecBase *_sysbase, struct IOClipReq *req,
                    UWORD cmd, APTR data, ULONG len)
{
    req->io_Command = cmd;
    req->io_Data = data;
    req->io_Length = len;
    return DoIO((struct IORequest *)req);
}

// Post `text` as FORM FTXT { CHRS }; returns the clip ID it was given.
static LONG post_text(struct ExecBase *_sysbase, struct IOClipReq *req,
                      const char *text, ULONG len)
{
    ULONG hdr[5];
    hdr[0] = IFF_ID('F', 'O', 'R', 'M');
    hdr[1] = 4 + 8 + len + (len & 1);
    hdr[2] = IFF_ID('F', 'T', 'X', 'T');
    hdr[3] = IFF_ID('C', 'H', 'R', 'S');
    hdr[4] = len;
    req->io_Offset = 0;
    req->io_ClipID = 0;
    req->io_Error = 0;
    if (clip_io(_sysbase, req, CMD_WRITE, hdr, sizeof(hdr)) != 0)
        return -1;
    if (clip_io(_sysbase, req, CMD_WRITE, (APTR)text, len) != 0)
        return -1;
    if (len & 1) {
        UBYTE padbyte = 0;
        clip_io(_sysbase, req, CMD_WRITE, &padbyte, 1);
    }
    if (clip_io(_sysbase, req, CMD_UPDATE, NULL, 0) != 0)
        return -1;
    return req->io_ClipID;
}

// Read the current clip into `buf` (at most BUF_SIZE bytes; the rest is
// drained), returning its length, or -1 on error.
static LONG read_clip(struct ExecBase *_sysbase, struct IOClipReq *req,
                      UBYTE *buf)
{
    req->io_Offset = 0;
    req->io_ClipID = 0;
    req->io_Error = 0;
    LONG total = 0;
    for (;;) {
        APTR dst = total < BUF_SIZE ? buf + total : buf;
        ULONG want = total < BUF_SIZE ? BUF_SIZE - total : BUF_SIZE;
        if (clip_io(_sysbase, req, CMD_READ, dst, want) != 0)
            return -1;
        if (req->io_Actual == 0)
            break;
        total += req->io_Actual;
    }
    return total < BUF_SIZE ? total : BUF_SIZE;
}

// The first CHRS chunk of a FORM FTXT: its data and length, or NULL.
static const UBYTE *ftxt_chrs(const UBYTE *iff, LONG len, ULONG *out_len)
{
    if (len < 12 || iff[0] != 'F' || iff[1] != 'O' || iff[2] != 'R' ||
        iff[3] != 'M' || iff[8] != 'F' || iff[9] != 'T' || iff[10] != 'X' ||
        iff[11] != 'T')
        return NULL;
    LONG pos = 12;
    while (pos + 8 <= len) {
        ULONG clen = ((ULONG)iff[pos + 4] << 24) | ((ULONG)iff[pos + 5] << 16) |
                     ((ULONG)iff[pos + 6] << 8) | iff[pos + 7];
        if (iff[pos] == 'C' && iff[pos + 1] == 'H' && iff[pos + 2] == 'R' &&
            iff[pos + 3] == 'S') {
            if ((LONG)clen > len - pos - 8)
                clen = len - pos - 8;
            *out_len = clen;
            return iff + pos + 8;
        }
        pos += 8 + clen + (clen & 1);
    }
    return NULL;
}

static LONG write_file(struct Library *_dosbase,
                       const char *name, const UBYTE *data, ULONG len)
{
    BPTR fh = Open((STRPTR)name, MODE_NEWFILE);
    if (fh == 0)
        return 20;
    LONG rc = Write(fh, (APTR)data, len) == (LONG)len ? 0 : 20;
    Close(fh);
    return rc;
}

// Install PROGDIR:clipboard.device as DEVS:clipboard.device when the boot
// volume has none (see the header comment). V36+ DOS only (PROGDIR:,
// AssignLock); a V34 system is expected to bring its own device.
static void install_device(
    struct Library *_dosbase, UBYTE *buf)
{
    if (_dosbase->lib_Version < 36)
        return;
    BPTR have = Lock((STRPTR) "DEVS:clipboard.device", ACCESS_READ);
    if (have != 0) {
        UnLock(have);
        return;
    }
    BPTR src = Open((STRPTR) "PROGDIR:clipboard.device", MODE_OLDFILE);
    if (src == 0)
        return;
    BPTR devs = Lock((STRPTR) "SYS:Devs", ACCESS_READ);
    if (devs == 0)
        devs = CreateDir((STRPTR) "SYS:Devs");
    if (devs != 0)
        AssignLock((STRPTR) "DEVS", devs); // takes over the lock
    BPTR dst = Open((STRPTR) "DEVS:clipboard.device", MODE_NEWFILE);
    if (dst != 0) {
        LONG n;
        while ((n = Read(src, buf, BUF_SIZE)) > 0)
            if (Write(dst, buf, n) != n)
                break;
        Close(dst);
    }
    Close(src);
}

LONG entry(void)
{
    struct ExecBase *_sysbase = sysbase();
    struct Library *_dosbase = OpenLibrary((STRPTR) "dos.library", 34);
    if (_dosbase == NULL)
        return 20;
    LONG rc = 20;

    struct MsgPort *port = AllocMem(sizeof(*port), MEMF_PUBLIC | MEMF_CLEAR);
    struct IOClipReq *req = AllocMem(sizeof(*req), MEMF_PUBLIC | MEMF_CLEAR);
    UBYTE *buf = AllocMem(BUF_SIZE, MEMF_PUBLIC);
    BYTE sig = AllocSignal(-1);
    if (port == NULL || req == NULL || buf == NULL || sig < 0)
        goto out;
    init_port(port, FindTask(NULL), sig);
    req->io_Message.mn_ReplyPort = port;
    req->io_Message.mn_Length = sizeof(*req);
    install_device(_dosbase, buf);
    if (OpenDevice((STRPTR) "clipboard.device", PRIMARY_CLIP,
                   (struct IORequest *)req, 0) != 0)
        goto out;

    static const char hello[] = "Hello from the guest";
    LONG own = post_text(_sysbase, req, hello, sizeof(hello) - 1);
    if (own < 0)
        goto close;

    // Poll for a clip that is not ours: up to a minute, twice a second,
    // each poll a complete read cycle (a cycle once begun must run to
    // io_Actual == 0, or writers block on the clip).
    rc = 10;
    for (int i = 0; i < 120; i++) {
        Delay(25);
        LONG len = read_clip(_sysbase, req, buf);
        if (len < 0)
            continue;
        LONG id = req->io_ClipID;
        if (id == 0 || id == own)
            continue;
        ULONG tlen;
        const UBYTE *text = ftxt_chrs(buf, len, &tlen);
        if (text == NULL) {
            rc = write_file(_dosbase, "clip-out.txt",
                            (const UBYTE *)"NOT FTXT", 8);
            break;
        }
        rc = write_file(_dosbase, "clip-out.txt", text, tlen);
        break;
    }
    if (rc == 10)
        write_file(_dosbase, "clip-out.txt",
                   (const UBYTE *)"TIMEOUT", 7);

close:
    CloseDevice((struct IORequest *)req);
out:
    if (sig >= 0)
        FreeSignal(sig);
    if (buf != NULL)
        FreeMem(buf, BUF_SIZE);
    if (req != NULL)
        FreeMem(req, sizeof(*req));
    if (port != NULL)
        FreeMem(port, sizeof(*port));
    CloseLibrary(_dosbase);
    return rc;
}

// AbsExecBase; the asm sidesteps GCC's array-bounds warning about
// dereferencing address 4 (see guest/services/handler.c).
static struct ExecBase *sysbase(void)
{
    struct ExecBase *base;
    __asm("move.l 4.w,%0" : "=r"(base));
    return base;
}
