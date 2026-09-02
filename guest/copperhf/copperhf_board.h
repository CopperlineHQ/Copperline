// SPDX-License-Identifier: GPL-3.0-or-later
//
// copperhf.device register map: shared source of truth between the future
// 68k device stub (guest/copperhf/, landing in M2) and the Rust host-side
// board (src/copperhf.rs). Keep the two in sync -- src/copperhf.rs mirrors
// every constant below and points back at this file.
//
// 64K Zorro II window layout:
//   0x0000-0x3FFF  reserved for the boot ROM / DiagArea (M2)
//   0x4000-0x40FF  register block (this file)
//
// The board is a 16-bit-bus device: the emulator's 68k bus may present a
// register access as one 32-bit long or as two 16-bit words (high word at
// the register's base offset, low word 2 bytes further on), and the
// register semantics below are defined for both. Byte-wide accesses are not
// supported by the hardware model; they read as all-ones and drop writes,
// like an unmapped offset.
//
// Doorbell/completion protocol (M1: fully synchronous -- the emulator
// executes a request and enqueues its completion before CHF_DOORBELL's
// write returns; async I/O is deferred to M5, and this protocol does not
// change under it):
//
//   1. Guest builds a struct IOStdReq-compatible request block anywhere in
//      Amiga memory and writes its address to CHF_DOORBELL. Convention:
//      io_Unit (the standard offset +24, normally a struct Unit pointer)
//      holds the raw copperhf unit NUMBER (0..CHF_UNITS-1) instead -- this
//      device has no per-unit Unit structures on the guest side.
//   2. The emulator executes the request synchronously and appends the
//      request pointer to the completion queue, then (if CHF_IRQ_ENABLE is
//      set) asserts INT2.
//   3. The guest's INT2 server reads CHF_COMPLETE_GET (repeatedly, since
//      more than one request may complete between interrupts) until it
//      reads back 0, ReplyMsg-ing each request in turn and writing
//      CHF_COMPLETE_ACK after each one to pop it from the queue.
//
// CHF_COMPLETE_GET is idempotent: reading it does not by itself remove the
// entry, so a lost interrupt cannot lose a completion, and CHF_COMPLETE_ACK
// is what actually pops the queue. A 32-bit read of CHF_COMPLETE_GET (or a
// 16-bit high-word read followed by a low-word read) always reflects one
// consistent snapshot of the queue head, even if the queue changes between
// the two word halves of a split access.
//
// IOStdReq field offsets this device reads/writes (standard exec.library
// layout; only the fields copperhf touches are listed):
//   +24 io_Unit    u32  raw unit number (see above), not a pointer
//   +28 io_Command u16
//   +30 io_Flags   u8
//   +31 io_Error   i8
//   +32 io_Actual  u32
//   +36 io_Length  u32
//   +40 io_Data    u32  guest APTR
//   +44 io_Offset  u32  byte offset into the unit, M1: 32-bit (TD64/NSD
//                       beyond 4 GiB is M4)
//
// Commands implemented in M1 (values match exec.library's io.h /
// trackdisk.h):
//   CMD_READ (2), CMD_WRITE (3), CMD_UPDATE (4), CMD_CLEAR (5),
//   TD_FORMAT (11) [treated as CMD_WRITE], TD_MOTOR (9),
//   TD_GETGEOMETRY (22)
// Any other command -> io_Error = IOERR_NOCMD.
//
// Error codes (io_Error, matching exec.library's io.h negative range):
//   IOERR_OPENFAIL    -1  targeted unit is absent (empty slot or >= CHF_UNITS)
//   IOERR_NOCMD       -3  unrecognised io_Command
//   IOERR_BADLENGTH   -4  io_Length/io_Offset not a 512-byte multiple, or the
//                         requested range runs past the end of the unit
//   IOERR_BADADDRESS  -5  the request header or its io_Data buffer could not
//                         be reached over DMA (bad guest pointer)
//
// CMD_READ/CMD_WRITE/TD_FORMAT require io_Length and io_Offset to both be
// multiples of 512 (this device has no sub-sector access); TD_GETGEOMETRY
// requires io_Length >= 32 (sizeof struct DriveGeometry) and always reports
// io_Actual = 0, matching trackdisk.device convention.
//
// struct DriveGeometry, written at io_Data for TD_GETGEOMETRY (32 bytes,
// big-endian, RDB_HEADS=16 / RDB_SPT=32 geometry -- see src/harddrive.rs):
//   +0  dg_SectorSize   u32  512
//   +4  dg_TotalSectors u32  unit's total 512-byte block count
//   +8  dg_Cylinders    u32  dg_TotalSectors / (16 * 32)
//   +12 dg_CylSectors   u32  512
//   +16 dg_Heads        u32  16
//   +20 dg_TrackSectors u32  32
//   +24 dg_BufMemType   u32  1 (MEMF_PUBLIC)
//   +28 dg_DeviceType   u8   0 (DG_DIRECT_ACCESS)
//   +29 dg_Flags        u8   0 (not removable)
//   +30 dg_Reserved     u16  0

#ifndef COPPERHF_BOARD_H
#define COPPERHF_BOARD_H

#define CHF_MANUFACTURER 0x1448 // dec0de Consulting (Copperline)
#define CHF_PRODUCT      0x08   // copperhf.device board

// Register offsets, window-relative.
#define CHF_MAGIC        0x4000 // u32 ro: 0x43504846 ("CPHF")
#define CHF_VERSION      0x4004 // u16 ro: register-protocol version (1)
#define CHF_UNITS        0x4006 // u16 ro: number of unit slots (7)
#define CHF_UNIT_PRESENT 0x4008 // u16 ro: bit n set = unit n attached
#define CHF_UNIT_RDONLY  0x400A // u16 ro: bit n set = unit n read-only
#define CHF_UNIT_SELECT  0x400C // u16 rw: selects the unit CHF_CHANGE_COUNT
                                //         and CHF_UNIT_BLOCKS report on;
                                //         values >= CHF_UNITS read back as
                                //         written but the queries read 0
#define CHF_CHANGE_COUNT 0x400E // u16 ro: disk-change counter of the
                                //         selected unit
#define CHF_UNIT_BLOCKS  0x4010 // u32 ro: total 512-byte blocks of the
                                //         selected unit
#define CHF_CHANGED_MASK 0x4014 // u16 ro (M4): bit n set = unit n's media
                                //         changed (eject, hot attach/detach)
                                //         and the guest has not acked it yet
#define CHF_CHANGED_ACK  0x4016 // u16 wo (M4): write a mask; clears those
                                //         CHF_CHANGED_MASK bits
#define CHF_UNIT_MEDIA   0x4018 // u16 ro (M4): bit n set = unit n currently
                                //         has media. CHF_UNIT_PRESENT keeps
                                //         meaning "slot configured" -- an
                                //         ejected/hot-detached unit stays
                                //         present (opens still succeed, like
                                //         a diskless trackdisk drive) with
                                //         its media bit clear. Before M4 the
                                //         two masks were always identical.
#define CHF_DOORBELL     0x4020 // u32 wo: guest pointer to an IOStdReq;
                                //         enqueues and (M1) executes it.
                                //         Written as two words, the high
                                //         word (this offset) only latches;
                                //         the low word (CHF_DOORBELL + 2)
                                //         commits using the latched high
                                //         half. A single 32-bit write
                                //         commits immediately.
#define CHF_COMPLETE_GET 0x4028 // u32 ro: guest pointer of the oldest
                                //         completed request, 0 if the
                                //         completion queue is empty.
                                //         Idempotent -- does not pop.
#define CHF_COMPLETE_ACK 0x402C // u16 wo: any write pops the oldest
                                //         completed request
#define CHF_IRQ_STATUS   0x4030 // u16 ro: bit 0 = completion queue non-empty
                                //         bit 1 = CHF_CHANGED_MASK non-zero
                                //         (M4)
#define CHF_IRQ_ENABLE   0x4032 // u16 rw: bit 0 = enable INT2 while
                                //         CHF_IRQ_STATUS is non-zero (any
                                //         bit; power-on/reset value: 0)

#define CHF_MAGIC_VALUE 0x43504846u // "CPHF"
// Version 2 = M4: CHF_CHANGED_MASK/ACK, CHF_UNIT_MEDIA, IRQ_STATUS bit 1,
// TD64/NSD/HD_SCSICMD command coverage. The guest ROM ships in lockstep
// with the host board, so nothing branches on this at runtime -- it exists
// so a register dump identifies the protocol vintage.
#define CHF_PROTOCOL_VERSION 2
#define CHF_NUM_UNITS 7

// IOStdReq field offsets copperhf reads/writes (see the protocol comment
// above).
#define CHF_IO_UNIT    24
#define CHF_IO_COMMAND 28
#define CHF_IO_FLAGS   30
#define CHF_IO_ERROR   31
#define CHF_IO_ACTUAL  32
#define CHF_IO_LENGTH  36
#define CHF_IO_DATA    40
#define CHF_IO_OFFSET  44

// Commands (exec.library / trackdisk.device numbering).
#define CHF_CMD_READ         2
#define CHF_CMD_WRITE        3
#define CHF_CMD_UPDATE       4
#define CHF_CMD_CLEAR        5
#define CHF_CMD_TD_MOTOR     9
#define CHF_CMD_TD_FORMAT    11
#define CHF_CMD_TD_GETGEOMETRY 22

// M4 command coverage. Host-side (doorbell) unless noted guest-side; the
// guest-side ones never reach the doorbell at all -- device.c's BeginIO
// answers them from the stub/ROM directly, because they need guest
// pointers (the NSD supported-command table) or guest state (the pending
// change-interrupt list) the host cannot provide.
#define CHF_CMD_TD_CHANGENUM    13 // io_Actual = unit's change counter
#define CHF_CMD_TD_CHANGESTATE  14 // io_Actual = 0 media present, 1 absent
#define CHF_CMD_TD_PROTSTATUS   15 // io_Actual = 0 writable, 1 read-only
#define CHF_CMD_TD_ADDCHANGEINT 20 // guest-side: queue io_Data Interrupt
#define CHF_CMD_TD_REMCHANGEINT 21 // guest-side: unqueue + reply the add
#define CHF_CMD_TD_EJECT        23 // io_Length != 0 ejects the media
                                   // (bumps the change counter, sets the
                                   // CHF_CHANGED_MASK bit); io_Length == 0
                                   // ("insert") is a successful no-op --
                                   // the host has nothing to load
// TD64: io_Actual carries the UPPER 32 bits of the 64-bit byte offset on
// entry, io_Offset the lower 32 (trackdisk64.doc); same 512-byte-multiple
// rules as CMD_READ/CMD_WRITE. The NSCMD_* variants are identical in
// layout, only the command numbers differ (NSD's newstyle.h).
#define CHF_CMD_TD_READ64       24
#define CHF_CMD_TD_WRITE64      25
#define CHF_CMD_TD_SEEK64       26 // no-op success (nothing to seek)
#define CHF_CMD_TD_FORMAT64     27 // treated as TD_WRITE64
#define CHF_CMD_HD_SCSICMD      28 // io_Data -> struct SCSICmd; see
                                   // src/copperhf.rs for the CDB coverage
#define CHF_NSCMD_DEVICEQUERY   0x4000 // guest-side: fills the
                                       // NSDeviceQueryResult at io_Data
                                       // from a ROM-resident command table
#define CHF_NSCMD_TD_READ64     0xC000
#define CHF_NSCMD_TD_WRITE64    0xC001
#define CHF_NSCMD_TD_SEEK64     0xC002
#define CHF_NSCMD_TD_FORMAT64   0xC003

// Plain 32-bit CMD_READ/CMD_WRITE/TD_FORMAT never wrap past 4 GiB:
// io_Offset + io_Length overflowing 32 bits fails with IOERR_BADADDRESS
// (COPPERHF-DEVICE-PLAN.md M4). The 64-bit commands are the only way to
// address beyond 4 GiB.

// io_Error values.
#define CHF_IOERR_OPENFAIL   (-1)
#define CHF_IOERR_NOCMD      (-3)
#define CHF_IOERR_BADLENGTH  (-4)
#define CHF_IOERR_BADADDRESS (-5)

#define CHF_SECTOR_SIZE 512

#endif // COPPERHF_BOARD_H
