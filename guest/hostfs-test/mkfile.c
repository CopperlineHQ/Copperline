// SPDX-License-Identifier: GPL-3.0-or-later
//
// mkfile: guest-side probe for the hostfs integration tests. Creates
// FROM-GUEST in the current directory and writes one line to it, so a test
// that types `mkfile` into a shell booted from a hostfs volume exercises
// LoadSeg, Open(MODE_NEWFILE), Write, and Close through the handler, and
// can verify the bytes from the host side. Returns 0 on success, RC 20 on
// any failure. Kickstart 1.3 (dos.library V34) is the floor, matching the
// services board.
//
// Built standalone (no startup code): start.s, linked first, branches to
// entry() -- the CLI enters at the start of the hunk, which the compiler
// may fill with rodata rather than the first function.

#include <exec/types.h>

#include <dos/dos.h>

#define EXEC_BASE_NAME _sysbase
#define DOS_BASE_NAME _dosbase
#include <inline/dos.h>
#include <inline/exec.h>

static struct ExecBase *sysbase(void);

LONG entry(void)
{
    struct ExecBase *_sysbase = sysbase();
    struct Library *_dosbase = OpenLibrary((STRPTR) "dos.library", 34);
    if (_dosbase == NULL)
        return 20;

    LONG rc = 20;
    BPTR fh = Open((STRPTR) "FROM-GUEST", MODE_NEWFILE);
    if (fh != 0) {
        static const char msg[] = "hello from the guest\n";
        if (Write(fh, (APTR)msg, sizeof(msg) - 1) == sizeof(msg) - 1)
            rc = 0;
        Close(fh);
    }

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
