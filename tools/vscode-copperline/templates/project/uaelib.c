/* SPDX-License-Identifier: CC0-1.0 */
#include <exec/types.h>
#include <proto/exec.h>
#include "uaelib.h"

typedef LONG (*UaeConfig)(LONG, LONG, const char *, LONG, char *, LONG);
typedef LONG (*UaeLog)(LONG, const char *);
typedef LONG (*UaeDebug)(ULONG, ULONG, ULONG, ULONG, ULONG);

#ifdef COPPERLINE_BARE_STARTUP
struct ExecBase *SysBase;
#endif

enum { DEBUG_REGISTER = 4, DEBUG_UNREGISTER = 6 };
enum { DEBUG_BITMAP = 0, DEBUG_COPPERLIST = 2 };

struct DebugResource {
    ULONG address;
    ULONG size;
    char name[32];
    UWORD type;
    UWORD flags;
    WORD width;
    WORD height;
    WORD planes;
};

static int present(void)
{
    UWORD opcode = *(volatile UWORD *)0xf0ff60u;
    return opcode == 0x4eb9 || opcode == 0xa00e;
}

void KPrintF(const char *format, ...)
{
    if (present())
        ((UaeLog)0xf0ff60u)(86, format);
}

void warpmode(int enabled)
{
    char result;
    const char *command = enabled ? "warp true" : "warp false";
    if (present())
        ((UaeConfig)0xf0ff60u)(82, -1, command, 0, &result, 1);
}

static void register_resource(struct DebugResource *resource)
{
    if (present())
        ((UaeDebug)0xf0ff60u)(88, DEBUG_REGISTER, (ULONG)resource, 0, 0);
}

void debug_register_bitmap(const void *address, const char *name,
                           UWORD width, UWORD height, UWORD planes, UWORD flags)
{
    struct DebugResource resource = {
        (ULONG)address, (ULONG)(width / 8) * height * planes, "",
        DEBUG_BITMAP, flags, width, height, planes
    };
    {
        unsigned int i;
        for (i = 0; i + 1 < sizeof(resource.name) && name[i]; ++i)
            resource.name[i] = name[i];
    }
    register_resource(&resource);
}

void debug_register_copperlist(const void *address, const char *name,
                               ULONG size, UWORD flags)
{
    struct DebugResource resource = {
        (ULONG)address, size, "", DEBUG_COPPERLIST, flags, 0, 0, 0
    };
    {
        unsigned int i;
        for (i = 0; i + 1 < sizeof(resource.name) && name[i]; ++i)
            resource.name[i] = name[i];
    }
    register_resource(&resource);
}

void debug_unregister(const void *address)
{
    if (present())
        ((UaeDebug)0xf0ff60u)(88, DEBUG_UNREGISTER, (ULONG)address, 0, 0);
}
