# NewTek Video Toaster Flyer: emulation spec for Copperline

Scope: the Flyer Zorro II card (1994-98) as the Video Toaster 4.2 software uses it in an
A4000, and how to add it to Copperline next to the existing Toaster 4000 model. The spec
is derived from NewTek's released sources and the installed 4.2 system. Nothing here was
checked against hardware, and the emulator was not run.

**Confidence tags.** **[H]** the source states it, or the code allows no other reading.
**[M]** strongly implied by how the code uses it. **[L]** inference or guess.
**[?]** the sources are silent.

**Path prefixes**

| prefix | path |
|---|---|
| `F/` | `<ovt-src>/flyer/flyer/`: Flyer 68000 firmware (SAS/C + asm), `exe/` builds |
| `A/` | `<ovt-src>/amiflyer/amiflyer/`: flyer.library (`src/flyerlib.asm` 4.44), flyerscsi.device, FlyerFileSystem, tools |
| `E/` | `<ovt-src>/Edit/Edit/`: Project Editor |
| `S/` | `<vt-src>/full/switcher/SRC/`: switcher 4.x |
| `B/` | `<installed>/`: the installed system the emulator boots |
| `C/` | `this repository/` |
| `CF` | `crates/flyerclip/FORMAT.md`: clip container and VTASC (already implemented in `C/crates/flyerclip`) |

**Installed versions** (B/NewTek/Programs): `flyer.library` 4.42 (26.03.97), with the same
150-entry vector table as the 4.44 source **[M]**; FlyerFileSystem 1.05 (04.08.95);
flyerscsi.device 1.30 (06.09.95). `Flyer_Support/*.dat` are byte-identical to `F/exe/*.dat`,
except `TR.dat` **[H]** (cmp). `Flyer.dat` holds one Flyer.bin (96,092 bytes, built Jun 1998)
four times **[H]**.

---------------------------------------------------------------------------

## 0. Summary of the architecture

```
 Amiga 4000                         Flyer card (own 68000, own bus)
 ----------                         --------------------------------------------------
 flyer.library  --Zorro II-->  256 KB dual-port SRAM  <--  68000 @ ~14.3 MHz (code runs IN this SRAM)
 (only interface:              $000-$4FF ctrl/guru        |  boot EPROM @ $000000 (not in the sources)
  8 x 32-byte command           $500-$5FF 8 cmd slots      +-- serial bus FPGAs: Timer/Router, Skew(route),
  mailbox + data pool,          $600-$EFFF Amiga pool      |     VTASC M/P coders x2 ch, Audio, Aligner, TBC
  all polled, no IRQ)           $18000- Flyer.bin          +-- DMA FPGA (7 ch) <-> 4 MB DRAM (block-addressed)
                                                           +-- 3 x AM53CF96 SCSI (buses A/B/C = FA/FB/FC)
                                                           +-- HSP43168 FIR, ADC (HI1175), DAC (MB40968)
                                                           +-- ADSP-2115 DSP + AD1847 codec (8 mono ch mix)
                                                           +-- DUART (timer, serial, LEDs, DIPs), EEPROM, HC05 genlock uC
 Toaster 4000 (video slot) <== ribbon ==> Flyer: Toaster VID3/VID4 are routed through the Flyer
                                           (Flyer ch A -> VID3, ch B -> VID4); Main/inputs feed its recorder
```

Everything the Amiga software does to the Flyer goes through command codes in the shared SRAM.
No Amiga code found writes Flyer chip registers directly; raw register access exists only as
mailbox commands used by test and calibration tools. **High-level emulation (HLE) of the
command set is therefore possible, and it is recommended (§6).**

---------------------------------------------------------------------------

## 1. Flyer CPU, memory map, boot

### 1.1 CPU

| Claim | Evidence | Tag |
|---|---|---|
| Plain 68000 family (68000/68HC000), 24-bit bus | `F/src/main.c:3` "Flyer/Phoenix 68000 Microcode"; `B/.../Documentation/Flyer/FlyerLEDs.doc` "Flyer software running on 68000"; the guru handler decodes the 68000 group-0 bus/address-error frame (R/W, I/N, FC, access address, IR) `F/src/debug.c:297-315`; `F/inc/exec.i:50` "Total number of 68000 vectors" | H |
| Clock about 14.3 MHz (likely 28.636 MHz VCXO / 2) | `F/src/Exec.asm:1529` "606 cycles or 42us" = 14.4 MHz; the synthesizers' reference is 14.31818 MHz `F/src/hard.c:1050` | M (CPU clock), H (ref clock) |
| Which package is the CPU | not stated. The heat-sinked large QFPs are more likely the ORCA ATT1C07 FPGAs (P-coders x2, DMA) than the CPU. | L |
| Firmware runs a small preemptive exec (tasks, signals, semaphores) | `F/src/Exec.asm`, `F/doc/exec.doc` | H |

Interrupt levels (autovectored, via a RAM revector table at `$C3FA00`, 6-byte `JMP abs.l`
entries, vectors 25-30; `F/inc/exec.i:46-64`, `F/src/Exec.asm:1411-1527`) **[H]**:

| IPL | handler | source |
|---|---|---|
| 1 | `SerialIRQ_Handler`: DUART (serial A/B, exec time-slice counter, `F/src/ser.c:172-187`) | DUART |
| 2 | `CPUDMAIRQ_Handler`: SRAM<->DRAM copy done | 3Port CPU-DMA |
| 3 | `ScsiIRQ_Handler` | 3 x SCSI |
| 4 | `FieldIRQ_Handler`: top/mid-of-field, video A/B line IRQs, genlock status (`F/src/vid.c:3190-3360`) | Timer/Router FPGA |
| 5 | `AudioIRQ_Handler`: DSP -> 68000 (audio DMA done), cleared by a write to `AudioCtrl.CLRIRQ` (`F/src/Audio.c:1542-1556`) | Audio FPGA |
| 6 | `VideoIRQ_Handler` | video |

The 3Port `IrqCtrl` word holds enables/status: MASTER b15, VIDEOB 8, VIDEOA 7, AUDIO 6,
FIELD 5, SCSI2..0 4..2, CPUDMA 1, SERIAL 0 (`F/inc/chips.h:121-131`) **[H]**.

### 1.2 Flyer-side memory map

| Address | What | Source | Tag |
|---|---|---|---|
| `$000000-` | Boot EPROM: vectors; ROM version long at `$0400`; ROM service vectors at `$0420-$043C` (`FindROMpatch` = `$0420`); `ColdBoot` re-enters it. **No ROM image or ROM source is in the release.** | `F/src/main.c:91`, `F/src/misc.asm:323-347`, `F/doc/BootLEDs.doc` | H (layout), H (absent) |
| `$A00000` | Internal serial register bus: register `r` at `SERBASE+1+2r`. Writes are direct. Reads are posted: the read starts on a read of the address, then poll `3Port.SERSTATUS` bit 0, then read `3Port.SERDATA` | `F/inc/flyer.h:73`, `F/src/misc.asm:71-82`, `F/src/hard.c:766-794` | H |
| `$A10000` | 3 x SCSI controller (`NCR53CF94` layout = AM53CF96). Chip n at `$A10000 + n*$200`, registers on odd bytes (TrxLo `+1`, TrxMid `+3`, FIFO `+5`, Command `+7`, ...) | `F/inc/SCSI.h:113-159`, `F/src/scsi.c:89` | H |
| `$A20000` | "3Port" chip: CPU-DMA regs `+0`, FPGA config bits for 9 FPGAs `+$11..+$21`, FPGA serial data bit `+$27`, clock-gen serial lines `+$29/+$2B`, HC05 genlock-uC bit-bang `+$2D`, EEPROM bit-bang `+$2F`, serial-bus status/data `+$30/+$31`, IrqCtrl `+$38` | `F/inc/chips.h:62-132`, `F/inc/chips.i` | H |
| `$A21000` / `$A21200` | HSP43168 FIR control (ctrl0/ctrl1) / coefficients (32 banks x 8 words) | `F/inc/flyer.h:76-77`, `F/inc/chips.h:23-32` | H |
| `$A22000` | DMA controller: ctrl + 7 channels (1 vid0, 2 vid1, 3 audio, 4 CPU, 5-7 SCSI0-2), 16-bit block pointers | `F/inc/DMA.h:29-81` | H |
| `$A30000` | DUART, low byte lanes: serial A/B (SMPTE timecode or MIDI/control devices), counter = exec tick, output port = LEDs, input port = DIP switches | `F/inc/flyer.h:79`, `F/src/ser.c` | H |
| `$C00000-$C3FFFF` | **Shared SRAM, 256 KB** (2 x TC551001 128Kx8, matches the board) | `F/inc/flyer.h:83-92`, `A/inc/flyerprivate.i:97` `SRAMSIZE $40000` | H |
| DRAM | 4 MB, **not CPU-mapped**: reached only by DMA (SCSI, video, audio, CPU-DMA), addressed in 512-byte blocks `0..$1FFF` | `F/inc/flyer.h:126-141` (`VTASCDRAM = $2000-2`), `F/src/ops.c:134-160` | H (blocks), M (not CPU-mapped) |

Serial-bus register groups (`F/inc/flyer.h:106-112`) **[H]**: TBC `$00-$0F` (optional TBC
module), Timer/Router `$40-$7F`, VTASC ch0 `$80-$87`, ch1 `$88-$8F`, Audio `$E0-$EF`,
Skew `$F0-$FB`, Aligner `$FC-$FF`. Register layouts are in `F/inc/chips.h:139-485`.

The FPGAs (`F/inc/chips.h:36-57`, configuration delays `F/src/hard.c:210-226`) **[H]** are:
1 Skew (XC3030-class delay); 2 Timer/Router (ORCA 1C05); 3/4 P-coders (1C07); 5/6 M-coders
(1C03); 7 DMA (1C07); 8 Audio-DMA (1C03); 9 Aligner (1C03). The "1C"/"2C"/"2CA"/"2CC" chip
sets in the `.dat` files are ORCA 1C and 2C board generations, picked by serial number
(§1.4) **[M]**.

The TI TPC1010 in the photos is antifuse, so it cannot be one of the downloadable FPGAs.
It is probably the 3Port or the Zorro glue **[L]**.

Clock synthesizers (ICS2061/ICD2061-type), four outputs: 0 DMA, 1 Audio, 2 M-coder,
3 P-coder (`F/src/hard.c:1041-1048`). Firmware defaults are 28 MHz / 84,587,402 Hz /
65 MHz / 14.318 MHz (`F/src/main.c:130-133`) **[H]**. The 40 MHz oscillator clocks the SCSI
chips: `ClkConv = 0` "40 MHz", 10 MB/s sync (`F/src/scsi.c:186-192`) **[H]**. The Flyer's
own SCSI ID is 5 (`F/src/scsi.c:212`) **[H]**.

### 1.3 Shared SRAM layout (Amiga offset = Flyer address - `$C00000`)

| Offset | Size | Use | Source | Tag |
|---|---|---|---|---|
| `$00000` | 2 x 256 | `SWITLIST1/2`: list mailboxes to a Phoenix 68332 switcher. **Unused with a Toaster 4000.** | `F/inc/flyer.h:86-87`, `F/src/switcher.c:44-88`, `F/doc/332.doc` | H |
| `$00400` | 128 | Guru area: `$DEAD`, trap#, ROM version, PC, SR, SSP, D0-A6 | `A/inc/flyerprivate.i:92,115-124` | H |
| `$00480` | 128 | `SharedCtrl`: `+0` SeqError.b, `+1` Done.b, `+2` UserID.l, `+6` MoreInfo.l | `F/inc/flyer.h:95-101`, `A/inc/flyerprivate.i:129-135` | H |
| `$00500` | 8 x 32 | **Command slots** | `F/inc/amishar.h:26-34`, `A/inc/flyerprivate.i:79-80,94` | H |
| `$00600-$0EFFF` | ~59 KB | Amiga-managed data pool. The library allocates per command (ClipActions, names, buffers, FPGA bitstreams) and passes **SRAM offsets** | `A/inc/flyerprivate.i:95`, `A/src/flyerlib.asm:563-575` | H |
| `$0F600-$13200` | 30 blk | Flyer SRAM pool | `F/inc/flyer.h:157-159` (computed) | H |
| `$13200-$145FF` | 10 blk | FS buffers | `F/inc/flyer.h:154-156` | H |
| `$14600` | 1 blk | index table block | `F/inc/flyer.h:153` | H |
| `$14800/$15C00/$16C00` | | misc / audio / video frame headers | `F/inc/flyer.h:145-151` | H |
| `$17C00-$17FFF` | 2 blk | `VTASCSRAM`: block buffer that **the Amiga reads directly** after `CPUDMA` (field reads, frame headers) | `A/inc/flyerprivate.i:99-101`, `A/src/flyerlibvtasc.asm:1320-1350` | H |
| `$18000-` | | `CODEBASE`: Flyer.bin is loaded and run here (was `$20000` up to 4.04) | `F/inc/flyer.h:92`, `A/src/flyerlib.asm:3843-3856` | H |
| to `$3F000` | | exec free pool (`POOLEND $C3F000`) | `F/inc/main.h:27` | H |
| `$3F9FC` / `$3FA00` | | trap-handler vector / IRQ revector table | `F/inc/Debug.h:73`, `F/inc/exec.i:48` | H |

Flyer.bin is a flat image. It starts with `lea $C2DB64,a4` (the SAS near-data base),
`move #$2000,SR`, then `bra` to the entry **[H]** (bytes at file offset 0).

### 1.4 How FlyerInit loads and starts the Flyer

`FlyerInit` (`B/.../System_Support/c/FlyerInit`, source `A/src/FlyerInit.c`) and the switcher
at startup (`S/switcher.a:1548-1584`) both call `InitFlyers(lock)`. The lock is on
`SYS:` or `WORK:NewTek/Programs/Flyer_Support` (`A/src/FlyerInit.c:35-36`). The library
behaves as follows (`A/src/flyerlib.asm`) **[H]**:

1. **Library init** (on OpenLibrary): `FindConfigDev(2191, 0)` up to 4 boards
   (`:488-500`); `unit_SRAMbase = cd_BoardAddr`, so the board window *is* the SRAM. It clears
   `$500-$EFFF` (`:559-563`) and puts `$600-$EFFF` in the SRAM pool.
2. `InitFlyers` (`:2831-3030`) returns at once if this unit is already set up (`FUB_SETUP`).
   The second call, from the switcher, is therefore free.
3. `LoadFlyerBin("Flyer.dat", chipID=0)` loads the first chunk. `.dat` format: repeated
   `{u32 chipID, u32 len, len bytes}`; IDs `'1C\0\0' '2C\0\0' '2CA\0' '2CC\0'`
   (`:3443-3610`, `A/inc/flyerprivate.i:356-360`).
4. `Firmware(0, len, data, $18000)` (`:3808-3888`): FastCopy the image to SRAM `+$18000`,
   slot `+4` = `$18000` (go offset), `+8` = length, opcode `op_FIRMWARE` (1), synchronous.
   The boot ROM jumps to it. Flyer.bin's `MainLoop` marks **slot 0** DONE as its "I'm
   running" handshake (`F/src/amishar.c:2427`). Once Flyer.bin runs, op_FIRMWARE is a no-op
   (`F/src/ops.c:81-84`).
5. `ReadEE(4)`, `ReadEE(3)` (`:2915-2932`): serial = (EE[4]<<16)|EE[3].
   `GetChipID("chipset.dat")` (`:3385-3440`) maps the serial to a chip set. Installed
   `chipset.dat` ranges (`B/.../Flyer_Support/chipset.asc`): 1C `0..8004800`;
   2C `8004801..8004900`; 2CA `8004901..8009900`; 2CC `8100000..8105000`. **If no range
   matches, InitFlyers fails with error 1.**
6. `QuitFlyer` (`op_QUIT`): the firmware marks done, waits for the library to clear the slot,
   then `ColdBoot`s into the ROM (`F/src/amishar.c:292-317`). The library then busy-waits
   1,000,000 loops (`:2954-2962`).
7. `ProgramChips` (`:3125-3355`):
   - `LoadFlyerBin("Flyer.dat", chipID)` and `Firmware` again;
   - `CacheTest` (§2.6);
   - for SK(1), TR(2), DMA(7), AUD(8), AL(9) (`:2773-2784`): load the chunk, check the
     `FlyerChipHeader` (`'FLYR'`, chipnum, defnum, rev, boardrev, id, speed, length;
     `A/inc/flyerprivate.i:345-354`), `SetClockGen(0, speed)` before DMA, then `PgmFPGA`;
   - `DSPboot(DSP.dat)`;
   - `TeachFPGA` for ME/PE/MD/PD. These definitions are **kept in Amiga RAM** and
     downloaded at every mode change;
   - `PlayMode`: `op_NOMODE`; `SetClockGen(3)` + `PgmFPGA` P-decoders (chips 3+4, dual);
     `SetClockGen(2)` + `PgmFPGA` M-decoders (5+6, dual); `op_PLAYMODE`
     (`:11464-11590`). `RecordMode` does the same with the encoders (`:11598-11730`).
   - `UpdateLEDs` sends `SetFlooby(item 14)` after each step (`:3360-3375`). The LED codes
     are in `FlyerLEDs.doc`.

Error codes are in `B/.../Documentation/Flyer/FlyerInitErrors.Doc`. Boot-ROM states (DIP 1 =
autoboot) are in `F/doc/BootLEDs.doc`: "Waiting download thru shared RAM", or the ROM can
load microcode from SCSI (a Phoenix path) **[H]**.

---------------------------------------------------------------------------

## 2. The Amiga <-> Flyer interface

### 2.1 Zorro identity

| Item | Value | Source | Tag |
|---|---|---|---|
| Manufacturer | 2191 = `$088F` (NewTek) | `A/inc/flyerprivate.i:86` | H |
| Product | 0 | `A/inc/flyerprivate.i:87` | H |
| Bus | Zorro II (also runs in an A2000) | `A/src/flyerlib.asm` rev log 1.9 ("BIG BUGs on 2000") | H |
| Window size | 256 KB = the SRAM; offset 0 = SRAM byte 0 | `flyerlib.asm:493-494` (`cd_BoardAddr` used as the SRAM base), `SRAMSIZE` | M |
| er_Type flags | I/O board, not in the memory list; no diag ROM evidence. `A/src/KillBoardMem.c` (a 1994 dev tool) toggles `ERTF_MEMLIST` | | L |
| Interrupts to the Amiga | **none**. flyer.library and flyerscsi.device install no interrupt servers; all completion is polled | grep of `A/src` | H |
| Other Amiga-visible registers | none found; no Amiga code addresses the window outside SRAM offsets | grep `unit_SRAMbase` in `flyerlib.asm` | M |

### 2.2 Command slot format (32 bytes, 8 slots at `$500`)

| Off | Field | Meaning |
|---|---|---|
| `+0` | opcode.w | bits 15-14 status: `00` blank, `01` NEW, `10` IN PROGRESS, `11` DONE; bits 13-0 opcode (`A/inc/flyerprivate.i:140-144`, `F/src/amishar.c:219,247,254-265`) |
| `+2` | cont.b | Amiga->Flyer: 1 = go, 0 = abort, `$FF` = pause, 1 = resume (`flyerlib.asm:1307-1316,1802-1830`). Flyer->Amiga: 2 = started (`flyerlib.asm:1258-1280`, `F/src/amishar.c:1197`) |
| `+3` | error.b | result (FERR_/ERR_ codes, identical numbering: `F/inc/Errors.h`, `A/inc/flyer.h:79-148`) |
| `+4..+31` | args | per command (§2.4). Pointers are **SRAM offsets**. `+28.l` is the progress/status long, copied to `ClipAction.Status` by CheckAction (`flyerlib.asm:1380`) |

Protocol **[H]**:

- **Amiga side** (`A/src/flyerlib.asm:859-1230`):
  1. `FindFreeCmd`, under Forbid, takes the first slot whose opcode word is 0 and writes
     `$0001` ("taken").
  2. It clears bytes 2-31 and fills the args.
  3. It writes `opcode|$4000` **last** (`:1183-1190`).
  4. By `ReturnTime`:
     - `RT_STOPPED`: busy-poll `(a3)` until DONE, run the follow-up (copy results back from
       SRAM), clear the opcode to 0 (`:1416-1440`);
     - `RT_STARTED`: poll until cont = 2 or DONE;
     - `RT_IMMED`: return; the ID = host-side cmd struct, and later `CheckAction`/`WaitAction`
       poll;
     - `RT_FREE`: fire and forget; reclaimed by `FlushAsyncs` (`:1002-1060`).
- **Flyer side** (`F/src/amishar.c:2409-2515`):
  - `MainLoop` round-robins the 8 slots. For each NEW opcode it sets IN PROGRESS
    (`CommandBusy`, which also zeroes error).
  - It then runs the handler from `FuncTable` (`:2266-2375`). The handler runs inline, or
    for a few opcodes as a separate task (`spawn`): FLYPLAY pri 3, FLYRECORD pri 0,
    READTEST/WRITETEST pri 5, SCSIDIRECT pri 0.
  - It sets DONE (`|= $C000`). An unknown opcode gives `ERR_BADCOMMAND` (5).
  - On a crash, `ShowHostCrash` sets error `$1E` and DONE on all slots (`:2520-2534`).
- The data pool is managed only by the Amiga (`AllocSRAM`/`AddChunk`). The Flyer never
  allocates in `$600-$EFFF` **[H]**.

### 2.3 Parameter structures in SRAM

`ClipAction` (130 bytes; Amiga `A/inc/flyer.h:308-345` = firmware `ACTION`
`F/inc/acts.h:52-90`, same offsets) **[H]**:

| off | field | off | field |
|---|---|---|---|
| 0 | Volume (SRAM offset of FlyerVolume) | 36 | VolAttack.w (ramp fields) |
| 4 | ID (library) | 38 | VolSust1.w (0xFFFF = 100% in the Editor UI) |
| 8 | ReturnTime.b | 40 | VolSust2.w |
| 9 | Channel.b (video ch 0=A, 1=B) | 42 | VolDecay.w (fields) |
| 10 | Flags.b: VIDEO 1, AUDIOL 2, AUDIOR 4, USEMATTE 8, NOPREROLL `$10`, APPEND `$20`, REPROCESS `$40`, AUDENV `$80` | 44 | AudioPan1.w (-`$8000` left .. `$7FFF` right) |
| 11 | PermissFlags.b: STEALOURVIDEO 1, KILLOTHERVIDEO 2, ERRIFBUSY 4, AUTOMUTE 8, USEHEADS `$10` | 46 | AudioPan2.w |
| 12 | VidStartField | 48 | TotalAudStart |
| 16 | AudStartField | 52 | TotalAudLength |
| 20 | VidFieldCount (0 = to end) | 72 | StartBlk (out: LBA, raw) |
| 24 | AudFieldCount | 76 | EndBlk |
| 28 | GoClock (field clock; 0 = "now+20") | 80 | UserID (reported in SharedCtrl) |
| 32 | MatteY.w, I.b, Q.b | 96 | Grip, 100 FileID, 104 Access.b, 106 FldHandle |
| | | 122 | Status; 126 LastFieldDone (out) |

`FlyerVolume` (24 bytes; `A/inc/flyer.h:296-303`, `F/inc/acts.h:37-44`): Path (SRAM offset of
a NUL string, **volume prefix already stripped** by the library), Board.b, SCSIdrive.b
(= bus*8 + target ID), Flags.b (`FVF_USENUMS`), pad, reserved[4] **[H]**. `PassClipAction`
lays out ClipAction, FlyerVolume and the string in one pool chunk, and copies the
ClipAction back to the caller when done (`flyerlib.asm:2313-2440`) **[H]**.

Other result structures copied through SRAM (Amiga layouts): `FlyerVolInfo`
(`A/inc/flyer.h:157`), `ClipInfo` (`:195`), `VidCompInfo` (`:263`), `TBCctrl` (`:404`),
`FlyAudCtrl` (`:470`), `SMPTEinfo` **[H]**.

**Volume naming.** `FindVolume` (`flyerlib.asm:2589-2670`) strips `NAME:` from the path and
looks NAME up in the library's mounted list, which `FlyerDriveInfo` builds
(`flyerlib.asm:7934-8015`, NewVolume/KillVolume). AmigaDOS devices are `FA0:`, `FB0:`, `FC0:`
(`B/.../System_Support/Devs/DOSDrivers/FA0`: `CHAN=n,UNIT=0`, DosType `$464C5900`
"FLY\0"). Default volume names are `FlyerA0`, `FlyerB0`, ... (Documentation/Readme, "Flyer
clips in ToasterPaint/LightWave") **[H]**.

### 2.4 Full command list

Opcode = position in `A/inc/opcodes.i:55-165` = index into `F/src/amishar.c:2266-2375`
**[H]**. Column "Use" says who calls it, and "HLE" gives the phase (§6) or what to do.

| op | name | firmware handler | slot args (after opcode/cont/err) | Use | HLE |
|---|---|---|---|---|---|
| 00 | NULL | Null_cmd | | | ok |
| 01 | FIRMWARE | ops.c:81 (no-op once running) | +4 go offset, +8 len | InitFlyers | P1: ok, mark running |
| 02 | CALLMOD | RunTest_cmd (Tests.c) | argc, args | test | BADCOMMAND |
| 03 | PGMFPGA | hard.c:202 | +4 data off, +8 bytes, +12 chip, +13 dual, +14 rev | init, mode change | P1: ok |
| 04/05 | SBUSW/SBUSR | hard.c:766/781 | +4 addr, +5 data | calib/test tools | stub (store/return) |
| 06/07 | CPUW/CPUR | amishar.c:1360/1378 | +4 addr.l, +8 data.w | test tools | stub |
| 08 | GETCLOCK | GetFieldClock (vid.c:3715) | out +4 field clock.l | switcher SyncClocks, Editor | **P1** |
| 09-0B | FIRINIT/FIRXCHG/FIRMAPRAM | hard.c:455/485/608 | FIRXCHG: +4 set, +5 rd, +6 pre/post, +8 8 coefs.w, +24 scale.w | FIRquery (field read/write, via software VTASC) | P2: return presets (flyerclip `fir_preset`) |
| 0C | DSPBOOT | hard.c:677 | +4 data off, +8 len | init | P1: ok |
| 0D | CPUDMA | ops.c:134 | +4 SRAM blk#, +8 DRAM blk#, +12 count.w, +14 dir.b (0 = DRAM->SRAM) | field read/write, frame headers | **P2** |
| 0E | VIDPARAM | amishar.c:1078 | vchan, tolerances, freq, lengths, firset | compression setup | store |
| 0F/10/11 | PLAYMODE/RECMODE/NOMODE | vid.c / ops.c:234 | | init, record panel | P1: mode state |
| 12 | TOASTMUX | hard.c:845 | +4 input3, +5 input4, +6 preview (0 = pass Toaster through) | switcher/Editor before play | **P3** (video routing) |
| 13 | INPUTSEL | hard.c:870 | +4 source (FI_*), +5 sync (0 = Toaster Main ref) | record panel | P1 store, P5 use |
| 14 | TERMINATE | hard.c:907 | +4 flags | setup | store |
| 15/16 | SRCHARGS/DOSEARCH | acts.c:315/588 | ClipAction | jog/shuttle, Begin/Do/EndFindField | P5 |
| 17 | FLYPLAY | Play1Clip (seq.c:1261), spawned | +4 ClipAction | switcher crouton play, Editor PlayFlyerEvent | **P3** |
| 18 | FLYRECORD | acts.c:882, spawned | +4 ClipAction | record panel, Editor Flyer_Record | P5 |
| 19 | CHGAUDIO | acts.c:1144 | ClipAction (attached) | live volume change | P4 |
| 1A | GETSMPTE | ops.c:253 | +4 SMPTEinfo off | | zeros |
| 1B-1F | NEWHEADLIST/ENDHEADLIST/MAKEHEAD/VOIDHEAD/VOIDALL | heads.c | | Editor project load, switcher | ok (no heads needed in HLE) |
| 20 | AUDEKEY | AddAudEKey_cmd | +4 time, +8 dur, +12 flags, +14 v1, +16 v2, +18 p1, +20 p2 | Editor audio envelopes in sequences | P4 |
| 21/22/23 | SCSIRST/SCSIINIT/FINDDRV | | drive; FINDDRV: drivebits, versions[8], lengths[8] | HDTools | P2: report present drives |
| 24 | COPYDATA | ops.c:272 | src/dst drive, lba, count | CopyClip, flyerstream | P5 |
| 25/26 | SCSICALL/READSIZE | ops.c:313/349 | | HDTools | synthesize |
| 27/28 | READ10/WRITE10 | ops.c:376/552 | +5 drive, +6 lba, +10 blocks.w, +12 **DRAM** blk# | field read/write, GetFrameHeader | **P2** (virtual LBA map) |
| 29 | DRIVEINFO | FSinfo (FILESYS.C:3549) | +5 drive, +6 FlyerVolInfo off | FS handler, Editor, switcher | **P2** (`SELTIMEOUT $40` if absent) |
| 2A | LOCATE | FS_Locate | ClipAction: Grip in/out, Access, StartBlk out | FS handler | P2 |
| 2B/2C/2E | FREEGRIP/COPYGRIP/PARENT | | +5 drive, +6 grip, out +10 newgrip, +14 block | FS handler | P2 |
| 2D/2F | CMPGRIPS/EXAMINE | Unknown (BADCOMMAND) | | | BADCOMMAND |
| 30 | DIRLIST | DirList (FILESYS.C:1573) | +5 drive, +6 grip, +10 ClipInfo off, +14 first.b, +15 fsonly.b | FS handler | P2 (`EXHAUSTED $23` at end) |
| 31 | FILEINFO | FileInfo (FILESYS.C:1685) | +4 FlyerVolume off, +8 ClipInfo off | GetClipInfo: Editor, switcher | **P2** |
| 32-36 | FILEOPEN/CLOSE/SEEK/READ/WRITE | amishar.c:617-760 | READ: +5 drive, +6 id, +10 len, +14 buf off, out +18 actual | FS handler (1.05 uses FILEREAD/WRITE) | P2 |
| 37-3F | CREATEDIR, DELETE, RENAME, RENAMEDISK, FORMAT, DEFRAG, SETBITS, SETDATE, SETCOMMENT | | | FS handler, Editor | P2 (DEFRAG: ok, progress at +28) |
| 40 | WRITEPROT | | +5 drive, +6 value, +7 set | FS handler | P2 |
| 41 | CHANGEMODE | Unknown | | | BADCOMMAND |
| 42 | MAKEFILE | CreateFile_cmd | +4 vol, +8 size, out +12 start, +16 frag | Editor backup, MakeAudClip | P2 |
| 43 | COPYCLIP | CopyFile_cmd | src/dst vol, verify, progress +28 | Editor DosHelp | P5 |
| 44 | DEBUGFLAGS | | | | ok |
| 45 | FLOOBY | SetFloobyDust (ops.c:579) | +4 chan, +5 item, +6 value.l | LEDs (14), still mode (9), calibration | P1: ok |
| 46/47 | READTEST/WRITETEST | spawned | | FlyerHDTools | synthetic speeds |
| 48 | QUIT | Suicide_cmd (amishar.c:292) | | InitFlyers, FlyerInit REBOOT | **P1** |
| 49 | SCSIDIRECT | spawned | +4 drive, +6 info off | flyerscsi.device (HDTools, HDToolBox) | P5 / CHECK-CONDITION |
| 4A, 4D-50 | OPENREADFLD, READLINE, WRITELINE, SETFILLCOLOR, SKIPLINES | Unknown | done in the library (software VTASC) | | BADCOMMAND |
| 4B/4C | OPENWRITEFLD/CLOSEFIELD | clip.c | write a still/field into a clip | CG/Paint/LightWave saving to Flyer | P5 |
| 51 | GETCOMPINFO | clip.c:124 | +4 drive, +5 field, +6 lba (0 = live), +10 VidCompInfo off | OpenReadField | **P2** |
| 52 | SETCLOCK | SetTimeClock_cmd | days, mins, ticks | switcher `Flyer_SetClock`, Editor | P1 |
| 53 | STRIPAUDIO | clip.c:1165 | | Editor | P5 |
| 54/55 | WRITECALIB/READCALIB | ops.c | +4 item.w, +6 value.w, +8 save | switcher tweak panel, FlyerCalibrate | P1: EEPROM-backed table |
| 56/57 | EEWRITE/EEREAD | hard.c:918/937 | +4 addr, +6 data.w | InitFlyers (serial) | **P1** |
| 58 | RESETFLYER | ActsReset_cmd | +4 flags | Editor HandleStop | P3: stop everything |
| 59 | SETCLOCKGEN | SetClockFreq | +4 clock, +6 Hz | init, mode change | P1: ok |
| 5A | LOADVID | vid.c:435 | | test | ok |
| 5B | SERDEVICE | SetDevice_cmd | port, type, make | SMPTE reader setup | store |
| 5C | SELFTEST | Unknown | | | BADCOMMAND |
| 5D | TBC | hard.c:1991 | +4 TBCctrl off, +8 oper | switcher startup (`S/switcher.a:2850`), TBC panel | P1: Status = 0 (no module) |
| 5E-60 | NEWCUTLIST/ADDSUBCLIP/MAKESUBCLIPS | | | Cutting Room (Editor via switcher) | P5 |
| 61 | CODECCTRL | CODEC_cmd (Audio.c:225) | +4 FlyAudCtrl off, +8 oper | record panel gain/meters (`S/pecomm.a:4481`, `E/src/flyer.c:104`) | P4 |
| 62-65 | NEWSEQ/ADDSEQCLIP/ENDSEQ/PLAYSEQ | seq.c:230/418/857/966 | ADDSEQ: ClipAction; ENDSEQ: +4 doit, progress +28; PLAYSEQ: +4 basetime | **Editor sequence playback** | **P3** |
| 66 | GETSETOPT | | +4 flags.l, +8 set | Editor setup (HQ5) | P1: persist |
| 67 | LOCATEFLD | LocateField (acts.c:126) | ClipAction: VidStartField -> StartBlk (CFRM LBA) | OpenReadField, Get/PutFrameHeader | **P2** |
| 68 | CACHETEST | amishar.c:2122 | handshake (§2.6) | InitFlyers | **P1** |
| 69-6B | FILEEXTEND/FILEHSREAD/FILEHSWRITE | | | newer FS/NewFileRead (not FS 1.05) | P2 |

### 2.5 What the 4.2 software calls, by task

Grepped call sites in `S/`, `E/src/`, `A/src/flyerfs1.c`; library functions map to the
opcodes above. **[H]** unless marked otherwise.

- **Startup (switcher)**
  - `Flyer_InitFlyers` -> InitFlyers (`S/switcher.a:1554`).
  - `InitFlyerPlay` = AbortPlayRec + `Flyer_DefaultSelect` + `Flyer_PlayMode`
    (`S/pecomm.a:4297-4305`; DefaultSelect's body is in the missing `Flyer.o` and is
    presumably TOASTMUX/INPUTSEL **[L]**).
  - `Flyer_SetClock` -> SETCLOCK (`S/switcher.a:1584`).
  - `Flyer_TBCcontrol(STATUS|MODES|ADJUST)` -> TBC (`S/switcher.a:2850`).
  - Failure of InitFlyers gives the requester "Unable to initialize the Flyer"
    (`S/switcher.a:3121`).
- **Drive/volume listing**: FlyerFileSystem polls `FlyerRunning` every 3 s, then calls
  `FlyerDriveInfo` (DRIVEINFO; `A/src/flyerfs1.c:96-104,470-488`). Directories and files use
  LOCATE, DIRLIST, PARENT, COPYGRIP/FREEGRIP, FILEOPEN/READ/SEEK/CLOSE, and for writing
  FILEWRITE, CREATEDIR, DELETE, RENAME, SETBITS/DATE/COMMENT, WRITEPROT. The Editor and
  switcher use `FlyerDriveInfo`, `GetClipInfo` (FILEINFO) and `FlyerOptions` (GETSETOPT)
  (`E/src/controls.c`, `S/pecomm.a:4329-4370`).
- **Clip thumbnails / stills / frame grabs**: `OpenReadField` = LOCATEFLD + GETCOMPINFO, then
  per 2 blocks READ10 (to DRAM `$1FFE`) + CPUDMA (to SRAM `$17C00`) + a direct Amiga read.
  Decoding is the library's software VTASC (`A/src/flyerlibvtasc.asm:80-150,1300-1350`).
  The same path serves GetFrameHeader (`flyerlib.asm:14105-14150`), the switcher's
  MakeClipIcon (`S/pecomm.a:4911`) and CG/Paint/LightWave stills. Writing:
  OPENWRITEFLD, CPUDMA (SRAM->DRAM), WRITE10, CLOSEFIELD.
- **Play one clip**:
  - `FlyerPlay` (FLYPLAY) from the switcher's crouton handler (`CroutonHandler.o` calls
    `Flyer_Play`; source not released **[H]** for the call, **[?]** for its arguments).
  - Editor `PlayFlyerEvent` (`E/src/flyer.c:61-100`).
  - The channel is routed first: `SetupPlayChannel` -> TOASTMUX (`S/pecomm.a:1908-1928`).
  - Live audio changes: `ChangeAudio` (CHGAUDIO, `S/pecomm.a:4638`).
- **In/out points**: ClipAction `VidStartField/VidFieldCount`, `AudStartField/AudFieldCount`
  (field units, 4 fields = 1 colour frame). Jog/shuttle use SRCHARGS/DOSEARCH (`S/pecomm.a`
  `Hand_Jog`/`Hand_Shuttle` ~3637-4280).
- **Sequencing** (`E/src/sequence.c:3214-3260,5690-5760`):
  1. GetClrSeqErr(clear) (SharedCtrl, no opcode).
  2. NEWSEQ.
  3. Per event, ADDSEQCLIP: GoClock = offset in the sequence, Channel A/B, audio
     vol/pan/attack/decay, CAF_AUDENV. Then AUDEKEY / AddAudEnv for envelopes.
  4. ENDSEQ (async with progress; the Flyer builds A/B "heads" when needed).
  5. `ES_StartSeq` -> switcher `SyncClocks` (§3.3).
  6. `PlaySeq` (`E/src/toastsupport.a:749-811`): TB_TimeAtStart = (MasterTime+28) & ~3,
     then PLAYSEQ(basetime). The Amiga then runs the switcher events (TAKE/AUTO of
     VID3/VID4) at `TimeAtStart + ev_Time`, calling `FlyerMuxedIn` -> TOASTMUX first
     (`:1095-1135`).
  7. Status and the end of the sequence are read from SharedCtrl (`flyerlib.asm:5080-5130`).
  8. RESETFLYER on stop (`E/src/sequence.c:5280`).
- **Record**: RECMODE, INPUTSEL (`S/pecomm.a:4442`), CODECCTRL (gain/source/mix/meters),
  FLYRECORD (async, `E/src/flyer.c:346-405`), pause via cont = `$FF`/1, abort via cont = 0.
- **Audio levels/pan**: per-clip ClipAction fields and AUDEKEY envelopes (DSP events,
  §4.3); CHGAUDIO for live changes. The record level is CODECCTRL.
- **Switcher integration**: only TOASTMUX + GETCLOCK + the Toaster's own buses. The
  Phoenix list mailbox (`SWITLIST`) is not used with the Toaster 4000 (PHXCODE only;
  `F/src/seq.c:1678-1680`, `F/src/ops.c:163-228`) **[H]**.

### 2.6 Commands with real-time handshakes

These have to be serviced inside the Amiga CPU's busy loop, with Amiga interrupts off.

- **CACHETEST** (`flyerlib.asm:10556-10640` vs `F/src/amishar.c:2122-2148`):
  1. The Amiga writes `$4068`.
  2. The Flyer sets IN PROGRESS and writes `$FEEDF00D` at `+28`; the Amiga waits for
     IN PROGRESS.
  3. The Amiga writes `$DEADD00D` to `+28`, then cont = 0.
  4. The Flyer writes `$12345678` to `+28` and sets DONE. The Amiga polls `+28` **at most
     201 times** for `$12345678`.
  Failure only sets `FLB_CACHED` (extra CacheClearU calls), so it is not fatal **[H]**.
- **GETCLOCK in SyncClocks** (`S/pecomm.a:3174-3210`), Amiga interrupts off:
  1. Read the clock until it changes.
  2. Wait for `vhposr >= 30` (low byte).
  3. TB_MasterTime := clock; clear pending VERTB.

  Comment: the clock "usually changes about 40% down the field". Each GETCLOCK is a full
  mailbox round trip **[H]**.
- **QUIT**: after DONE, the firmware waits for the slot to be cleared before rebooting
  (`F/src/amishar.c:303-316`) **[H]**.

---------------------------------------------------------------------------

## 3. Video path

### 3.1 Playback into the Toaster

- Two video channels A and B (`NUMVIDCHANS 2`, `F/inc/flyer.h:64`). Each runs DRAM ->
  VTASC decoder (P+M coder pair) -> FIR post-filter (HSP43168) -> DAC A/B (Aligner
  `DACACTRL/DACBCTRL`, `F/inc/chips.h:472-485`) **[H]**.
  The MB40968 is the DAC (one dual part or one per channel) **[L]**.
- **Channel A -> Toaster VID3, channel B -> Toaster VID4.** `ToasterMux(in3, in4, prv)`
  sets Skew `Route`. A nonzero in3/in4 puts the Flyer channel on that Toaster input;
  0 passes the external input through (`SKRF_Sel3/Sel4`). preview = 0 passes the Toaster's
  Preview through; 1 shows the TBC/camcorder signal on Preview (`F/src/hard.c:845-865`,
  `F/inc/chips.h:415-461`; channel mapping `E/src/toastsupport.a:606-630,1095-1135`,
  `S/pecomm.a:1889-1928`) **[H]**.
- The user-level "Flyer Out" setup option: inputs 3 and 4 come from the Flyer or from the
  external connectors on the back of the Toaster card. Playing a clip or sequence reverts
  it to Flyer control (Documentation/Readme, "Flyer Out") **[H]**.
- The ribbon cable carries:
  - Toaster VID3/VID4 out to the Flyer's mux and back (intercepted);
  - VID1, VID2, Main and Preview to the Flyer's record mux (`ISRC_*` `F/inc/chips.h:454-461`);
  - Preview back (replaceable);
  - Main as genlock reference.
  Skew `Term` has termination bits for Main, VID1, VID3 and VID4 (`F/inc/chips.h:425-430`).
  **[M]** (signals), **[?]** (pinout).
- The Flyer genlocks to Toaster Main (`InputSelect sync = 0` -> `SKRF_MainRef`,
  `F/src/hard.c:894-895`; 28.636 MHz VCXO, HC05 genlock controller
  `F/inc/chips.h:100-104`). The Toaster supplies sync to the Amiga, so the Amiga, Toaster
  and Flyer fields are locked **[M]**. Loss of sync lights LED 8 (FlyerLEDs.doc) **[H]**.
- Field clock: `FieldClock++` at each top-of-field IRQ, forced to `&~3` at colour-frame
  field 1, so `FieldClock % 4` tracks the 4-field NTSC sequence (`F/src/vid.c:3375-3386`)
  **[H]**. Every start time is rounded down to a multiple of 4 (`F/src/seq.c:1005,1687`)
  **[H]**.
- Idle and end-of-clip behaviour:
  - PLAYSEQ and mode changes output black on both channels (`BlackOut`,
    `F/src/vid.c:4317`; `F/src/seq.c:1010`) **[H]**.
  - At the end of a clip the Flyer shows a matte (MatteY/I/Q, default black) if
    `CAF_USEMATTE`/`EF_GOMATTE` is set or the clip was aborted (`F/src/vid.c:2016-2020`)
    **[H]**; otherwise the last field stays up **[M]**.
  - Single-clip play with `GoClock = 0` starts at `(clock+20) & ~3` (`F/src/seq.c:1685-1687`);
    a sequence with `basetime = 0` starts at `(clock+60) & ~3` (`:1003`) **[H]**.
- Pipeline delay from a Flyer field to the Toaster input: none is modelled in the sources
  (the DSP gets a 1-field lead, `Audio.c` "clock+1") **[?]**.

### 3.2 Record path

- One ADC (HI1175, 8-bit) **[L: one part in the photos; the code has a single record
  engine]** -> FIR pre-filter -> VTASC encoders -> DRAM -> SCSI.
- `InputSelect(source, sync)` (`F/src/hard.c:870-903`, `A/inc/flyer.h:373-382`) **[H]**:

  | source | input |
  |---|---|
  | 0 | camcorder (composite, via TBC) |
  | 1 | S-VHS/YC (via TBC) |
  | 2 | Toaster VID1 |
  | 3 | Toaster VID2 |
  | 4 | Toaster Main |
  | 5 | Toaster Preview |

  sync: 0 = Toaster Main, 1 = Toaster VID1.
- The switcher's record panel selects `FLY_VideoSource_VID1` by default on entering record
  mode (`S/pecomm.a` `.ensure_rec_mode`) **[H]**.
- `GETCOMPINFO` with lba = 0 snapshots the live input buffer ("direct" OpenReadField)
  (`F/src/clip.c:134-157`) **[H]**.
- The optional TBC module (`TBCctrl`, `F/src/hard.c:1991+`) drives the camcorder
  composite/YC inputs and their termination **[H]**. The board's RCA "Video In x2" and Y/C
  are probably those inputs **[L]**.

### 3.3 Timing contract that HLE must keep

1. Exactly one field-clock tick per Amiga field, at a fixed beam line. The real change is
   ~40% down the field (`S/pecomm.a` SyncClocks comment).
2. `FieldClock % 4 == 0` on Toaster field I.
3. A clip event starts at field `StartTime`, which is a multiple of 4; event field k shows
   clip field `VidStartField + k`.
4. Stop after `VidFieldCount` fields.

---------------------------------------------------------------------------

## 4. Audio path

### 4.1 Hardware and boot

- **DSP code = `DSP.dat`** (`A/src/flyerlib.asm:2757`). All four chip-set chunks are the
  same 2,744-byte image = `F/exe/*/DSP.bin` **[H]** (md5). There is **no ADSP-21xx source in
  the release**: `<ovt-src>/DSP_Code` contains only CroutonLib **[H]**.
- Format: 686 program-memory words, 4 bytes each (24-bit word big-endian + filler byte)
  **[M]**. Decoded:
  - vectors: reset -> `JUMP $001C`, IRQ2 -> `$01CD`, SPORT0 TX -> `$0289`,
    SPORT0 RX -> `$01E4`, SPORT1/IRQ1 -> `$01C4`, SPORT1/IRQ0 -> `$0142`, timer -> RTI **[M]**;
  - PM word 630 = `AY0 = 735` **[H]**;
  - init stores to `DM[$3FF3,$3FF6-$3FFA,$3FFE,$3FFF]` (SPORT0 autobuffer/multichannel,
    wait states, system control), consistent with the AD1847 on SPORT0 **[L]**;
  - immediates `$860F/$8600/$8700` look like AD1847 control words for indexes 6/7 (DAC
    attenuation) **[L]**.
- Boot: the 68000 writes the image byte by byte into the DSP boot RAM through the Audio FPGA
  (`setup.AddrLo/Hi/Data`, auto-increment, `ACF_RWenbl` holds the DSP in reset). It
  verifies by reading back, then sets `ACF_DSPrun` and sends a NOP (`$60`) as an alive test
  (`F/src/hard.c:677-760`, `F/inc/chips.h:356-404`) **[H]**.
- 68000 -> DSP interface: write DATAHI, DATALO, then CMD; wait for `CMD` bit 0 (ACK) to
  toggle, ~0.5 s timeout (`F/src/Audio.c:1298-1360`) **[H]**. Commands
  (`F/inc/Audio.h:53-86`) **[H]**:

  | code | command |
  |---|---|
  | `$00` | NOP |
  | `$10` | RESDMA |
  | `$20+reg+ch` | channel register: VOLUME `$00`, PAN `$08`, RATE `$10` (ch 0-7) |
  | `$40+n` | global: MODE (0 play / 1 rec), MUTING (L b0, R b1) |
  | `$50` | WRCODEC (reg<<8 \| data) |
  | `$60` | RDCODEC (status in DATAHI) |
  | `$70` | READMETERS (8-bit L/R peaks, DSP v48+) |
  | `$80+ch` | DMAREAD n blocks |
  | `$90+ch` | DMAWRITE |

- **AD1847 setup from the host** (`CODEC_cmd`, `F/src/Audio.c:225-353`) **[H]**:
  - CR0/CR1 = input source << 6 | (gain-1) & 15. Gain 0 means mute via the DSP MUTING bits.
    Sources (`A/inc/flyer.h:494-496`): 0 RCA line-in on the card's rear, 1 AUX1 header
    JP19, 2 LINE2 header JP1.
  - CR2/3 = AUX1 mix and CR4/5 = AUX2 mix: 5-bit gain/attenuation, `$80` = mute (analog
    mix into the codec output).
  - Overrange via RDCODEC, cleared with `$8F00`; 8-bit meters via READMETERS.
  - Defaults: line 1, gain index 8 (+12 dB) (`F/inc/Audio.h:35-36`, `F/src/Audio.c:218`).
- **Sample rate.** 735 samples per channel per field (DSP constant; `MONOAUDFIELD = 3`
  blocks = 768 slots; `CF` §4) **[H]**. The audio synthesizer runs at 84,587,402 Hz
  (`F/src/main.c:131`) = 1920 x 44,055.94 (10 Hz synthesizer resolution) **[H value,
  M ratio]**. It is referenced to the genlocked 14.318 MHz, so audio is locked to video at
  exactly 735 samples per field, **fs = 735 x 60000/1001 = 44,055.94 Hz** **[M]**. The
  AD1847 is probably clocked at 84.587/5 = 16.917 MHz with its /384 divider **[L]**.

### 4.2 Data flow

- **Play**:
  1. Each clip frame (CFRM) holds per mono channel 4 fields x 3 blocks of 16-bit
     big-endian samples, channel-major (`CF` §4).
  2. The firmware DMAs one field's 3 blocks per active channel from DRAM to the DSP
     (`BeginAudioDMA`/`AudioDeviceProc`, DMA channel 3, `DMAREAD+ch`; `F/src/Audio.c:1506-1714`).
  3. The DSP mixes **8 mono channels** (`NUMAUDCHANS 8`), each with volume, pan and ramp,
     into the stereo DAC of the AD1847 -> **L/R Audio Out RCA** **[H mix/channels, M jack]**.
  4. Playback allocates channels in stereo pairs. PLAYSEQ sets up 4 stereo tracks
     (`F/src/seq.c:978-979`) **[H]**.
- **Record**: AD1847 ADC (source per CR0/1) -> DSP -> DMAWRITE -> DRAM -> clip (AUDRECCHAN = 1;
  `F/inc/Audio.h:35`) **[H]**. Whether the DSP passes the input through to the output while
  recording is **[?]**.
- **Live inputs vs clip audio**:
  - The AUX1/AUX2 headers are mixed analogically into the output by the codec (CR2-5).
  - The RCA line-in only reaches the output through the DSP **[?]**.
  - The Toaster 4000 has no audio. The Flyer's audio out is the system's audio out.

### 4.3 Levels, pan, fades

- Each `DSPevent {time, chan 1-8, volume, pan, rate}` is sent at top of field when
  `clock+1 >= time` (one field of DSP lead) (`F/src/Audio.c:1564-1636`) **[H]**.
  - **Pan**: the API's signed WORD (-`$8000` = left, `$7FFF` = right) is XOR'd with `$8000`
    into the DSP's "0 = left, FFFF = right" (`:1627`; `E/src/envelope.c:219-220`) **[H]**.
  - **Volume**: UWORD, `$FFFF` = 100% in the Editor UI (`E/src/panfunctions.c:1012`) **[M]**.
    Linear gain is **[L]**.
  - **Rate**: ramp length in fields (VolAttack/VolDecay) **[H]**.
- **Per clip** (`MakeDSPevents`, `F/src/seq.c:2602-2668`) **[H]**:
  - at gotime, ramp to VolSust1/2 and Pan1/2 over VolAttack fields;
  - at `gotime + numflds - VolDecay`, ramp to 0 over VolDecay fields.
  - With `CAF_AUDENV` the ramps are replaced by AUDEKEY key frames (time, duration, vol1/2,
    pan1/2) on the last clip's channels (`:2672-2700`).
- Stereo/mono rules (`F/src/seq.c:1360-1425`) **[H]**:
  - L+R requested from a mono clip plays L only;
  - R-only from an audio-only stereo clip plays as stereo with L muted;
  - a clip with no audio drops the audio flags.
- Audio-only events can lead or trail the video (`frontaudio`/`backaudio` split events)
  (`F/src/seq.c:1525-1630`) **[H]**.

---------------------------------------------------------------------------

## 5. Storage

### 5.1 SCSI buses

- Three AM53CF96 = three independent SCSI buses, chip n at `$A10000 + n*$200`, IRQ bits
  SCSI0-2, DMA channels 5-7 **[H]**.
- Bus n = AmigaDOS device `F{A,B,C}u:` (`CHAN=n`). Drive number = `bus*8 + target`
  (`Drv2Chan(drv) = drv>>3`, `F/src/seq.c:118`; the Editor's error text "F%c%d"
  `E/src/sequence.c:3352`) **[H]**.
- The roles are **symmetric in the firmware**. Whether a drive can serve video or audio is
  the ROOT flags `FVIF_VIDEOREADY/AUDIOREADY`, set when FlyerHDTools formats it. Playing
  video from a non-video drive fails with `DRIVEINCAPABLE` (`F/src/seq.c:509-515`) **[H]**.
- One SCSI semaphore per bus. Overlapping video events on one bus are resolved at ENDSEQ by
  copying "heads" to another drive, or fail with `NO_BROLLDRIVE`/`FULL`
  (`F/src/seq.c:2031-2330`) **[M]**. Typical use is A-roll on FA, B-roll on FB, audio/data
  on FC **[L]**.

### 5.2 Flyer filesystem

- The on-disk format (ROOT/FILE/FDIR headers, hash 100, one contiguous extent per file,
  freelist, head lists, chip-patch area) is documented in `CF` §2.1 from `F/inc/filesys.h`
  and `F/src/FILESYS.C` **[H]**.
- AmigaDOS sees it only through flyer.library file operations:
  - FlyerFileSystem 1.05 (`A/src/flyerfs1.c`, `A/src/FlyerFS3.c`) maps DOS packets to
    LOCATE/DIRLIST/FILEOPEN/FILEREAD/...; locks = grips.
  - Grips are opaque 32-bit values: Flyer-RAM pointers to `F_GRIP`; 0 = root
    (`A/inc/flyerprivate.i:371-396`, `F/src/FILESYS.C:3596-3605`) **[H]**.
  - `id_NumBlocks` etc. come from DRIVEINFO (`A/src/flyerfs1.c:580-600`) **[H]**.
- flyerscsi.device is a raw SCSI pass-through for HD tools (SCSIDIRECT) and plays no part
  in the filesystem (`A/src/SCSIDev.asm` header) **[H]**.
- Raw LBAs leak to the Amiga in three places **[H]**:
  1. ClipAction StartBlk/EndBlk and ClipInfo Start/IndexBlk/EndBlk;
  2. READ10/WRITE10 by LBA in the software-VTASC field path;
  3. COPYDATA, DRIVEINFO block counts, and the HD tools.

  Any HLE storage model must therefore expose a **consistent block address space per
  drive**.

---------------------------------------------------------------------------

## 6. Recommendation: HLE

### 6.1 Why HLE

| | HLE (mailbox in Rust) | LLE (second 68000 running Flyer.bin) |
|---|---|---|
| Missing pieces | none | the **boot EPROM** (vectors, `$0400` version, `$0420+` services, ColdBoot, download loop) is not in the release; the **HC05 genlock firmware** is absent; **no DSP source** (needs an ADSP-2115 core + AD1847 + SPORT model) |
| Chips to model | none | 3x AM53CF96 (Copperline has only WD33C93 `C/src/scsi.rs` and NCR 53C710 `C/src/a4091.rs`), 3Port, DMA FPGA, Timer/Router field and line IRQs, VTASC coder FPGAs with DMA pacing and status, Skew/Aligner/FIR, DUART, EEPROM, clock gen |
| CPU | — | `m68k::CpuCore::new()` is a plain struct over any `AddressBus` (`C/src/disasm.rs:164`, `C/crates/cputest-runner`), so a second instance is feasible **[M]** |
| Storage | host folders + virtual LBAs | real Flyer-formatted SCSI images |
| Effort | ~3-4 weeks to useful | 2-4 months; timing-sensitive, high risk |

Nothing makes HLE impossible **[M]**:

- The Amiga talks to the card only through the SRAM mailbox and a few fixed SRAM areas:
  SharedCtrl, the `$17C00` VTASC buffer, the code area.
- It never touches Flyer registers except through SBUSW/SBUSR/CPUW/CPUR/CALLMOD, which only
  test and calibration tools use.
- Caveats:
  - A consistent virtual LBA space is required (§5.2).
  - Two handshakes need sub-millisecond service with Amiga interrupts off (§2.6).
  - CALLMOD (downloaded 68k test modules) cannot be supported.
  - FlyerHDTools, Drive2File and RecoverFiles expect real SCSI/FS block contents. Serve
    them synthesized blocks or errors.
  - Firmware quirks (heads, drive arbitration) are replaced by "always possible"; this is
    harmless.

### 6.2 Copperline integration points

- **Board**: a new `BoardDevice::Flyer` implementing `ZorroDevice`
  (`C/src/zorro_device.rs:455-502`), plus a `BoardSpec` with Zorro II, 256 KB,
  manufacturer 2191, product 0, I/O space.
  - `read`/`write` back a 256 KB `Vec<u8>`.
  - The `write` hook watches `$500-$5FF`: an opcode word whose status bits become `01`
    starts a command (the Amiga writes it last); cont-byte writes on active slots mean
    abort/pause/CACHETEST.
  - `tick(cck)` advances long-running jobs.
  - Savestate: add the variant to `C/src/zorro_device/state.rs` (the enum is tagged).
- **Field timing**: `DeviceHost` exposes no beam (`C/src/zorro_device.rs:199-228`). Add a
  shared `FlyerLink` (Arc<Mutex<..>>) that the bus calls at each field boundary, where it
  already services the video slot (`C/src/bus.rs` ~6760-6790). Tick the field clock at a
  fixed line, ~105 = 40% (§3.3); keep `% 4 == 0` on LOF + colour-frame field I.
- **Video into the Toaster**:
  - `Toaster::source_picture` takes `Mux::Vid(n)` from `video.input_picture(n)`
    (`C/src/toaster/mod.rs:800`). For n = 2/3, when `FlyerLink.mux3/mux4` is set, take
    Flyer channel A/B's frame instead. This covers program, preview, key and the IS mux.
  - The Toaster publishes per frame (`C/src/toaster/video.rs:176`), so the Flyer delivers
    one woven 752x480 frame per Amiga frame: fields I/II or III/IV, decoded with
    `flyerclip::Clip::decode_field` and converted with `C/src/toaster/composite.rs`
    (`decode_single`/`decode_line`), as `C/src/flyer_convert.rs` already does for export.
  - Preview replacement (TOASTMUX preview = 1) only matters with a TBC model; ignore it.
- **Record sources**: FlyerLink exposes the Toaster's last Program and Preview frames and
  the input 1/2 pictures. INPUTSEL picks one; camcorder maps to a host folder, or to
  input 1.
- **Audio out**: mix at 44,055.94 Hz, resample to `MIX_SAMPLE_RATE` 44,100
  (`C/src/audio/mod.rs:34`). Add it to the Toaster's program sound before `publish`
  (`C/src/toaster/video.rs:237-268`), so it reaches the host speakers through
  `take_audio` and the Program recordings. Flyer audio is independent of the video
  switching (a separate physical output). When the Flyer drives VID3/VID4, those inputs'
  host-folder sound should go silent.
- **Host storage**: `C/src/toaster/media.rs:84-123` already creates `Amiga Toaster Files/Flyer`.
  - Use subfolders `FA0`, `FB0`, `FC0` (a flat `Flyer/` = FA0) with the `CF` §2.3 layout:
    one file per clip, verbatim CLIP...tail bytes, plus `.flyervol`/`.flymeta` sidecars.
  - Default volume names `FlyerA0`, `FlyerB0`, `FlyerC0`; flags video+audio ready.
  - At mount, give each file a contiguous virtual LBA range with slack after it; map
    READ10/WRITE10 to file offsets.
  - Non-clip files (the switcher's `.i` icon files, project data) are ordinary files.
  - Grips: nonzero opaque handles; 0 = root.
- **DRAM**: a 4 MB `Vec<u8>` addressed in 512-byte blocks, for READ10/WRITE10 and CPUDMA
  only. The VTASCSRAM window is ordinary SRAM.
- **Determinism**: no wall clock. Host file I/O is synchronous, or prefetched
  deterministically per field.

### 6.3 Phased plan

**Phase 0: card present (0.5-1 day).**
- Zorro board, SRAM window, slot state machine.
- Unknown opcodes complete with `ERR_OKAY` and a log line, not BADCOMMAND, so init never
  stalls on a missed command.
- SharedCtrl, the guru area left zero.
- Test: `flyer.library` opens (FindConfigDev succeeds); `FlyerInit` reaches `Firmware`.

**Phase 1: FlyerInit succeeds, the switcher starts with the Flyer (1-2 days).**

| opcode | HLE behaviour |
|---|---|
| FIRMWARE | set "running"; DONE |
| EEREAD | EEPROM table: `NVOL_MAJIC = $BEEF`; serial lo/hi giving a 1C-range serial (e.g. 4,000,000); calibration defaults; `F/inc/flyer.h:163-184` |
| QUIT | DONE, then reset HLE state once the slot is cleared |
| CACHETEST | §2.6 sequence |
| PGMFPGA, SETCLOCKGEN, DSPBOOT, FLOOBY | accept |
| NOMODE/PLAYMODE/RECMODE | mode state |
| GETCLOCK | write the field clock at `+4` |
| SETCLOCK | store the date |
| TOASTMUX, INPUTSEL, TERMINATE | store |
| TBC | Status = 0 |
| READCALIB/WRITECALIB, GETSETOPT, DEBUGFLAGS, VIDPARAM, RESETFLYER | store/accept |

Test: `FlyerInit` prints "Okay"; the switcher shows no "Unable to initialize the Flyer";
LED patterns end at "Completely configured".

**Phase 2: the Toaster sees the Flyer drive and its clips (4-6 days).**
- Host-folder filesystem with virtual LBAs:
  - DRIVEINFO (SELTIMEOUT for missing drives);
  - LOCATE, DIRLIST, PARENT, COPYGRIP, FREEGRIP, FILEINFO;
  - FILEOPEN/CLOSE/SEEK/READ/WRITE;
  - CREATEDIR, DELETE, RENAME, RENAMEDISK, SETBITS/DATE/COMMENT, WRITEPROT, MAKEFILE;
  - FILEEXTEND/HSREAD/HSWRITE;
  - FINDDRV/SCSIINIT;
  - DEFRAG = ok.
- The field-read path: LOCATEFLD (CFRM LBA of `field & ~3`), GETCOMPINFO (from the CFRM
  field record: DataStart/End = absolute block range, tolerance, seed, rnd freq, FIR set,
  algo), READ10/WRITE10 (virtual LBA <-> DRAM), CPUDMA (DRAM <-> SRAM), FIRXCHG read
  (presets from `flyerclip::vtasc::fir_preset`).
- Test: FA0: shows as `FlyerA0:` on Workbench; `dir`/`copy` work; the Editor/Grazer lists
  clips with thumbnails (library software VTASC decodes our blocks); GetClipInfo returns
  correct field counts.

**Phase 3: playing clips into the Toaster (4-6 days).**
- `FlyerLink` field clock and VID3/VID4 substitution.
- FLYPLAY (Play1Clip semantics):
  - validate the channel/drive/in-out points (BADPARAM), WRONGMODE unless PLAY;
  - `cont = 2` at once; start at `(GoClock or clock+20) & ~3`;
  - cont = 0 aborts (matte), `$FF`/1 pauses/resumes;
  - fill LastFieldDone/StartBlk;
  - hold the last field, or matte with USEMATTE;
  - STEAL/KILL permissions: the newest wins on its channel.
- NEWSEQ/ADDSEQCLIP/ENDSEQ (progress at `+28` -> DONE)/PLAYSEQ: per-channel event lists
  offset by basetime; SharedCtrl.Done = 1 when every track is idle; SeqError/UserID/MoreInfo
  on failures (`F/src/seq.c:966-1090,3242-3250`, `F/src/amishar.c:2558-2570`).
- HEADLIST/MAKEHEAD/VOID*: accept. RESETFLYER: stop all.
- Test: an Editor project with two clips on A/B rolls and a dissolve plays in sync with the
  switcher events; a switcher crouton TAKE shows the clip on Program.

**Phase 4: audio reaches the output (2-3 days).**
- 8-channel DSP model: per channel a target volume/pan and a linear ramp over `rate`
  fields, applied at the top of field with one field of lead.
- Clip audio is pulled per field from the clip's CFRM (`flyerclip` `Clip::audio`/chunk
  offsets).
- Mix: L += s·v·(1-p), R += s·v·p, where p = pan/65535 and v = vol/65535 **[L: pan law]**.
- Mute bits; output at 44,055.94 Hz, resampled to 44.1 kHz into Toaster program sound /
  host output.
- CHGAUDIO, AUDEKEY/AddAudEnv.
- CODECCTRL: store gain/source/mix; meters from the record source's peak.
- Test: sequence audio fades per attack/decay; pan follows the Editor's balance; the Program
  recording carries the sound.

**Phase 5 (optional, 5-8 days).**
- FLYRECORD from the INPUTSEL source using `flyerclip::ClipWriter` (std/HQ5/HQ6 grade per
  GETSETOPT/VIDPARAM), audio from Toaster program sound or a host "Flyer Audio In" folder;
  pause/abort.
- SRCHARGS/DOSEARCH (jog/shuttle with audio scrub).
- Cut lists (NEWCUTLIST/ADDSUBCLIP/MAKESUBCLIPS).
- OPENWRITEFLD/CLOSEFIELD (CG/Paint/LightWave saving stills to the Flyer).
- COPYCLIP/STRIPAUDIO/COPYDATA.
- Live capture (GETCOMPINFO lba 0).
- SCSIDIRECT/READTEST/WRITETEST (synthetic), SMPTE.

**Total to "clip with audio plays through the Toaster from the Editor": about 2.5-3.5 weeks
(Phases 0-4).**

### 6.4 Minimum command subset per milestone

| Milestone | Opcodes |
|---|---|
| FlyerInit succeeds | 01 FIRMWARE, 57 EEREAD, 48 QUIT, 68 CACHETEST, 03 PGMFPGA, 59 SETCLOCKGEN, 0C DSPBOOT, 45 FLOOBY, 11 NOMODE, 0F PLAYMODE |
| Toaster/switcher startup | + 12 TOASTMUX, 13 INPUTSEL, 52 SETCLOCK, 5D TBC, 08 GETCLOCK, 55 READCALIB (tweak panel), 66 GETSETOPT |
| Sees the drive | + 29 DRIVEINFO, 2A LOCATE, 30 DIRLIST, 2B FREEGRIP, 2C COPYGRIP, 2E PARENT, 31 FILEINFO, 32-35 FILEOPEN/CLOSE/SEEK/READ (+36-40 for writes) |
| Thumbnails/stills | + 67 LOCATEFLD, 51 GETCOMPINFO, 27 READ10, 0D CPUDMA, 0A FIRXCHG |
| Play a clip | + 17 FLYPLAY, 58 RESETFLYER, 1B-1F head-list ops; for sequences 62-65 NEWSEQ/ADDSEQCLIP/ENDSEQ/PLAYSEQ + SharedCtrl |
| Audio out | + DSP model, 19 CHGAUDIO, 20 AUDEKEY, 61 CODECCTRL |

### 6.5 Open questions

1. Zorro window size and er_Type flags (256 KB inferred) **[M]**.
2. Pipeline latency from a Flyer field to the Toaster input, and from DSP output to video
   **[?]**.
3. DSP volume curve and pan law (binary only; disassembling the 686 words would settle it)
   **[L]**.
4. Whether record mode monitors the input at the output **[?]**.
5. `Flyer_DefaultSelect`/`Flyer_Play` arguments: the switcher's `Flyer.a` and
   `CroutonHandler.a` sources are missing; only objects remain
   (`<vt-src>/old/switcher/OBJ/Flyer.o`, `CroutonHandler.o`) **[?]**. HLE below
   flyer.library does not depend on them.
6. The ribbon pinout and the function of the Flyer's own RCA video inputs **[?]**.
