// SPDX-License-Identifier: GPL-3.0-or-later
//
// modemtest: guest-side probe for the modem integration test
// (tests/modem_e2e.rs). Talks to Paula's serial port through
// serial.device directly -- OpenDevice/DoIO, the way a real terminal
// program (Term, NComm) does it -- rather than through a dos.library
// filename: SER: needs a Mount entry this boot volume has no reason to
// carry, and AUX: hands back an interactive console/shell rather than a
// byte stream a program can read and write itself.
//
// Reads the dial target from DIALTARGET (host:port, no trailing
// newline) in the current directory -- the host test writes this next
// to the binary before boot, so the target's ephemeral port never has
// to be baked into the binary -- runs a short AT command sequence
// (reset, dial, send a line, escape, hang up), and writes a full
// transcript to MODEMLOG in the current directory for the host side to
// inspect afterwards.
//
// Ordinary relocatable hunk executable, standard -noixemul startup (see
// ../toolchain.mk's Makefile comment and guest/zz9kprobe's, which
// verified this startup style against both AROS and Kickstart 3.1) --
// unlike guest/hostfs-test's hand-rolled entry(), this program links
// dos.library/exec.library through the ordinary proto/ headers, which
// -noixemul's startup code initializes before main() runs.

#include <stdio.h>
#include <string.h>
#include <exec/types.h>
#include <exec/io.h>
#include <exec/errors.h>
#include <devices/serial.h>
#include <proto/exec.h>
#include <proto/dos.h>
#include <clib/alib_protos.h>

static FILE *log_fp;
static struct MsgPort *port;
static struct IOExtSer *req;

static void log_line(const char *tag, const char *buf, long n) {
    fprintf(log_fp, "[%s %ld] ", tag, n);
    for (long i = 0; i < n; i++) {
        unsigned char c = (unsigned char)buf[i];
        if (c == '\r') fprintf(log_fp, "\\r");
        else if (c == '\n') fprintf(log_fp, "\\n");
        else if (c >= 32 && c < 127) fputc(c, log_fp);
        else fprintf(log_fp, "\\x%02x", c);
    }
    fprintf(log_fp, "\n");
    fflush(log_fp);
}

static void send_cmd(const char *cmd) {
    char line[160];
    long n = 0;
    while (cmd[n] && n < (long)sizeof(line) - 1) {
        line[n] = cmd[n];
        n++;
    }
    line[n++] = '\r';
    req->IOSer.io_Command = CMD_WRITE;
    req->IOSer.io_Data = line;
    req->IOSer.io_Length = n;
    DoIO((struct IORequest *)req);
    log_line("TX", line, n);
}

// Poll SDCMD_QUERY/CMD_READ for up to `ticks` short spins, stopping
// early after a few consecutive empty polls once something has already
// arrived -- an AT response is available within a handful of ticks or
// not coming at all inside this budget, so there is nothing to gain by
// spinning the full budget on an idle line.
static void drain(long ticks) {
    char buf[256];
    long quiet = 0;
    for (long t = 0; t < ticks && quiet < 8; t++) {
        req->IOSer.io_Command = SDCMD_QUERY;
        DoIO((struct IORequest *)req);
        long avail = req->IOSer.io_Actual;
        if (avail > 0) {
            quiet = 0;
            if (avail > (long)sizeof(buf)) avail = sizeof(buf);
            req->IOSer.io_Command = CMD_READ;
            req->IOSer.io_Data = buf;
            req->IOSer.io_Length = avail;
            DoIO((struct IORequest *)req);
            if (req->IOSer.io_Actual > 0) {
                log_line("RX", buf, req->IOSer.io_Actual);
            }
        } else {
            quiet++;
        }
        for (volatile long i = 0; i < 20000; i++) {}
    }
}

// Write bytes exactly as given, with no CR appended -- send_cmd's trailing
// CR is right for an AT command line but fatal for the escape sequence,
// which is framed by silence rather than terminated by a carriage return.
static void send_raw(const char *bytes, long n) {
    req->IOSer.io_Command = CMD_WRITE;
    req->IOSer.io_Data = (APTR)bytes;
    req->IOSer.io_Length = n;
    DoIO((struct IORequest *)req);
    log_line("TX", bytes, n);
}

// Wait without transmitting anything, reading whatever arrives. Unlike
// drain() this never stops early: the guard windows around `+++` are
// defined by the absence of guest traffic for a full S12, so cutting the
// wait short on a quiet line is exactly the thing that must not happen.
static void idle(long ticks) {
    char buf[256];
    for (long t = 0; t < ticks; t++) {
        req->IOSer.io_Command = SDCMD_QUERY;
        DoIO((struct IORequest *)req);
        long avail = req->IOSer.io_Actual;
        if (avail > 0) {
            if (avail > (long)sizeof(buf)) avail = sizeof(buf);
            req->IOSer.io_Command = CMD_READ;
            req->IOSer.io_Data = buf;
            req->IOSer.io_Length = avail;
            DoIO((struct IORequest *)req);
            if (req->IOSer.io_Actual > 0) {
                log_line("RX", buf, req->IOSer.io_Actual);
            }
        }
        for (volatile long i = 0; i < 20000; i++) {}
    }
}

int main(void) {
    log_fp = fopen("MODEMLOG", "w");
    if (!log_fp) return 20;

    char target[128];
    FILE *tf = fopen("DIALTARGET", "r");
    if (!tf) {
        fprintf(log_fp, "could not open DIALTARGET\n");
        fclose(log_fp);
        return 20;
    }
    long tn = (long)fread(target, 1, sizeof(target) - 1, tf);
    fclose(tf);
    while (tn > 0 && (target[tn - 1] == '\n' || target[tn - 1] == '\r')) tn--;
    target[tn] = '\0';

    port = CreateMsgPort();
    if (!port) {
        fprintf(log_fp, "CreateMsgPort failed\n");
        fclose(log_fp);
        return 20;
    }
    req = (struct IOExtSer *)CreateExtIO(port, sizeof(struct IOExtSer));
    if (!req) {
        fprintf(log_fp, "CreateExtIO failed\n");
        DeleteMsgPort(port);
        fclose(log_fp);
        return 20;
    }

    LONG err = OpenDevice((CONST_STRPTR) "serial.device", 0, (struct IORequest *)req, 0);
    if (err) {
        fprintf(log_fp, "OpenDevice(serial.device) err=%ld\n", (long)err);
        fclose(log_fp);
        DeleteExtIO((struct IORequest *)req);
        DeleteMsgPort(port);
        return 20;
    }

    req->io_SerFlags &= ~SERF_XDISABLED;
    req->io_Baud = 9600;
    req->IOSer.io_Command = SDCMD_SETPARAMS;
    DoIO((struct IORequest *)req);

    send_cmd("ATZ");
    drain(40);

    char dial[160];
    snprintf(dial, sizeof(dial), "ATDT%s", target);
    send_cmd(dial);
    drain(80);

    send_cmd("HELLO FROM AMIGA");
    drain(60);

    // Hayes escape: S12 of silence, exactly three S2 characters, then S12
    // of silence again -- and nothing else transmitted inside either guard.
    // send_cmd would append a CR that lands in the trailing guard and
    // cancels the whole thing, leaving ATH below to be relayed to the
    // remote end as online data instead of executed as a command.
    idle(60);
    send_raw("+++", 3);
    idle(60);
    drain(40);

    send_cmd("ATH");
    drain(40);

    CloseDevice((struct IORequest *)req);
    DeleteExtIO((struct IORequest *)req);
    DeleteMsgPort(port);

    fprintf(log_fp, "modemtest done\n");
    fclose(log_fp);
    return 0;
}
