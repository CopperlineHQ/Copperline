/* SPDX-License-Identifier: CC0-1.0 */
#ifndef COPPERLINE_UAELIB_H
#define COPPERLINE_UAELIB_H

void KPrintF(const char *format, ...);
void warpmode(int enabled);
void debug_register_bitmap(const void *address, const char *name,
                           unsigned short width, unsigned short height,
                           unsigned short planes, unsigned short flags);
void debug_register_copperlist(const void *address, const char *name,
                               unsigned int size, unsigned short flags);
void debug_unregister(const void *address);

#endif
