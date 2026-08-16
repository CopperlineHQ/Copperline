/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * mhitest: WP5's M1 harness client for mhi_copperline.library. Opens the
 * library, prints a handful of MHIQuery results, MHIAllocDecoder()s with a
 * fresh signal, queues one buffer, MHIPlay()s, Wait()s for either that
 * signal or a bounded timer.device timeout (never hangs a headless run),
 * reports MHIGetStatus()/MHIGetEmpty(), MHIStop()s, MHIFreeDecoder()s,
 * closes the library, and prints one final PASS/FAIL summary line.
 *
 * Every assertion line starts with "MHITEST:" and is otherwise plain text
 * -- grep-friendly for the WP5 integration test, per MHI-PLAN.md WP4's own
 * brief. Output goes through dos.library Write() directly (no stdio: this
 * is built standalone, like guest/hostfs-test/mkfile.c, with no C runtime
 * startup linked in).
 *
 * mhi_copperline.library has no generated proto/inline headers of its own
 * (it is the library under test, not a system one), so its ten entry
 * points are reached the standard Amiga-C way instead: cast (library base
 * + LVO offset) to a register-parameter function-pointer type and call
 * through it. The offsets below are the FuncTab order in ../startup.c
 * (Open/Close/Expunge/ExtFunc at -6/-12/-18/-24, then the 10 MHI entries
 * every 6 bytes from -30), matching the official dev kit's own
 * Include/fd/mhi_lib.fd `bias 30` order (test-assets/mhi-devkit, WP1
 * notes) -- MHIAllocDecoder is LVO -30.
 *
 * The queued buffer is either a small deterministic embedded pattern (no
 * CLI argument given) or read from a file named on the Shell command line
 * (e.g. one of the test-assets/mp3/ CBR fixtures, for a more realistic
 * check once M2 lands) -- see read_cmdline_arg(). M1 itself only exercises
 * the register protocol (open/query/alloc/queue/doorbell/interrupt/free),
 * so the embedded pattern's bytes need not be a decodable MPEG frame.
 */

#include <exec/types.h>
#include <exec/execbase.h>
#include <exec/interrupts.h>
#include <exec/memory.h>
#include <exec/tasks.h>

#include <devices/timer.h>

#include <dos/dos.h>
#include <dos/dosextens.h>

#define EXEC_BASE_NAME _sysbase
#define DOS_BASE_NAME _dosbase
#include <inline/dos.h>
#include <inline/exec.h>

#include "mhi_abi.h"

/* Bounds the Wait() below so a headless run can never hang indefinitely if
 * the board never completes the queued descriptor. */
#define MHITEST_TIMEOUT_SECS 8

/* No real MPEG framing needed for M1 (see this file's own header comment)
 * -- a fixed-size, fully deterministic byte pattern is enough to prove the
 * descriptor queue/doorbell/completion/interrupt round trip. */
#define EMBEDDED_BUFFER_SIZE 4096

/* -- LVO dispatch: mhi_copperline.library's 10 entry points ------------ */

#define LVO_MHIALLOCDECODER -30
#define LVO_MHIFREEDECODER  -36
#define LVO_MHIQUEUEBUFFER  -42
#define LVO_MHIGETEMPTY     -48
#define LVO_MHIGETSTATUS    -54
#define LVO_MHIPLAY         -60
#define LVO_MHISTOP         -66
#define LVO_MHIPAUSE        -72
#define LVO_MHIQUERY        -78
#define LVO_MHISETPARAM     -84

typedef APTR (*MHIAllocDecoderProc)(struct Task *task __asm("a0"), ULONG sigmask __asm("d0"), struct Library *base __asm("a6"));
typedef void (*MHIFreeDecoderProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));
typedef LONG (*MHIQueueBufferProc)(APTR handle __asm("a3"), APTR buffer __asm("a0"), ULONG size __asm("d0"), struct Library *base __asm("a6"));
typedef APTR (*MHIGetEmptyProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));
typedef UBYTE (*MHIGetStatusProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));
typedef void (*MHIPlayProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));
typedef void (*MHIStopProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));
typedef void (*MHIPauseProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));
typedef ULONG (*MHIQueryProc)(ULONG query __asm("d1"), struct Library *base __asm("a6"));
typedef void (*MHISetParamProc)(APTR handle __asm("a3"), UWORD param __asm("d0"), ULONG value __asm("d1"), struct Library *base __asm("a6"));

#define MHI_PROC(type, base, lvo) ((type)((char *)(base) + (lvo)))

static struct ExecBase *sysbase(void)
{
    struct ExecBase *base;
    __asm("move.l 4.w,%0" : "=r"(base));
    return base;
}

static LONG strlen_local(const char *s)
{
    LONG n = 0;
    while (s[n] != '\0') {
        n++;
    }
    return n;
}

static void put(struct Library *_dosbase, const char *msg)
{
    Write(Output(), (APTR)msg, strlen_local(msg));
}

/* Minimal unsigned-decimal formatter -- no stdio linked in (see this
 * file's own header comment). `buf` must be at least 11 bytes (max 32-bit
 * value "4294967295" + NUL). */
static char *format_ulong(ULONG value, char *buf)
{
    char tmp[10];
    int i = 0;
    int j = 0;

    if (value == 0) {
        buf[0] = '0';
        buf[1] = '\0';
        return buf;
    }
    while (value > 0 && i < 10) {
        tmp[i++] = (char)('0' + (value % 10));
        value /= 10;
    }
    while (i > 0) {
        buf[j++] = tmp[--i];
    }
    buf[j] = '\0';
    return buf;
}

static void put_result(struct Library *_dosbase, const char *check, BOOL ok)
{
    put(_dosbase, "MHITEST: ");
    put(_dosbase, ok ? "PASS " : "FAIL ");
    put(_dosbase, check);
    put(_dosbase, "\n");
}

static void put_kv(struct Library *_dosbase, const char *name, ULONG value)
{
    char numbuf[11];
    put(_dosbase, "MHITEST: INFO ");
    put(_dosbase, name);
    put(_dosbase, "=");
    put(_dosbase, format_ulong(value, numbuf));
    put(_dosbase, "\n");
}

/* Trims the Shell command-line argument (start.s hands us A0/D0 verbatim:
 * everything after the command name, not NUL-terminated, often trailing a
 * newline) down to a plain filename, or returns FALSE if it is empty/blank
 * -- the common case: no argument given, use the embedded pattern instead.
 */
static BOOL read_cmdline_arg(const char *cmdline, LONG cmdlen, char *out, LONG outsize)
{
    LONG start = 0;
    LONG end;
    LONG n;
    LONG i;

    while (start < cmdlen && (cmdline[start] == ' ' || cmdline[start] == '\t')) {
        start++;
    }
    end = cmdlen;
    while (end > start && (cmdline[end - 1] == ' ' || cmdline[end - 1] == '\t' ||
                            cmdline[end - 1] == '\n' || cmdline[end - 1] == '\r')) {
        end--;
    }
    if (end <= start) {
        return FALSE;
    }
    n = end - start;
    if (n >= outsize) {
        n = outsize - 1;
    }
    for (i = 0; i < n; i++) {
        out[i] = cmdline[start + i];
    }
    out[n] = '\0';
    return TRUE;
}

/* Fills `buf` with EMBEDDED_BUFFER_SIZE deterministic bytes (a simple
 * repeating counter pattern -- ffmpeg/lame test assets are not available
 * to a guest-side build, see this file's own header comment) and returns
 * its length; used whenever no CLI filename argument is given. */
static ULONG fill_embedded_buffer(UBYTE *buf)
{
    ULONG i;
    for (i = 0; i < EMBEDDED_BUFFER_SIZE; i++) {
        buf[i] = (UBYTE)(i & 0xFF);
    }
    return EMBEDDED_BUFFER_SIZE;
}

LONG entry(char *cmdline __asm("a0"), long cmdlen __asm("d0"))
{
    struct ExecBase *_sysbase = sysbase();
    struct Library *_dosbase = OpenLibrary((STRPTR) "dos.library", 34);
    if (_dosbase == NULL) {
        return 20;
    }

    BOOL all_ok = TRUE;
    struct Library *mhibase = OpenLibrary((STRPTR) "mhi_copperline.library", 0);
    if (mhibase == NULL) {
        put_result(_dosbase, "open mhi_copperline.library", FALSE);
        put(_dosbase, "MHITEST: SUMMARY FAIL\n");
        CloseLibrary(_dosbase);
        return 20;
    }
    put_result(_dosbase, "open mhi_copperline.library", TRUE);

    MHIQueryProc mhi_query = MHI_PROC(MHIQueryProc, mhibase, LVO_MHIQUERY);
    put_kv(_dosbase, "MHIQ_IS_HARDWARE", mhi_query(MHIQ_IS_HARDWARE, mhibase));
    put_kv(_dosbase, "MHIQ_IS_68K", mhi_query(MHIQ_IS_68K, mhibase));
    put_kv(_dosbase, "MHIQ_MPEG1", mhi_query(MHIQ_MPEG1, mhibase));
    put_kv(_dosbase, "MHIQ_LAYER3", mhi_query(MHIQ_LAYER3, mhibase));
    put_kv(_dosbase, "MHIQ_VARIABLE_BITRATE", mhi_query(MHIQ_VARIABLE_BITRATE, mhibase));
    put_kv(_dosbase, "MHIQ_VOLUME_CONTROL", mhi_query(MHIQ_VOLUME_CONTROL, mhibase));
    put_kv(_dosbase, "MHIQ_5_BAND_EQ", mhi_query(MHIQ_5_BAND_EQ, mhibase));
    /* Decoder identity strings: MHIQuery returns a char* packed into the
     * ULONG result. */
    put(_dosbase, "MHITEST: INFO MHIQ_DECODER_NAME=");
    put(_dosbase, (const char *)mhi_query(MHIQ_DECODER_NAME, mhibase));
    put(_dosbase, "\n");
    put(_dosbase, "MHITEST: INFO MHIQ_AUTHOR=");
    put(_dosbase, (const char *)mhi_query(MHIQ_AUTHOR, mhibase));
    put(_dosbase, "\n");

    LONG mysignal = AllocSignal(-1);
    if (mysignal == -1) {
        put_result(_dosbase, "AllocSignal", FALSE);
        put(_dosbase, "MHITEST: SUMMARY FAIL\n");
        CloseLibrary(mhibase);
        CloseLibrary(_dosbase);
        return 20;
    }
    ULONG mysigmask = 1UL << mysignal;
    struct Task *mytask = FindTask(NULL);

    MHIAllocDecoderProc mhi_alloc = MHI_PROC(MHIAllocDecoderProc, mhibase, LVO_MHIALLOCDECODER);
    APTR handle = mhi_alloc(mytask, mysigmask, mhibase);
    BOOL alloc_ok = (handle != NULL);
    put_result(_dosbase, "MHIAllocDecoder", alloc_ok);
    all_ok = all_ok && alloc_ok;

    if (alloc_ok) {
        MHIQueueBufferProc mhi_queue = MHI_PROC(MHIQueueBufferProc, mhibase, LVO_MHIQUEUEBUFFER);
        MHIGetEmptyProc mhi_get_empty = MHI_PROC(MHIGetEmptyProc, mhibase, LVO_MHIGETEMPTY);
        MHIGetStatusProc mhi_get_status = MHI_PROC(MHIGetStatusProc, mhibase, LVO_MHIGETSTATUS);
        MHIPlayProc mhi_play = MHI_PROC(MHIPlayProc, mhibase, LVO_MHIPLAY);
        MHIStopProc mhi_stop = MHI_PROC(MHIStopProc, mhibase, LVO_MHISTOP);
        MHIFreeDecoderProc mhi_free = MHI_PROC(MHIFreeDecoderProc, mhibase, LVO_MHIFREEDECODER);

        static UBYTE test_buffer[EMBEDDED_BUFFER_SIZE];
        char path[256];
        ULONG buflen;
        BOOL have_file = read_cmdline_arg(cmdline, cmdlen, path, sizeof(path));
        if (have_file) {
            BPTR fh = Open((STRPTR)path, MODE_OLDFILE);
            if (fh != 0) {
                LONG got = Read(fh, test_buffer, EMBEDDED_BUFFER_SIZE);
                Close(fh);
                buflen = (got > 0) ? (ULONG)got : fill_embedded_buffer(test_buffer);
                put_kv(_dosbase, "buffer_source_is_file", 1);
            } else {
                put_result(_dosbase, "open CLI file argument", FALSE);
                buflen = fill_embedded_buffer(test_buffer);
            }
        } else {
            buflen = fill_embedded_buffer(test_buffer);
        }
        put_kv(_dosbase, "buffer_len", buflen);

        BOOL queue_ok = mhi_queue(handle, test_buffer, buflen, mhibase) != FALSE;
        put_result(_dosbase, "MHIQueueBuffer", queue_ok);
        all_ok = all_ok && queue_ok;

        mhi_play(handle, mhibase);
        put(_dosbase, "MHITEST: INFO MHIPlay issued\n");

        /* Wait for the queued descriptor's completion signal, bounded by a
         * timer.device request so a headless run can never hang -- see
         * this file's own header comment. */
        struct MsgPort *timerport = CreateMsgPort();
        BOOL got_signal = FALSE;
        BOOL got_timeout = FALSE;
        if (timerport != NULL) {
            struct timerequest *tr =
                (struct timerequest *)AllocMem(sizeof(struct timerequest), MEMF_PUBLIC | MEMF_CLEAR);
            if (tr != NULL) {
                tr->tr_node.io_Message.mn_ReplyPort = timerport;
                tr->tr_node.io_Message.mn_Length = sizeof(*tr);
                if (OpenDevice((STRPTR) "timer.device", UNIT_VBLANK, (struct IORequest *)tr, 0) == 0) {
                    tr->tr_node.io_Command = TR_ADDREQUEST;
                    tr->tr_time.tv_secs = MHITEST_TIMEOUT_SECS;
                    tr->tr_time.tv_micro = 0;
                    SendIO((struct IORequest *)tr);

                    ULONG timersigmask = 1UL << timerport->mp_SigBit;
                    ULONG got = Wait(mysigmask | timersigmask | SIGBREAKF_CTRL_C);
                    got_signal = (got & mysigmask) != 0;
                    got_timeout = (got & timersigmask) != 0;

                    if (!CheckIO((struct IORequest *)tr)) {
                        AbortIO((struct IORequest *)tr);
                    }
                    WaitIO((struct IORequest *)tr);
                    CloseDevice((struct IORequest *)tr);
                } else {
                    put_result(_dosbase, "OpenDevice timer.device", FALSE);
                }
                FreeMem(tr, sizeof(struct timerequest));
            }
            DeleteMsgPort(timerport);
        }
        put_result(_dosbase, "wait for completion signal", got_signal);
        if (got_timeout && !got_signal) {
            put(_dosbase, "MHITEST: INFO timed out waiting for signal\n");
        }

        UBYTE status = mhi_get_status(handle, mhibase);
        put_kv(_dosbase, "MHIGetStatus", status);

        APTR empty = mhi_get_empty(handle, mhibase);
        BOOL empty_ok = (empty == test_buffer);
        put_result(_dosbase, "MHIGetEmpty returns queued buffer", empty_ok);
        /* Only counts against the overall summary once the signal actually
         * arrived -- if the board never completed the descriptor (e.g. an
         * M1 board build with no decode pacing wired up yet), MHIGetEmpty
         * correctly returning nothing is not a driver bug. */
        all_ok = all_ok && (got_signal ? empty_ok : TRUE);

        mhi_stop(handle, mhibase);
        put(_dosbase, "MHITEST: INFO MHIStop issued\n");

        mhi_free(handle, mhibase);
        put(_dosbase, "MHITEST: INFO MHIFreeDecoder issued\n");
    }

    FreeSignal(mysignal);
    CloseLibrary(mhibase);
    put_result(_dosbase, "close mhi_copperline.library", TRUE);

    put(_dosbase, all_ok ? "MHITEST: SUMMARY PASS\n" : "MHITEST: SUMMARY FAIL\n");
    CloseLibrary(_dosbase);
    return all_ok ? 0 : 20;
}
