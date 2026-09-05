// SPDX-License-Identifier: GPL-3.0-or-later
// Small standalone commands for Copperline's generated boot scripts.
// Only Exec/DOS V33 calls and the public V1 Process/CLI fields are used.
// These implement the launcher syntax, not the full Workbench utilities.

#include <exec/types.h>
#include <dos/dos.h>
#include <dos/dosextens.h>

#define __NOLIBBASE__
#define EXEC_BASE_NAME _sysbase
#define DOS_BASE_NAME _dosbase
#include <inline/exec.h>
#include <inline/dos.h>
#include <exec/memory.h>

// Decode exactly one argument, including AmigaDOS quotes and * escapes.
// The shell has already removed redirection. Never depend on a terminating
// NUL beyond the length supplied in d0, or use ReadArgs (a V36 API).
static LONG argument(const char *src, ULONG length, char *out, ULONG capacity)
{
    ULONG i = 0, n = 0;
    int quoted = 0;
    while (i < length && (src[i] == ' ' || src[i] == '\t')) ++i;
    if (i < length && src[i] == '"') { quoted = 1; ++i; }
    while (i < length) {
        char c = src[i++];
        if (!c || c == '\n' || c == '\r') {
            if (quoted) return -1;
            break;
        }
        if (quoted && c == '"') { quoted = 0; break; }
        if (!quoted && (c == ' ' || c == '\t')) break;
        if (c == '*') {
            if (i == length) return -1;
            c = src[i++];
            if (c == 'n' || c == 'N') c = '\n';
            if (c == 'e' || c == 'E') c = 27;
        }
        if (n + 1 >= capacity) return -1;
        out[n++] = c;
    }
    if (quoted) return -1;
    while (i < length) {
        char c = src[i++];
        if (c && c != ' ' && c != '\t' && c != '\n' && c != '\r') return -1;
    }
    out[n] = 0;
    return (LONG)n;
}

#if COMMAND == 1 || COMMAND == 3
static LONG number(const char *s, ULONG *value)
{
    ULONG n = 0;
    if (!*s) return 0;
    while (*s) {
        ULONG digit = (UBYTE)*s++ - '0';
        if (digit > 9 || n > 214748364UL ||
            (n == 214748364UL && digit > 7)) return 0;
        n = (n << 3) + (n << 1) + digit;
    }
    *value = n;
    return 1;
}
#endif

LONG entry(const char *args, ULONG length)
{
    struct ExecBase *_sysbase;
    __asm("move.l 4.w,%0" : "=r"(_sysbase));
    struct Library *_dosbase = OpenLibrary((STRPTR)"dos.library", 33);
    if (!_dosbase) return RETURN_FAIL;
    struct Process *process = (struct Process *)FindTask(NULL);
    struct CommandLineInterface *cli = BADDR(process->pr_CLI);
    char arg[256];
    LONG count = argument(args, length, arg, sizeof(arg));
    LONG rc = RETURN_ERROR;
    if (!cli || count < 0) goto done;

#if COMMAND == 1 || COMMAND == 3
    ULONG value;
    if (!number(arg, &value)) goto done;
#if COMMAND == 1
    cli->cli_FailLevel = value;
#else
    if (value < 2048 || value > 2147483644UL) goto done;
    cli->cli_DefaultStack = (value + 3) >> 2; // CLI stores longwords.
#endif
    rc = RETURN_OK;
#elif COMMAND == 2
    if (!count) goto done;
    BPTR lock = Lock((STRPTR)arg, ACCESS_READ);
    if (!lock) goto done;
    struct FileInfoBlock *info = AllocMem(sizeof(*info), MEMF_PUBLIC | MEMF_CLEAR);
    if (!info || !Examine(lock, info) || info->fib_DirEntryType < 0) {
        if (info) FreeMem(info, sizeof(*info));
        UnLock(lock);
        goto done;
    }
    FreeMem(info, sizeof(*info));
    BPTR old = CurrentDir(lock);
    if (old) UnLock(old);
    // The public CLI structure gives a BSTR length but no buffer capacity.
    // Updating a name no longer than the old one is always safe (the
    // launcher changes RunBoot: to the equally long RunProg:).
    if (cli->cli_SetName) {
        UBYTE *name = BADDR(cli->cli_SetName);
        if (count <= name[0]) {
            name[0] = count;
            for (LONG i = 0; i < count; ++i) name[i + 1] = arg[i];
        }
    }
    rc = RETURN_OK;
#elif COMMAND == 4
    BPTR output = Output();
    if (Write(output, arg, count) == count && Write(output, (APTR)"\n", 1) == 1)
        rc = RETURN_OK;
#elif COMMAND == 5
    // Run's child CLI has no enclosing script. Hand it the generated
    // Detached-Run file; the CLI owns and closes the handle at EOF.
    // Reject nested scripts rather than discard the caller's remainder.
    if (!count || cli->cli_CurrentInput != cli->cli_StandardInput) goto done;
    BPTR script = Open((STRPTR)arg, MODE_OLDFILE);
    if (!script) goto done;
    cli->cli_CurrentInput = script;
    rc = RETURN_OK;
#endif

done:
    if (rc) {
        static const char error[] = "Copperline boot command failed\n";
        Write(Output(), (APTR)error, sizeof(error) - 1);
    }
    CloseLibrary(_dosbase);
    return rc;
}
