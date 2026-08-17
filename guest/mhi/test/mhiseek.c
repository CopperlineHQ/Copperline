/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * mhiseek: the WP5 M3 harness client for mhi_copperline.library
 * (MHI-PLAN-M3-M4.md WP3.3). MHI has no seek call of its own -- seeking is
 * entirely the player's job (MHIStop, reposition the player's own file
 * read, MHIQueueBuffer from the new position; see docs/internals/mhi.md's
 * "Seek-entry hardening"). This client proves that sequence end to end
 * against the real board: play one small file to completion, MHIStop,
 * open a *different* file (from the board's perspective indistinguishable
 * from a seek within the same file -- it only ever sees STOP followed by
 * fresh descriptors either way) and play that to completion too. The Rust
 * integration test verifies the `--audio-wav` capture carries each file's
 * distinct tone in its own window, proving the post-STOP decode is real
 * audio and not silence/garbage left over from the discarded first stream.
 *
 * Same structure and conventions as mhitest.c (LVO dispatch, no stdio, no
 * C runtime startup -- see that file's own header comment for why); every
 * assertion line starts with "MHISEEK:" -- grep-friendly for the
 * integration test, same convention as mhitest's "MHITEST:" lines.
 *
 * Command line: two whitespace-separated filenames, e.g.
 * "SYS:tone_a.mp3 SYS:tone_b.mp3". Both files are read whole into a fixed
 * buffer (comfortably larger than the small fixtures this test uses -- see
 * BUFFER_SIZE) and queued as a single descriptor each; no multi-buffer
 * preloading loop is needed at this fixture size, unlike mhitest's
 * embedded-pattern case or MHIplay's general-purpose streaming loop.
 */

#include <exec/types.h>
#include <exec/execbase.h>
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

/* Bounds every Wait() below so a headless run can never hang indefinitely
 * if the board never completes a queued descriptor. */
#define MHISEEK_TIMEOUT_SECS 8

/* Comfortably larger than the committed test fixtures (tests/data/mhi/
 * golden_tone_cbr64_mono.mp3 and golden_tone2_880hz_cbr64_mono.mp3 are
 * ~12.5 KiB each). */
#define BUFFER_SIZE (64 * 1024)

/* -- LVO dispatch: mhi_copperline.library's 10 entry points (same table
 * as mhitest.c -- see that file's own comment for the FuncTab-order
 * derivation). */

#define LVO_MHIALLOCDECODER -30
#define LVO_MHIFREEDECODER  -36
#define LVO_MHIQUEUEBUFFER  -42
#define LVO_MHIGETSTATUS    -54
#define LVO_MHIPLAY         -60
#define LVO_MHISTOP         -66
#define LVO_MHIQUERY        -78

typedef APTR (*MHIAllocDecoderProc)(struct Task *task __asm("a0"), ULONG sigmask __asm("d0"), struct Library *base __asm("a6"));
typedef void (*MHIFreeDecoderProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));
typedef LONG (*MHIQueueBufferProc)(APTR handle __asm("a3"), APTR buffer __asm("a0"), ULONG size __asm("d0"), struct Library *base __asm("a6"));
typedef UBYTE (*MHIGetStatusProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));
typedef void (*MHIPlayProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));
typedef void (*MHIStopProc)(APTR handle __asm("a3"), struct Library *base __asm("a6"));

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
    put(_dosbase, "MHISEEK: ");
    put(_dosbase, ok ? "PASS " : "FAIL ");
    put(_dosbase, check);
    put(_dosbase, "\n");
}

/* Splits the Shell command-line tail (start.s hands entry() A0/D0
 * verbatim, per mhitest.c's identical helper) into two whitespace-
 * separated filename tokens. Returns FALSE if either is missing. */
static BOOL split_two_args(const char *cmdline, LONG cmdlen, char *out1, LONG out1size,
                            char *out2, LONG out2size)
{
    LONG i = 0;
    LONG start;
    LONG n;

    while (i < cmdlen && (cmdline[i] == ' ' || cmdline[i] == '\t')) {
        i++;
    }
    start = i;
    while (i < cmdlen && cmdline[i] != ' ' && cmdline[i] != '\t' && cmdline[i] != '\n' &&
           cmdline[i] != '\r') {
        i++;
    }
    n = i - start;
    if (n <= 0 || n >= out1size) {
        return FALSE;
    }
    { LONG j; for (j = 0; j < n; j++) { out1[j] = cmdline[start + j]; } }
    out1[n] = '\0';

    while (i < cmdlen && (cmdline[i] == ' ' || cmdline[i] == '\t')) {
        i++;
    }
    start = i;
    while (i < cmdlen && cmdline[i] != ' ' && cmdline[i] != '\t' && cmdline[i] != '\n' &&
           cmdline[i] != '\r') {
        i++;
    }
    n = i - start;
    if (n <= 0 || n >= out2size) {
        return FALSE;
    }
    { LONG j; for (j = 0; j < n; j++) { out2[j] = cmdline[start + j]; } }
    out2[n] = '\0';

    return TRUE;
}

/* Reads `path` whole into `buf` (up to BUFFER_SIZE bytes), MHIQueueBuffers
 * it as a single descriptor, MHIPlays, and waits (bounded) for the
 * completion signal. Returns TRUE iff every step succeeded. */
static BOOL play_file_to_completion(struct Library *_dosbase, struct Library *mhibase,
                                     APTR handle, ULONG sigmask, const char *path, UBYTE *buf,
                                     const char *label)
{
    struct ExecBase *_sysbase = sysbase();
    MHIQueueBufferProc mhi_queue = MHI_PROC(MHIQueueBufferProc, mhibase, LVO_MHIQUEUEBUFFER);
    MHIPlayProc mhi_play = MHI_PROC(MHIPlayProc, mhibase, LVO_MHIPLAY);

    BPTR fh = Open((STRPTR)path, MODE_OLDFILE);
    if (fh == 0) {
        put_result(_dosbase, label, FALSE);
        put(_dosbase, "MHISEEK: INFO could not open file\n");
        return FALSE;
    }
    LONG got = Read(fh, buf, BUFFER_SIZE);
    Close(fh);
    if (got <= 0) {
        put_result(_dosbase, label, FALSE);
        put(_dosbase, "MHISEEK: INFO empty read\n");
        return FALSE;
    }

    BOOL queue_ok = mhi_queue(handle, buf, (ULONG)got, mhibase) != FALSE;
    if (!queue_ok) {
        put_result(_dosbase, label, FALSE);
        put(_dosbase, "MHISEEK: INFO MHIQueueBuffer failed\n");
        return FALSE;
    }

    mhi_play(handle, mhibase);

    struct MsgPort *timerport = CreateMsgPort();
    BOOL got_signal = FALSE;
    if (timerport != NULL) {
        struct timerequest *tr =
            (struct timerequest *)AllocMem(sizeof(struct timerequest), MEMF_PUBLIC | MEMF_CLEAR);
        if (tr != NULL) {
            tr->tr_node.io_Message.mn_ReplyPort = timerport;
            tr->tr_node.io_Message.mn_Length = sizeof(*tr);
            if (OpenDevice((STRPTR) "timer.device", UNIT_VBLANK, (struct IORequest *)tr, 0) == 0) {
                tr->tr_node.io_Command = TR_ADDREQUEST;
                tr->tr_time.tv_secs = MHISEEK_TIMEOUT_SECS;
                tr->tr_time.tv_micro = 0;
                SendIO((struct IORequest *)tr);

                ULONG timersigmask = 1UL << timerport->mp_SigBit;
                ULONG signals = Wait(sigmask | timersigmask | SIGBREAKF_CTRL_C);
                got_signal = (signals & sigmask) != 0;

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
    put_result(_dosbase, label, got_signal);
    return got_signal;
}

LONG entry(char *cmdline __asm("a0"), long cmdlen __asm("d0"))
{
    struct ExecBase *_sysbase = sysbase();
    struct Library *_dosbase = OpenLibrary((STRPTR) "dos.library", 34);
    if (_dosbase == NULL) {
        return 20;
    }

    char path_a[256];
    char path_b[256];
    if (!split_two_args(cmdline, cmdlen, path_a, sizeof(path_a), path_b, sizeof(path_b))) {
        put(_dosbase, "MHISEEK: FAIL parse two filename arguments\n");
        put(_dosbase, "MHISEEK: SUMMARY FAIL\n");
        CloseLibrary(_dosbase);
        return 20;
    }

    BOOL all_ok = TRUE;
    struct Library *mhibase = OpenLibrary((STRPTR) "mhi_copperline.library", 0);
    if (mhibase == NULL) {
        put_result(_dosbase, "open mhi_copperline.library", FALSE);
        put(_dosbase, "MHISEEK: SUMMARY FAIL\n");
        CloseLibrary(_dosbase);
        return 20;
    }
    put_result(_dosbase, "open mhi_copperline.library", TRUE);

    LONG mysignal = AllocSignal(-1);
    if (mysignal == -1) {
        put_result(_dosbase, "AllocSignal", FALSE);
        put(_dosbase, "MHISEEK: SUMMARY FAIL\n");
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
        MHIGetStatusProc mhi_get_status = MHI_PROC(MHIGetStatusProc, mhibase, LVO_MHIGETSTATUS);
        MHIStopProc mhi_stop = MHI_PROC(MHIStopProc, mhibase, LVO_MHISTOP);
        MHIFreeDecoderProc mhi_free = MHI_PROC(MHIFreeDecoderProc, mhibase, LVO_MHIFREEDECODER);

        static UBYTE buf[BUFFER_SIZE];

        BOOL first_ok =
            play_file_to_completion(_dosbase, mhibase, handle, mysigmask, path_a, buf,
                                     "play file A to completion");
        all_ok = all_ok && first_ok;

        UBYTE status_after_a = mhi_get_status(handle, mhibase);
        put(_dosbase, "MHISEEK: INFO status_after_a=");
        put(_dosbase, status_after_a == MHIF_OUT_OF_DATA ? "OUT_OF_DATA" : "OTHER");
        put(_dosbase, "\n");

        /* The seek itself: MHIStop, then (from the board's perspective
         * indistinguishably) queue and play a fresh source -- exactly the
         * sequence a real seeking player performs, see this file's own
         * header comment. */
        mhi_stop(handle, mhibase);
        put(_dosbase, "MHISEEK: INFO MHIStop issued (the seek)\n");

        BOOL second_ok =
            play_file_to_completion(_dosbase, mhibase, handle, mysigmask, path_b, buf,
                                     "play file B to completion after seek");
        all_ok = all_ok && second_ok;

        mhi_stop(handle, mhibase);
        mhi_free(handle, mhibase);
        put(_dosbase, "MHISEEK: INFO MHIFreeDecoder issued\n");
    }

    FreeSignal(mysignal);
    CloseLibrary(mhibase);
    put_result(_dosbase, "close mhi_copperline.library", TRUE);

    put(_dosbase, all_ok ? "MHISEEK: SUMMARY PASS\n" : "MHISEEK: SUMMARY FAIL\n");
    CloseLibrary(_dosbase);
    return all_ok ? 0 : 20;
}
