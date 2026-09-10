// SPDX-License-Identifier: GPL-3.0-or-later
// retcode: return the decimal number given as the argument as the CLI
// return code, so a --run --exit-on-return session can be checked against a
// known guest exit status:
//
//   copperline --run guest/run-tools/retcode --run-args 7 --exit-on-return
//   echo $?     # 7
//
// No argument, or anything but a non-negative decimal number, returns 20
// (RETURN_FAIL). Freestanding: no libc, no startup code beyond start.s.

#include <exec/types.h>
#include <dos/dos.h>

LONG entry(const char *args, ULONG length)
{
    ULONG i = 0;
    LONG value = 0;
    int digits = 0;
    while (i < length && (args[i] == ' ' || args[i] == '\t')) ++i;
    while (i < length && args[i] >= '0' && args[i] <= '9') {
        if (value > 214748364L) return RETURN_FAIL;
        value = (value << 3) + (value << 1) + (args[i++] - '0'); // no __mulsi3
        ++digits;
    }
    while (i < length) {
        char c = args[i++];
        if (c && c != ' ' && c != '\t' && c != '\n' && c != '\r') return RETURN_FAIL;
    }
    return digits ? value : RETURN_FAIL;
}
