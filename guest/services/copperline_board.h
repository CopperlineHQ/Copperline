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
//           and AddDosEntry'd by the handler (TRAP_RES_ADDVOLUME)
//   0x8000  emulator-managed guest object pool (FileLocks etc.); the handler
//           never touches it

#ifndef COPPERLINE_BOARD_H
#define COPPERLINE_BOARD_H

#define BOARD_MANUFACTURER 0x1448 // dec0de Consulting
#define BOARD_PRODUCT      0x05   // Copperline services board

#define ROM_OFFSET         0x0008
#define MOUNTS_OFFSET      0x3800
#define MOUNT_ENTRY_SIZE   32
#define MOUNT_MAX_COUNT    16
#define VOLUMES_OFFSET     0x7000
#define VOLUME_SLOT_SIZE   128
// Per-unit FileSysStartupMsg, written by the emulator at expansion init;
// dn_Startup points here so the Early Startup boot menu can display the
// device name, unit, and dostype instead of dereferencing garbage. Each
// FSSM references a per-unit DosEnvec whose de_BootPri carries the
// configured AddBootNode priority.
#define FSSM_OFFSET        0x7800
#define FSSM_SLOT_SIZE     16

// A-line opcodes reserved for host traps (see FilesysHle in src/filesys.rs).
#define TRAP_DIAG_ENTRY    0xA400 // DiagPoint entered (logged by the host)
#define TRAP_PACKET        0xA402 // D1 = struct DosPacket *, A1 = handler port

// trap_packet return values (D0).
#define TRAP_RES_REPLY     0 // packet complete: reply it to the sender
#define TRAP_RES_NOREPLY   1 // host keeps the packet (reserved, not yet used)
#define TRAP_RES_ADDVOLUME 2 // reply, then AddDosEntry the volume DosList
                             // node the host built (returned in A0): only
                             // guest code may take the DosList semaphore
#define TRAP_RES_DIE       3 // ACTION_DIE accepted: reply, RemDosEntry the
                             // volume node (in A0), and exit the process
                             // (dn_Task is already cleared, so the next
                             // reference restarts the handler)

#endif // COPPERLINE_BOARD_H
