# Video Toaster 4000 ↔ Amiga interface (recovered)

Recovered from NewTek's released switcher source (`AmigaVideoToasterComplete.zip`:
`switcher/SRC/*.a`), the compiled objects in `open-video-toaster-src/switcher.zip`
(`OBJ/vthand.AA_o`, `OBJ/XProgram.AA_o`, `OBJ/grab.o`), the includes
(`INC/elh.i`, `INC/vthand.i`, `INC/hardsets.i`), and the debug-symbol
Toasterless build `EXE/toaster/Switcher.main.AA` (6,073 symbols). AA = the
A4000/AGA build, which is what the 4.2 software runs.

Status: derived from code, not yet checked against hardware. Items marked
**open** need the real card or a run of the real switcher to settle.

## 1. Physical channels

| Direction | Channel | Notes |
|---|---|---|
| Amiga → card | Denise/Lisa digital RGB on the video slot | Everything: FPGA bitstreams, control registers, framestore writes, read requests |
| Card → Amiga | CIA-A port B, `$BFE101` | Switcher sets DDRB (`$BFE301`) to input, polls with interrupts and DMA mostly off |
| Card → Amiga | Genlock sync | Software sets `BPLCON0` ERSY and checks the beam still moves |
| Amiga → card | CIA-B PRA bit 2 (`$BFD000`, parallel SEL) | Pulsed once after a successful FPGA load |

The switcher never looks the card up in the expansion list. (`flyer.library`
does find the Flyer by `FindConfigDev` manufacturer `$088F`.)

## 2. The four input lines: R, G, B, I

The card samples four digital lines per pixel. Each is driven by a *pair* of
RGB bits; on AGA these are bits of the **low** nibble of each 8-bit gun
(set with `BPLCON3.LOCT`), on ECS the 12-bit colour nibble.

| Line | RGB bits | Carried by (headers) | Carried by (data bitplanes) |
|---|---|---|---|
| R | R2 / R1 | sprite 0 plane A (`SPR0DATA`) | plane 0 (`$400` = R2) |
| G | B2 / G1 | sprite 0 plane B (`SPR0DATB`) | plane 1 (`$004` = B2) |
| B | R0 / B0 | sprite 1 plane A (`SPR1DATA`) | plane 2 (`$100` = R0) |
| I | G0 / B1 | sprite 1 plane B (`SPR1DATB`) | plane 3 (`$010` = G0) |

Evidence: `NormalSpritePalette` (cop.a: idx1 `$600`, idx2 `$024`, idx4 `$101`,
idx8 `$012`), the SafeWrite low-nibble palette (`$400,$004,$100,$010`), the read
palette `ReadColorMap` (I = G0/B1, B = R0/B0), and the FPGA loader (data1 `$100`,
data2 `$400`, clock `$012`). Names R/G/B/I are NewTek's (`elh.i`
`VTSetUpRedGreen` / `VTSetUpBlueIntensity`).

A pixel's 4-bit value is `R | G<<1 | B<<2 | I<<3` (= bitplane index for data).

## 3. Per-line headers (sprites 0 + 1, attached)

Every control-carrying scan line starts with sprite 0 (+ attached sprite 1)
at `HSTART $5B` (lo-res), sprite lines `$15`–`$105` (`SPRITE0CTRL241 =
$152D0603`, `SPRITE1CTRL241 = $152D0683`). The 16 header bits per line per
input are the sprite data words; NewTek uses bits 15–8 (the first 8 lo-res
pixels).

On AGA the switcher complements sprite 1 in some lists and compensates with
`ComplementedSpritePalette`; the values on the wire are the true ones.

Header words per line: `R` (sprite 0 A), `G` (sprite 0 B), `B` (sprite 1 A),
`I` (sprite 1 B).

- **Line number** `L` (0–239): `B` bits 15–12 = L3–L0, `I` bits 15–12 = L7–L4
  (`ror.l #4 / ror.w #4` encoding in `dswsub`, `FillSprite`). `L ≥ $F0` (I
  nibble `$F`) marks a non-data line: an extended header or a dummy
  (`$F000F000` = "not an ELH, dummy line 255").
- **Extended header select** `E`: `R` bits 15–12. `%1111` = none
  (`VTI_ENONE`); E0 `%1000`, E1 `%0100`, E2 `%1010`, E3 `%0110`, E4 `%1001`,
  E5 `%0101`, E6 `%1011`, E7 `%0111`. E6 has 16 sub-pages E60–E6F in bits 11–8.
- **Field** `F`: `G` bits 13–12. 0 = I, 1 = II, 2 = III, 3 = IV. III/IV are the
  colour-subcarrier-inverted ("complemented") fields.

### 3a. Write lines (framestore write; `DoSafeWrite`/`dswsub`, `tblI`, `tblII`)

`R = $F000`, `G` nibble = `1 h F1 F0`:

| G word | meaning |
|---|---|
| `$C000` / `$D000` | normal, high nibble, field I / II |
| `$8000` / `$9000` | normal, low nibble, field I / II |
| `$E000` / `$F000` | complemented, high nibble, field III / IV |
| `$A000` / `$B000` | complemented, low nibble, field III / IV |

`G` bit 15 = write, bit 14 = high nibble. Four consecutive lines (NH, NL, CH,
CL) make one stored line. Toaster interlaced line number `n` (1–480 usable)
= `2L + 1` for fields I/III, `2L` for fields II/IV (`lsr.w #1,d0 ; bcs field I`).

The data is the line's hi-res pixels: one 4-bit nibble per pixel (plane 0 =
LSB). 736 samples on ECS, 768 on AGA (`bm_BytesPerRow` 92/96). Stored sample
= `NH<<4 | NL` (normal) and `CH<<4 | CL` (complemented). **Bank** (DV1/DV2) is
set through the extended headers before the write (`dswsprchng`) — **open**:
exact field.

Write display: `DIWSTRT $155C`, `DDFSTRT $20`, `BPLCON0 $8204/$9204/$C204`
(hi-res, 1/4 planes, ECSENA), `FMODE 3`, `BPLCON1 $44BB/$44FF`. The sprite
header sits left of the bitmap via the scroll ("put sprite left of bm").

### 3b. Read-request lines (`ReadScanLine`, `ReadPart1/2`, `FillSprite`)

`B/I` = line number `L` (requested framestore line >> 1). `R` bits 14–13 =
`DL`, the pixel within each quad to output (0–3; codes `%0000 %0010 %1100
%1110`). `G` bit 13 = complemented, bit 12 = field II/IV. Reads come in groups
of 8 lines: pixels 0–3 × normal/complemented.

### 3c. Extended Line Headers (ELH)

23 headers: EH0–EH5, EH60–EH6F, EH7, each four words (R, G, B, I), bits
15–8 used. They hold the card's control state: preview/program/key muxes
(`PV`, `LK`, `AM`, `BM`: VID1–4, DAC0/1, ENCODER, MAIN/EXT), mattes
(`MA`, `MB`, `MP`), `CDS` (chroma/lum key), `DP`/`EP` (hue phase), `LKA`/`LKB`,
`PED`, `GAIN`, and flag bits (`CERB`, `LCE`, `NOPAIRS`, `LKNOT`, `BRIDGE`,
`LUT`, `SA`, `LKEC`, `LINP`, `ABSE`, `WERB`, `LINR`, `AFR`, `AFP`, `OBR`,
`TERM1–4`, `USERON`, `MATTE`, `NOSNOW`, `NOKEYINVERT`, `NOKEYSHIFT`,
`NOLOCK`, `PVMUTE`). Full bit map: `INC/elh.i`. They are sent as lines at
the top/bottom of a field (`SendELHList2Toaster`, `MainBlankList = EH3,EH2,EH3`).

## 4. Framestore readback (`ReadScanLine`, `autocal.a`)

While reading, the Amiga displays a 2-plane "read preset" bitmap
(`InitReadScanLine`). Every quad (4 hi-res pixels) carries an 8-bit **quad
number** `Q`: across the quad's 4 pixels, `I` carries Q7–Q4 and `B` carries
Q3–Q0, MSB first (`Bytes2BitMaps` + `InitBytes2BmLUT` store the complement;
`ReadColorMap` inverts it back). Runs of `CaliQuadsPerGrab` quads repeat the
same `Q`.

The card puts framestore byte `[bank][line][4·Q + DL]` (normal or complemented
per the header) on the parallel port while that quad is on screen. The CPU
polls `$BFE101` at times taken from the calibration tables (`HardSets`
`RSLTable7/10`: 7 or 10 grabs per line, `GrabCode` table, timed off
`VHPOSR`), collects the bytes over `CaliGroupsOf8 × 8` lines, then `rearrange`s
them. **Open**: the card's pipeline delay between pixel and port output.

## 5. Startup checks and what makes them pass

From `switcher.a` (blocks under `IFND TOASTERLESS`):

1. `LoadPatches` + `ProgramChips` (XProgram): FPGA bitstreams `vta`/`vti`
   ("TPOG/MOM/XBOD" files) are drawn as bitplanes — clock on `I` (pattern
   `$3333`), data1 on `B`, data2 on `R` — over a stretched frame (CPU rewrites
   `VHPOSW = $2400` on each copper interrupt so DMA keeps streaming). Then
   `BPLCON0 = $0202` (ERSY), two `VHPOSR` reads a timer delay apart: equal ⇒
   "no sync" ⇒ failure. Copperline ignores ERSY, so this passes as-is.
2. `SendELHList2Toaster(elhlistconf)`, `InstallToaster`.
3. `CaliReadScanLine` → `TestReadScanLine`: writes a 736-byte ramp
   (71…200…71…200…71…200…110) to bank 0 line 504 with `SendBytes2Toaster`,
   reads it back with `ReadScanLine`, compares 501 bytes of both the normal
   and complemented copies. Retries other read tables, then a full calibration.
   **Failure here = "Video Toaster not responding"** (error 80).
4. `TestMain2EXT`, `AutoCalibrate` (pedestal/gain/phase from the card's
   digitised inputs), `TestVid1Camera`, AutoHue.

## 6. Emulation notes (Copperline)

- Copperline's framebuffer starts at lo-res `$62` (colour clock `$28`); the
  header at `$5B` and the first data pixels at `$5C` are outside it. The card
  must decode headers and data from the chipset capture (sprite data words,
  palette, bitplane words) rather than from painted pixels.
- The video-slot tap (`src/videoslot.rs`) supplies the parallel-port drive and
  the painted picture for keying/overlay.
- ERSY is not modelled; a genlock is effectively always present.

## 7. Source map

| Topic | Where |
|---|---|
| Startup sequence | `switcher.a` ~1820–2470 |
| FPGA load | `OBJ/XProgram.AA_o` (`ProgramChips`, `FillWithData`, `madelist`) |
| Write | `cop.a` `DoSafeWrite`/`dswsub`/`dswsprchng`, `tblI/tblII`, `SafeWriteCopList1` (in `Switcher.main.AA`) |
| Read | `autocal.a` `ReadScanLine`, `ReadPart1/2`, `InitReadScanLine`, `GrabCode`; `OBJ/grab.o` |
| Calibration | `autocal.a` `CaliReadScanLine`, `TestReadScanLine`, `AutoCalibrate`; `INC/hardsets.i` |
| Headers | `INC/elh.i`, `OBJ/vthand.AA_o` (`SendELH2Toaster`, `ELH2Sprites`, `FillSprite`) |
| ToasterBase vectors | jump table before `DataPool` in `Switcher.main.AA` |
