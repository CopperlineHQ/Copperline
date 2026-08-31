//
// Copyright 2022-2025 Chris Hooper & Stefan Reinauer
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

#ifndef _DEVICE_H
#define _DEVICE_H

/*
 * Error codes are returned in response to AmigaOS DoIO() requests.
 */
#define ERROR_OPEN_FAIL       -1  // IOERR_OPENFAIL
#define ERROR_UNKNOWN_COMMAND -3  // IOERR_NOCMD
#define ERROR_BAD_LENGTH      -4  // IOERR_BADLENGTH
#define ERROR_NO_MEMORY       31  // TDERR_NoMem
#define ERROR_BAD_UNIT        32  // TDERR_BadUnitNum
#define ERROR_BAD_DRIVE_TYPE  33  // TDERR_BadDriveType
#define ERROR_SELF_UNIT       40  // HFERR_SelfUnit
#define ERROR_INQUIRY_FAILED  46  // (HFERR_BadStatus + 1)
#define ERROR_TIMEOUT         47  // (HFERR_BadStatus + 2)
#define ERROR_BUS_RESET       48  // (HFERR_BadStatus + 3)
#define ERROR_TRY_AGAIN       49  // (HFERR_BadStatus + 4)
#define ERROR_NO_BOARD        50  // HFERR_NoBoard
#define ERROR_BAD_BOARD       51  // (HFERR_NoBoard + 1)
#define ERROR_SENSE_CODE      52  // (HFERR_NoBoard + 2)
#define ERROR_NOT_READY       53  // (HFERR_NoBoard + 3)

#define TDF_DEBUG_OPEN    (1<<7)  // Open unit in debug mode (no I/O)

#define HD_WIDESCSI       8       // Wide SCSI detection bit

/*
 * Unfortunately many of the above overlap with Unix-style error codes
 * (ENOMEM for example).
 *
 * Unix error macros known to be used by this driver:
 *      EACCES
 *      EBADF
 *      EBUSY
 *      EINTR
 *      EINVAL
 *      EIO
 *      EJUSTRETURN
 *      ENODEV
 *      ENOMEM
 *      ENOSPC
 *      ENXIO
 *      ERESTART
 * The above should be modified or captured so they use values compatible
 * with AmigaOS DoIO() error codes.
 *
 * The SCSIPI layer may also return error codes which are one of
 * the following:
 *      XS_NOERROR,             // 0 there is no error, (sense is invalid)
 *      XS_SENSE,               // 1 Check the returned sense for the error
 *      XS_SHORTSENSE,          // 2 Check the ATAPI sense for the error
 *      XS_DRIVER_STUFFUP,      // 3 Driver failed to perform operation
 *      XS_RESOURCE_SHORTAGE,   // 4 adapter resource shortage
 *      XS_SELTIMEOUT,          // 5 The device timed out.. turned off?
 *      XS_TIMEOUT,             // 6 The Timeout reported was caught by SW
 *      XS_BUSY,                // 7 The device busy, try again later?
 *      XS_RESET,               // 8 bus was reset; possible retry command
 *      XS_REQUEUE              // 9 requeue this command
 * The above should be modified to use values compatible with AmigaOS
 * DoIO() error codes.
 */

extern struct MsgPort *myPort;
extern char real_device_name[];

#endif /* _DEVICE_H */
