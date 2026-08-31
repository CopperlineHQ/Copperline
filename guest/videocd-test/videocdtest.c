/*
 * SPDX-FileCopyrightText: 2026 The Copperline project
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Guest-side ABI probe for the open cartridge-resident videocd.library.
 * It exercises every custom vector, including the reserved no-op, against a
 * mounted Video CD and writes a compact transcript to VIDEOCD-RESULT for the
 * host test.
 */

#include <exec/execbase.h>
#include <exec/libraries.h>
#include <exec/types.h>
#include <utility/tagitem.h>

#include <dos/dos.h>

#define EXEC_BASE_NAME _sysbase
#define DOS_BASE_NAME _dosbase
#include <inline/dos.h>
#include <inline/exec.h>

#define VIDEOCD_DISC_VCD 4

#define VCDTAG_TITLE         (TAG_USER | 0x100aUL)
#define VCDTAG_TRACK_COUNT   (TAG_USER | 0x100cUL)
#define VCDTAG_ITEM_KIND     (TAG_USER | 0x1065UL)
#define VCDTAG_VOLUME_COUNT  (TAG_USER | 0x1067UL)
#define VCDTAG_VOLUME_NUMBER (TAG_USER | 0x1068UL)
#define VCDTAG_ENTRY_COUNT   (TAG_USER | 0x1070UL)
#define VCDTAG_TRACK_NUMBER  (TAG_USER | 0x1100UL)
#define VCDTAG_START_LSN     (TAG_USER | 0x1101UL)
#define VCDTAG_END_LSN       (TAG_USER | 0x1102UL)

#define VCD_ITEM_DISC 0x101UL
#define VCD_ITEM_TRACK 0x102UL

#define LVO_RESERVED         -30
#define LVO_CLASSIFY_DISC    -36
#define LVO_OPEN_DISC        -42
#define LVO_CLOSE_DISC       -48
#define LVO_DESCRIBE_ITEM    -54
#define LVO_FREE_DESCRIPTION -60

typedef ULONG (*ReservedProc)(struct Library *base __asm("a6"));
typedef ULONG (*ClassifyDiscProc)(struct Library *base __asm("a6"));
typedef APTR (*OpenDiscProc)(
    APTR source __asm("a0"),
    const struct TagItem *tags __asm("a1"),
    struct Library *base __asm("a6"));
typedef void (*CloseDiscProc)(
    APTR disc __asm("a0"),
    struct Library *base __asm("a6"));
typedef struct TagItem *(*DescribeItemProc)(
    ULONG item __asm("d0"),
    APTR disc __asm("a0"),
    const struct TagItem *tags __asm("a1"),
    struct Library *base __asm("a6"));
typedef void (*FreeDescriptionProc)(
    struct TagItem *description __asm("a0"),
    struct Library *base __asm("a6"));

#define VCD_PROC(type, base, lvo) ((type)((char *)(base) + (lvo)))

static struct ExecBase *sysbase(void)
{
    struct ExecBase *base;
    __asm("move.l 4.w,%0" : "=r"(base));
    return base;
}

static LONG strlen_local(const char *text)
{
    LONG length = 0;
    while (text[length] != '\0')
        length++;
    return length;
}

static void write_text(struct Library *_dosbase, BPTR file, const char *text)
{
    Write(file, (APTR)text, strlen_local(text));
}

static void write_hex(
    struct Library *_dosbase,
    BPTR file,
    const char *name,
    ULONG value)
{
    static const char digits[] = "0123456789abcdef";
    char buffer[9];
    int i;

    for (i = 0; i < 8; i++)
        buffer[i] = digits[(value >> ((7 - i) * 4)) & 0x0f];
    buffer[8] = '\0';
    write_text(_dosbase, file, name);
    write_text(_dosbase, file, "=");
    write_text(_dosbase, file, buffer);
    write_text(_dosbase, file, "\n");
}

static BOOL find_tag(
    const struct TagItem *tags,
    ULONG wanted,
    ULONG *value)
{
    while (tags && tags->ti_Tag != TAG_DONE) {
        if (tags->ti_Tag == wanted) {
            *value = tags->ti_Data;
            return TRUE;
        }
        tags++;
    }
    return FALSE;
}

LONG entry(void)
{
    struct ExecBase *_sysbase = sysbase();
    struct Library *_dosbase = OpenLibrary((STRPTR)"dos.library", 34);
    struct Library *vcd_base;
    BPTR file;
    ULONG classification;
    APTR disc;
    struct TagItem *disc_tags;
    struct TagItem *track_tags;
    ULONG item_kind = 0;
    ULONG title = 0;
    ULONG track_count = 0;
    ULONG entry_count = 0;
    ULONG volume_count = 0;
    ULONG volume_number = 0;
    ULONG track_kind = 0;
    ULONG track_number = 0;
    ULONG start_lsn = 0;
    ULONG end_lsn = 0;
    BOOL ok;

    if (!_dosbase)
        return 20;
    file = Open((STRPTR)"VIDEOCD-RESULT", MODE_NEWFILE);
    if (!file) {
        CloseLibrary(_dosbase);
        return 20;
    }

    vcd_base = OpenLibrary((STRPTR)"videocd.library", 41);
    if (!vcd_base) {
        write_text(_dosbase, file, "VIDEOTEST: FAIL open\n");
        Close(file);
        CloseLibrary(_dosbase);
        return 20;
    }

    ok = VCD_PROC(ReservedProc, vcd_base, LVO_RESERVED)(vcd_base) == 0;
    classification = VCD_PROC(
        ClassifyDiscProc, vcd_base, LVO_CLASSIFY_DISC)(vcd_base);
    write_hex(_dosbase, file, "class", classification);
    ok = ok && classification == VIDEOCD_DISC_VCD;

    disc = VCD_PROC(OpenDiscProc, vcd_base, LVO_OPEN_DISC)(
        NULL, NULL, vcd_base);
    disc_tags = disc ? VCD_PROC(
        DescribeItemProc, vcd_base, LVO_DESCRIBE_ITEM)(
            0, disc, NULL, vcd_base) : NULL;
    ok = ok && disc && disc_tags;
    if (disc_tags) {
        ok = find_tag(disc_tags, VCDTAG_ITEM_KIND, &item_kind) && ok;
        ok = find_tag(disc_tags, VCDTAG_TITLE, &title) && ok;
        ok = find_tag(disc_tags, VCDTAG_TRACK_COUNT, &track_count) && ok;
        ok = find_tag(disc_tags, VCDTAG_ENTRY_COUNT, &entry_count) && ok;
        ok = find_tag(disc_tags, VCDTAG_VOLUME_COUNT, &volume_count) && ok;
        ok = find_tag(disc_tags, VCDTAG_VOLUME_NUMBER, &volume_number) && ok;
        write_hex(_dosbase, file, "disc_kind", item_kind);
        write_text(_dosbase, file, "album=");
        if (title)
            write_text(_dosbase, file, (const char *)title);
        write_text(_dosbase, file, "\n");
        write_hex(_dosbase, file, "tracks", track_count);
        write_hex(_dosbase, file, "entries", entry_count);
        write_hex(_dosbase, file, "volume_count", volume_count);
        write_hex(_dosbase, file, "volume_number", volume_number);
        ok = ok && item_kind == VCD_ITEM_DISC && track_count >= 1 &&
            entry_count >= track_count && title != 0 && volume_count == 1 &&
            volume_number == 1;
    }

    track_tags = disc ? VCD_PROC(
        DescribeItemProc, vcd_base, LVO_DESCRIBE_ITEM)(
            1, disc, NULL, vcd_base) : NULL;
    ok = ok && track_tags;
    if (track_tags) {
        ok = find_tag(track_tags, VCDTAG_ITEM_KIND, &track_kind) && ok;
        ok = find_tag(track_tags, VCDTAG_TRACK_NUMBER, &track_number) && ok;
        ok = find_tag(track_tags, VCDTAG_START_LSN, &start_lsn) && ok;
        ok = find_tag(track_tags, VCDTAG_END_LSN, &end_lsn) && ok;
        write_hex(_dosbase, file, "track_kind", track_kind);
        write_hex(_dosbase, file, "track_number", track_number);
        write_hex(_dosbase, file, "start_lsn", start_lsn);
        write_hex(_dosbase, file, "end_lsn", end_lsn);
        ok = ok && track_kind == VCD_ITEM_TRACK && track_number == 2 &&
            end_lsn > start_lsn;
    }

    if (track_tags)
        VCD_PROC(FreeDescriptionProc, vcd_base, LVO_FREE_DESCRIPTION)(
            track_tags, vcd_base);
    if (disc_tags)
        VCD_PROC(FreeDescriptionProc, vcd_base, LVO_FREE_DESCRIPTION)(
            disc_tags, vcd_base);
    if (disc)
        VCD_PROC(CloseDiscProc, vcd_base, LVO_CLOSE_DISC)(disc, vcd_base);
    CloseLibrary(vcd_base);

    write_text(_dosbase, file,
        ok ? "VIDEOTEST: PASS\n" : "VIDEOTEST: FAIL data\n");
    Close(file);
    CloseLibrary(_dosbase);
    return ok ? 0 : 20;
}
