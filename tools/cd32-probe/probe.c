/*
 * cd32-probe: real-hardware measurement program for the CD32.
 *
 * Runs in place of SYS:FSCD on a modified copy of the Fightin' Spirit
 * disc image (see make-images.py), so the boot allocation history up to
 * this point -- SetPatch, CDInit, the extended ROM show -- is exactly the
 * game's. It renders every measurement as hex rows on an OS screen and
 * never exits (the startup-sequence's SYS:Reset must not run).
 *
 * Three question groups, matching docs/internals notes:
 *  - boot layout / freeanim teardown (does the game's uninitialized
 *    OpenLibrary version pass on real hardware, and what does the full
 *    show teardown release?)
 *  - CD drive locate/read timing (calibrates the Akiko seek model)
 *  - bus-class timing for unmapped/ROM/chip reads and writes (calibrates
 *    the slow-external billing), all against the CIA E-clock via
 *    timer.device ReadEClock.
 *
 * Build: ./build.sh (m68k-amigaos-gcc, -nostartfiles; entry() must stay
 * the first function in this file).
 */

#include "amiga-mini.h"

#define MEMF_LARGEST 0x20000UL

#define CMD_READ 2
#define CD_SEEK 10

#define EXEC_PORTLIST 392

struct Library *GfxBase;
struct Library *IntuitionBase;
struct Library *TimerBase;

static void probe_main(void);
static void halt(void);

/* AmigaDOS enters at the first byte of the first hunk. */
LONG entry(void)
{
    probe_main();
    return 0;
}

/* ----- tiny runtime (no libnix startup) ------------------------------- */

static void bzero_(void *p, ULONG n)
{
    UBYTE *b = (UBYTE *)p;
    while (n--) {
        *b++ = 0;
    }
}

/* ----- results --------------------------------------------------------- */

#define NROWS 30

static ULONG row_val[NROWS];
static const char *row_label[NROWS] = {
    "CLITASK",  /* 00 FindTask(NULL): the Initial CLI process address    */
    "LARGEST0", /* 01 AvailMem(CHIP|LARGEST) at entry                    */
    "FREE0",    /* 02 AvailMem(CHIP) at entry                            */
    "ANIMPORT", /* 03 FindPort("Startup Animation")                      */
    "FANFARE0", /* 04 FindTask("Fanfare")                                */
    "BUGOPEN",  /* 05 OpenLibrary(freeanim, version=task address)        */
    "BUGCLOSE", /* 06 E-ticks its CloseLibrary blocked (teardown wait)   */
    "OLDOPEN",  /* 07 OldOpenLibrary(freeanim), only if BUGOPEN == 0     */
    "OLDCLOSE", /* 08 E-ticks its CloseLibrary blocked                   */
    "LARGEST1", /* 09 AvailMem(CHIP|LARGEST) after teardown              */
    "FREE1",    /* 10 AvailMem(CHIP) after teardown                      */
    "FANFARE1", /* 11 FindTask("Fanfare") after (0 = task exited)        */
    "EFREQ",    /* 12 ReadEClock frequency (sanity: 709379 PAL)          */
    "ENTRYTIM", /* 13 E-clock at probe entry (ticks since power-on)      */
    "SKSW100",  /* 14 min E-ticks CD_SEEK +100 (driver-internal, no wire)*/
    "RD500",    /* 15 min E-ticks 1-sector CMD_READ at +500 (in track 1) */
    "RD1K",     /* 16 .. +1000 (track 1 edge)                            */
    "RD10K",    /* 17 .. +10000 (data only on the padded disc)           */
    "RD100K",   /* 18 .. +100000 (ditto)                                 */
    "RD200K",   /* 19 .. +200000 (ditto)                                 */
    "PL10K",    /* 20 min E-ticks CD_PLAYLSN start at LBA 11000          */
    "PL100K",   /* 21 .. at 101000                                       */
    "PL200K",   /* 22 .. at 201000                                       */
    "RDRATE64", /* 23 E-ticks for 64 sequential sectors (track 1)        */
    "URD",      /* 24 E-ticks 65536x CMP.W (A4)+ over $A80000 (unmapped) */
    "ROMRD",    /* 25 .. over $F80000 (Kickstart ROM)                    */
    "CHIPRD",   /* 26 .. over chip RAM                                   */
    "UWR",      /* 27 E-ticks 65536x MOVE.W D2,(A4)+ over $A80000        */
    "CHIPWR",   /* 28 .. over chip RAM                                   */
    "ULRD",     /* 29 E-ticks 32768x MOVE.L (A4)+,D1 over $A80000       */
};

/* ----- E-clock --------------------------------------------------------- */

static struct MsgPort timer_port;
static struct IOStdReq timer_io;
static ULONG eclock_freq;

static ULONG eticks(void)
{
    struct EClockVal ev;
    ReadEClock(&ev);
    return ev.ev_lo;
}

static struct MsgPort *port_init(struct MsgPort *port)
{
    BYTE sig = AllocSignal(-1);
    bzero_(port, sizeof(*port));
    port->mp_Node.ln_Type = NT_MSGPORT;
    port->mp_SigBit = sig;
    port->mp_SigTask = FindTask(NULL);
    port->mp_MsgList.lh_Head = (struct Node *)&port->mp_MsgList.lh_Tail;
    port->mp_MsgList.lh_TailPred = (struct Node *)&port->mp_MsgList.lh_Head;
    return port;
}

static int timer_init(void)
{
    port_init(&timer_port);
    bzero_(&timer_io, sizeof(timer_io));
    timer_io.io_Message.mn_ReplyPort = &timer_port;
    timer_io.io_Message.mn_Length = sizeof(timer_io);
    if (OpenDevice("timer.device", 0, (struct IORequest *)&timer_io, 0)) {
        return 0;
    }
    TimerBase = (struct Library *)timer_io.io_Device;
    {
        struct EClockVal ev;
        eclock_freq = ReadEClock(&ev);
    }
    return 1;
}

/* ----- layout / freeanim group ----------------------------------------- */

static struct Node *find_port(const char *name)
{
    struct List *ports = (struct List *)((UBYTE *)SysBase + EXEC_PORTLIST);
    return FindName(ports, name);
}

static void measure_layout(void)
{
    struct Task *self = FindTask(NULL);
    struct Library *lib;

    row_val[0] = (ULONG)self;
    row_val[1] = AvailMem(MEMF_CHIP | MEMF_LARGEST);
    row_val[2] = AvailMem(MEMF_CHIP);
    row_val[3] = (ULONG)find_port("Startup Animation");
    row_val[4] = (ULONG)FindTask("Fanfare");

    /* Replicate the game's bug exactly: CDInit calls OpenLibrary with D0
     * still holding its FindTask(NULL) result. The version check is a
     * signed word compare against lib_Version=40, so this passes only
     * when the process address's low word reads as <= 40. */
    lib = OpenLibrary("freeanim.library", (ULONG)self);
    row_val[5] = (ULONG)lib;
    row_val[6] = 0;
    row_val[7] = 0;
    row_val[8] = 0;
    if (lib) {
        ULONG t0 = eticks();
        CloseLibrary(lib);
        row_val[6] = eticks() - t0;
    } else {
        lib = OldOpenLibrary("freeanim.library");
        row_val[7] = (ULONG)lib;
        if (lib) {
            ULONG t0 = eticks();
            CloseLibrary(lib);
            row_val[8] = eticks() - t0;
        }
    }
    row_val[9] = AvailMem(MEMF_CHIP | MEMF_LARGEST);
    row_val[10] = AvailMem(MEMF_CHIP);
    row_val[11] = (ULONG)FindTask("Fanfare");
    row_val[12] = eclock_freq;
    row_val[13] = eticks();
}

/* ----- CD locate / read group ------------------------------------------ */

#define CD_PLAYLSN 39
#define TR_ADDREQUEST 9

static struct MsgPort cd_port;
static struct IOStdReq cd_io;
static struct MsgPort watchdog_port;
static struct IOStdReq watchdog_io; /* io_Actual/io_Length = tv_secs/micro */
static UBYTE *cd_buf; /* 128 KiB chip, shared with the CPU sweeps */

#define CD_TIMEOUT 0xCCCCCCCCUL

/* DoIO with a watchdog: an unknown drive/driver reaction to an odd
 * target must never hang the probe on a burned disc. Returns io_Error,
 * or -1 with *timed_out set. */
static BYTE cd_do_timeout(ULONG secs, int *timed_out)
{
    ULONG cdmask = 1UL << cd_port.mp_SigBit;
    ULONG wdmask = 1UL << watchdog_port.mp_SigBit;
    *timed_out = 0;
    cd_io.io_Flags = 0;
    SendIO((struct IORequest *)&cd_io);
    watchdog_io.io_Command = TR_ADDREQUEST;
    watchdog_io.io_Flags = 0;
    watchdog_io.io_Actual = secs; /* tv_secs */
    watchdog_io.io_Length = 0;    /* tv_micro */
    SendIO((struct IORequest *)&watchdog_io);
    for (;;) {
        ULONG got = Wait(cdmask | wdmask);
        if (CheckIO((struct IORequest *)&cd_io)) {
            WaitIO((struct IORequest *)&cd_io);
            AbortIO((struct IORequest *)&watchdog_io);
            WaitIO((struct IORequest *)&watchdog_io);
            SetSignal(0, cdmask | wdmask);
            return cd_io.io_Error;
        }
        if (got & wdmask && CheckIO((struct IORequest *)&watchdog_io)) {
            WaitIO((struct IORequest *)&watchdog_io);
            AbortIO((struct IORequest *)&cd_io);
            WaitIO((struct IORequest *)&cd_io);
            SetSignal(0, cdmask | wdmask);
            *timed_out = 1;
            return -1;
        }
    }
}

static BYTE cd_seek(ULONG lba)
{
    int to;
    cd_io.io_Command = CD_SEEK;
    cd_io.io_Offset = lba << 11;
    cd_io.io_Length = 0;
    cd_io.io_Data = NULL;
    return cd_do_timeout(6, &to);
}

static BYTE cd_read(ULONG lba, ULONG sectors, int *timed_out)
{
    cd_io.io_Command = CMD_READ;
    cd_io.io_Offset = lba << 11;
    cd_io.io_Length = sectors << 11;
    cd_io.io_Data = cd_buf;
    return cd_do_timeout(6 + sectors / 32, timed_out);
}

static BYTE cd_play(ULONG lba, int *timed_out)
{
    cd_io.io_Command = CD_PLAYLSN;
    cd_io.io_Offset = lba;
    cd_io.io_Length = 75; /* one second of audio */
    cd_io.io_Data = NULL;
    return cd_do_timeout(8, timed_out);
}

static BYTE cd_stop(void)
{
    int to;
    cd_io.io_Command = 6; /* CMD_STOP */
    cd_io.io_Offset = 0;
    cd_io.io_Length = 0;
    cd_io.io_Data = NULL;
    return cd_do_timeout(6, &to);
}

#define SEEK_BASE 1000UL

static ULONG timed_seek(ULONG distance)
{
    ULONG best = 0xFFFFFFFF;
    int rep;
    for (rep = 0; rep < 3; rep++) {
        ULONG t0, dt;
        if (cd_seek(SEEK_BASE)) {
            return 0xEE000000UL | (UBYTE)cd_io.io_Error;
        }
        t0 = eticks();
        if (cd_seek(SEEK_BASE + distance)) {
            return 0xEE000000UL | (UBYTE)cd_io.io_Error;
        }
        dt = eticks() - t0;
        if (dt < best) {
            best = dt;
        }
    }
    return best;
}

static ULONG timed_play(ULONG lba)
{
    ULONG best = 0xFFFFFFFF;
    int rep;
    for (rep = 0; rep < 3; rep++) {
        ULONG t0, dt;
        int to;
        BYTE err;
        cd_read(SEEK_BASE, 1, &to); /* drag the head back */
        t0 = eticks();
        err = cd_play(lba, &to);
        dt = eticks() - t0;
        if (to) {
            return CD_TIMEOUT;
        }
        if (err) {
            return 0xEE000000UL | (UBYTE)err;
        }
        cd_stop();
        if (dt < best) {
            best = dt;
        }
    }
    return best;
}

static ULONG timed_read1(ULONG distance)
{
    ULONG best = 0xFFFFFFFF;
    int rep;
    for (rep = 0; rep < 3; rep++) {
        ULONG t0, dt;
        int to;
        BYTE err;
        /* Drag the head back with a real read: CD_SEEK is serviced
         * inside the driver without moving the drive. */
        if (cd_read(SEEK_BASE, 1, &to)) {
            return to ? CD_TIMEOUT : (0xEE000000UL | (UBYTE)cd_io.io_Error);
        }
        t0 = eticks();
        err = cd_read(SEEK_BASE + distance, 1, &to);
        dt = eticks() - t0;
        if (to) {
            return CD_TIMEOUT;
        }
        if (err) {
            return 0xEE000000UL | (UBYTE)err;
        }
        if (dt < best) {
            best = dt;
        }
    }
    return best;
}

static void measure_cd(void)
{
    BYTE err;
    int to;

    port_init(&cd_port);
    bzero_(&cd_io, sizeof(cd_io));
    cd_io.io_Message.mn_ReplyPort = &cd_port;
    cd_io.io_Message.mn_Length = sizeof(cd_io);
    port_init(&watchdog_port);
    bzero_(&watchdog_io, sizeof(watchdog_io));
    watchdog_io.io_Message.mn_ReplyPort = &watchdog_port;
    watchdog_io.io_Message.mn_Length = sizeof(watchdog_io);
    if (OpenDevice("timer.device", 0, (struct IORequest *)&watchdog_io, 0)) {
        row_val[14] = 0xDD00DD00UL;
        return;
    }
    err = OpenDevice("cd.device", 0, (struct IORequest *)&cd_io, 0);
    if (err) {
        row_val[14] = 0xDD000000UL | (UBYTE)err;
        return;
    }

    row_val[14] = timed_seek(100);
    row_val[15] = timed_read1(500);
    row_val[16] = timed_read1(1000);
    row_val[17] = timed_read1(10000);
    row_val[18] = timed_read1(100000);
    row_val[19] = timed_read1(200000);
    row_val[20] = timed_play(11000);
    row_val[21] = timed_play(101000);
    row_val[22] = timed_play(201000);

    /* Sustained sequential rate at whatever speed CDInit configured;
     * LBA 1200..1263 stays inside the data track on both disc layouts. */
    if (!cd_read(1200, 1, &to)) {
        ULONG t0 = eticks();
        if (!cd_read(1201, 64, &to)) {
            row_val[23] = eticks() - t0;
        } else {
            row_val[23] = to ? CD_TIMEOUT : (0xEE000000UL | (UBYTE)cd_io.io_Error);
        }
    }
}

/* ----- bus-class group ------------------------------------------------- */

/* 65536 iterations of the ROM scan's data idiom, timed with interrupts
 * off (the E-clock keeps counting). DBF instead of the ROM's DBEQ so the
 * iteration count is constant regardless of swept data. */
static ULONG sweep_read_w(ULONG base)
{
    ULONG t0, dt;
    Disable();
    t0 = eticks();
    __asm__ __volatile__("move.l %0,%%a4\n\t"
                         "move.w #0x4afc,%%d2\n\t"
                         "move.l #65535,%%d0\n\t"
                         ".balign 4\n"
                         "1:\n\t"
                         "cmp.w (%%a4)+,%%d2\n\t"
                         "dbf %%d0,1b\n\t"
                         :
                         : "g"(base)
                         : "a4", "d0", "d2", "cc");
    dt = eticks() - t0;
    Enable();
    return dt;
}

static ULONG sweep_write_w(ULONG base)
{
    ULONG t0, dt;
    Disable();
    t0 = eticks();
    __asm__ __volatile__("move.l %0,%%a4\n\t"
                         "moveq #0,%%d2\n\t"
                         "move.l #65535,%%d0\n\t"
                         ".balign 4\n"
                         "1:\n\t"
                         "move.w %%d2,(%%a4)+\n\t"
                         "dbf %%d0,1b\n\t"
                         :
                         : "g"(base)
                         : "a4", "d0", "d2", "cc", "memory");
    dt = eticks() - t0;
    Enable();
    return dt;
}

static ULONG sweep_read_l(ULONG base)
{
    ULONG t0, dt;
    Disable();
    t0 = eticks();
    __asm__ __volatile__("move.l %0,%%a4\n\t"
                         "move.l #32767,%%d0\n\t"
                         ".balign 4\n"
                         "1:\n\t"
                         "move.l (%%a4)+,%%d1\n\t"
                         "dbf %%d0,1b\n\t"
                         :
                         : "g"(base)
                         : "a4", "d0", "d1", "cc");
    dt = eticks() - t0;
    Enable();
    return dt;
}

static ULONG min3(ULONG (*fn)(ULONG), ULONG base)
{
    ULONG best = 0xFFFFFFFF;
    int rep;
    for (rep = 0; rep < 3; rep++) {
        ULONG dt = fn(base);
        if (dt < best) {
            best = dt;
        }
    }
    return best;
}

static void measure_bus(void)
{
    row_val[24] = min3(sweep_read_w, 0xA80000UL);
    row_val[25] = min3(sweep_read_w, 0xF80000UL);
    row_val[26] = cd_buf ? min3(sweep_read_w, (ULONG)cd_buf) : 0;
    row_val[27] = min3(sweep_write_w, 0xA80000UL);
    row_val[28] = cd_buf ? min3(sweep_write_w, (ULONG)cd_buf) : 0;
    row_val[29] = min3(sweep_read_l, 0xA80000UL);
}

/* ----- display --------------------------------------------------------- */

/* Never exit: the startup-sequence's SYS:Reset must not run. The nop
 * keeps -Os from discarding the empty infinite loop. */
static void halt(void)
{
    for (;;) {
        __asm__ __volatile__("nop");
    }
}

static const char hexdigit[16] = "0123456789ABCDEF";

static void render(void)
{
    struct NewScreen ns;
    struct Screen *screen;
    struct RastPort *rp;
    int i;

    IntuitionBase = OpenLibrary("intuition.library", 0);
    GfxBase = OpenLibrary("graphics.library", 0);
    if (!IntuitionBase || !GfxBase) {
        halt();
    }
    bzero_(&ns, sizeof(ns));
    ns.Width = 320;
    ns.Height = 256;
    ns.Depth = 1;
    ns.DetailPen = 0;
    ns.BlockPen = 1;
    ns.Type = 0xF; /* CUSTOMSCREEN */
    screen = OpenScreen(&ns);
    if (!screen) {
        halt();
    }
    ShowTitle(screen, FALSE);
    rp = &screen->RastPort;
    SetAPen(rp, 1);
    for (i = 0; i < NROWS; i++) {
        char line[24];
        const char *label = row_label[i];
        ULONG v = row_val[i];
        int n = 0;
        int j;
        line[n++] = '0' + i / 10;
        line[n++] = '0' + i % 10;
        line[n++] = ' ';
        for (j = 0; j < 8; j++) {
            line[n++] = *label ? *label++ : ' ';
        }
        line[n++] = ' ';
        for (j = 28; j >= 0; j -= 4) {
            line[n++] = hexdigit[(v >> j) & 0xF];
        }
        GfxMove(rp, 8, 16 + i * 8);
        GfxText(rp, line, n);
    }
    GfxMove(rp, 168, 254);
    GfxText(rp, "CD32-PROBE 1", 12);
    halt();
}

static void probe_main(void)
{
    if (!timer_init()) {
        halt();
    }
    measure_layout();
    cd_buf = AllocMem(131072, MEMF_CHIP | MEMF_CLEAR);
    measure_cd();
    measure_bus();
    render();
}
