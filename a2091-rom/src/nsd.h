//
// Copyright 2022-2023 Chris Hooper
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice,
//    this list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
//

#ifndef _NSD_H
#define _NSD_H

#define NSCMD_DEVICEQUERY   0x4000

#define NSCMD_TD_READ64     0xC000
#define NSCMD_TD_WRITE64    0xC001
#define NSCMD_TD_SEEK64     0xC002
#define NSCMD_TD_FORMAT64   0xC003

#define NSCMD_ETD_READ64    0xE000
#define NSCMD_ETD_WRITE64   0xE001
#define NSCMD_ETD_SEEK64    0xE002
#define NSCMD_ETD_FORMAT64  0xE003

#define NSDEVTYPE_TRACKDISK 5   // Trackdisk-like block storage device

#define DRIVE_NEWSTYLE          0x4E535459L /* NSTY */

#define NSCMD_TDF_EXTCOM (1<<13)  // Mask for extended NSD commands

struct NSDeviceQueryResult
{
    /*
    ** Standard information
    */
    ULONG   DevQueryFormat;         /* this is type 0               */
    ULONG   SizeAvailable;          /* bytes available              */

    /*
    ** Common information (READ ONLY!)
    */
    UWORD   DeviceType;             /* what the device does         */
    UWORD   DeviceSubType;          /* depends on the main type     */
    UWORD   *SupportedCommands;     /* 0 terminated list of cmd's   */

    /* May be extended in the future! Check SizeAvailable! */
};

#endif /* _NSD_H */
