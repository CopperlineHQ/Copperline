/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * copperhf.device: M3 boot-ROM mounter. Walks each present unit's RDSK
 * (src/harddrive.rs guarantees every attached image -- RDB or bare --
 * presents as a valid RDSK within the first 16 sectors, so this is the
 * mounter's only input shape: no bare-partition path exists), builds a
 * DeviceNode per PART block, and adds it to the system's mount list;
 * separately walks rdb_FileSysHeaderList and loads any FSHD's LSEG chain
 * into FileSystem.resource so a partition's own filesystem code travels
 * with the disk when Kickstart doesn't already have that dostype built in.
 *
 * Narrow interface, one entry point (chf_mount_all), so MIRAGE's ROM can
 * link this object file unchanged once it has its own board/ConfigDev to
 * hand in -- see COPPERHF-DEVICE-PLAN.md's M3 section.
 *
 * Called from device.c's resident_init() strictly after AddDevice() and
 * strictly before AddIntServer()/CHF_IRQ_ENABLE: every I/O this file issues
 * is the polled doorbell protocol below (chf_do_io), which spins on
 * CHF_COMPLETE_GET/ACK itself and never expects int_handler.s's ReplyMsg to
 * run -- the portless IORequest blocks built here have no MsgPort at all,
 * so if the INT2 server were already live it would ReplyMsg into garbage.
 * Mounting first, with the completion queue left fully drained afterwards,
 * makes the two paths mutually exclusive by construction.
 *
 * -mpcrel/no-relocations/no-data-bss discipline (entry.s's header comment):
 * every allocation here is AllocMem'd (real Amiga RAM); the only static
 * storage this file touches is entry.s's read-only device_name string
 * (ordinary compiler-generated PC-relative code, not a hand data
 * directive -- device.c already relies on the same thing for ln_Name).
 *
 * Behavioural reference: https://github.com/LIV2/lide.device (GPL-2.0-only,
 * READ ONLY -- Copperline is GPL-3.0-or-later, so this file is written
 * fresh against the NDK 3.2 autodocs/includes and this project's own
 * guest/services/handler.c, never copied from lide).
 */

#include <exec/types.h>
#include <exec/nodes.h>
#include <exec/lists.h>
#include <exec/memory.h>
#include <exec/execbase.h>
#include <exec/resident.h>

#include <dos/dos.h>
#include <dos/dosextens.h>
#include <dos/filehandler.h>
#include <dos/doshunks.h>

#include <devices/hardblocks.h>
#include <resources/filesysres.h>

#include <libraries/configvars.h>
#include <libraries/expansion.h>
#include <libraries/expansionbase.h>

#define EXEC_BASE_NAME _sysbase
#define EXPANSION_BASE_NAME _expbase
#include <inline/exec.h>
#include <inline/expansion.h>

#include "copperhf_board.h"

/* entry.s: the shared "copperhf.device" name string, ordinary extern data
 * (compiler-generated PC-relative access, not a hand `.long` -- see this
 * file's header comment). */
extern char device_name[];

/* -------------------------------------------------------------------
 * Polled I/O: a 48-byte word-aligned IOStdReq-shaped block on the stack,
 * addressed purely by copperhf_board.h's byte offsets (the same idiom
 * device.c/int_handler.s use for the board window itself) rather than a C
 * struct, so nothing here depends on the compiler's struct-packing of a
 * type this ROM does not otherwise define. CHF_COMPLETE_GET is idempotent
 * (copperhf_board.h) -- ack only after we have positively identified our
 * own request, and pop defensively if some other pointer is sitting there
 * (never possible in the pre-IRQ, single-in-flight mounter, but the loop
 * must not wedge if it somehow is).
 * ------------------------------------------------------------------- */

#define CHF_IOB_LONGS 12 /* 12*4 = 48 bytes: covers every field up to +44 */

/* AbsExecBase -- device.c's sysbase() idiom; needed here because
 * chf_do_io's callers don't all carry _sysbase and the cache calls below
 * want it. */
static struct ExecBase *chf_sysbase(void)
{
    struct ExecBase *base;
    __asm("move.l 4.w,%0" : "=r"(base));
    return base;
}

/* DMA cache maintenance around the polled path, mirroring device.c's
 * chf_pre_dma/chf_post_dma (see the full rationale there): the host reads
 * the request block at doorbell time and writes io_Actual/io_Error (plus
 * the CMD_READ payload) back behind the CPU's data cache. The mounter
 * only ever rings CMD_READ, so these cover just the request block and its
 * read buffer. V33/V34 (Kickstart 1.3) has no cache vectors -- and no
 * cached CPUs -- so both helpers no-op there. */
static void chf_cache_pre_io(ULONG *iob)
{
    struct ExecBase *_sysbase = chf_sysbase();
    UBYTE *b = (UBYTE *)iob;
    ULONG len;

    if (_sysbase->LibNode.lib_Version < 37)
        return;
    len = CHF_IOB_LONGS * 4;
    CachePreDMA((APTR)iob, (LONG *)&len, 0);
    if (*(volatile UWORD *)(b + CHF_IO_COMMAND) == CHF_CMD_READ) {
        len = *(volatile ULONG *)(b + CHF_IO_LENGTH);
        CachePreDMA((APTR)*(volatile ULONG *)(b + CHF_IO_DATA), (LONG *)&len, 0);
    }
}

static void chf_cache_post_io(ULONG *iob)
{
    struct ExecBase *_sysbase = chf_sysbase();
    UBYTE *b = (UBYTE *)iob;
    ULONG len;

    if (_sysbase->LibNode.lib_Version < 37)
        return;
    /* The request block first: io_Error (read by chf_do_io's caller
     * contract below) and io_Actual are host-written. */
    len = CHF_IOB_LONGS * 4;
    CachePostDMA((APTR)iob, (LONG *)&len, 0);
    if (*(volatile UWORD *)(b + CHF_IO_COMMAND) == CHF_CMD_READ) {
        len = *(volatile ULONG *)(b + CHF_IO_LENGTH);
        CachePostDMA((APTR)*(volatile ULONG *)(b + CHF_IO_DATA), (LONG *)&len, 0);
    }
}

static BOOL chf_do_io(UBYTE *board, ULONG *iob)
{
    UBYTE *b = (UBYTE *)iob;
    ULONG req = (ULONG)iob;
    ULONG spins;

    chf_cache_pre_io(iob);
    *(volatile ULONG *)(board + CHF_DOORBELL) = req;

    /* The board executes synchronously today (M1/M5 protocol comment in
     * copperhf_board.h), so this loop resolves on its very first pass in
     * practice; it is bounded anyway so a future asynchronous board (or a
     * genuinely wedged one) degrades this unit rather than hanging boot
     * forever. */
    for (spins = 0; spins < 2000000UL; spins++) {
        ULONG done = *(volatile ULONG *)(board + CHF_COMPLETE_GET);
        if (done == req) {
            *(volatile UWORD *)(board + CHF_COMPLETE_ACK) = 0;
            /* Before io_Error (host-written) is believed, drop any stale
             * cached lines over the request block and the read buffer. */
            chf_cache_post_io(iob);
            return *(volatile BYTE *)(b + CHF_IO_ERROR) == 0;
        }
        if (done != 0) {
            /* Not ours -- pop it so the queue can drain to our entry
             * instead of stalling behind it forever. Never expected before
             * AddIntServer, but cheap to handle. */
            *(volatile UWORD *)(board + CHF_COMPLETE_ACK) = 0;
        }
    }
    return FALSE;
}

static BOOL chf_read_block(UBYTE *board, ULONG unit, ULONG blocknum, UBYTE *dest)
{
    ULONG iob[CHF_IOB_LONGS];
    /* HARD-WON: these field stores MUST be volatile. They go through
     * UBYTE*-then-recast pointers into a ULONG[] object -- strict-aliasing
     * UB that this toolchain's GCC really acts on: the non-volatile version
     * of every store below was silently dead-store-eliminated at -Os (the
     * disassembly showed the block zeroed and the doorbell rung with no
     * field ever written), so the board saw io_Command == 0 and failed
     * every mount-time read with IOERR_NOCMD. The Makefile also passes
     * -fno-strict-aliasing now, but volatile is what makes these stores
     * non-negotiable to the optimizer. */
    volatile UBYTE *b = (volatile UBYTE *)iob;
    ULONG i;
    for (i = 0; i < CHF_IOB_LONGS; i++)
        iob[i] = 0;

    *(volatile ULONG *)(b + CHF_IO_UNIT) = unit;
    *(volatile UWORD *)(b + CHF_IO_COMMAND) = CHF_CMD_READ;
    *(volatile ULONG *)(b + CHF_IO_LENGTH) = CHF_SECTOR_SIZE;
    *(volatile ULONG *)(b + CHF_IO_DATA) = (ULONG)dest;
    *(volatile ULONG *)(b + CHF_IO_OFFSET) = blocknum * CHF_SECTOR_SIZE;

    return chf_do_io(board, iob);
}

/* -------------------------------------------------------------------
 * LSEG chain streaming reader: rather than buffer a whole hunk file (this
 * ROM has no data/bss and rt_Init's stack is not to be trusted with large
 * frames -- see handler.c's own HANDLER_STACK comment), the hunk loader
 * below pulls bytes one LSEG block at a time out of the shared per-unit
 * sector buffer, transparently issuing the next CMD_READ as the current
 * block's 492-byte payload (512 - the 20-byte LSEG header) runs out.
 * ------------------------------------------------------------------- */

#define LSEG_PAYLOAD 492 /* 123 longs, matches struct LoadSegBlock */

struct LsegStream {
    UBYTE *board;
    ULONG unit;
    ULONG next_block; /* ~0 = no more LSEG blocks */
    UBYTE *buf;        /* shared CHF_SECTOR_SIZE scratch buffer */
    UWORD pos;         /* consumed bytes of the current payload */
    UWORD guard;       /* bounds a corrupt/cyclic chain */
};

static BOOL lseg_refill(struct LsegStream *s)
{
    struct LoadSegBlock *lsb;
    /* 2048 blocks * 492 bytes bounds the chain at ~1 MiB -- generous for
     * any real filesystem (pfs3aio, the largest common RDB-carried one, is
     * well under 200 KiB) while still terminating on a cyclic chain. */
    if (s->next_block == (ULONG)~0UL || s->guard++ > 2048)
        return FALSE;
    if (!chf_read_block(s->board, s->unit, s->next_block, s->buf))
        return FALSE;
    lsb = (struct LoadSegBlock *)s->buf;
    if (lsb->lsb_ID != IDNAME_LOADSEG)
        return FALSE;
    s->next_block = lsb->lsb_Next;
    s->pos = 0;
    return TRUE;
}

static BOOL lseg_skip(struct LsegStream *s, ULONG n)
{
    while (n > 0) {
        ULONG avail, chunk;
        if (s->pos >= LSEG_PAYLOAD && !lseg_refill(s))
            return FALSE;
        avail = LSEG_PAYLOAD - s->pos;
        chunk = (n < avail) ? n : avail;
        s->pos += (UWORD)chunk;
        n -= chunk;
    }
    return TRUE;
}

static BOOL lseg_read_bytes(struct LsegStream *s, UBYTE *dest, ULONG n)
{
    while (n > 0) {
        ULONG avail, chunk, i;
        UBYTE *src;
        if (s->pos >= LSEG_PAYLOAD && !lseg_refill(s))
            return FALSE;
        avail = LSEG_PAYLOAD - s->pos;
        chunk = (n < avail) ? n : avail;
        src = s->buf + 20 + s->pos;
        for (i = 0; i < chunk; i++)
            dest[i] = src[i];
        dest += chunk;
        s->pos += (UWORD)chunk;
        n -= chunk;
    }
    return TRUE;
}

static BOOL lseg_read_long(struct LsegStream *s, ULONG *out)
{
    UBYTE b[4];
    if (!lseg_read_bytes(s, b, 4))
        return FALSE;
    *out = ((ULONG)b[0] << 24) | ((ULONG)b[1] << 16) | ((ULONG)b[2] << 8) | b[3];
    return TRUE;
}

static BOOL lseg_read_word(struct LsegStream *s, UWORD *out)
{
    UBYTE b[2];
    if (!lseg_read_bytes(s, b, 2))
        return FALSE;
    *out = (UWORD)(((UWORD)b[0] << 8) | b[1]);
    return TRUE;
}

/* -------------------------------------------------------------------
 * Minimal hunk loader: HUNK_HEADER, HUNK_CODE/DATA/BSS, HUNK_RELOC32 (and
 * its V37/V39 synonyms), HUNK_SYMBOL/HUNK_DEBUG (skipped), HUNK_END.
 * Anything else (overlay, EXT, unknown) fails the load rather than risk
 * misparsing -- callers treat a NULL seglist as "no filesystem code
 * available", which is always safe (the DeviceNode still mounts with
 * dn_SegList clear; see chf_mount_partition).
 *
 * Seglist layout matches ordinary AmigaDOS LoadSeg output: each segment is
 * one AllocMem'd block -- a size longword first (the whole allocation's
 * byte count, exactly what UnLoadSeg would FreeMem), then the
 * next-segment BPTR link (0 = last), then the hunk's own bytes. The BPTR
 * handed back to callers (and stored as dn_SegList/fse_SegList) points at
 * the link longword, so code starts at BPTR*4 + 4, per the standard
 * seglist convention.
 *
 * Each hunk is allocated at its HUNK_HEADER size-table entry, not its
 * body's own length: the header entry is the hunk's *memory* size and the
 * body may legally be shorter (trailing zeros truncated), with the gap
 * expected zero-filled -- and RELOC32 offsets range over the memory size,
 * so allocating from the body length would let a legal reloc write past
 * the buffer.
 * ------------------------------------------------------------------- */

#define CHF_MAX_HUNKS 16
/* Bound on a single hunk's declared memory size, chosen far above any real
 * filesystem handler this device will ever LSEG-load (pfs3aio, the
 * largest common one, is ~60 KiB) but far enough below the ULONG range
 * that `hunk_size + 8` (the seglist link/size header, see
 * chf_load_lseg_chain) can never wrap. HARD-WON: an unbounded hunk size
 * read straight off disk is exploitable, not just theoretical -- a
 * crafted image naming a hunk size near 0x3FFFFFFF makes
 * `AllocMem(size + 8, ...)` wrap to a handful of bytes (`+8` overflows
 * ULONG), so the allocation "succeeds" tiny while every later bounds
 * check still trusts the huge unwrapped `sizes[i]`, and the hunk body
 * copy that follows heap-overflows guest memory with attacker-controlled
 * bytes. */
#define CHF_MAX_HUNK_BYTES (4UL * 1024 * 1024)

static BPTR chf_load_lseg_chain(struct ExecBase *_sysbase, UBYTE *board, ULONG unit,
                                 LONG first_block, UBYTE *buf)
{
    struct LsegStream s;
    ULONG v, table_size, first_hunk, last_hunk, nhunks;
    ULONG sizes[CHF_MAX_HUNKS];
    UBYTE *segs[CHF_MAX_HUNKS];
    ULONG cur, i;
    BOOL ok = TRUE;

    if (first_block < 0)
        return 0;

    s.board = board;
    s.unit = unit;
    s.buf = buf;
    s.next_block = (ULONG)first_block;
    s.pos = LSEG_PAYLOAD; /* forces the first read to refill */
    s.guard = 0;

    if (!lseg_read_long(&s, &v) || v != HUNK_HEADER)
        return 0;

    /* Resident-library name table: (length-longs, that many longs of
     * name) pairs, terminated by a zero length. Boot-time load files never
     * carry one, but the format requires we consume it either way. */
    for (;;) {
        ULONG len;
        if (!lseg_read_long(&s, &len))
            return 0;
        if (len == 0)
            break;
        if (!lseg_skip(&s, len * 4))
            return 0;
    }

    if (!lseg_read_long(&s, &table_size) || !lseg_read_long(&s, &first_hunk) ||
        !lseg_read_long(&s, &last_hunk))
        return 0;
    (void)table_size;
    if (last_hunk < first_hunk)
        return 0;
    nhunks = last_hunk - first_hunk + 1;
    if (nhunks == 0 || nhunks > CHF_MAX_HUNKS)
        return 0;

    for (i = 0; i < nhunks; i++) {
        ULONG sz;
        if (!lseg_read_long(&s, &sz))
            return 0;
        sizes[i] = (sz & 0x3FFFFFFFUL) * 4;
        /* Reject anything past CHF_MAX_HUNK_BYTES before it ever reaches
         * an AllocMem call -- see that constant's own comment for why an
         * unbounded size here is a heap-overflow primitive, not just a
         * wasteful allocation. */
        if (sizes[i] > CHF_MAX_HUNK_BYTES)
            return 0;
        segs[i] = NULL;
    }

    /* HARD-WON (M6): every hunk is allocated here, from the header's own
     * size table, before any hunk body or relocation is read -- exactly
     * what real LoadSeg does, and required for correctness: a
     * HUNK_RELOC32/RELOC32SHORT record may legally target a hunk that
     * appears *later* in the file (a hunk 0 field pointing at hunk 2, for
     * instance), and the hunk format places no ordering constraint on
     * that at all. The previous version of this loop allocated segs[cur]
     * lazily, only when it reached that hunk's own CODE/DATA/BSS record --
     * so a forward-referencing reloc always found segs[idx] still NULL
     * and (correctly, per that stale check) failed the whole load. Every
     * hunk covered by this project's own tests before M6 happened to
     * only reference *earlier* hunks, so this never showed up until a
     * fixture actually needed a forward reference. */
    for (i = 0; i < nhunks; i++) {
        segs[i] = AllocMem(sizes[i] + 8, MEMF_PUBLIC | MEMF_CLEAR);
        if (segs[i] == NULL) {
            ULONG j;
            for (j = 0; j < nhunks; j++)
                if (segs[j] != NULL)
                    FreeMem(segs[j], sizes[j] + 8);
            return 0;
        }
    }

    cur = 0;
    while (ok && cur < nhunks) {
        ULONG htype, szlong, bytes;

        if (!lseg_read_long(&s, &htype)) {
            ok = FALSE;
            break;
        }

        if (htype == HUNK_CODE || htype == HUNK_DATA || htype == HUNK_BSS) {
            if (!lseg_read_long(&s, &szlong)) {
                ok = FALSE;
                break;
            }
            bytes = (szlong & 0x3FFFFFFFUL) * 4;
            if (bytes > sizes[cur]) {
                ok = FALSE; /* body longer than the header's memory size:
                             * corrupt file, and the header size is what we
                             * allocate (see the layout comment above) */
                break;
            }
            /* Already allocated above (MEMF_CLEAR there covers both BSS
             * and a truncated CODE/DATA tail here). */
            if (htype != HUNK_BSS && !lseg_read_bytes(&s, segs[cur] + 8, bytes)) {
                ok = FALSE;
                break;
            }
        } else {
            ok = FALSE; /* hunk body must open with CODE/DATA/BSS */
            break;
        }

        /* Trailer records for this hunk: RELOC32 (and synonyms), SYMBOL,
         * DEBUG, terminated by HUNK_END. */
        for (;;) {
            ULONG rec;
            if (!lseg_read_long(&s, &rec)) {
                ok = FALSE;
                break;
            }
            if (rec == HUNK_END)
                break;
            if (rec == HUNK_RELOC32) {
                for (;;) {
                    ULONG count, idx, base, k;
                    if (!lseg_read_long(&s, &count)) {
                        ok = FALSE;
                        break;
                    }
                    if (count == 0)
                        break;
                    /* A reloc record's hunk number is ABSOLUTE (the file's
                     * global first_hunk..last_hunk numbering, per the hunk
                     * format -- see this loop's own comment on why the
                     * memory-size table is read in that same numbering),
                     * not a 0-based index into this file's own local
                     * segs[]/sizes[] tables -- normalize by first_hunk
                     * before indexing. first_hunk is 0 for almost every
                     * real linked executable, which is why this bug is
                     * silent until a file legitimately sets it. */
                    if (!lseg_read_long(&s, &idx) || idx < first_hunk || idx > last_hunk) {
                        ok = FALSE;
                        break;
                    }
                    idx -= first_hunk;
                    if (segs[idx] == NULL) {
                        ok = FALSE;
                        break;
                    }
                    base = (ULONG)(segs[idx] + 8);
                    for (k = 0; k < count; k++) {
                        ULONG off, old, nv;
                        UBYTE *field;
                        if (!lseg_read_long(&s, &off)) {
                            ok = FALSE;
                            break;
                        }
                        if (off + 4 > sizes[cur]) {
                            ok = FALSE;
                            break;
                        }
                        field = segs[cur] + 8 + off;
                        old = ((ULONG)field[0] << 24) | ((ULONG)field[1] << 16) |
                              ((ULONG)field[2] << 8) | field[3];
                        nv = old + base;
                        field[0] = (UBYTE)(nv >> 24);
                        field[1] = (UBYTE)(nv >> 16);
                        field[2] = (UBYTE)(nv >> 8);
                        field[3] = (UBYTE)nv;
                    }
                    if (!ok)
                        break;
                }
            } else if (rec == HUNK_RELOC32SHORT || rec == HUNK_DREL32) {
                /* The V37+ compact form (HUNK_DREL32 doubles as its id in
                 * *load* files, per the RKRM's own errata): every field --
                 * count, hunk number, and each offset -- is a 16-bit WORD,
                 * and the record is padded back to a longword boundary
                 * with one zero word if the total word count came out odd.
                 * Parsing this as the longword form (or vice versa) shears
                 * the whole remaining stream. */
                ULONG words = 0;
                for (;;) {
                    UWORD count, idx, k;
                    if (!lseg_read_word(&s, &count)) {
                        ok = FALSE;
                        break;
                    }
                    words++;
                    if (count == 0)
                        break;
                    /* Same absolute-vs-local hunk-number normalization as
                     * the HUNK_RELOC32 loop above -- see its comment. */
                    if (!lseg_read_word(&s, &idx) || (ULONG)idx < first_hunk ||
                        (ULONG)idx > last_hunk) {
                        ok = FALSE;
                        break;
                    }
                    idx = (UWORD)((ULONG)idx - first_hunk);
                    if (segs[idx] == NULL) {
                        ok = FALSE;
                        break;
                    }
                    words++;
                    for (k = 0; k < count; k++) {
                        UWORD off;
                        ULONG base, old, nv;
                        UBYTE *field;
                        if (!lseg_read_word(&s, &off)) {
                            ok = FALSE;
                            break;
                        }
                        words++;
                        if ((ULONG)off + 4 > sizes[cur]) {
                            ok = FALSE;
                            break;
                        }
                        base = (ULONG)(segs[idx] + 8);
                        field = segs[cur] + 8 + off;
                        old = ((ULONG)field[0] << 24) | ((ULONG)field[1] << 16) |
                              ((ULONG)field[2] << 8) | field[3];
                        nv = old + base;
                        field[0] = (UBYTE)(nv >> 24);
                        field[1] = (UBYTE)(nv >> 16);
                        field[2] = (UBYTE)(nv >> 8);
                        field[3] = (UBYTE)nv;
                    }
                    if (!ok)
                        break;
                }
                if (ok && (words & 1)) {
                    UWORD pad;
                    if (!lseg_read_word(&s, &pad))
                        ok = FALSE;
                }
            } else if (rec == HUNK_SYMBOL) {
                for (;;) {
                    ULONG len;
                    if (!lseg_read_long(&s, &len)) {
                        ok = FALSE;
                        break;
                    }
                    if (len == 0)
                        break;
                    if (!lseg_skip(&s, len * 4 + 4)) { /* name + value long */
                        ok = FALSE;
                        break;
                    }
                }
            } else if (rec == HUNK_DEBUG) {
                ULONG len;
                if (!lseg_read_long(&s, &len) || !lseg_skip(&s, len * 4)) {
                    ok = FALSE;
                    break;
                }
            } else {
                ok = FALSE; /* HUNK_EXT or anything else: bail, don't guess */
                break;
            }
            if (!ok)
                break;
        }
        if (!ok)
            break;
        cur++;
    }

    if (!ok || cur != nhunks) {
        for (i = 0; i < nhunks; i++)
            if (segs[i] != NULL)
                FreeMem(segs[i], sizes[i] + 8);
        return 0;
    }

    for (i = 0; i < nhunks; i++) {
        *(ULONG *)segs[i] = sizes[i] + 8; /* UnLoadSeg's FreeMem size */
        *(ULONG *)(segs[i] + 4) =
            (i + 1 < nhunks) ? (ULONG)MKBADDR(segs[i + 1] + 4) : 0;
    }
    return MKBADDR(segs[0] + 4);
}

/* -------------------------------------------------------------------
 * FileSystem.resource: created lazily (V34 has no ROM-seeded one; V36+
 * normally already has DOS\0/DOS\1 entries, which is exactly the case
 * that lets an FSHD walk be a no-op -- see this file's header comment and
 * the AROS CI gate, which never needs the LSEG path at all).
 * ------------------------------------------------------------------- */

static struct FileSysResource *chf_get_or_create_fsr(struct ExecBase *_sysbase)
{
    struct FileSysResource *fsr = (struct FileSysResource *)OpenResource((CONST_STRPTR)FSRNAME);
    if (fsr != NULL)
        return fsr;

    fsr = AllocMem(sizeof(*fsr), MEMF_PUBLIC | MEMF_CLEAR);
    if (fsr == NULL)
        return NULL;
    fsr->fsr_Node.ln_Type = NT_RESOURCE;
    fsr->fsr_Node.ln_Name = (char *)FSRNAME;
    fsr->fsr_Creator = (char *)device_name;
    /* Manual struct List init (the empty-list idiom exec/lists.h's own
     * NewList macro expands to): avoids depending on which list-init
     * helper name this toolchain's NDK vintage exposes. */
    fsr->fsr_FileSysEntries.lh_Head = (struct Node *)&fsr->fsr_FileSysEntries.lh_Tail;
    fsr->fsr_FileSysEntries.lh_Tail = NULL;
    fsr->fsr_FileSysEntries.lh_TailPred = (struct Node *)&fsr->fsr_FileSysEntries.lh_Head;
    AddResource(fsr);
    return fsr;
}

/* Finds an existing FileSysEntry for `dostype`, or -- if `fshdlist` names a
 * matching FSHD block -- loads its LSEG chain and adds one. `buf` is
 * clobbered (shared per-unit scratch sector). Returns NULL if neither
 * source has this dostype, or on any read/parse/allocation failure --
 * every caller treats that as "mount without filesystem code", never as a
 * reason to abandon the mount itself. */
static struct FileSysEntry *chf_find_or_load_filesystem(struct ExecBase *_sysbase, UBYTE *board,
                                                          ULONG unit, ULONG fshdlist,
                                                          ULONG dostype, UBYTE *buf)
{
    struct FileSysResource *fsr = (struct FileSysResource *)OpenResource((CONST_STRPTR)FSRNAME);
    ULONG node;
    UWORD guard;

    if (fsr != NULL) {
        struct FileSysEntry *fse;
        for (fse = (struct FileSysEntry *)fsr->fsr_FileSysEntries.lh_Head;
             fse->fse_Node.ln_Succ != NULL; fse = (struct FileSysEntry *)fse->fse_Node.ln_Succ) {
            if (fse->fse_DosType == dostype)
                return fse; /* Already present -- an existing entry, ROM or
                             * ours, always wins over re-loading from disk. */
        }
    }

    if (fshdlist == (ULONG)~0UL)
        return NULL;

    node = fshdlist;
    guard = 0;
    while (node != (ULONG)~0UL && guard++ < 200) {
        struct FileSysHeaderBlock *fhb;
        ULONG next;

        if (!chf_read_block(board, unit, node, buf))
            return NULL;
        fhb = (struct FileSysHeaderBlock *)buf;
        if (fhb->fhb_ID != IDNAME_FILESYSHEADER)
            return NULL;
        next = fhb->fhb_Next;

        if (fhb->fhb_DosType == dostype) {
            /* Copy every field we still need before chf_load_lseg_chain
             * reuses `buf` for the LSEG reads. */
            ULONG dt = fhb->fhb_DosType, ver = fhb->fhb_Version, patch = fhb->fhb_PatchFlags;
            ULONG type = fhb->fhb_Type, task = fhb->fhb_Task, lock = fhb->fhb_Lock;
            ULONG handler = fhb->fhb_Handler, stack = fhb->fhb_StackSize;
            LONG prio = fhb->fhb_Priority, startup = fhb->fhb_Startup;
            LONG seglistblk = fhb->fhb_SegListBlocks, gv = fhb->fhb_GlobalVec;
            BPTR seglist;
            struct FileSysResource *r;
            struct FileSysEntry *fse;

            seglist = chf_load_lseg_chain(_sysbase, board, unit, seglistblk, buf);
            if (seglist == 0)
                return NULL;

            r = chf_get_or_create_fsr(_sysbase);
            if (r == NULL)
                return NULL;
            fse = AllocMem(sizeof(*fse), MEMF_PUBLIC | MEMF_CLEAR);
            if (fse == NULL)
                return NULL;
            fse->fse_Node.ln_Name = (char *)device_name;
            fse->fse_Node.ln_Type = NT_UNKNOWN;
            fse->fse_DosType = dt;
            fse->fse_Version = ver;
            fse->fse_PatchFlags = patch;
            fse->fse_Type = type;
            fse->fse_Task = (CPTR)task;
            fse->fse_Lock = (BPTR)lock;
            fse->fse_Handler = (BSTR)handler;
            fse->fse_StackSize = stack;
            fse->fse_Priority = prio;
            fse->fse_Startup = (BPTR)startup;
            fse->fse_SegList = seglist;
            fse->fse_GlobalVec = (BPTR)gv;
            AddTail(&r->fsr_FileSysEntries, &fse->fse_Node);
            return fse;
        }
        node = next;
    }
    return NULL;
}

/* fse_PatchFlags bit assignment (filesysres.doc's own example, "$180 for
 * substitute SegList & GlobalVec"): one bit per DeviceNode field, in the
 * field's declared order starting at dn_Type. */
#define FSE_PF_TYPE 0x001
#define FSE_PF_TASK 0x002
#define FSE_PF_LOCK 0x004
#define FSE_PF_HANDLER 0x008
#define FSE_PF_STACKSIZE 0x010
#define FSE_PF_PRIORITY 0x020
#define FSE_PF_STARTUP 0x040
#define FSE_PF_SEGLIST 0x080
#define FSE_PF_GLOBALVEC 0x100

static void chf_apply_patch(struct DeviceNode *dn, struct FileSysEntry *fse)
{
    ULONG pf = fse->fse_PatchFlags;
    if (pf & FSE_PF_TYPE)
        dn->dn_Type = fse->fse_Type;
    if (pf & FSE_PF_TASK)
        dn->dn_Task = (struct MsgPort *)fse->fse_Task;
    if (pf & FSE_PF_LOCK)
        dn->dn_Lock = fse->fse_Lock;
    if (pf & FSE_PF_HANDLER)
        dn->dn_Handler = fse->fse_Handler;
    if (pf & FSE_PF_STACKSIZE)
        dn->dn_StackSize = fse->fse_StackSize;
    if (pf & FSE_PF_PRIORITY)
        dn->dn_Priority = fse->fse_Priority;
    if (pf & FSE_PF_STARTUP)
        dn->dn_Startup = fse->fse_Startup;
    if (pf & FSE_PF_SEGLIST)
        dn->dn_SegList = fse->fse_SegList;
    if (pf & FSE_PF_GLOBALVEC)
        dn->dn_GlobalVec = fse->fse_GlobalVec;
}

/* -------------------------------------------------------------------
 * PART -> DeviceNode -> boot list.
 * ------------------------------------------------------------------- */

/* V34 has no AddBootNode (V36+): hand-build the NT_BOOTNODE
 * guest/services/handler.c's own resident_init already builds and proves
 * on real 1.3/2.0/3.1, Enqueue'd on eb_MountList with ln_Name = ConfigDev
 * so strap can trace the winning boot vote back to our DiagArea and its
 * da_BootPoint (entry.s). */
static void chf_add_boot_node_v34(struct ExecBase *_sysbase, struct Library *_expbase,
                                   LONG pri, struct DeviceNode *dn, struct ConfigDev *cd)
{
    struct BootNode *bn = AllocMem(sizeof(*bn), MEMF_PUBLIC | MEMF_CLEAR);
    if (bn == NULL) {
        AddDosNode(pri, ADNF_STARTPROC, dn);
        return;
    }
    bn->bn_Node.ln_Type = NT_BOOTNODE;
    bn->bn_Node.ln_Pri = (BYTE)pri;
    bn->bn_Node.ln_Name = (char *)cd;
    bn->bn_DeviceNode = dn;
    Forbid();
    Enqueue(&((struct ExpansionBase *)_expbase)->MountList, &bn->bn_Node);
    Permit();
}

static void chf_mount_partition(struct ExecBase *_sysbase, struct Library *_expbase, UBYTE *board,
                                 struct ConfigDev *cd, ULONG unit, const UBYTE *namebstr,
                                 const ULONG *env, ULONG flags, ULONG fshdlist, UBYTE *buf)
{
    UBYTE namelen;
    UBYTE namepad;
    ULONG tablesize, envwords, dostype;
    LONG bootpri;
    BOOL bootable = (flags & PBFF_BOOTABLE) != 0;
    UBYTE devnamelen;
    ULONG allocsize;
    UBYTE *mem;
    struct DeviceNode *dn;
    UBYTE *bname, *devbstr;
    struct FileSysStartupMsg *fssm;
    ULONG *envcopy;
    struct FileSysEntry *fse;
    ULONG i;

    if (flags & PBFF_NOMOUNT)
        return;

    namelen = namebstr[0];
    if (namelen > 31)
        namelen = 31;
    /* HARD-WON (M6): `bname` starts longword-aligned (mem is AllocMem'd,
     * sizeof(struct DeviceNode) is a whole number of longs), but the
     * volume BSTR occupies 1+namelen bytes -- an ODD total whenever
     * namelen is itself even -- and fssm/envcopy, laid out right after
     * it below, are addressed with ordinary MOVE.L (FileSysStartupMsg's
     * BPTR/ULONG fields, and envcopy is ULONG[]). 68000 raises an
     * Address Error on any odd-address long access, and every partition
     * name this project's own tests used before M6 ("DH0", length 3)
     * happened to need no padding -- M6's "TST0" (length 4) does, and
     * without this pad every M6-shaped RDB address-faults mid-mount, in
     * the *general* partition-mounting code, nothing FSHD/LSEG-specific.
     * Pad up to the next longword so this can never recur regardless of
     * name length. */
    namepad = (UBYTE)((4 - ((1 + (ULONG)namelen) & 3)) & 3);

    tablesize = env[0];
    if (tablesize > 19) /* pb_Environment[20]: indices 0..19 only */
        tablesize = 19;
    envwords = tablesize + 1;
    dostype = env[16];
    bootpri = bootable ? (LONG)env[15] : -128;

    devnamelen = 0;
    while (device_name[devnamelen] != '\0' && devnamelen < 200)
        devnamelen++;

    allocsize = sizeof(struct DeviceNode) + 1 + namelen + namepad /* volume BSTR, longword-padded */
                + sizeof(struct FileSysStartupMsg) + envwords * sizeof(ULONG) + 1 +
                devnamelen; /* device-name BSTR */

    mem = AllocMem(allocsize, MEMF_PUBLIC | MEMF_CLEAR);
    if (mem == NULL)
        return; /* No memory: skip this partition, boot continues. */

    dn = (struct DeviceNode *)mem;
    bname = mem + sizeof(struct DeviceNode);
    fssm = (struct FileSysStartupMsg *)(bname + 1 + namelen + namepad);
    envcopy = (ULONG *)(fssm + 1);
    devbstr = (UBYTE *)(envcopy + envwords);

    bname[0] = namelen;
    for (i = 0; i < namelen; i++)
        bname[1 + i] = namebstr[1 + i];

    for (i = 0; i < envwords; i++)
        envcopy[i] = env[i];
    /* If the PART block's de_TableSize claimed more entries than
     * pb_Environment can physically hold (clamped above), the copy's own
     * de_TableSize must describe what was actually copied -- DOS trusts it
     * and would read past the copy otherwise. */
    envcopy[0] = tablesize;

    devbstr[0] = devnamelen;
    for (i = 0; i < devnamelen; i++)
        devbstr[1 + i] = device_name[i];

    fssm->fssm_Unit = unit;
    fssm->fssm_Device = MKBADDR(devbstr);
    fssm->fssm_Environ = MKBADDR(envcopy);
    fssm->fssm_Flags = 0;

    dn->dn_Type = DLT_DEVICE;
    dn->dn_Task = NULL;
    dn->dn_Lock = 0;
    /* Never NULL: 1.3's BCPL boot init dereferences DeviceNode fields it
     * finds along the way, and a NULL BPTR walks address 0 -- the same
     * reasoning guest/services/handler.c documents for its own dn_Handler.
     * Harmless when actually used as a LoadSeg name (no matching file just
     * fails DOS's mount cleanly) since dn_SegList, when set below, always
     * takes priority. */
    dn->dn_Handler = MKBADDR(devbstr);
    dn->dn_StackSize = 0;
    dn->dn_Priority = 0;
    dn->dn_Startup = MKBADDR(fssm);
    dn->dn_SegList = 0;
    dn->dn_GlobalVec = (BPTR)-1; /* not a BCPL program */
    dn->dn_Name = MKBADDR(bname);

    fse = chf_find_or_load_filesystem(_sysbase, board, unit, fshdlist, dostype, buf);
    if (fse != NULL)
        chf_apply_patch(dn, fse);

    /* HARD-WON (M6): ADNF_STARTPROC ("start a handler process
     * immediately") is documented (expansion.doc/AddBootNode) as normally
     * unset -- "the process is started only when the device node is
     * first referenced" otherwise. This file used to pass it
     * unconditionally for every partition, bootable or not: harmless for
     * M3's single bootable DH0 (autoboot legitimately needs its handler
     * running right away), but for a non-bootable partition it forces
     * AmigaDOS to start the loaded SegList as a real process during the
     * boot-time device scan, before anything ever opens the volume.
     * Likewise "Pass a NULL ConfigDev pointer to create a non-bootable
     * node" (same autodoc) -- this file always passed the real `cd`.
     * Neither flag change alone turned out to be the actual fix for
     * M6's own probe (see guest/copperhf-test/gen_lsegfix.py's own
     * header comment for what was: a handler that properly answers
     * every DOS packet it receives) -- both are still worth doing, since
     * both are exactly what the autodocs say a non-bootable mount should
     * do, independent of anything M6-specific. */
    {
        ULONG start_flags = bootable ? ADNF_STARTPROC : 0;
        struct ConfigDev *boot_cd = bootable ? cd : NULL;

        if (_expbase->lib_Version >= 36) {
            AddBootNode(bootpri, start_flags, dn, boot_cd);
        } else if (bootpri == -128) {
            AddDosNode(bootpri, start_flags, dn);
        } else {
            chf_add_boot_node_v34(_sysbase, _expbase, bootpri, dn, cd);
        }
    }
}

static void chf_mount_unit(struct ExecBase *_sysbase, struct Library *_expbase, UBYTE *board,
                            struct ConfigDev *cd, ULONG unit, UBYTE *buf)
{
    ULONG b, partlist, fshdlist, pnode;
    UWORD guard;
    BOOL found = FALSE;

    partlist = (ULONG)~0UL;
    fshdlist = (ULONG)~0UL;

    for (b = 0; b < RDB_LOCATION_LIMIT; b++) {
        struct RigidDiskBlock *rdb;
        if (!chf_read_block(board, unit, b, buf))
            break; /* a read failure this early means the unit is not
                     * usable at all -- stop scanning it, try the next
                     * unit. */
        rdb = (struct RigidDiskBlock *)buf;
        if (rdb->rdb_ID == IDNAME_RIGIDDISK) {
            partlist = rdb->rdb_PartitionList;
            fshdlist = rdb->rdb_FileSysHeaderList;
            found = TRUE;
            break;
        }
    }
    if (!found)
        return; /* No RDSK in the first 16 sectors: junk/foreign media --
                  * src/harddrive.rs guarantees this cannot happen for any
                  * image copperhf itself attached, but a corrupt image is
                  * still just "nothing to mount", never a boot hazard. */

    pnode = partlist;
    guard = 0;
    while (pnode != (ULONG)~0UL && guard++ < 64) {
        struct PartitionBlock *pb;
        UBYTE namebstr[32];
        ULONG env[20], flags, next;
        ULONG i;

        if (!chf_read_block(board, unit, pnode, buf))
            break;
        pb = (struct PartitionBlock *)buf;
        if (pb->pb_ID != IDNAME_PARTITION)
            break;

        /* Copy out everything chf_mount_partition needs before any further
         * block read (FSHD/LSEG) reuses `buf`. */
        for (i = 0; i < 32; i++)
            namebstr[i] = pb->pb_DriveName[i];
        for (i = 0; i < 20; i++)
            env[i] = pb->pb_Environment[i];
        flags = pb->pb_Flags;
        next = pb->pb_Next;

        chf_mount_partition(_sysbase, _expbase, board, cd, unit, namebstr, env, flags, fshdlist,
                             buf);
        pnode = next;
    }
}

/* Entry point: called from device.c's resident_init(), after AddDevice()
 * and before AddIntServer()/CHF_IRQ_ENABLE (see this file's header
 * comment). `sysbase`/`board`/`cd` are exactly resident_init()'s own
 * ExecBase, board base, and ConfigDev -- nothing here re-derives them. */
void chf_mount_all(struct ExecBase *sysbase, APTR board, struct ConfigDev *cd)
{
    struct ExecBase *_sysbase = sysbase;
    UBYTE *brd = (UBYTE *)board;
    UWORD units, present, unit;
    UBYTE *buf;
    struct Library *_expbase;

    units = *(volatile UWORD *)(brd + CHF_UNITS);
    present = *(volatile UWORD *)(brd + CHF_UNIT_PRESENT);
    if (units > CHF_NUM_UNITS) /* defensive: never trust the board past the
                                 * protocol's own declared cap */
        units = CHF_NUM_UNITS;

    if (present == 0)
        return; /* nothing attached: skip the library opens entirely */

    buf = AllocMem(CHF_SECTOR_SIZE, MEMF_PUBLIC);
    if (buf == NULL)
        return;

    _expbase = OpenLibrary((STRPTR) "expansion.library", 0);
    if (_expbase == NULL) {
        FreeMem(buf, CHF_SECTOR_SIZE);
        return;
    }

    for (unit = 0; unit < units; unit++) {
        if (present & (1U << unit))
            chf_mount_unit(_sysbase, _expbase, brd, cd, unit, buf);
    }

    CloseLibrary(_expbase);
    FreeMem(buf, CHF_SECTOR_SIZE);
}
