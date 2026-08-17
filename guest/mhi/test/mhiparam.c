/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * mhiparam: the M4 harness client for mhi_copperline.library. Plays
 * one small fixture, waits a fixed
 * emulated interval, then issues MHISetParam mid-playback for two params
 * at once (a hard volume drop and a hard pan-right) -- proving a live
 * MHISetParam call actually reaches the board's DSP chain
 * (docs/internals/mhi.md's "M4: the DSP chain") rather than only latching
 * inertly, and does so at a real emulated instant, not just "eventually".
 *
 * Same structure and conventions as mhitest.c/mhiseek.c: LVO dispatch, no
 * stdio, no C runtime startup -- see mhitest.c's own header comment for
 * why. Every assertion line starts with "MHIPARAM:" -- grep-friendly for
 * the integration test, same convention as its siblings' own prefixes.
 *
 * Command line: one filename, e.g. "SYS:tone.mp3".
 */

#include <exec/execbase.h>
#include <exec/memory.h>
#include <exec/tasks.h>
#include <exec/types.h>

#include <devices/timer.h>

#include <dos/dos.h>
#include <dos/dosextens.h>

#define EXEC_BASE_NAME _sysbase
#define DOS_BASE_NAME _dosbase
#include <inline/dos.h>
#include <inline/exec.h>

#include "mhi_abi.h"

/* Bounds the final completion Wait() so a headless run can never hang
 * indefinitely if the board never completes the queued descriptor. */
#define MHIPARAM_TIMEOUT_SECS 8

/* How long into playback (real timer.device time, not tied to the MHI
 * completion signal) the param change fires -- comfortably inside the
 * fixture's 3s duration, past any startup transient, leaving a clear
 * "before" and "after" window either side for the Rust test to sample. */
#define MHIPARAM_CHANGE_AT_SECS 1

/* Comfortably larger than the committed fixture
 * (tests/data/mhi/param_tone_cbr64_mono.mp3, ~24 KiB). */
#define BUFFER_SIZE (64 * 1024)

/* -- LVO dispatch: mhi_copperline.library's 10 entry points (same table
 * as mhitest.c/mhiseek.c -- see mhitest.c's own comment for the
 * FuncTab-order derivation). */

#define LVO_MHIALLOCDECODER -30
#define LVO_MHIFREEDECODER  -36
#define LVO_MHIQUEUEBUFFER  -42
#define LVO_MHIGETSTATUS    -54
#define LVO_MHIPLAY         -60
#define LVO_MHISTOP         -66
#define LVO_MHISETPARAM     -84

typedef APTR (*MHIAllocDecoderProc)(struct Task *task __asm("a0"), ULONG sigmask __asm("d0"), struct Library *base __asm("a6"));
typedef void (*MHIFreeDecoderProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));
typedef LONG (*MHIQueueBufferProc)(APTR handle __asm("a3"), APTR buffer __asm("a0"), ULONG size __asm("d0"), struct Library *base __asm("a6"));
typedef UBYTE (*MHIGetStatusProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));
typedef void (*MHIPlayProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));
typedef void (*MHIStopProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));
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

static void put_result(struct Library *_dosbase, const char *check, BOOL ok)
{
    put(_dosbase, "MHIPARAM: ");
    put(_dosbase, ok ? "PASS " : "FAIL ");
    put(_dosbase, check);
    put(_dosbase, "\n");
}

/* Trims the Shell command-line tail to a plain filename -- identical
 * shape to mhitest.c's own `read_cmdline_arg`. */
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

/* Waits `secs` real timer.device seconds (independent of the MHI
 * completion signal -- this is what lets the param change fire at a known
 * emulated instant mid-playback rather than only after EOF). */
static void wait_secs(ULONG secs)
{
    struct ExecBase *_sysbase = sysbase();
    struct MsgPort *timerport = CreateMsgPort();
    if (timerport == NULL) {
        return;
    }
    struct timerequest *tr =
        (struct timerequest *)AllocMem(sizeof(struct timerequest), MEMF_PUBLIC | MEMF_CLEAR);
    if (tr != NULL) {
        tr->tr_node.io_Message.mn_ReplyPort = timerport;
        tr->tr_node.io_Message.mn_Length = sizeof(*tr);
        if (OpenDevice((STRPTR) "timer.device", UNIT_VBLANK, (struct IORequest *)tr, 0) == 0) {
            tr->tr_node.io_Command = TR_ADDREQUEST;
            tr->tr_time.tv_secs = secs;
            tr->tr_time.tv_micro = 0;
            DoIO((struct IORequest *)tr);
            CloseDevice((struct IORequest *)tr);
        }
        FreeMem(tr, sizeof(struct timerequest));
    }
    DeleteMsgPort(timerport);
}

LONG entry(char *cmdline __asm("a0"), long cmdlen __asm("d0"))
{
    struct ExecBase *_sysbase = sysbase();
    struct Library *_dosbase = OpenLibrary((STRPTR) "dos.library", 34);
    if (_dosbase == NULL) {
        return 20;
    }

    char path[256];
    if (!read_cmdline_arg(cmdline, cmdlen, path, sizeof(path))) {
        put(_dosbase, "MHIPARAM: FAIL parse filename argument\n");
        put(_dosbase, "MHIPARAM: SUMMARY FAIL\n");
        CloseLibrary(_dosbase);
        return 20;
    }

    BOOL all_ok = TRUE;
    struct Library *mhibase = OpenLibrary((STRPTR) "mhi_copperline.library", 0);
    if (mhibase == NULL) {
        put_result(_dosbase, "open mhi_copperline.library", FALSE);
        put(_dosbase, "MHIPARAM: SUMMARY FAIL\n");
        CloseLibrary(_dosbase);
        return 20;
    }
    put_result(_dosbase, "open mhi_copperline.library", TRUE);

    LONG mysignal = AllocSignal(-1);
    if (mysignal == -1) {
        put_result(_dosbase, "AllocSignal", FALSE);
        put(_dosbase, "MHIPARAM: SUMMARY FAIL\n");
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
        MHIPlayProc mhi_play = MHI_PROC(MHIPlayProc, mhibase, LVO_MHIPLAY);
        MHISetParamProc mhi_set_param = MHI_PROC(MHISetParamProc, mhibase, LVO_MHISETPARAM);
        MHIStopProc mhi_stop = MHI_PROC(MHIStopProc, mhibase, LVO_MHISTOP);
        MHIFreeDecoderProc mhi_free = MHI_PROC(MHIFreeDecoderProc, mhibase, LVO_MHIFREEDECODER);

        static UBYTE buf[BUFFER_SIZE];
        BPTR fh = Open((STRPTR)path, MODE_OLDFILE);
        BOOL open_ok = (fh != 0);
        put_result(_dosbase, "open fixture file", open_ok);
        all_ok = all_ok && open_ok;

        if (open_ok) {
            LONG got = Read(fh, buf, BUFFER_SIZE);
            Close(fh);
            BOOL read_ok = (got > 0);
            put_result(_dosbase, "read fixture file", read_ok);
            all_ok = all_ok && read_ok;

            if (read_ok) {
                BOOL queue_ok = mhi_queue(handle, buf, (ULONG)got, mhibase) != FALSE;
                put_result(_dosbase, "MHIQueueBuffer", queue_ok);
                all_ok = all_ok && queue_ok;

                mhi_play(handle, mhibase);
                put(_dosbase, "MHIPARAM: INFO MHIPlay issued\n");

                wait_secs(MHIPARAM_CHANGE_AT_SECS);

                /* The param change itself: a hard volume drop (100 ->
                 * 20) and a hard pan to the right (50 -> 100), applied
                 * together, mid-playback -- exactly what a live "duck
                 * the volume and shift the balance" UI action does. */
                mhi_set_param(handle, MHIP_VOLUME, 20, mhibase);
                mhi_set_param(handle, MHIP_PANNING, 100, mhibase);
                put(_dosbase, "MHIPARAM: INFO MHISetParam volume=20 panning=100 issued\n");

                struct MsgPort *timerport = CreateMsgPort();
                BOOL got_signal = FALSE;
                if (timerport != NULL) {
                    struct timerequest *tr = (struct timerequest *)AllocMem(
                        sizeof(struct timerequest), MEMF_PUBLIC | MEMF_CLEAR);
                    if (tr != NULL) {
                        tr->tr_node.io_Message.mn_ReplyPort = timerport;
                        tr->tr_node.io_Message.mn_Length = sizeof(*tr);
                        if (OpenDevice((STRPTR) "timer.device", UNIT_VBLANK,
                                       (struct IORequest *)tr, 0) == 0) {
                            tr->tr_node.io_Command = TR_ADDREQUEST;
                            tr->tr_time.tv_secs = MHIPARAM_TIMEOUT_SECS;
                            tr->tr_time.tv_micro = 0;
                            SendIO((struct IORequest *)tr);

                            ULONG timersigmask = 1UL << timerport->mp_SigBit;
                            ULONG signals = Wait(mysigmask | timersigmask | SIGBREAKF_CTRL_C);
                            got_signal = (signals & mysigmask) != 0;

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
                all_ok = all_ok && got_signal;
            }
        }

        mhi_stop(handle, mhibase);
        mhi_free(handle, mhibase);
        put(_dosbase, "MHIPARAM: INFO MHIFreeDecoder issued\n");
    }

    FreeSignal(mysignal);
    CloseLibrary(mhibase);
    put_result(_dosbase, "close mhi_copperline.library", TRUE);

    put(_dosbase, all_ok ? "MHIPARAM: SUMMARY PASS\n" : "MHIPARAM: SUMMARY FAIL\n");
    CloseLibrary(_dosbase);
    return all_ok ? 0 : 20;
}
