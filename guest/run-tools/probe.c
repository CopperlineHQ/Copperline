// SPDX-License-Identifier: GPL-3.0-or-later
// End-to-end observation of the CLI that launches a program. A relative
// output exercises CD; returning 20 requires FailAt to preserve the marker.
#include <exec/types.h>
#include <dos/dos.h>
#include <dos/dosextens.h>
#define __NOLIBBASE__
#define EXEC_BASE_NAME _sysbase
#define DOS_BASE_NAME _dosbase
#include <inline/exec.h>
#include <inline/dos.h>

LONG entry(const char *args, ULONG length, const ULONG *stack_top)
{
    struct ExecBase *_sysbase;
    __asm("move.l 4.w,%0" : "=r"(_sysbase));
    struct Library *_dosbase = OpenLibrary((STRPTR)"dos.library", 33);
    if (!_dosbase) return 20;
    struct Process *process = (struct Process *)FindTask(NULL);
    struct CommandLineInterface *cli = BADDR(process->pr_CLI);
    if (cli) {
        ULONG observation[5] = {
            cli->cli_FailLevel, cli->cli_DefaultStack << 2,
            *stack_top,
            cli->cli_Background, process->pr_ConsoleTask != NULL,
        };
        BPTR file = Open((STRPTR)"FROM-GUEST", MODE_NEWFILE);
        if (file) {
            Write(file, observation, sizeof(observation));
            Write(file, (APTR)args, length);
            Close(file);
        }
    }
    CloseLibrary(_dosbase);
    return 20;
}
