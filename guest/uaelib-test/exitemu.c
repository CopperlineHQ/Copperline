// SPDX-License-Identifier: GPL-3.0-or-later
//
// exitemu: guest-side probe for uaelib function 13, WinUAE's ExitEmu
// (src/uaelib.rs). Calls it through the trap at $F0FF60 exactly as the
// vscode-amiga-debug helpers reach the other functions, then returns 20:
// under an emulator that honours the call the session ends at the next
// frame boundary with exit status 0, so a run that instead reports 20
// (--exit-on-return) or keeps going shows the function was not provided.
//
//   copperline --run guest/uaelib-test/exitemu --noaudio
//       --screenshot-after 60 /tmp/never.png; echo $?    # 0, no screenshot
//
// Built standalone (no startup code, no libc): start.s, linked first,
// branches to entry().

#include <exec/types.h>
#include <dos/dos.h>

LONG entry(void)
{
    long (*UaeLib)(long function) = (long (*)(long))0xf0ff60;
    if (*((UWORD *)UaeLib) == 0x4eb9 || *((UWORD *)UaeLib) == 0xa00e)
        UaeLib(13);
    return RETURN_FAIL;
}
