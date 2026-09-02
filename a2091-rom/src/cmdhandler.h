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

#ifndef _CMD_HANDLER_H
#define _CMD_HANDLER_H

int open_unit(uint scsi_target, void **io_Unit, uint flags);
void close_unit(void *io_Unit);

int start_cmd_handler(uint *boardnum);
void stop_cmd_handler(void);
void cmd_complete(void *ior, int8_t rc);

void td_addchangeint(struct IORequest *ior);
void td_remchangeint(struct IORequest *ior);
void td_remove(struct IORequest *ior);

/* Trackdisk-64 enhanced commands */
/* Check before defining. AmigaOS 3.2 NDK provides these in
 * trackdisk.h
 */
#ifndef TD_READ64
#define TD_READ64    24      // Read at 64-bit offset
#endif
#ifndef TD_WRITE64
#define TD_WRITE64   25      // Write at 64-bit offset
#endif
#ifndef TD_SEEK64
#define TD_SEEK64    26      // Seek to 64-bit offset
#endif
#ifndef TD_FORMAT64
#define TD_FORMAT64  27      // Format (write) at 64-bit offset
#endif

/* Internal commands */
#define CMD_TERM     0x2ef0  // Terminate command handler (end process)
#define CMD_ATTACH   0x2ff1  // Attach (open) SCSI peripheral
#define CMD_DETACH   0x2ef2  // Detach (close) SCSI peripheral

#endif /* _CMD_HANDLER_H */
