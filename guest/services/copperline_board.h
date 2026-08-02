// SPDX-License-Identifier: GPL-3.0-or-later
//
// Board-window layout and trap opcodes shared between the guest handler and
// the emulator. Keep in sync with the constants in src/filesys.rs; the Rust
// unit tests lock the layout.
//
// 64K window layout:
//   0x0000  u32: fake seglist length (longwords), for tools that look at it
//   0x0004  u32: 0 (seglist next pointer); dn_SegList = MKBADDR(board + 4)
//   0x0008  handler code (services_rom.bin). Entry table:
//             +0     process entry (DOS RunHandler starts the handler here)
//             +4     expansion-init entry (jsr-ed by the DiagArea stub
//                    with the DiagPoint registers; mounts the volumes)
//             +0x40  struct DiagArea (er_InitDiagVec points here; the
//                    DiagPoint stub reaches the ROM via jsr 12(a0))
//   0x3800  mount table, written by the emulator:
//             u16 count, then count fixed-size entries of the DOS device name
//             as a NUL-terminated string ("HOSTFS0", ...)
//   0x7000  per-unit volume DosList nodes, built by the emulator at startup
//           and AddDosEntry'd by the handler (RES_ADDVOLUME)
//   0x7C00  per-unit host registers (see below)
//   0x7E00  DIAG_DOORBELL
//   0x8000  emulator-managed guest object pool (FileLocks etc.); the handler
//           never touches it

#ifndef COPPERLINE_BOARD_H
#define COPPERLINE_BOARD_H

#define BOARD_MANUFACTURER 0x1448 // dec0de Consulting
#define BOARD_PRODUCT      0x05   // Copperline services board

#define ROM_OFFSET         0x0008
#define MOUNTS_OFFSET      0x3800
#define MOUNT_ENTRY_SIZE   32
#define VOLUMES_OFFSET     0x7000
#define VOLUME_SLOT_SIZE   128
// Per-unit FileSysStartupMsg, written by the emulator at expansion init;
// dn_Startup points here so the Early Startup boot menu can display the
// device name, unit, and dostype instead of dereferencing garbage. Each
// FSSM references a per-unit DosEnvec whose de_BootPri carries the
// configured AddBootNode priority.
#define FSSM_OFFSET         0x7800
#define FSSM_SLOT_SIZE      16
#define FSSM_DEVNAME_OFFSET 0x7900

// Host registers (see the ZorroDevice impl in src/filesys.rs). One bank of
// longword registers per mount unit, so each handler process talks to its
// own bank and no locking is needed between them. Registers with a write
// side effect sit alone on a 16-byte boundary, so nothing else is disturbed
// even if a future CPU model bursts whole cache lines at the window.
#define REGS_OFFSET        0x7C00
#define REG_BANK_SIZE      0x40
// Write: struct DosPacket APTR. The doorbell: the host handles the packet
// synchronously within the write, filling dp_Res1/dp_Res2 and latching
// RESULT/ARG before the next instruction runs.
#define REG_DOSPKT         0x00
// Write: the handler process MsgPort APTR, once at startup. Cleared to 0
// when the process exits, so a nonzero MSGPORT means the unit is live.
#define REG_MSGPORT        0x10
// Read: what the handler must do with the packet just rung in (RES_* below).
#define REG_RESULT         0x20
// Read: the volume DosList node APTR for RES_ADDVOLUME / RES_DIE.
#define REG_ARG            0x30
// A per-unit EVENT register is planned for when runtime volume eject/load
// lands: the board will raise INT2, a small INTB_PORTS server will Signal()
// the unit's handler process, and the process will read the event from its
// bank (a sleeping handler cannot poll, so it must be an interrupt).
// Write: expansion-init strobe, value = the board base (DiagPoint's A0).
// Global, not per-unit: it runs before any handler process exists.
#define DIAG_DOORBELL      0x7E00

// REG_RESULT values.
#define RES_REPLY     0 // packet complete: reply it to the sender
#define RES_NOREPLY   1 // host keeps the packet (reserved, not yet used)
#define RES_ADDVOLUME 2 // reply, then AddDosEntry the volume DosList
                        // node the host built (in REG_ARG): only
                        // guest code may take the DosList semaphore
#define RES_DIE       3 // ACTION_DIE accepted: reply, RemDosEntry the
                        // volume node (in REG_ARG), and exit the process
                        // (dn_Task is already cleared, so the next
                        // reference restarts the handler)

#endif // COPPERLINE_BOARD_H
