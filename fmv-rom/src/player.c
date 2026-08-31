/*
 * SPDX-FileCopyrightText: 2026 The Copperline project
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Clean-room Video CD cold-start player. The resident participates in the
 * standard Exec resident list, replacing the CD32 extended ROM's lower-version
 * cdstrap. It only takes over when the inserted disc has White Book metadata;
 * all other media continue through the normal CD32 boot path.
 */

#define __USE_SYSBASE

#include <stdint.h>
#include <string.h>

#include <clib/alib_protos.h>
#include <devices/inputevent.h>
#include <exec/errors.h>
#include <exec/execbase.h>
#include <exec/io.h>
#include <exec/libraries.h>
#include <exec/memory.h>
#include <exec/resident.h>
#include <graphics/gfxbase.h>
#include <intuition/intuition.h>
#include <intuition/screens.h>
#include <libraries/lowlevel.h>
#include <proto/exec.h>
#include <proto/graphics.h>
#include <proto/intuition.h>
#include <proto/lowlevel.h>
#include <utility/tagitem.h>

#include "cd32mpeg.h"
#include "videocd.h"

extern struct ExecBase *SysBase;

struct IntuitionBase *IntuitionBase;
struct GfxBase *GfxBase;
struct Library *LowLevelBase;

#define PLAYER_MAX_ROWS 15

#define ACTION_UP   (1U << 0)
#define ACTION_DOWN (1U << 1)
#define ACTION_PLAY (1U << 2)
#define ACTION_BACK (1U << 3)

struct TrackInfo {
    ULONG number;
    ULONG start_lsn;
    ULONG end_lsn;
    ULONG duration;
};

struct PlayerState {
    struct Screen *screen;
    struct Window *window;
    struct MsgPort *mpeg_port;
    struct IOMPEGReq *mpeg;
    struct TrackInfo *tracks;
    ULONG track_count;
    ULONG selected;
    ULONG previous_joy;
    ULONG previous_key;
    char title[33];
};

static ULONG CDStrapInit(
    ULONG dummy __asm("d0"),
    BPTR seglist __asm("a0"),
    struct ExecBase *sysbase __asm("a6"));
static void set_video_visible(struct PlayerState *state, BOOL visible);
static void player_task(void);

static const char strap_name[] = "cdstrap";
static const char strap_id[] = "cdstrap 41.0 (31.08.2026)";
static const char ver_string[] __attribute__((used)) =
    "\0$VER: cdstrap 41.0 (31.08.2026)";
static APTR fallback_init;

struct Resident CDStrapROMTag __attribute__((used)) = {
    RTC_MATCHWORD,
    &CDStrapROMTag,
    (APTR)(&CDStrapROMTag + 1),
    RTF_COLDSTART,
    41,
    NT_UNKNOWN,
    -58,
    (char *)strap_name,
    (char *)strap_id,
    CDStrapInit,
};

void player_set_fallback(APTR init)
{
    fallback_init = init;
}

static ULONG run_fallback(struct ExecBase *sysbase)
{
    register ULONG d0 __asm("d0") = 0;
    register APTR a0 __asm("a0") = NULL;
    register APTR a1 __asm("a1") = fallback_init;
    register struct ExecBase *a6 __asm("a6") = sysbase;

    if (!fallback_init)
        return 0;
    __asm volatile("jsr (a1)"
        : "+r"(d0), "+r"(a0), "+r"(a1)
        : "r"(a6)
        : "d1", "cc", "memory");
    return d0;
}

static ULONG vcd_classify(struct VideoCDBase *base)
{
    register struct VideoCDBase *a6 __asm("a6") = base;
    register ULONG result __asm("d0");

    __asm volatile("jsr -36(a6)"
        : "=r"(result)
        : "r"(a6)
        : "d1", "a0", "a1", "cc", "memory");
    return result;
}

static struct VideoCDDisc *vcd_open(struct VideoCDBase *base)
{
    register struct VideoCDBase *a6 __asm("a6") = base;
    register APTR a0 __asm("a0") = NULL;
    register APTR a1 __asm("a1") = NULL;
    register struct VideoCDDisc *result __asm("d0");

    __asm volatile("jsr -42(a6)"
        : "=r"(result), "+r"(a0), "+r"(a1)
        : "r"(a6)
        : "d1", "cc", "memory");
    return result;
}

static void vcd_close(
    struct VideoCDBase *base,
    struct VideoCDDisc *disc)
{
    register struct VideoCDBase *a6 __asm("a6") = base;
    register struct VideoCDDisc *a0 __asm("a0") = disc;

    __asm volatile("jsr -48(a6)"
        : "+r"(a0)
        : "r"(a6)
        : "d0", "d1", "a1", "cc", "memory");
}

static struct TagItem *vcd_describe(
    struct VideoCDBase *base,
    struct VideoCDDisc *disc,
    ULONG item)
{
    register struct VideoCDBase *a6 __asm("a6") = base;
    register struct VideoCDDisc *a0 __asm("a0") = disc;
    register APTR a1 __asm("a1") = NULL;
    register ULONG result __asm("d0") = item;

    __asm volatile("jsr -54(a6)"
        : "+r"(result), "+r"(a0), "+r"(a1)
        : "r"(a6)
        : "d1", "cc", "memory");
    return (struct TagItem *)(uintptr_t)result;
}

static void vcd_free_description(
    struct VideoCDBase *base,
    struct TagItem *description)
{
    register struct VideoCDBase *a6 __asm("a6") = base;
    register struct TagItem *a0 __asm("a0") = description;

    __asm volatile("jsr -60(a6)"
        : "+r"(a0)
        : "r"(a6)
        : "d0", "d1", "a1", "cc", "memory");
}

static ULONG tag_data(
    const struct TagItem *tags,
    ULONG wanted,
    ULONG fallback)
{
    while (tags && tags->ti_Tag != TAG_DONE) {
        if (tags->ti_Tag == wanted)
            return tags->ti_Data;
        tags++;
    }
    return fallback;
}

static void copy_title(char *destination, const char *source, ULONG size)
{
    ULONG i = 0;

    if (source) {
        while (i + 1 < size && source[i] != '\0') {
            destination[i] = source[i];
            i++;
        }
    }
    destination[i] = '\0';
}

static BOOL load_tracks(
    struct PlayerState *state,
    struct VideoCDBase *base,
    struct VideoCDDisc *disc)
{
    struct TagItem *description;
    ULONG i;

    description = vcd_describe(base, disc, 0);
    if (!description)
        return FALSE;
    state->track_count = tag_data(description, VCDTAG_TRACK_COUNT, 0);
    copy_title(state->title,
        (const char *)(uintptr_t)tag_data(description, VCDTAG_TITLE, 0),
        sizeof(state->title));
    vcd_free_description(base, description);
    if (state->track_count == 0 || state->track_count > 98)
        return FALSE;

    state->tracks = AllocMem(
        state->track_count * sizeof(*state->tracks),
        MEMF_PUBLIC | MEMF_CLEAR);
    if (!state->tracks)
        return FALSE;

    for (i = 0; i < state->track_count; i++) {
        description = vcd_describe(base, disc, i + 1);
        if (!description)
            return FALSE;
        state->tracks[i].number = tag_data(
            description, VCDTAG_TRACK_NUMBER, i + 2);
        state->tracks[i].start_lsn = tag_data(
            description, VCDTAG_START_LSN, 0);
        state->tracks[i].end_lsn = tag_data(
            description, VCDTAG_END_LSN, 0);
        state->tracks[i].duration = tag_data(
            description, VCDTAG_DURATION, 0);
        vcd_free_description(base, description);
        if (state->tracks[i].end_lsn <= state->tracks[i].start_lsn)
            return FALSE;
    }
    return TRUE;
}

static ULONG divide_ulong(ULONG value, ULONG divisor, ULONG *remainder)
{
    ULONG quotient = 0;
    ULONG rest = 0;
    WORD bit;

    for (bit = 31; bit >= 0; bit--) {
        rest = (rest << 1) | ((value >> bit) & 1);
        if (rest >= divisor) {
            rest -= divisor;
            quotient |= 1UL << bit;
        }
    }
    *remainder = rest;
    return quotient;
}

static char *append_ulong(char *out, ULONG value)
{
    char reverse[11];
    ULONG count = 0;
    ULONG remainder;

    do {
        value = divide_ulong(value, 10, &remainder);
        reverse[count++] = (char)('0' + remainder);
    } while (value != 0);
    while (count != 0)
        *out++ = reverse[--count];
    return out;
}

static void track_label(char *out, const struct TrackInfo *track)
{
    ULONG discarded_frames;
    ULONG seconds;
    ULONG total_seconds = divide_ulong(
        track->duration, 75, &discarded_frames);
    ULONG minutes = divide_ulong(total_seconds, 60, &seconds);

    memcpy(out, "Track ", 6);
    out += 6;
    out = append_ulong(out, track->number);
    memcpy(out, "                 ", 17);
    out += 17;
    out = append_ulong(out, minutes);
    *out++ = ':';
    if (seconds < 10)
        *out++ = '0';
    out = append_ulong(out, seconds);
    *out = '\0';
}

static void draw_text(
    struct RastPort *rast_port,
    WORD x,
    WORD y,
    UBYTE pen,
    const char *text)
{
    SetAPen(rast_port, pen);
    Move(rast_port, x, y);
    Text(rast_port, (CONST_STRPTR)text, (ULONG)strlen(text));
}

static void draw_player(struct PlayerState *state)
{
    struct RastPort *rast_port = &state->screen->RastPort;
    ULONG first;
    ULONG rows;
    ULONG i;
    WORD y;
    char label[48];

    SetRast(rast_port, 0);
    SetAPen(rast_port, 2);
    RectFill(rast_port, 0, 0, state->screen->Width - 1, 29);
    draw_text(rast_port, 12, 19, 1, "COPPERLINE VIDEO CD");
    draw_text(rast_port, 12, 43, 3,
        state->title[0] != '\0' ? state->title : "Video CD");
    rows = state->track_count < PLAYER_MAX_ROWS ?
        state->track_count : PLAYER_MAX_ROWS;
    first = state->selected >= rows ? state->selected - rows + 1 : 0;
    if (first + rows > state->track_count)
        first = state->track_count - rows;
    y = 61;
    for (i = 0; i < rows; i++, y += 11) {
        ULONG index = first + i;
        track_label(label, &state->tracks[index]);
        if (index == state->selected) {
            SetAPen(rast_port, 4);
            RectFill(rast_port, 8, y - 8, state->screen->Width - 9, y + 2);
            draw_text(rast_port, 13, y, 1, label);
        } else {
            draw_text(rast_port, 13, y, 3, label);
        }
    }
    draw_text(rast_port, 12, state->screen->Height - 18, 5,
        "UP/DOWN  RED PLAY  BLUE BACK");
}

static ULONG poll_actions(struct PlayerState *state)
{
    struct IntuiMessage *message;
    ULONG actions = 0;
    ULONG joy = ReadJoyPort(1);
    ULONG key = GetKey();
    ULONG pressed = 0;
    ULONG edge;

    if ((joy & JP_TYPE_MASK) != JP_TYPE_NOTAVAIL)
        pressed = joy & (JP_DIRECTION_MASK | JP_BUTTON_MASK);
    edge = pressed & ~state->previous_joy;
    state->previous_joy = pressed;
    if (edge & JPF_JOY_UP)
        actions |= ACTION_UP;
    if (edge & JPF_JOY_DOWN)
        actions |= ACTION_DOWN;
    if (edge & JPF_BUTTON_RED)
        actions |= ACTION_PLAY;
    if (edge & JPF_BUTTON_BLUE)
        actions |= ACTION_BACK;

    if (key != state->previous_key) {
        switch (key & 0xff) {
        case 0x4c:
            actions |= ACTION_UP;
            break;
        case 0x4d:
            actions |= ACTION_DOWN;
            break;
        case 0x44:
            actions |= ACTION_PLAY;
            break;
        case 0x45:
            actions |= ACTION_BACK;
            break;
        default:
            break;
        }
        state->previous_key = key;
    }

    while (state->window && (message = (struct IntuiMessage *)GetMsg(
            state->window->UserPort)) != NULL) {
        if (message->Class == IDCMP_RAWKEY &&
            (message->Code & IECODE_UP_PREFIX) == 0) {
            switch (message->Code) {
            case 0x4c:
                actions |= ACTION_UP;
                break;
            case 0x4d:
                actions |= ACTION_DOWN;
                break;
            case 0x44:
                actions |= ACTION_PLAY;
                break;
            case 0x45:
                actions |= ACTION_BACK;
                break;
            default:
                break;
            }
        }
        ReplyMsg((struct Message *)message);
    }
    return actions;
}

static BOOL open_player_screen(struct PlayerState *state)
{
    struct TagItem screen_tags[] = {
        { SA_Width, 320 },
        { SA_Height, 256 },
        { SA_Depth, 3 },
        { SA_Type, CUSTOMSCREEN },
        { SA_ShowTitle, FALSE },
        { SA_Quiet, TRUE },
        { TAG_DONE, 0 },
    };
    state->screen = OpenScreenTagList(NULL, screen_tags);
    if (!state->screen)
        return FALSE;

    SetRGB4(&state->screen->ViewPort, 0, 0, 0, 1);
    SetRGB4(&state->screen->ViewPort, 1, 15, 15, 15);
    SetRGB4(&state->screen->ViewPort, 2, 1, 3, 9);
    SetRGB4(&state->screen->ViewPort, 3, 7, 11, 15);
    SetRGB4(&state->screen->ViewPort, 4, 2, 7, 13);
    SetRGB4(&state->screen->ViewPort, 5, 9, 12, 15);
    return TRUE;
}

static BOOL open_mpeg(struct PlayerState *state)
{
    state->mpeg_port = CreateMsgPort();
    if (state->mpeg_port)
        state->mpeg = (struct IOMPEGReq *)CreateIORequest(
            state->mpeg_port, sizeof(*state->mpeg));
    if (!state->mpeg || OpenDevice((CONST_STRPTR)CD32MPEG_NAME, 0,
            (struct IORequest *)state->mpeg, 0) != 0)
        return FALSE;
    set_video_visible(state, FALSE);
    return TRUE;
}

static void set_video_visible(struct PlayerState *state, BOOL visible)
{
    struct MPEGVideoParamsSet parameters;

    parameters.mvp_Fade = visible ? 0xffff : 0;
    parameters.mvp_DisplayType = 0;
    state->mpeg->iomr_Req.io_Command = MPEGCMD_SETVIDEOPARAMS;
    state->mpeg->iomr_Req.io_Data = &parameters;
    state->mpeg->iomr_Req.io_Length = sizeof(parameters);
    DoIO((struct IORequest *)state->mpeg);
}

static void play_selected(struct PlayerState *state)
{
    ULONG start_lsn;
    ULONG end_lsn;

    /* Amiga library calls use A6 for their library base.  Do not retain a
     * pointer-valued local across DoIO(): older m68k GCC inline prototypes
     * do not consistently describe that register use to the optimiser. */
    SetRast(&state->screen->RastPort, 0);
    set_video_visible(state, TRUE);
    start_lsn = state->tracks[state->selected].start_lsn;
    end_lsn = state->tracks[state->selected].end_lsn;
    state->mpeg->iomr_Req.io_Command = MPEGCMD_PLAYLSN;
    state->mpeg->iomr_Req.io_Data = NULL;
    state->mpeg->iomr_Req.io_Offset = start_lsn;
    state->mpeg->iomr_Req.io_Length = end_lsn - start_lsn;
    state->mpeg->iomr_StreamType = 3;
    state->mpeg->iomr_Arg1 = 2328;
    state->mpeg->iomr_Arg2 = start_lsn;
    SendIO((struct IORequest *)state->mpeg);
    while (!CheckIO((struct IORequest *)state->mpeg)) {
        if (poll_actions(state) & ACTION_BACK) {
            AbortIO((struct IORequest *)state->mpeg);
            break;
        }
        WaitTOF();
    }
    WaitIO((struct IORequest *)state->mpeg);
    set_video_visible(state, FALSE);
    draw_player(state);
}

static void close_player(struct PlayerState *state)
{
    if (state->mpeg && state->mpeg->iomr_Req.io_Device)
        CloseDevice((struct IORequest *)state->mpeg);
    if (state->mpeg)
        DeleteIORequest((struct IORequest *)state->mpeg);
    if (state->mpeg_port)
        DeleteMsgPort(state->mpeg_port);
    if (state->window)
        CloseWindow(state->window);
    if (state->screen)
        CloseScreen(state->screen);
    if (state->tracks)
        FreeMem(state->tracks,
            state->track_count * sizeof(*state->tracks));
}

static void run_player(
    struct VideoCDBase *base,
    struct VideoCDDisc *disc)
{
    struct PlayerState state;
    BOOL running = TRUE;

    memset(&state, 0, sizeof(state));
    GfxBase = (struct GfxBase *)OpenLibrary(
        (CONST_STRPTR)"graphics.library", 39);
    IntuitionBase = (struct IntuitionBase *)OpenLibrary(
        (CONST_STRPTR)"intuition.library", 39);
    LowLevelBase = OpenLibrary((CONST_STRPTR)"lowlevel.library", 40);
    if (!GfxBase || !IntuitionBase || !LowLevelBase ||
        !load_tracks(&state, base, disc) ||
        !open_player_screen(&state) || !open_mpeg(&state))
        running = FALSE;

    if (running)
        draw_player(&state);
    while (running) {
        ULONG actions = poll_actions(&state);
        if ((actions & ACTION_UP) && state.selected != 0) {
            state.selected--;
            draw_player(&state);
        }
        if ((actions & ACTION_DOWN) &&
            state.selected + 1 < state.track_count) {
            state.selected++;
            draw_player(&state);
        }
        if (actions & ACTION_PLAY)
            play_selected(&state);
        if (actions & ACTION_BACK)
            running = FALSE;
        WaitTOF();
    }

    close_player(&state);
    if (LowLevelBase)
        CloseLibrary(LowLevelBase);
    if (IntuitionBase)
        CloseLibrary((struct Library *)IntuitionBase);
    if (GfxBase)
        CloseLibrary((struct Library *)GfxBase);
    LowLevelBase = NULL;
    IntuitionBase = NULL;
    GfxBase = NULL;
}

static ULONG CDStrapInit(
    ULONG dummy __asm("d0"),
    BPTR seglist __asm("a0"),
    struct ExecBase *sysbase __asm("a6"))
{
    struct VideoCDBase *base;
    BOOL is_vcd;

    (void)dummy;
    (void)seglist;
    SysBase = sysbase;
    base = (struct VideoCDBase *)OpenLibrary(
        (CONST_STRPTR)VIDEOCD_NAME, VIDEOCD_VERSION);
    if (!base)
        return run_fallback(sysbase);
    is_vcd = vcd_classify(base) == VIDEOCD_DISC_VCD;
    CloseLibrary((struct Library *)base);
    /* Keep controller handling ahead of the decoder worker (priority 10).
     * The worker intentionally runs hard while a CD_READ is ready; a lower
     * priority player would not get a frame boundary in which to AbortIO. */
    if (is_vcd && CreateTask((CONST_STRPTR)"Video CD player", 15,
            (APTR)player_task, 16384) != NULL)
        return 0;
    return run_fallback(sysbase);
}

static void player_task(void)
{
    struct VideoCDBase *base;
    struct VideoCDDisc *disc;

    base = (struct VideoCDBase *)OpenLibrary(
        (CONST_STRPTR)VIDEOCD_NAME, VIDEOCD_VERSION);
    if (base) {
        disc = vcd_open(base);
        if (disc) {
            run_player(base, disc);
            vcd_close(base, disc);
        }
        CloseLibrary((struct Library *)base);
    }
    RemTask(NULL);
    for (;;)
        ;
}
