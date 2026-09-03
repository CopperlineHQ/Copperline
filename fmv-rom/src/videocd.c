/*
 * SPDX-FileCopyrightText: 2026 The Copperline project
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Clean-room Video CD metadata library. The binary vector order and disc
 * classifier result were established by observing the original public API;
 * the parser below is an independent implementation of the White Book
 * INFO.VCD and ENTRIES.VCD sector formats using the public cd.device API.
 */

#define __USE_SYSBASE

#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include <devices/cd.h>
#include <dos/dos.h>
#include <exec/errors.h>
#include <exec/execbase.h>
#include <exec/io.h>
#include <exec/memory.h>
#include <exec/resident.h>
#include <proto/exec.h>
#include <utility/tagitem.h>

#include "videocd.h"

extern struct ExecBase *SysBase;

#define VCD_SECTOR_SIZE 2048UL
#define VCD_INFO_LSN 150UL
#define VCD_ENTRIES_LSN 151UL
#define VCD_MAX_TRACK 99
#define VCD_MAX_ENTRIES 500
#define VCD_DESCRIPTION_MAGIC 0x56434454UL /* "VCDT" */

struct VCDEntry {
    ULONG lsn;
    UBYTE track;
    UBYTE reserved[3];
};

struct VideoCDDisc {
    ULONG magic;
    UWORD entry_count;
    UBYTE first_track;
    UBYTE last_track;
    UWORD volume_count;
    UWORD volume_number;
    ULONG lead_out_lsn;
    ULONG track_lsn[VCD_MAX_TRACK + 1];
    struct VCDEntry *entries;
    char album_id[17];
};

struct VCDDescriptionHeader {
    ULONG magic;
    ULONG size;
};

struct VCDSession {
    struct MsgPort *port;
    struct IOStdReq *io;
    struct CDInfo saved_info;
    BOOL device_open;
    BOOL configured;
};

struct InitTable {
    ULONG base_size;
    APTR *function_table;
    APTR data_table;
    APTR init_function;
};

static struct VideoCDBase *InitLibrary(
    struct ExecBase *sysbase __asm("a6"),
    BPTR seglist __asm("a0"),
    struct VideoCDBase *base __asm("d0"));
static struct VideoCDBase *OpenLibraryVector(
    struct VideoCDBase *base __asm("a6"));
static BPTR CloseLibraryVector(struct VideoCDBase *base __asm("a6"));
static BPTR ExpungeLibraryVector(struct VideoCDBase *base __asm("a6"));
static ULONG ExtFuncLibraryVector(void);
static ULONG ReservedLibraryVector(void);
static ULONG ClassifyDiscVector(struct VideoCDBase *base __asm("a6"));
static struct VideoCDDisc *OpenDiscVector(
    APTR source __asm("a0"),
    const struct TagItem *tags __asm("a1"),
    struct VideoCDBase *base __asm("a6"));
static void CloseDiscVector(
    struct VideoCDDisc *disc __asm("a0"),
    struct VideoCDBase *base __asm("a6"));
static struct TagItem *DescribeItemVector(
    ULONG item __asm("d0"),
    struct VideoCDDisc *disc __asm("a0"),
    const struct TagItem *tags __asm("a1"),
    struct VideoCDBase *base __asm("a6"));
static void FreeDescriptionVector(
    struct TagItem *description __asm("a0"),
    struct VideoCDBase *base __asm("a6"));

static const char library_name[] = VIDEOCD_NAME;
static const char library_id[] = VIDEOCD_NAME " 41.0 (31.08.2026)";
static const char ver_string[] __attribute__((used)) =
    "\0$VER: " VIDEOCD_NAME " 41.0 (31.08.2026)";

static APTR VideoCDFuncTab[] = {
    OpenLibraryVector,
    CloseLibraryVector,
    ExpungeLibraryVector,
    ExtFuncLibraryVector,
    ReservedLibraryVector,
    ClassifyDiscVector,
    OpenDiscVector,
    CloseDiscVector,
    DescribeItemVector,
    FreeDescriptionVector,
    (APTR)-1,
};

static struct InitTable VideoCDInitTab = {
    sizeof(struct VideoCDBase),
    VideoCDFuncTab,
    NULL,
    InitLibrary,
};

struct Resident VideoCDROMTag __attribute__((used)) = {
    RTC_MATCHWORD,
    &VideoCDROMTag,
    (APTR)(&VideoCDROMTag + 1),
    RTF_AUTOINIT,
    VIDEOCD_VERSION,
    NT_LIBRARY,
    0,
    (char *)library_name,
    (char *)library_id,
    &VideoCDInitTab,
};

static UWORD read_be16(const UBYTE *p)
{
    return ((UWORD)p[0] << 8) | p[1];
}

static int bcd_to_int(UBYTE value)
{
    int high = value >> 4;
    int low = value & 0x0f;
    if (high > 9 || low > 9)
        return -1;
    return high * 10 + low;
}

static ULONG align4(ULONG value)
{
    return (value + 3) & ~3UL;
}

static void restore_cd_config(struct VCDSession *session)
{
    struct TagItem tags[] = {
        { TAGCD_READSPEED, session->saved_info.ReadSpeed },
        { TAGCD_SECTORSIZE, session->saved_info.SectorSize },
        { TAGCD_XLECC, session->saved_info.XLECC },
        { TAG_DONE, 0 },
    };

    session->io->io_Command = CD_CONFIG;
    session->io->io_Data = tags;
    session->io->io_Length = 0;
    DoIO((struct IORequest *)session->io);
}

static void close_cd(struct VCDSession *session)
{
    if (session->configured)
        restore_cd_config(session);
    if (session->device_open)
        CloseDevice((struct IORequest *)session->io);
    if (session->io)
        DeleteIORequest((struct IORequest *)session->io);
    if (session->port)
        DeleteMsgPort(session->port);
    memset(session, 0, sizeof(*session));
}

static BOOL open_cd(struct VCDSession *session)
{
    struct TagItem tags[] = {
        { TAGCD_READSPEED, 75 },
        { TAGCD_SECTORSIZE, VCD_SECTOR_SIZE },
        { TAG_DONE, 0 },
    };

    memset(session, 0, sizeof(*session));
    session->port = CreateMsgPort();
    if (session->port)
        session->io = (struct IOStdReq *)CreateIORequest(
            session->port, sizeof(*session->io));
    if (!session->io || OpenDevice((CONST_STRPTR)"cd.device", 0,
            (struct IORequest *)session->io, 0) != 0) {
        close_cd(session);
        return FALSE;
    }
    session->device_open = TRUE;

    session->io->io_Command = CD_INFO;
    session->io->io_Data = &session->saved_info;
    session->io->io_Length = sizeof(session->saved_info);
    DoIO((struct IORequest *)session->io);
    if (session->io->io_Error != 0 ||
        session->io->io_Actual < sizeof(session->saved_info)) {
        close_cd(session);
        return FALSE;
    }

    session->io->io_Command = CD_CONFIG;
    session->io->io_Data = tags;
    session->io->io_Length = 0;
    session->configured = TRUE;
    DoIO((struct IORequest *)session->io);
    if (session->io->io_Error != 0) {
        close_cd(session);
        return FALSE;
    }
    return TRUE;
}

static BOOL read_sector(
    struct VCDSession *session,
    ULONG lsn,
    UBYTE *buffer)
{
    session->io->io_Command = CD_READ;
    session->io->io_Data = buffer;
    session->io->io_Length = VCD_SECTOR_SIZE;
    session->io->io_Offset = lsn * VCD_SECTOR_SIZE;
    DoIO((struct IORequest *)session->io);
    return session->io->io_Error == 0 &&
        session->io->io_Actual == VCD_SECTOR_SIZE;
}

static BOOL read_toc(
    struct VCDSession *session,
    struct VideoCDDisc *disc)
{
    union CDTOC summary;
    union CDTOC *toc = NULL;
    ULONG count;
    ULONG i;
    BOOL ok = FALSE;

    session->io->io_Command = CD_TOCLSN;
    session->io->io_Data = &summary;
    session->io->io_Length = 1;
    session->io->io_Offset = 0;
    DoIO((struct IORequest *)session->io);
    if (session->io->io_Error != 0 || session->io->io_Actual != 1 ||
        summary.Summary.FirstTrack == 0 ||
        summary.Summary.LastTrack < summary.Summary.FirstTrack ||
        summary.Summary.LastTrack > VCD_MAX_TRACK)
        return FALSE;

    count = (ULONG)summary.Summary.LastTrack + 1;
    toc = AllocMem(count * sizeof(*toc), MEMF_PUBLIC | MEMF_CLEAR);
    if (!toc)
        return FALSE;

    session->io->io_Command = CD_TOCLSN;
    session->io->io_Data = toc;
    session->io->io_Length = count;
    session->io->io_Offset = 0;
    DoIO((struct IORequest *)session->io);
    if (session->io->io_Error != 0 || session->io->io_Actual < count)
        goto out;

    disc->first_track = toc[0].Summary.FirstTrack;
    disc->last_track = toc[0].Summary.LastTrack;
    disc->lead_out_lsn = toc[0].Summary.LeadOut.LSN;
    for (i = 1; i < count; i++) {
        UBYTE track = toc[i].Entry.Track;
        if (track <= VCD_MAX_TRACK)
            disc->track_lsn[track] = toc[i].Entry.Position.LSN;
    }
    for (i = disc->first_track; i <= disc->last_track; i++) {
        ULONG end = i < disc->last_track ?
            disc->track_lsn[i + 1] : disc->lead_out_lsn;
        if (end <= disc->track_lsn[i])
            goto out;
    }
    ok = TRUE;

out:
    FreeMem(toc, count * sizeof(*toc));
    return ok;
}

static BOOL parse_entries(
    struct VideoCDDisc *disc,
    const UBYTE *sector)
{
    UWORD count;
    UWORD i;

    if (memcmp(sector, "ENTRYVCD", 8) != 0)
        return FALSE;
    count = read_be16(sector + 10);
    if (count == 0 || count > VCD_MAX_ENTRIES ||
        12UL + (ULONG)count * 4 > VCD_SECTOR_SIZE)
        return FALSE;
    if (count != 0) {
        disc->entries = AllocMem(
            (ULONG)count * sizeof(*disc->entries), MEMF_PUBLIC | MEMF_CLEAR);
        if (!disc->entries)
            return FALSE;
    }
    disc->entry_count = count;

    for (i = 0; i < count; i++) {
        const UBYTE *entry = sector + 12 + (ULONG)i * 4;
        int track = bcd_to_int(entry[0]);
        int minute = bcd_to_int(entry[1]);
        int second = bcd_to_int(entry[2]);
        int frame = bcd_to_int(entry[3]);
        ULONG absolute;

        if (track < 2 || track > disc->last_track || minute < 0 ||
            second < 0 || second >= 60 || frame < 0 || frame >= 75)
            return FALSE;
        absolute = ((ULONG)minute * 60 + (ULONG)second) * 75 +
            (ULONG)frame;
        disc->entries[i].track = (UBYTE)track;
        disc->entries[i].lsn = absolute >= 150 ? absolute - 150 : 0;
        if (disc->entries[i].lsn < disc->track_lsn[track] ||
            disc->entries[i].lsn >= disc->lead_out_lsn)
            return FALSE;
    }
    return TRUE;
}

static void parse_info(struct VideoCDDisc *disc, const UBYTE *sector)
{
    ULONG i;
    ULONG end = 16;

    for (i = 0; i < 16; i++)
        disc->album_id[i] = (char)sector[10 + i];
    while (end > 0 && (disc->album_id[end - 1] == ' ' ||
                       disc->album_id[end - 1] == '\0'))
        end--;
    disc->album_id[end] = '\0';
    if (end == 0)
        strcpy(disc->album_id, "Video CD");
    disc->volume_count = read_be16(sector + 26);
    disc->volume_number = read_be16(sector + 28);
}

static BOOL has_vcd_signature(const UBYTE *info, const UBYTE *entries)
{
    return memcmp(info, "VIDEO_CD", 8) == 0 &&
        memcmp(entries, "ENTRYVCD", 8) == 0;
}

static void free_disc(struct VideoCDDisc *disc)
{
    if (!disc)
        return;
    if (disc->entries)
        FreeMem(disc->entries,
            (ULONG)disc->entry_count * sizeof(*disc->entries));
    FreeMem(disc, sizeof(*disc));
}

static struct VCDDescriptionHeader *alloc_description(
    ULONG tag_count,
    ULONG extra_size)
{
    ULONG size = sizeof(struct VCDDescriptionHeader) +
        tag_count * sizeof(struct TagItem) + extra_size;
    struct VCDDescriptionHeader *header =
        AllocMem(size, MEMF_PUBLIC | MEMF_CLEAR);
    if (header) {
        header->magic = VCD_DESCRIPTION_MAGIC;
        header->size = size;
    }
    return header;
}

static struct TagItem *describe_disc(struct VideoCDDisc *disc)
{
    const ULONG tag_count = 9;
    ULONG title_size = align4((ULONG)strlen(disc->album_id) + 1);
    ULONG entries_size = (ULONG)disc->entry_count * sizeof(ULONG);
    struct VCDDescriptionHeader *header =
        alloc_description(tag_count, title_size + entries_size);
    struct TagItem *out;
    char *title;
    ULONG *entry_lsns;
    ULONG i;
    ULONG video_tracks;

    if (!header)
        return NULL;
    out = (struct TagItem *)(header + 1);
    title = (char *)(out + tag_count);
    entry_lsns = (ULONG *)(title + title_size);
    strcpy(title, disc->album_id);
    for (i = 0; i < disc->entry_count; i++)
        entry_lsns[i] = disc->entries[i].lsn;
    video_tracks = disc->last_track >= 2 ? disc->last_track - 1 : 0;

    out[0].ti_Tag = VCDTAG_ITEM_KIND;
    out[0].ti_Data = VCD_ITEM_DISC;
    out[1].ti_Tag = VCDTAG_TITLE;
    out[1].ti_Data = (ULONG)(uintptr_t)title;
    out[2].ti_Tag = VCDTAG_VOLUME_COUNT;
    out[2].ti_Data = disc->volume_count;
    out[3].ti_Tag = VCDTAG_VOLUME_NUMBER;
    out[3].ti_Data = disc->volume_number;
    out[4].ti_Tag = VCDTAG_TRACK_COUNT;
    out[4].ti_Data = video_tracks;
    out[5].ti_Tag = VCDTAG_ENTRY_KIND;
    out[5].ti_Data = VCD_ITEM_DISC;
    out[6].ti_Tag = VCDTAG_ENTRY_LSNS;
    out[6].ti_Data = (ULONG)(uintptr_t)entry_lsns;
    out[7].ti_Tag = VCDTAG_ENTRY_COUNT;
    out[7].ti_Data = disc->entry_count;
    out[8].ti_Tag = TAG_DONE;
    return out;
}

static struct TagItem *describe_track(
    struct VideoCDDisc *disc,
    ULONG item)
{
    const ULONG tag_count = 9;
    ULONG track = item + 1;
    ULONG start;
    ULONG end;
    ULONG first_entry = 0;
    ULONG entry_count = 0;
    ULONG i;
    struct VCDDescriptionHeader *header;
    struct TagItem *out;

    if (track < 2 || track > disc->last_track)
        return NULL;
    start = disc->track_lsn[track];
    end = track < disc->last_track ?
        disc->track_lsn[track + 1] : disc->lead_out_lsn;
    for (i = 0; i < disc->entry_count; i++) {
        if (disc->entries[i].track == track) {
            if (entry_count == 0)
                first_entry = i;
            entry_count++;
        }
    }

    header = alloc_description(tag_count, 0);
    if (!header)
        return NULL;
    out = (struct TagItem *)(header + 1);
    out[0].ti_Tag = VCDTAG_ITEM_KIND;
    out[0].ti_Data = VCD_ITEM_TRACK;
    out[1].ti_Tag = VCDTAG_TRACK_NUMBER;
    out[1].ti_Data = track;
    out[2].ti_Tag = VCDTAG_START_LSN;
    out[2].ti_Data = start;
    out[3].ti_Tag = VCDTAG_END_LSN;
    out[3].ti_Data = end;
    out[4].ti_Tag = VCDTAG_DURATION;
    out[4].ti_Data = end > start ? end - start : 0;
    out[5].ti_Tag = VCDTAG_FIRST_ENTRY;
    out[5].ti_Data = first_entry;
    out[6].ti_Tag = VCDTAG_TRACK_ENTRIES;
    out[6].ti_Data = entry_count;
    out[7].ti_Tag = VCDTAG_TITLE;
    out[7].ti_Data = 0;
    out[8].ti_Tag = TAG_DONE;
    return out;
}

static struct VideoCDBase *InitLibrary(
    struct ExecBase *sysbase __asm("a6"),
    BPTR seglist __asm("a0"),
    struct VideoCDBase *base __asm("d0"))
{
    (void)seglist;
    SysBase = sysbase;
    base->sys_base = sysbase;
    base->library.lib_Node.ln_Type = NT_LIBRARY;
    base->library.lib_Node.ln_Name = (char *)library_name;
    base->library.lib_Flags = LIBF_SUMUSED | LIBF_CHANGED;
    base->library.lib_Version = VIDEOCD_VERSION;
    base->library.lib_Revision = VIDEOCD_REVISION;
    base->library.lib_IdString = (APTR)library_id;
    InitSemaphore(&base->session_lock);
    return base;
}

static struct VideoCDBase *OpenLibraryVector(
    struct VideoCDBase *base __asm("a6"))
{
    base->library.lib_OpenCnt++;
    base->library.lib_Flags &= ~LIBF_DELEXP;
    return base;
}

static BPTR CloseLibraryVector(struct VideoCDBase *base __asm("a6"))
{
    if (base->library.lib_OpenCnt != 0)
        base->library.lib_OpenCnt--;
    return 0;
}

static BPTR ExpungeLibraryVector(struct VideoCDBase *base __asm("a6"))
{
    (void)base;
    return 0;
}

static ULONG ExtFuncLibraryVector(void)
{
    return 0;
}

static ULONG ReservedLibraryVector(void)
{
    return 0;
}

static ULONG ClassifyDiscVector(struct VideoCDBase *base __asm("a6"))
{
    struct VCDSession session;
    UBYTE *sectors;
    ULONG result = VIDEOCD_DISC_UNKNOWN;

    ObtainSemaphore(&base->session_lock);
    if (open_cd(&session)) {
        sectors = AllocMem(VCD_SECTOR_SIZE * 2, MEMF_PUBLIC);
        if (sectors && read_sector(&session, VCD_INFO_LSN, sectors) &&
            read_sector(&session, VCD_ENTRIES_LSN,
                sectors + VCD_SECTOR_SIZE) &&
            has_vcd_signature(sectors, sectors + VCD_SECTOR_SIZE))
            result = VIDEOCD_DISC_VCD;
        if (sectors)
            FreeMem(sectors, VCD_SECTOR_SIZE * 2);
        close_cd(&session);
    }
    ReleaseSemaphore(&base->session_lock);
    return result;
}

static struct VideoCDDisc *OpenDiscVector(
    APTR source __asm("a0"),
    const struct TagItem *tags __asm("a1"),
    struct VideoCDBase *base __asm("a6"))
{
    struct VCDSession session;
    struct VideoCDDisc *disc = NULL;
    UBYTE *sectors = NULL;

    (void)source;
    (void)tags;
    ObtainSemaphore(&base->session_lock);
    if (open_cd(&session)) {
        sectors = AllocMem(VCD_SECTOR_SIZE * 2, MEMF_PUBLIC);
        disc = AllocMem(sizeof(*disc), MEMF_PUBLIC | MEMF_CLEAR);
        if (!sectors || !disc ||
            !read_sector(&session, VCD_INFO_LSN, sectors) ||
            !read_sector(&session, VCD_ENTRIES_LSN,
                sectors + VCD_SECTOR_SIZE) ||
            !has_vcd_signature(sectors, sectors + VCD_SECTOR_SIZE) ||
            !read_toc(&session, disc) ||
            !parse_entries(disc, sectors + VCD_SECTOR_SIZE)) {
            free_disc(disc);
            disc = NULL;
        } else {
            disc->magic = VCD_DESCRIPTION_MAGIC;
            parse_info(disc, sectors);
        }
        if (sectors)
            FreeMem(sectors, VCD_SECTOR_SIZE * 2);
        close_cd(&session);
    }
    ReleaseSemaphore(&base->session_lock);
    return disc;
}

static void CloseDiscVector(
    struct VideoCDDisc *disc __asm("a0"),
    struct VideoCDBase *base __asm("a6"))
{
    (void)base;
    if (disc && disc->magic == VCD_DESCRIPTION_MAGIC)
        free_disc(disc);
}

static struct TagItem *DescribeItemVector(
    ULONG item __asm("d0"),
    struct VideoCDDisc *disc __asm("a0"),
    const struct TagItem *tags __asm("a1"),
    struct VideoCDBase *base __asm("a6"))
{
    (void)tags;
    (void)base;
    if (!disc || disc->magic != VCD_DESCRIPTION_MAGIC)
        return NULL;
    if (item == 0)
        return describe_disc(disc);
    return describe_track(disc, item);
}

static void FreeDescriptionVector(
    struct TagItem *description __asm("a0"),
    struct VideoCDBase *base __asm("a6"))
{
    struct VCDDescriptionHeader *header;

    (void)base;
    if (!description)
        return;
    header = (struct VCDDescriptionHeader *)description - 1;
    if (header->magic == VCD_DESCRIPTION_MAGIC &&
        header->size >= sizeof(*header) + sizeof(struct TagItem))
        FreeMem(header, header->size);
}
