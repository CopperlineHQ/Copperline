//
// Copyright 2022-2025 Stefan Reinauer & Chris Hooper
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice,
//    this list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
//

#ifdef DEBUG_PORT
#define USE_SERIAL_OUTPUT
#endif

#include "port.h"

#include <stdlib.h>
#include <string.h>
#include <proto/dos.h>
#include <proto/exec.h>
#include <clib/debug_protos.h>
#include <clib/alib_protos.h>
#include <clib/exec_protos.h>
#include <clib/intuition_protos.h>
#include <devices/timer.h>
#include <intuition/intuition.h>
#include <inline/intuition.h>
#include <exec/io.h>
#include <exec/execbase.h>
#include "device.h"
#include "printf.h"

#include "attach.h"

#ifdef DEBUG_CALLOUT
#define PRINTF_CALLOUT(args...) printf(args)
#else
#define PRINTF_CALLOUT(args...)
#endif

// amiga-gcc falls over when the data segment is entirely
// empty. Put one short there until we know what is going on.
//
// Compiling with -fbaserel will result in a linker error
// otherwise:
//
//   relocation truncated to fit: DREL16 against `.bss'
//
short bug=TRUE;

void
panic(const char *fmt, ...)
{
    va_list ap;
    struct Library *IntuitionBase;
    struct EasyStruct es = {
        sizeof (es),
        0,
        XSTR(DEVNAME)" Panic",
        (char *) fmt,
        "OK",
    };
    printf("PANIC: ");
    va_start(ap, fmt);
    vprintf(fmt, ap);
    va_end(ap);
    printf("\n\n");

    IntuitionBase = OpenLibrary("intuition.library", 37);
    if (IntuitionBase != NULL) {
        va_start(ap, fmt);
        (void) EasyRequestArgs(NULL, &es, NULL, ap);
        va_end(ap);
        CloseLibrary(IntuitionBase);
    }
}

static void wait_for_timer(struct timerequest *tr, struct timeval *tv)
{
    tr->tr_node.io_Command = TR_ADDREQUEST;
    tr->tr_time = *tv;
    DoIO((struct IORequest *)tr);
}

static void delete_timer(struct timerequest *tr)
{
    struct MsgPort *tp;

    if (tr != NULL) {
        tp = tr->tr_node.io_Message.mn_ReplyPort;
        CloseDevice((struct IORequest *)tr);
        DeleteExtIO((struct IORequest *)tr);
        if (tp != NULL)
            DeletePort(tp);
    }
}

static struct timerequest *create_timer(ULONG unit)
{
    LONG error;
    struct MsgPort *tp;
    struct timerequest *tr;

    tp = CreatePort(NULL, 0);
    if (tp == NULL)
        return (NULL);

    tr = (struct timerequest *)
                CreateExtIO(tp, sizeof(struct timerequest));
    if (tr == NULL) {
        DeletePort(tp);
        return (NULL);
    }

    error = OpenDevice(TIMERNAME, unit, (struct IORequest *)tr, 0L);
    if (error) {
        delete_timer(tr);
        return (NULL);
    }
    return tr;
}

void
delay(int usecs)
{
    struct timerequest *tr;
    struct timeval tv;

    if (SysBase->IDNestCnt >= 0) {
        printf("delay(%d): Interrupts disabled, using delay loop.\n", usecs);
        for (unsigned long i = 0; i < ((unsigned long)usecs << 3); i++)
            asm("nop");
	return;
    }

    if (usecs < 20000)
        tr = create_timer(UNIT_MICROHZ);
    else
        tr = create_timer(UNIT_VBLANK);

    if (tr == NULL) {
        printf("timer.device handle invalid.\n");
        return;
    }

    tv.tv_secs  = usecs / 1000000;
    tv.tv_micro = usecs % 1000000;
    wait_for_timer(tr, &tv);
    delete_timer(tr);
}

const char *
device_xname(void *ptr)
{
    (void)ptr;
    return (XSTR(DEVNAME));
}

#ifdef USE_BASEREL
void
__restore_a4(void)
{
    __asm volatile("\tlea ___a4_init, a4");
}
#endif

char *itoa(int value, char *string, int radix)
{
    int n,r,a,b = 0;
    n = (value<0)?-value:value;
    b = 0;
    while (n)
    {
        r = n % radix;
        if (r >= 10)
            string[b++] = 'a' + (r - 10);
        else
            string[b++] = '0' + r;
        n /= radix;
    }
    if (b == 0)
        string[b++] = '0';
    if (value < 0 && radix == 10)
        string[b++] = '-';
    string[b] = '\0';
    a=0; b--;
    while (a < b) {
        char temp = string[a];
        string[a] = string[b];
        string[b] = temp;
        a++;b--;
    }
    return string;
}

/* callout */

callout_t *callout_head = NULL;

static void
callout_add(callout_t *c)
{
    c->co_prev = NULL;
    c->co_next = callout_head;
    if (callout_head != NULL)
        callout_head->co_prev = c;
    callout_head = c;
}

static void
callout_remove(callout_t *c)
{
    if (c == callout_head) {
        callout_head = c->co_next;
        if (callout_head != NULL)
            callout_head->co_prev = NULL;
        return;
    }
    if (c->co_prev != NULL)
        c->co_prev->co_next = c->co_next;
    if (c->co_next != NULL)
        c->co_next->co_prev = c->co_prev;
}

void
callout_init(callout_t *c, u_int flags)
{
    (void)flags;
    c->func = NULL;
}

#ifdef DEBUG
void
callout_list(void)
{
    callout_t *cur;

    for (cur = callout_head; cur != NULL; cur = cur->co_next) {
        printf("%c %d %p(%p)\n", (cur == callout_head) ? '>' : ' ',
               cur->ticks, cur->func, cur->arg);
    }
}
#endif

int
callout_pending(callout_t *c)
{
    PRINTF_CALLOUT("callout %spending\n", (c->func == NULL) ? "not " : "");
    return (c->func != NULL);
}

int
callout_stop(callout_t *c)
{
    int pending = (c->func != NULL);
    PRINTF_CALLOUT("callout stop %p\n", c->func);
    c->func = NULL;
    callout_remove(c);
    return (pending);
}

void
callout_reset(callout_t *c, int ticks, void (*func)(void *), void *arg)
{
    c->ticks = ticks;
    c->func = func;
    c->arg = arg;

    callout_remove(c);
    callout_add(c);
    PRINTF_CALLOUT("callout_reset %p(%x) at %d\n",
                   c->func, (uint32_t) c->arg, ticks);
}

void
callout_call(callout_t *c)
{
    if (c->func == NULL) {
        PRINTF_CALLOUT("callout to NULL function\n");
        return;
    }

    PRINTF_CALLOUT("callout_call %p(%x)\n", c->func, (uint32_t) c->arg);
    c->func(c->arg);
}

void
callout_run_timeouts(void)
{
    callout_t *cur;

    for (cur = callout_head; cur != NULL; cur = cur->co_next) {
        if (cur->ticks == 1) {
            cur->ticks = 0;
            callout_call(cur);
        } else if (cur->ticks != 0) {
            cur->ticks -= TICKS_PER_SECOND;
            if (cur->ticks < 1)
                cur->ticks = 1;
        }
    }
}
