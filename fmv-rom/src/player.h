/* SPDX-License-Identifier: GPL-3.0-or-later */
#ifndef COPPERLINE_FMV_PLAYER_H
#define COPPERLINE_FMV_PLAYER_H

#include <exec/resident.h>

#define RESLIST_NEXT 0x80000000UL

extern struct Resident CDStrapROMTag;
void player_set_fallback(APTR init);

#endif
