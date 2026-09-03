// SPDX-License-Identifier: GPL-3.0-or-later
//
// uaelibtest: guest-side probe for the WinUAE-compatible uaelib trap at
// $F0FF60 (src/uaelib.rs). Carries the vscode-amiga-debug template's
// warpmode(), KPrintF() and debug_*() helpers as written, so a run under
// --run exercises exactly the calls a program built from that template
// makes, then records what each direct call returned in UAELIB-RESULT
// next to the binary:
//
//   present=4eb9 r86=1 r0=0 r88=0 r82=0 r82e=ffffffff out=0 load=0 match=0
//
// With `[emulation] uaelib_files = true`, the final fields instead report
// `load=12 match=1` after a save-clear-load round trip rooted beside this
// executable.
//
// (hex case as the Kickstart's RawDoFmt prints it; AROS uses upper case).
//
// With the trap disabled ([emulation] uaelib = false) `present` is the
// floating bus and every call is skipped; KPrintF then reaches the host
// through the serial port instead.
//
// Built standalone (no startup code, no libc): start.s, linked first,
// branches to entry(); support.s carries the RawDoFmt sinks.

#include <exec/types.h>

#include <dos/dos.h>

// __NOLIBBASE__: the sfdc 1.12 inline headers (amiga-gcc 16.2 image)
// define the ...Tags varargs wrappers as file-scope static functions that
// name DOS_BASE_NAME directly, which cannot resolve to a function-local
// base; with __NOLIBBASE__ they take the base as an explicit argument at
// the call site instead, where the local is in scope.
#define __NOLIBBASE__
#define EXEC_BASE_NAME _sysbase
#define DOS_BASE_NAME _dosbase
#include <inline/dos.h>
#include <inline/exec.h>

#include <stdarg.h>
#include <stddef.h>

void PutChar(void);
void KPutCharX(void);

static struct ExecBase *sysbase(void);

// No libc: the compiler may still emit these for struct initialisers.
void *memset(void *dst, int value, size_t len)
{
    unsigned char *p = dst;
    while (len--)
        *p++ = (unsigned char)value;
    return dst;
}

void *memcpy(void *dst, const void *src, size_t len)
{
    unsigned char *d = dst;
    const unsigned char *s = src;
    while (len--)
        *d++ = *s++;
    return dst;
}

size_t strlen(const char *s)
{
    size_t n = 0;
    while (s[n])
        n++;
    return n;
}

static void copy_name(char *dst, const char *src, size_t len)
{
    size_t i = 0;
    for (; i < len && src[i]; i++)
        dst[i] = src[i];
    for (; i < len; i++)
        dst[i] = 0;
}

// ---- vscode-amiga-debug template/support/gcc8_c_support.c, verbatim ----

void warpmode(int on) { // bool
	long(*UaeConf)(long mode, int index, const char* param, int param_len, char* outbuf, int outbuf_len);
	UaeConf = (long(*)(long, int, const char*, int, char*, int))0xf0ff60;
	if(*((UWORD *)UaeConf) == 0x4eb9 || *((UWORD *)UaeConf) == 0xa00e) {
		char outbuf;
		UaeConf(82, -1, on ? "cpu_speed max" : "cpu_speed real", 0, &outbuf, 1);
		UaeConf(82, -1, on ? "cpu_cycle_exact false" : "cpu_cycle_exact true", 0, &outbuf, 1);
		UaeConf(82, -1, on ? "cpu_memory_cycle_exact false" : "cpu_memory_cycle_exact true", 0, &outbuf, 1);
		UaeConf(82, -1, on ? "blitter_cycle_exact false" : "blitter_cycle_exact true", 0, &outbuf, 1);
		UaeConf(82, -1, on ? "warp true" : "warp false", 0, &outbuf, 1);
	}
}

static struct ExecBase *_sysbase;

void KPrintF(const char* fmt, ...) {
	va_list vl;
	va_start(vl, fmt);
	long(*UaeDbgLog)(long mode, const char* string) = (long(*)(long, const char*))0xf0ff60;
	if(*((UWORD *)UaeDbgLog) == 0x4eb9 || *((UWORD *)UaeDbgLog) == 0xa00e) {
		char temp[128];
		RawDoFmt((CONST_STRPTR)fmt, vl, PutChar, temp);
		UaeDbgLog(86, temp);
	} else {
		RawDoFmt((CONST_STRPTR)fmt, vl, KPutCharX, 0);
	}
	va_end(vl);
}

static void debug_cmd(unsigned int arg1, unsigned int arg2, unsigned int arg3, unsigned int arg4) {
	long(*UaeLib)(unsigned int arg0, unsigned int arg1, unsigned int arg2, unsigned int arg3, unsigned int arg4);
	UaeLib = (long(*)(unsigned int, unsigned int, unsigned int, unsigned int, unsigned int))0xf0ff60;
	if(*((UWORD *)UaeLib) == 0x4eb9 || *((UWORD *)UaeLib) == 0xa00e) {
		UaeLib(88, arg1, arg2, arg3, arg4);
	}
}

enum barto_cmd {
	barto_cmd_clear,
	barto_cmd_rect,
	barto_cmd_filled_rect,
	barto_cmd_text,
	barto_cmd_register_resource,
	barto_cmd_set_idle,
	barto_cmd_unregister_resource,
	barto_cmd_load,
	barto_cmd_save,
};

enum debug_resource_type {
	debug_resource_type_bitmap,
	debug_resource_type_palette,
	debug_resource_type_copperlist,
};

enum debug_resource_flags {
	debug_resource_bitmap_interleaved = 1 << 0,
	debug_resource_bitmap_masked = 1 << 1,
	debug_resource_bitmap_ham = 1 << 2,
};

struct debug_resource {
	unsigned int address; // can't use void* because WinUAE is 64-bit
	unsigned int size;
	char name[32];
	unsigned short type; // enum debug_resource_type
	unsigned short flags; // enum debug_resource_flags
	union {
		struct {
			short width;
			short height;
			short numPlanes;
		} bitmap;
		struct {
			short numEntries;
		} palette;
	};
};

void debug_register_bitmap(const void* addr, const char* name, unsigned short width, unsigned short height, unsigned short numPlanes, unsigned short flags) {
	struct debug_resource resource = {
		.address = (unsigned int)addr,
		.size = width / 8 * height * numPlanes,
		.type = debug_resource_type_bitmap,
		.flags = flags,
		.bitmap = {
			.width = width,
			.height = height,
			.numPlanes = numPlanes
		}
	};
	if(flags & debug_resource_bitmap_masked)
		resource.size *= 2;
	copy_name(resource.name, name, sizeof(resource.name));
	debug_cmd(barto_cmd_register_resource, (unsigned int)&resource, 0, 0);
}

void debug_register_palette(const void* addr, const char* name, unsigned short numEntries, unsigned short flags) {
	struct debug_resource resource = {
		.address = (unsigned int)addr,
		.size = numEntries * 2,
		.type = debug_resource_type_palette,
		.flags = flags,
		.palette = {
			.numEntries = numEntries
		}
	};
	copy_name(resource.name, name, sizeof(resource.name));
	debug_cmd(barto_cmd_register_resource, (unsigned int)&resource, 0, 0);
}

void debug_register_copperlist(const void* addr, const char* name, unsigned int size, unsigned short flags) {
	struct debug_resource resource = {
		.address = (unsigned int)addr,
		.size = size,
		.type = debug_resource_type_copperlist,
		.flags = flags,
	};
	copy_name(resource.name, name, sizeof(resource.name));
	debug_cmd(barto_cmd_register_resource, (unsigned int)&resource, 0, 0);
}

void debug_unregister(const void* addr) {
	debug_cmd(barto_cmd_unregister_resource, (unsigned int)addr, 0, 0);
}

void debug_clear() {
	debug_cmd(barto_cmd_clear, 0, 0, 0);
}

void debug_rect(short left, short top, short right, short bottom, unsigned int color) {
	debug_cmd(barto_cmd_rect, ((unsigned int)left << 16) | (unsigned short)top, ((unsigned int)right << 16) | (unsigned short)bottom, color);
}

void debug_filled_rect(short left, short top, short right, short bottom, unsigned int color) {
	debug_cmd(barto_cmd_filled_rect, ((unsigned int)left << 16) | (unsigned short)top, ((unsigned int)right << 16) | (unsigned short)bottom, color);
}

void debug_text(short left, short top, const char* text, unsigned int color) {
	debug_cmd(barto_cmd_text, ((unsigned int)left << 16) | (unsigned short)top, (unsigned int)text, color);
}

void debug_start_idle() {
	debug_cmd(barto_cmd_set_idle, 1, 0, 0);
}

void debug_stop_idle() {
	debug_cmd(barto_cmd_set_idle, 0, 0, 0);
}

unsigned int debug_load(void* addr, const char* name) {
	long(*UaeLib)(unsigned int arg0, unsigned int arg1, unsigned int arg2, unsigned int arg3, unsigned int arg4);
	UaeLib = (long(*)(unsigned int, unsigned int, unsigned int, unsigned int, unsigned int))0xf0ff60;
	if(*((UWORD *)UaeLib) == 0x4eb9 || *((UWORD *)UaeLib) == 0xa00e)
		return UaeLib(88, barto_cmd_load, (unsigned int)addr, (unsigned int)name, 0);
	return 0;
}

void debug_save(const void* addr, unsigned int size, const char* name) {
	long(*UaeLib)(unsigned int arg0, unsigned int arg1, unsigned int arg2, unsigned int arg3, unsigned int arg4);
	UaeLib = (long(*)(unsigned int, unsigned int, unsigned int, unsigned int, unsigned int))0xf0ff60;
	if(*((UWORD *)UaeLib) == 0x4eb9 || *((UWORD *)UaeLib) == 0xa00e)
		UaeLib(88, barto_cmd_save, (unsigned int)addr, size, (unsigned int)name);
}

// ---- end of template code ----

LONG entry(void)
{
    _sysbase = sysbase();
    struct Library *_dosbase = OpenLibrary((STRPTR) "dos.library", 34);
    if (_dosbase == NULL)
        return 20;

    // The template's own detection: the first word of the trap.
    UWORD present = *(volatile UWORD *)0xf0ff60;
    int fitted = present == 0x4eb9 || present == 0xa00e;

    KPrintF("hello from uaelib %ld\n", 42L);
    warpmode(1);
    warpmode(0);
    debug_register_bitmap((const void *)0x20000, "screen", 320, 256, 5,
                          debug_resource_bitmap_interleaved);
    debug_register_palette((const void *)0x30000, "pal", 32, 0);
    debug_register_copperlist((const void *)0x40000, "cop", 1000, 0);
    debug_start_idle();
    debug_stop_idle();
    // A windowed session shows these over the picture; headless runs
    // record them as accepted commands.
    debug_filled_rect(16, 16, 200, 60, 0x224488);
    debug_rect(12, 12, 204, 64, 0xFFFFFF);
    debug_text(24, 32, "uaelib overlay", 0xFFFF00);

    static const char payload[] = "host bridge";
    // The trap ABI carries the pointer as an integer, so make the buffer
    // volatile: otherwise GCC cannot see that debug_load writes through it
    // and may constant-fold the comparison after the clear below.
    volatile char filebuf[sizeof(payload)];
    debug_save(payload, sizeof(payload), "UAELIB-BLOB");
    for (unsigned int i = 0; i < sizeof(filebuf); i++)
        filebuf[i] = 0;
    LONG rload = (LONG) debug_load((void *)filebuf, "UAELIB-BLOB");
    __asm volatile("" : : : "memory");
    LONG filematch = 1;
    for (unsigned int i = 0; i < sizeof(filebuf); i++) {
        if (filebuf[i] != payload[i]) {
            filematch = 0;
            break;
        }
    }
    LONG r86 = 0, r0 = 0, r88 = 0, r82 = 0, r82e = 0;
    char out = 0x55;
    if (fitted) {
        long (*UaeLog)(long, const char *) = (long (*)(long, const char *))0xf0ff60;
        long (*UaeVer)(long) = (long (*)(long))0xf0ff60;
        long (*UaeDbg)(long, long, long, long, long) =
            (long (*)(long, long, long, long, long))0xf0ff60;
        long (*UaeConf)(long, int, const char *, int, char *, int) =
            (long (*)(long, int, const char *, int, char *, int))0xf0ff60;
        r86 = UaeLog(86, "second line");
        r0 = UaeVer(0);
        r88 = UaeDbg(88, 1, 2, 3, 4);
        r82 = UaeConf(82, -1, "warp true", 0, &out, 1);
        r82e = UaeConf(82, -1, "warp", 0, &out, 1);
    }
    debug_unregister(0);

    LONG args[9] = { present, r86, r0, r88, r82, r82e, out, rload, filematch };
    char line[128];
    RawDoFmt((CONST_STRPTR) "present=%04lx r86=%ld r0=%ld r88=%ld r82=%ld r82e=%lx out=%ld load=%ld match=%ld\n",
             args, PutChar, line);
    LONG len = (LONG) strlen(line);

    LONG rc = 20;
    BPTR fh = Open((STRPTR) "UAELIB-RESULT", MODE_NEWFILE);
    if (fh != 0) {
        if (Write(fh, line, len) == len)
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
