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
//             +4     rt_Init trampoline (a real PC-relative branch to the
//                    mounting code; the Romtag's rt_Init field must not
//                    name it via a data-directive extern -- see entry.s)
//             +8     expansion-init entry (jsr-ed by the DiagArea stub
//                    with the DiagPoint registers; patches the Romtag)
//             +0x40  struct DiagArea (er_InitDiagVec points here; the
//                    DiagPoint stub reaches the ROM via jsr 16(a0))
//   0x3800  mount table, written by the emulator:
//             u16 count, then count fixed-size entries of the DOS device name
//             as a NUL-terminated string ("HOSTFS0", ...); the entry's last
//             byte is its kind (MOUNT_KIND_*)
//   0x3A00  clipboard unit register bank (CLIP_REGS_OFFSET, see below)
//   0x4000  host -> guest clipboard text window (CLIP_H2G_OFFSET, 4K)
//   0x5000  guest -> host clipboard IFF window (CLIP_G2H_OFFSET, 4K)
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
// The entry's kind byte sits after the longest possible device name (30
// characters plus NUL). A filesys entry is a HOSTFS<n> mount served by the
// packet pump; the clipboard entry is the DOS device (HOSTCLIP) whose
// handler process runs the clipboard bridge instead (clipboard_main).
#define MOUNT_KIND_OFFSET  (MOUNT_ENTRY_SIZE - 1)
#define MOUNT_KIND_FILESYS   0
#define MOUNT_KIND_CLIPBOARD 1
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

// Clipboard unit (src/clipboard.rs). Its own 256-byte bank away from the
// mount units, so it never collides with a full set of eight HOSTFS
// mounts: the first 0x40 bytes carry the same REG_DOSPKT/MSGPORT/RESULT/ARG
// pump registers as a mount bank (the HOSTCLIP DOS device answers its
// startup packet through them), the rest is the bridge itself.
#define CLIP_REGS_OFFSET   0x3A00
// Write: CLIP_CTRL_* verb, acted on within the write.
#define CLIP_REG_CTRL      0x40
// Write: byte offset for FETCH (into the staged host text) and PUSH (of
// the chunk within the guest's IFF stream; 0 starts a new stream).
#define CLIP_REG_OFFSET    0x50
// FETCH: read, the number of bytes the host placed in the H2G window (0 =
// end of text). PUSH: write, the number of bytes the guest placed in the
// G2H window.
#define CLIP_REG_LEN       0x60
// Read: total length of the staged host text, valid after FETCH offset 0.
#define CLIP_REG_TOTAL     0x70
// Read: CLIP_ST_* bits.
#define CLIP_REG_STATUS    0x80
// Read: generation of the newest host text (bumped whenever the host
// stages one); the doorbell interrupt announces a bump.
#define CLIP_REG_HOSTGEN   0x90
// Write: the generation the guest has finished writing into
// clipboard.device (an acknowledgement the host records).
#define CLIP_REG_GUESTGEN  0xA0
// Read: generation of the text FETCH offset 0 staged for transfer. It stays
// fixed until the next FETCH offset 0, so a transfer is never torn by newer
// host text arriving mid-way.
#define CLIP_REG_STAGEDGEN 0xB0
// Transfer windows: the host fills H2G with a FETCH chunk of the staged
// text, the guest fills G2H with a PUSH chunk of the raw IFF clip.
#define CLIP_H2G_OFFSET    0x4000
#define CLIP_G2H_OFFSET    0x5000
#define CLIP_CHUNK_SIZE    0x1000

// CLIP_REG_CTRL verbs.
#define CLIP_CTRL_ENABLE  1 // the guest bridge is up: interrupts may be raised
#define CLIP_CTRL_DISABLE 2 // the guest bridge is going down
#define CLIP_CTRL_IRQACK  3 // from the INT2 server: drop the interrupt line
#define CLIP_CTRL_FETCH   4 // stage text[OFFSET .. OFFSET+4K) into H2G
#define CLIP_CTRL_PUSH    5 // append LEN bytes from G2H at OFFSET
#define CLIP_CTRL_COMMIT  6 // the pushed IFF stream is complete

// CLIP_REG_STATUS bits.
#define CLIP_ST_IRQ     0x01 // host text is waiting (INT2 asserted)
#define CLIP_ST_PRESENT 0x02 // the host side is sharing its clipboard

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
