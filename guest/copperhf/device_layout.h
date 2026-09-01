/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * copperhf.device: guest-only layout constant shared between device.c and
 * int_handler.s. NOT part of copperhf_board.h (the host/guest register
 * protocol contract) -- this is purely an internal detail of how this
 * ROM's own struct CopperhfDevice (device.c) is laid out in memory, which
 * the host side never needs to know.
 *
 * M4's int_handler.s now needs the device pointer (struct CopperhfDevice*),
 * not just the raw board base, because CHF_IRQ_STATUS bit 1 (changed-mask)
 * handling walks the per-device pending TD_ADDCHANGEINT list -- so
 * resident_init (device.c) hands AddIntServer's is_Data the device pointer
 * instead of the M1-M3 board pointer, and int_handler.s computes
 * board = dev + CHF_DEV_BOARDBASE_OFFSET itself on entry (a plain numeric
 * displacement, same idiom as guest/mhi/board.h's MHI_OFF_* constants).
 *
 * CHF_DEV_BOARDBASE_OFFSET is sizeof(struct Library) (exec/libraries.h),
 * since dev_BoardBase is struct CopperhfDevice's first field after the
 * embedded struct Library:
 *   struct Node lib_Node    14  (4 ln_Succ + 4 ln_Pred + 1 ln_Type +
 *                                 1 ln_Pri + 4 ln_Name)
 *   UBYTE  lib_Flags         1
 *   UBYTE  lib_pad           1
 *   UWORD  lib_NegSize       2
 *   UWORD  lib_PosSize       2
 *   UWORD  lib_Version       2
 *   UWORD  lib_Revision      2
 *   APTR   lib_IdString      4
 *   ULONG  lib_Sum           4
 *   UWORD  lib_OpenCnt       2
 *                           ----
 *                            34
 * struct Library is a fixed OS ABI layout, not something this project's own
 * code could accidentally drift -- but device.c's own _Static_assert
 * cross-checks this number against offsetof() at every build anyway, so a
 * toolchain/ABI surprise fails the build loudly instead of Guru-ing at
 * runtime. */
#ifndef COPPERHF_DEVICE_LAYOUT_H
#define COPPERHF_DEVICE_LAYOUT_H

#define CHF_DEV_BOARDBASE_OFFSET 34

#endif /* COPPERHF_DEVICE_LAYOUT_H */
