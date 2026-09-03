// SPDX-License-Identifier: GPL-3.0-or-later
//
// hello: the DAP adapter's debug-information probe. Built with -g -O0,
// so its trailing HUNK_DEBUG block carries DWARF for a small call chain
// (entry -> scale -> add), globals of a few types, stack locals with
// plain frame-base locations, and call-frame information. The unit
// tests in src/debuginfo/ read the committed binary; the ignored
// end-to-end test in tests/dap_stdio.rs runs it under the adapter.
//
// Prints one line through dos.library and again to the serial port
// (the debugger's stdout), and returns 0 (RC 20 when dos.library is
// missing). Freestanding: start.s branches to entry()
// and wraps the three library calls, so no NDK headers are needed.

typedef long LONG;
typedef unsigned long ULONG;
typedef unsigned char UBYTE;

struct Library;

extern struct Library *OpenLibrary(const char *name, ULONG version);
extern void CloseLibrary(struct Library *lib);
extern LONG PutStr(const char *str);
extern void RawPutStr(const char *str);

// dos.library base, read by the PutStr wrapper in start.s.
struct Library *_dosbase;

// Globals the adapter shows in its Globals scope.
LONG counter = 5;
UBYTE flag;
const char *greeting = "dap-test: hello from the guest\n";

struct point {
    LONG x;
    LONG y;
};

struct point origin = { 3, 4 };

LONG add(LONG a, LONG b)
{
    LONG sum = a + b;
    counter = counter + sum;
    return sum;
}

// Shifts, not a multiply: a 32-bit multiply on the 68000 is a libgcc
// call (__mulsi3), which this freestanding probe does not link.
LONG scale(LONG n)
{
    LONG r = add(n, 0);
    r = r << 2;
    return r;
}

LONG entry(void)
{
    LONG total = 0;
    struct point p;
    int i;

    _dosbase = OpenLibrary("dos.library", 34);
    if (_dosbase == 0)
        return 20;
    p.x = origin.x;
    p.y = origin.y;
    for (i = 1; i <= 3; i++)
        total += scale(i);
    total += add(p.x, p.y);
    flag = (UBYTE) (total & 0xff);
    PutStr(greeting);
    RawPutStr(greeting);
    CloseLibrary(_dosbase);
    return total - 21;
}
