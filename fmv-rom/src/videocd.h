/*
 * SPDX-FileCopyrightText: 2026 The Copperline project
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Clean-room public surface for the cartridge's resident Video CD parser.
 */
#ifndef COPPERLINE_VIDEOCD_H
#define COPPERLINE_VIDEOCD_H

#include <exec/libraries.h>
#include <exec/semaphores.h>
#include <exec/types.h>
#include <utility/tagitem.h>

#define VIDEOCD_NAME "videocd.library"
#define VIDEOCD_VERSION 41
#define VIDEOCD_REVISION 0

/* The original library returns 4 for a White Book Video CD. */
#define VIDEOCD_DISC_UNKNOWN 0
#define VIDEOCD_DISC_VCD 4

/*
 * Disc-description tags recovered from the original library's public output.
 * The descriptive names below document the values established by the INFO.VCD
 * and ENTRIES.VCD sectors. Consumers must treat unrecognised tags as optional.
 */
#define VCDTAG_TITLE          (TAG_USER | 0x100aUL) /* STRPTR */
#define VCDTAG_TRACK_COUNT    (TAG_USER | 0x100cUL) /* ULONG */
#define VCDTAG_ITEM_KIND      (TAG_USER | 0x1065UL) /* ULONG */
#define VCDTAG_VOLUME_COUNT   (TAG_USER | 0x1067UL) /* ULONG */
#define VCDTAG_VOLUME_NUMBER  (TAG_USER | 0x1068UL) /* ULONG */
#define VCDTAG_ENTRY_KIND     (TAG_USER | 0x106fUL) /* ULONG */
#define VCDTAG_ENTRY_COUNT    (TAG_USER | 0x1070UL) /* ULONG */
#define VCDTAG_ENTRY_LSNS     (TAG_USER | 0x1071UL) /* const ULONG * */

/* Additional track-description tags provided by this open implementation. */
#define VCDTAG_TRACK_NUMBER   (TAG_USER | 0x1100UL) /* ULONG */
#define VCDTAG_START_LSN      (TAG_USER | 0x1101UL) /* ULONG */
#define VCDTAG_END_LSN        (TAG_USER | 0x1102UL) /* ULONG, exclusive */
#define VCDTAG_DURATION       (TAG_USER | 0x1103UL) /* frames at 75 Hz */
#define VCDTAG_FIRST_ENTRY    (TAG_USER | 0x1104UL) /* ULONG */
#define VCDTAG_TRACK_ENTRIES  (TAG_USER | 0x1105UL) /* ULONG */

#define VCD_ITEM_DISC 0x101UL
#define VCD_ITEM_TRACK 0x102UL

struct VideoCDBase {
    struct Library library;
    struct ExecBase *sys_base;
    struct SignalSemaphore session_lock;
};

struct VideoCDDisc;

extern struct Resident VideoCDROMTag;

#endif
