# Video Toaster 4000: how the switcher drives the Program and Preview outputs

Derived from NewTek's released switcher source (`full/switcher/SRC/*.a`), the
includes (`old/switcher/INC/*.i`), the CG source (`full/CG/src`), the
disassembly of `vthand.AA_o` (`a disassembly, not included`), and data tables
read from the debug-symbol build `EXE/toaster/Switcher.main.AA` (labels given as
`Name@address`). Builds on `toaster-protocol.md`; nothing there is repeated
unless it is needed.

Confidence tags: **[H]** the source states it or the code leaves no other reading.
**[M]** strongly implied by how the code uses it. **[L]** inference or hypothesis.
**[?]** the source is silent.

Names used here:
* `VTSU`: the software's shadow copy of all header words (`TB_VTSetUp`; R/G words
  in `VTSU_RG`, B/I words in `VTSU_BI`, `elh.i:56-157`).
* Bus names: `AM`/`BM` are the A and B sides of the program fader, `PV` is the
  preview mux, `LK` the luma-key source mux, `IS` the framestore-input
  (digitiser) mux.
* Software button masks (`M_*`) and UI names: `M_VIDEO1..4` = $1/$2/$4/$8,
  `M_DV0`=$10 (UI "DV1", bank 0), `M_DV1`=$20 (UI "DV2", bank 1),
  `M_ENCODER`=$40 (UI "BKG"), `M_MONO`=$100, `M_MAIN`=$200, `M_EXTER`=$400.
  These values come from the `From*` constant decoder in `GetFrom`
  (keylogic.a:4548, binary @19b1b0). M_ENCODER = BKG comes from CGSliceCode.a:801
  ("fake a press on BKG overlay button", `%01000000`). **[H]**

---------------------------------------------------------------------------

## 0. The model in one picture

```
 sources: 0 VID1  1 VID2  2 VID3  3 VID4
          4 DAC0  (framestore bank 0 = "DV1"; live-DVE output too)
          5 DAC1  (framestore bank 1 = "DV2")
          6 ENCODER  (MATTE=1: matte generator / BKG; MATTE=0: Amiga RGB encoded)
          7 MONO / latch (see 3.4)

   AM mux ──┐                       PV mux (7 = copy of Program) ──► PREVIEW
            ├─ FADER k ──► PROGRAM          (+ Amiga UI overlay if USERON)
   BM mux ──┘    ▲
                 │ k = f(CDS):  CD     → constant CD (header, 8 bit)
                 │              LUMKEY → luma(LK mux) vs LKA/LKB
                 │              DIB/DIBGR/PIXELSW/QUAF/... → per-pixel Amiga bits
   LK mux ──► luma keyer / MONO bus
   IS mux ──► digitiser ──► framestore record (per-line CERB, bank OBR)
```

There is a single A/B fader in the program path. Every program effect (cut,
fade, wipe, key, DVE) is some choice of AM, BM and the source of the fader
coefficient k (CDS). **[H]** (SetAMandBM buttons.a:1566; ProcessDuring
keylogic.a:4225; cop.a:2487-2515 "which side of the Fader...").

---------------------------------------------------------------------------

## 1. Header word map

Every Toaster line carries 4 words: R = sprite0 DATA, G = sprite0 DATB,
B = sprite1 DATA, I = sprite1 DATB. Bits 15-12 have a fixed meaning on every
line, including extended-header (ELH) lines:

| word | bits 15-12 on every line |
|---|---|
| R | `E`: ELH select (%1111 = not an ELH) |
| G | per-line control: bit 15 `CERB` (record/write enable for this line), bit 14 `LCE`, bits 13-12 `F` (field). Normal lines carry $F (live) or $7 (frozen). Write lines use the same bits as write/nibble/field (`toaster-protocol.md` 3a). |
| B | line number L3-L0 |
| I | line number L7-L4 |

On ELH lines the line-number nibbles are still filled in from the current
sprite-1 source (`ELH2Sprite1`, vthand.txt:469-528), and the G nibble is ORed in
by `ELH2Sprite0`/`Headers2Sprite0` (vthand.txt:596-695). **[H]**

Bits 11-8 of each word are the fields below. The macro that writes each field
names the word, so these positions are **[H]** (elh.i line numbers given).

| EH | R bits 11-8 | G bits 11-8 | B bits 11-8 | I bits 11-8 |
|---|---|---|---|---|
| EH0 (R=%1000) | 11 LINR, 10 AFR, 9 AFP, 8 OBR (693-758) | 11 LKEC, 10 LINP, 9 ABSE, 8 WERB (625-690) | 11 LKNOT, 10 BRIDGE, 9 LUT, 8 SA (556-622) | 8 NOPAIRS = LUST (520-553) |
| EH1 (%0100) | 11 MATTE (846), 10-8 LK (1059) | 11 USERON (829), 10-8 PV (1038) | 11-8 TERM1..TERM4 (761-826, 1017) | unused |
| EH2 (%1010) | 11 AMWIPE (897), 10-8 AM (1122) | 11 ISWIPE (880), 10-8 IS (1101) | 11 BMWIPE (863), 10-8 BM (1080) | unused |
| EH3 (%0110) | MP (1188) | MA (1166) | MB (1144) | 8 NOSNOW (917) |
| EH4 (%1001) | CD bits 3-0 (1234-1262) | CD bits 7-4 | 11 PVMUTE (994), 10-8 CDS (1211) | 10 NOLOCK, 9 NOKEYSHIFT, 8 NOKEYINVERT (937-991) |
| EH5 (%0101) | EP bits 5-2 (1301) | DP bits 5-2 (1266) | 11-10 DP bits 1-0, 9-8 EP bits 1-0 | unused |
| EH6x (%1011) | low nibble of value | high nibble of value | **sub-page x** (`VTI_E6x`=x<<8) | unused |
| EH68 | LKA 3-0 | LKA 7-4 | 8 | |
| EH69 | LKB 3-0 | LKB 7-4 | 9 | |
| EH6A | ~PED 3-0 | ~PED 7-4 (PED is stored inverted, 1401-1431) | $A | |
| EH6B | ~GAIN 3-0 | ~GAIN 7-4 (inverted, 1435-1465) | $B | |
| EH60-67, 6C-6F, EH7 | never defined or sent (`DCB.l 0`, switcher.a:7012, 7035, 7039) | | | |

Note that the EH6 sub-page sits in the **B** word, not in R (switcher.a:7063-7075,
`DC.w VTI_E68` etc.). **[H]**

"Unused" bits are sent as 1 (`VTI_NOTRANS`=$0F00 in I words). The reason is
that an all-zero bit column across R/G/B/I is a transparent sprite pixel, and
the playfield would show through. The elh.i notes at 518, 914, 934, 954 and
977 ("make sure SA, WERB or OBR is set...") are about this, not about
function. **[H]**

Defaults after start-up (VTSetUp, switcher.a:6984-7085, plus InitToaster):

| | R | G | B | I |
|---|---|---|---|---|
| EH0 | $8E00 (LINR AFR AFP) | $0500 (LINP WERB) | $0100 (SA) | $0F00 |
| EH1 | $4800 (MATTE, LK=VID1) | $0000 (PV=VID1) | $0F00 (TERM=%1111) | $0F00 |
| EH2 | $A000 (AM=VID1) | $0000 (IS=VID1) | $0000 (BM=VID1) | $0F00 |
| EH3 | $6000 (MP=0) | $0000 (MA=0) | $0000 (MB=0) | $0F00 (NOSNOW) |
| EH4 | $9F00 | $0F00 (CD=$FF) | $0700 (CDS=CD) | $0F00 |
| EH5 | $5800 | $0800 (EP=DP=32) | $0000 | $0F00 |
| EH68/69 | $B000 | $0800 (LKA=LKB=$80) | $0800/$0900 | $0F00 |
| EH6A/6B | $B000 | $0800 (PED=GAIN=$7F) | $0A00/$0B00 | $0F00 |

---------------------------------------------------------------------------

## 2. Line layout per field, and when header values take effect (Q7)

**Sprite placement** **[H]**: sprites 0+1 are attached, `SPRxPOS/CTL =
$152D/$0603(/$0683)`. That is HSTART $5B, VSTART $15 (line 21), VSTOP $106,
241 lines (lines 21-261). Sprite line index i (0..240) is display line 21+i.
Sprites 2-7 are nulled. The copper lists only set SPR0PT/SPR1PT once per field
(`AVEInterfaceMainI1/II1`, cop.a:2075-2120); they contain no header data.

**Normal switcher display (AVE/AVEI)** is static sprite data in the binary:

| sprite | buffer | every line |
|---|---|---|
| 0 live | `Sprite0LiveA/B` @17ac84/@17b050 | R=$F000 (no ELH), G=$F000 (CERB=1) |
| 0 frozen | `Sprite0FreezeA/B` @17b41c/@17b7e8 | R=$F000, G=$7000 (CERB=0) |
| 1 | `Sprite1Linear` @17e1b4 (TB_CurrentSprite1 default, set in InitEFX) | line i: B=(i&15)<<12, I=(i>>4)<<12, bits 11-8 = 0 |

So outside ELH lines, every line is "no extended header, CERB = live/frozen,
L = sprite line index". `FreezeThawDVE` switches the copper list's sprite-0
pointer between the Live and Freeze buffers (vthand.txt:1670-1733; InitEFX
@121b86 sets up the pointers). **[H]**

**ELH sprite (`SendELH2Toaster`, the common case after almost any button):**
both sprites are replaced by a built sprite (`ELH2Sprites`, vthand.txt:726)
that is patched into the current copper list and stays there until the next
install (`SendSprites2Toaster`, vthand.txt:853). Layout **[H]**:

| line | sprite line | content |
|---|---|---|
| 21 | 0 | EH0. G = EH0G\|$F000, and if frozen ANDed with $7EFF (CERB=0, WERB=0). B/I = line# \| EH0 B/I, and SA cleared if frozen. |
| 22.. | 1.. | each EH in the list, G\|=$7000 or $F000 (CERB per live/frozen) |
| next | | EH0 again, G nibble $7/$F (WERB not cleared) |
| rest | | R=$F000, G=$F000/$7000; B/I copied from TB_CurrentSprite1 (or $F000F000 "dummy line 255" if none) |

The default list is `EH2, EH4, EH1, EH68` (`EHListALL`, elh.i:193-207;
`ehlistall`@10f132 = 03 05 02 0F). So the usual field starts with
`EH0, EH2, EH4, EH1, EH68, EH0` on lines 21-26.

Other lists (bytes decoded from the binary) **[H]**:

| list | contents | used by |
|---|---|---|
| `ehinitlist`@10f860 | EH1 EH2 EH3 EH4 EH5 EH68 EH69 EH6A EH6B | InitToaster, sent 21 times |
| `justeh0`@10ffd4 | EH0 × 241 (whole field) | FreezeThawDVE, going to freeze |
| `live2freezelist`@1100c6 | EH0 EH2 EH4 EH1(CERB high) EH0 EH68 | FreezeThawDVE |
| `freeze2livelist`@1100cd | EH2 EH0 EH4 EH1(CERB high) EH0 EH68 | FreezeThawDVE |
| `elhlist6`@110904 | EH2 EH3 | InstallSBM |
| `elh2` | EH2 | grabs, ChangeIS, CookFreeze |
| `elhclips` | EH68 EH69 | ChangeClips |
| `elhlist6A/6B/6AB`, `elh5list`, `elh56list` | PED/GAIN, EH5, EH1 EH2 EH4 EH5 EH6A EH6B | autocal |
| `MainBlankList` | EH3 EH2 EH3 | frame loading (black matte on main) |

In `Headers2Sprite0` list encoding, $FF = EH0 (with the freeze mask), a
negative number −n = EHn with CERB forced high, and 0 = end.

**Your observation "EH0 R=$8E00 G=$7400 + incrementing L on every line"** is
`justeh0`: 241 lines of EH0 with G = ($0500\|$F000)&$7EFF = $7400 and B/I taken
from Sprite1Linear. FreezeThawDVE sends it for one field when entering freeze,
then sends `live2freezelist` and re-points sprite 0 at Sprite0Freeze. In
steady state the non-ELH lines are R=$F000. **[H]**. An emulator has to accept
an EH0 on any line, and treat EH0 lines as display lines too.

**When values take effect** **[H]**: header values are **latched registers that
persist** and take effect from the line on which they arrive.
* Many headers are sent only on demand: EH3 when the matte changes, EH5 at
  calibration, EH69/6A/6B at init or autocal. They must persist across fields.
* `DoSafeWrite` builds a single field with header blocks *mid-field*: EH3, EH1,
  EH2, EH0 (write mode), then the write lines, then EH0, EH2, EH1, EH3 to
  restore, then interface lines, and the same again for a bottom write
  (`dswsprchng` cop.a:3564, `dswsprrest`, cop.a:2766-2820). The program/preview
  muxing changes for just the write band, so changes must apply from that line
  on.
* elh.i:10-11, 197-199: the order of `EHListALL` was changed "to limit
  flashing at top after FX". The top lines visibly show partial states.
* **NOLOCK** (EH4 I bit 10): `LockToasterELH` (buttons.a:2124) sends a sprite
  holding only EH4 with NOLOCK=0 at shutdown. switcher.a:980-984 says "Since
  EH4 will be the last sent, turning on the LOCK, the final EH0 will never be
  seen". So **NOLOCK=0 freezes the header registers and the card ignores later
  ELHs** until unlocked; mode 0 sends NOLOCK=1. **[M]**
* Clip levels and matte are analog. The software waits a field for big LKA/LKB
  changes (`ChangeClips` keylogic.a:5230) and warns that matte cannot change
  quickly (cop.a:2532-2535). An emulator can ignore this.

**Field numbering**: fields I/III = LOF=1 (long) fields. `InstallFieldIorIII`
waits for LOF=1 (vthand.txt:51-77). **[M]**

**Line numbers**: do not treat L ≥ $F0 as "not data" in general. LUT mode
displays framestore lines with L = 252/253 (`EnterShiftedKey`,
buttons.a:2220-2290), and readback tests line 504 (L=252). $F000F000 (L=255
with R=$F) is the only explicit dummy. Whether a line is an ELH is decided by R
bits 15-12 alone. **[H]**

---------------------------------------------------------------------------

## 3. Mux fields (Q1)

### 3.1 Values (elh.i:449-503) and the software's mask mapping (buttons.a:178-375)

| val | PV (EH1G) → **Preview** | AM (EH2R) / BM (EH2B) → **Program fader A / B** | LK (EH1R) → key source | IS (EH2G) → framestore input |
|---|---|---|---|---|
| 0-3 | VID1-4 | VID1-4 | VID1-4 | VID1-4 |
| 4 | DAC0 | DAC0 | DAC0 | MAIN (program output fed back) |
| 5 | DAC1 | DAC1 | DAC1 | EXT = DAC0 (both equates are 5; used for "MAINFEEDBACK", autocal.a:3238) |
| 6 | ENCODER | ENCODER | ENCODER | ENCODER |
| 7 | MAIN (= Program) | MONO | EXT | MONO |

* **Preview output = PV mux** (plus the UI overlay, 3.3). `CookPrvw` →
  `Mask2PV(PrvwSec)` (buttons.a:1157-1240). An empty preview row gives
  PV=MAIN. **[H]**
* **Program output = fader(AM, BM)**. **[H]**
* **LK** feeds the luma keyer (CDS=LUMKEY) and the MONO bus (3.4). **[M]**
* **IS** chooses what the framestore records (grabs, live DVE, LUT index).
  `Grab1Bank(IS, bank)` (autocal.a:1367). **[H]**

### 3.2 DAC0 / DAC1 **[M-H]**
DAC0 is framestore bank 0 (UI "DV1") and DAC1 is bank 1 (UI "DV2"):
* `Mask2AM`: M_DV0 → DAC0 "LIVE DVE or FROZEN DV0 = DV0", M_DV1 → DAC1
  "FROZEN DV1" (buttons.a:197-200).
* DoSafeWrite needs matte when "writing to bank 0" and AM/BM/PV = DAC0, or bank
  1 and DAC1 (cop.a:3600-3650).
* CG: "CG to go to main is in DV1" → BM=DAC0; "in DV2" → BM=DAC1
  (cgsupport.a:1553-1562). CG preview of the render bank: PV=DAC0 with OBR=0,
  or PV=DAC1 with OBR=1 (CG edita.a:248-263).
* Live DVE (live digitised video) only ever comes out of **DAC0**. DAC1 is
  only ever a frozen bank.

### 3.3 ENCODER, MATTE, USERON, PVMUTE
* **ENCODER with MATTE=1 (EH1R bit 11) is the matte generator** (BKG button,
  MA/MB/MP colour). MATTE is set by default and after every take
  (PEtake keylogic.a:3079-3097, ProcessTakeUnTake 4902). DoSafeWrite shows matte
  with "AM to MATTE" = AM=ENCODER plus MATTE on (cop.a:3720-3790). `IsMatteShown`
  tests `B_ENCODER` (keylogic.a:4111). **[H]**
* **ENCODER with MATTE=0 is the Amiga's own RGB display, encoded.** At shutdown
  ("Put it in genlock mode", switcher.a:958-990): AM=Main, BM=PV=ENCODER,
  MATTE=0, CDS=PIXELSW, which is a classic genlock of the Amiga screen over
  video. AnimFX "Encoder" effects clear MATTE (animfxhandler.a:2685-2700), and
  the `EncoderAlpha` logic keys "encoded graphics" (tags.i:420-421). **[M-H]**
* **USERON** (EH1G bit 11) is the "preview overlay" (2-monitor mode,
  `spB_PrvwOLay` → `TB_UserOn`, pecomm.a:5046-5056; default on, switcher.a:6683).
  It overlays the switcher UI on the **Preview** output. It is always set
  together with **PVMUTE** (EH4B bit 11). USERON and PVMUTE are cleared during
  effects unless `HonorPreviewOverLay` is set (animfxhandler.a:2727-2731), and
  during DIB key fades. **[H]** for usage.
  * Overlay level per pixel = 2 bits on the R and G lines. These are the AA
    "UIO" bits r1 ($200) and g1 ($020) (tags.i:2101-2105,
    `PACO_UIO0..3`=$000/$200/$020/$220). The AA interface palette forces them
    per colour: colour 0 → 0, 1 → $200, 2 → $020, 3 → $220
    (cop.a:3995-4015). The 2-plane default pairs them with grays
    $666/$999/$DDD. On ECS the 2-monitor palette is $000/$842/$7BC/$FFF, i.e.
    r3,g3 = 00/01/10/11 (cop.a:3926). **[M]**
  * Interpretation **[L]**: 0 = preview video shows through, 1-3 = three
    card-generated overlay grey levels. PVMUTE's exact effect is unknown
    (perhaps dims or mutes the preview video under the overlay). **[?]**

### 3.4 Value 7 on AM/BM/IS ("MONO") **[M/L]**
* For IS (and AM/BM in calibration), MONO = **luminance of the LK-selected
  source**. B&W LUT mode sets LK = source, IS = MONO (buttons.a:707-714;
  `FromLutMonoHell` keylogic.a:4623-4633 "use MONO if BW LUT"). Pedestal
  calibration grabs AM=BM=MONO with LK=DAC0 as "analog", then IS=MONO as
  "digital" (autocal.a:1860-1905).
* "Latch" effects set AM (or BM) = 7 **and** AMWIPE (BMWIPE) = 1
  (animfxhandler.a:2709-2716, 4150-4157). See 4.9.

---------------------------------------------------------------------------

## 4. Program output: the fader (Q2)

### 4.1 Fader and CD **[H]**
Program = A·(1−k) + B·k, with A = AM source, B = BM source.
`VTI_CD_SHOWA`=0 shows A and `VTI_CD_SHOWB`=255 shows B (elh.i:505-506).
CD is 8 bits: bits 3-0 in EH4R 11-8, bits 7-4 in EH4G 11-8. Whether the
analog fader is linear in CD is **[?]**; use k = CD/255.

### 4.2 CDS: where k comes from (EH4B bits 10-8)

| CDS | name | k source | used by | conf. |
|---|---|---|---|---|
| 7 | CD | constant CD | idle, cuts, fades, T-bar, AUTO | H |
| 3 | LUMKEY | luma(LK source) against LKA (clip; LKB second level), polarity NOKEYINVERT | luma keys, EncoderAlpha, TDE+key | H for usage |
| 5 | DIB | 2-bit per-pixel code from the Amiga (I,B lines) | non-ANIM wipes (TSEwipe), 1-2 plane alpha keys (CG) | H for usage |
| 4 | DIBGR | 4-bit per-pixel code (I,B,G,R lines), CD also matters (key fade) | 3-4 plane alpha keys, CG key fades | H for usage |
| 1 | PIXELSW | per-pixel genlock ZD (colour-0 or palette T-bit) | shutdown genlock, ANIM "color0/bit15 pixelsw" | M |
| 0 | QUAF | per-quad A/B from Amiga data (digital FX) | TDEon/TDEoff stage, QuafAlpha | M usage, ? meaning |
| 2 | DG = TRANSQUAD | transparent-quad (drop shadow) variant | QuafAlpha FX | L |
| 6 | RGQUAD | ? | not used in source | ? |

Default and idle state: CDS=CD, CD=$FF (SetAMandBM buttons.a:1566-1597;
PEtake keylogic.a:3079-3085). Leaving a key restores CDS=CD, CD=SHOWB,
NOKEYINVERT=1, NOKEYSHIFT=1.

**So does the Amiga's per-pixel output act as the mix value?** Only when CDS is
DIB, DIBGR, PIXELSW, QUAF, TRANSQUAD or RGQUAD. **For cuts, fades, T-bar and
AUTO, CDS=CD and the Amiga pixels are ignored on the program path. Fades are
done by rewriting CD (EH4) every field.** **[H]**

### 4.3 Idle (no transition, no key) **[H]**
`MainOLayClip` → `SetAMandBM(MainSec)`: AM = BM = main source, CDS=CD,
CD=SHOWB, PV = preview source, LINR=1, LINP=1 unless LUT (buttons.a:1305-1597).
So Program = Main and Preview = PV.

### 4.4 TAKE (cut) **[H]**
`PEtake` (keylogic.a:3063-3100; CDS/CD/flags at 3079-3097) sets CDS=CD, CD=SHOWB, NOKEYINVERT/NOKEYSHIFT=1,
MATTE=1, AM/BM/ISWIPE=0, NOPAIRS=1, USERON/PVMUTE per TB_UserOn. Then
`CookTake` (buttons.a:638) swaps the Main and Prvw rows and `MainOLayClip`
gives AM=BM=new main and PV=old main. `ServeAVE_AVEI` sends the headers. A cut
is a single EH2/EH1 change that is visible from the field where it arrives.

### 4.5 AUTO / T-bar fade (the internal "Smooth Fade") **[H]**
* The internal fade uses logic `TSEab` with `DefaultFadeData`
  (switcher.a:5267-5333). Mode `SEF_DOELH1` means "per field, recompute TELH1
  (CD, LKA, LKB) and send the ELHs".
* `HandleSwitcher1` (@18192a) packs the TELH1 results into VTSU: CD → EH4R/G,
  LKA → EH68, LKB → EH69, where negative = ignore. TELH2 (AM BM IS PV LK CDS)
  and TELH3 (MA MB MP) can also be animated per field (`SEF_DOELH2/3`,
  rect.i:283-304).
* CD curve: `TVT_HARMON`, amplitude 127.5, offset 127.5, frequency 0.5, phase
  0.75 (16.16 fixed point). That gives **CD(t) = 127.5·(1 − cos πt)**, an
  S-curve from 0 to 255 over t = 0..1. LKA and LKB are IGNORE. **[M]** for the
  exact shape (fixed-point and phase units inferred), **[H]** for 0 → 255.
* T-bar: TValSec 0..511 / TBarTime 0..$FFFF gives t. Each T-bar movement runs
  the same per-field evaluation (`PEtbar` keylogic.a:3204, `DoTBar`). The CG
  slice's own T-bar fade is simply CD = TVal>>1 (`TBarHandler` CGSliceCode.a:1020-1030).
* AUTO field counts (switcher.a:7456-7464): Variable 60, Slow 75, Medium 45,
  Fast 20 fields. `HandleSwitcher1Auto` uses `TB+$1F0` (60 if 0). The card
  only ever sees a stream of CD values.
* Bus setup through the logic tables (4.6): at the first field, the Prvw source
  is copied to the OLay row. Then AM=Main (outgoing), BM=OLay (incoming),
  PV=Prvw, IS=LiveDVE, CDS unchanged (= CD). At the bottom, `ProcessTake` swaps
  rows and restores AM=BM=new Main, CD=SHOWB (keylogic.a:4787-4925).

The same sequence, written out plainly, appears in CGSliceCode.a:970-1070:
"setup for TBarFade" (BM=Prvw, AM=Main, CDS=CD, CD=SHOWA), then per move
CD=TVal/2, then end (PV=Prvw, AM=BM=Main, CD=SHOWB).

### 4.6 Transition logic tables (decoded from the binary) **[H]**
Each effect has `EKE_KeysTbl[4]` and `EKE_ELHTbl[4]` (offsets for live, frozen,
live+key and frozen+key), flags, and a list of stage boundaries ending at
$FFFF. An ELH table is UnTake, Take, Stage1..n of 6 bytes:
`TST_AM, TST_BM, TST_PV, TST_IS, TST_LK, TST_CDS`. The layout is confirmed by
the source-form tables in cgsupport.a:1850-1940. From-codes: 0 OLay, 1 Main,
2 Prvw, $32 LiveDVE, $46-$4B DAC0-or-row (LUT), $50-$59 constants, anything
else = no change; CDS $FF = keep.

| logic | stage | AM | BM | PV | IS | LK | CDS |
|---|---|---|---|---|---|---|---|
| TSEab (fades) | 1 | Main | OLay | Prvw | LiveDVE | – | keep (CD) |
| TSEwipe (wipes) | 1 | Main | OLay | Prvw | LiveDVE | – | **DIB** |
| TDEon (DVE push-on) | 1 | Main | OLay | Prvw | OLay | OLay | **QUAF** |
| TDEon + key | Take/1 | Main | OLay | Prvw | OLay | OLay | LUMKEY |
| TStoMtoTS (via matte) | 1 / 2 | Main / OLay | OLay / Main | Prvw | LiveDVE | – | keep |
| EncoderAlpha | 1 | Main | ENCODER | Prvw | LiveDVE | ENCODER | LUMKEY |

Take/UnTake rows are AM=BM=Main, PV=Prvw, CDS=CD or keep. Frozen variants use
"DAC0-or-row" (only differs in LUT mode). Script used: `/tmp/vtre/tse.py`.

### 4.7 Wipes **[H]** for setup, **[L]** for levels
Non-ANIM wipes (TSEwipe): AM = outgoing, BM = incoming, CDS = DIB. The Amiga
draws the wipe shape and its per-pixel (I,B) code sets k. Code 0 = A
(background or outgoing). The software warns that non-safe areas need "an RGB
colour that sets the Fader to 0" for AM (cop.a:2487-2491). Code 3 = B (key
colour "ib=11", cgsupport.a:2730-2747). Codes 1 and 2 are intermediate soft-edge
levels; the curve is **[?]**. Suggested start: k = code/3.

### 4.8 DVE ("TDE") **[M]**
DVE output comes out of **DAC0**: the framestore, live-recorded from IS (=
incoming) or a frozen bank. During digital FX **LINP is cleared**
(`OverrideProcessDuring` animfxhandler.a:4100-4115), so the per-line **line numbers in sprite 1**
(DVESprite1, built with `Bytes2Sprite1`) choose which framestore line each
output line shows (vertical geometry). Per-pixel horizontal data comes from the
Amiga bitplanes. `PACO_Mask_Warp`=$0110 = r0, g0, i.e. the B and I lines, with
`PACO_Warp0..3` = $110/$010/$100/$000 (tags.i:2073-2076). CDS=QUAF picks
DVE-image vs background per quad. OBR is set to the bank *not* used as the
frozen source (ProcessDuring keylogic.a:4418-4434). The exact pixel-addressing
scheme was not decoded **[?]**.

### 4.9 Latch effects **[M]** for mechanism, **[L]** for decoding
`LatchAM`/`LatchBM` tags set AMWIPE/BMWIPE=1 and AM/BM=7. Per pixel, the Amiga
colour carries a 3-bit source number in the same 0-7 numbering as AM/BM
(VID1..4, DAC0, DAC1, ENC, MONO) plus the genlock T bit
(`PACO_Mask_Latch`=$80000604; `LatchLUT` animfxhandler.a:4206-4215).
Bit0 = b2 ($004), bit1 = r2 ($400), bit2 = r1 ($200). "Latching is in effect"
blocks row changes mid-transition (keylogic.a:2985-2997). The xxWIPE bits are
cleared after every transition.

### 4.10 Which Amiga bits each per-pixel mode reads (AA low nibbles)

| mode | bits (palette low word) | card lines (protocol pairing) |
|---|---|---|
| write data | r2 $400, b2 $004, r0 $100, g0 $010 | R, G, B, I |
| DIB | b0 $001 = code bit0, b1 $002 = bit1 (`PACO_DIB0..3`) | B, I |
| DIBGR | r1 $200 = bit0, g1 $020 = bit1, b0 = bit2, b1 = bit3 (`PACO_DIBGR0..f`, tags.i:2055-2070) | R, G, B, I |
| UI overlay | r1, g1 | R, G |
| Warp (DVE) | r0, g0 | B, I |
| Latch | b2, r2, r1 + T(bit 31) | G, **R, R** + ZD |
| PIXELSW | genlock ZD: colour-0, or palette T bit when BPLCON2.ZDCTEN. The switcher's BPLCON2=$0624 "ties color bit #31 to pixelsw" (cop.i:121-122, cop.a:4667-4670) | not one of the 4 lines |

ECS equivalents from the `AFXT_Remap_*` code (animfxhandler.a:4661-4690)
**[H]**: R=r3, G=g3, B=b3, I=b0. DIB on ECS = b3/b0 (`PACO_NonAA_Mask_DIB`
$009). Latch on ECS = pixelsw→r3, index→g3, b3, b0.

**Inconsistency to flag:** under the "line = OR of a bit pair" model, the AA
latch codes 3 ($400) and DAC0 ($200) would be identical on the R line. So
either the card sees more than 4 AA bits, or the pairing differs by mode.
**Recommendation:** have Copperline pass the card the full 24-bit pixel value
plus the T/ZD bit, and decode per mode as tabled.

DIB/DIBGR transfer curve **[?]**. Evidence: DIBGR with CD=$FF = key on and
CD=0 = key off (`CUTITONOFF` cgsupport.a:1370-1400). Key fades step CD and the
palettes together. Suggested start: k = (code/15)·(CD/255) for DIBGR and
k = code/3 for DIB.

### 4.11 During framestore writes (DoSafeWrite) **[H]**
cop.a:2528-2536: on write lines, if the bank being written is shown on
program, AM and BM become matte (ENCODER + MATTE, MA=MB=0 black); if it is
shown on preview, PV becomes matte. The headers before the write block set this
and the headers after it restore it (2). Write mode EH0 is LINR=1, AFR=0,
AFP=1, **OBR=bank**, LINP=1, WERB=1, ABSE=0, BRIDGE=1, SA=1 (0 for sync
write), LUT=0, LUST per zipper (cop.a:3796-3820).

---------------------------------------------------------------------------

## 5. Mattes and BKG (Q3)

* MA (EH3G), MB (EH3B), MP (EH3R) are 4 bits each, plus NOSNOW (EH3I bit 8).
* `SetMatteColor(code)` (binary @122dc8, `TB_ToastBGC`) **[H]**:
  * `code & $8000` means **snow** (noise): MP=$F, NOSNOW=0.
  * Otherwise NOSNOW=1, **MA = code bits 11-8, MB = bits 7-4, MP = bits 3-0**,
    and EH3 is sent.
* The BKG colour codes are the `FG_IndexID` of the built-in BGColor croutons
  (`MT_BGColorFGs`@185d10). Same order as `AFXT_Matte_*`, tags.i:651-660.

| colour | code | MA | MB | MP |
|---|---|---|---|---|
| black | $000 | 0 | 0 | 0 |
| white | $BB0 ($FFF if "100% white", animfxhandler.a:2648) | B | B | 0 |
| red | $906 | 9 | 0 | 6 |
| green | $0BA | 0 | B | A |
| blue | $071 | 0 | 7 | 1 |
| yellow | $D51 | D | 5 | 1 |
| magenta | $90A | 9 | 0 | A |
| cyan | $2D6 | 2 | D | 6 |
| snow | $8000 | – | – | (MP=F, NOSNOW=0) |

* Model **[L]**, consistent with all 8 codes: the matte alternates between
  levels MA and MB at the subcarrier with phase MP.
  * luma ≈ (MA+MB)/2, giving yellow 9 > cyan 7.5 > green 5.5 > magenta =
    red 4.5 > blue 3.5. This matches the NTSC bar order.
  * chroma ≈ |MA−MB|/2.
  * hue = MP, about 11.25° per step over 180°. Swapping MA and MB adds 180°:
    complementary pairs share MP (yellow/blue 1, red/cyan 6, green/magenta A).
* Calibration slider presets (adjust.a:2056-2085): red MA=9 MB=0, green MA=0
  MB=$B, blue MA=0 MB=7, with MP = hue slider.
* Default matte is black (`TB_ToastBGC`=0, switcher.a:6730). Matte changes are
  slow in hardware (cop.a:2532).

---------------------------------------------------------------------------

## 6. Luma key, CG overlay, Amiga graphics (Q4)

* **Luma key** (CookOLay buttons.a:959, MainOLayClip 1440-1530) **[H]** setup:
  * AM = Main (background), BM = OLay (fill), LK = OLay (key source), CDS=LUMKEY.
  * If OLay = Main there is no real key (CDS stays CD).
  * Clip: LKA = ClipASec−1 when keying on black, or ClipAMax−ClipASec−1 when
    keying on white (CookOLay buttons.a:984-1000; 0 or max clip short-circuits to FG or
    BG). `VTI_LKA_KEYBLACK_FG=0`, `_BG=255`, `KEYWHITE_FG=255`, `_BG=0`.
  * **NOKEYINVERT**=1 keys on black, 0 keys on white.
  * **NOKEYSHIFT**=1 is a standard key; 0 is the "shifted key". In that mode
    the key passes through a LUT ramp written to bank 0 lines 504-507, shown
    with sprite-1 L=252/253, LUT=1, OBR=1, LINP=0 (`EnterShiftedKey`
    buttons.a:2220-2290; keylogic.a:4398-4416).
  * LKB (EH69) default $80, saved and restored but never set by button logic.
    Probably a second clip or softness. **[?]**
  * Model **[L]**: k = step or ramp of (luma(LK) − LKA), inverted per
    NOKEYINVERT.
  * **LKNOT, LKEC, LCE** are never used by the source. **[?]**
* **ToasterCG in 4.x** **[H]**:
  * The page is *rendered into a framestore bank* (DV1/DV2), with OBR selecting
    the bank (edita.a:248-263).
  * The Amiga displays an alpha bitmap (`TB_CurrentAlphaBM`) whose palette
    holds DIB/DIBGR codes. AM = Main, BM = DAC0/DAC1, CDS = DIB (≤2 planes) or
    DIBGR (3-4 planes) (cgsupport.a:940-966, 1550-1567).
  * Key fade in and out = step CD (and the palette) with CDS=DIBGR
    (cgsupport.a:1370-1400).
* **Amiga RGB over program**: through ENCODER with MATTE=0, either luma-keyed
  (`EncoderAlpha`: BM=LK=ENCODER, CDS=LUMKEY) or pixel-switched (shutdown
  genlock: BM=ENCODER, CDS=PIXELSW; colour-0 or T-bit pixels → A, otherwise
  → B **[M]**).

---------------------------------------------------------------------------

## 7. Framestore display and recording (Q5)

* **Banks**: DAC0 = bank 0 ("DV1"), DAC1 = bank 1 ("DV2"), see 3.2.
* **Line choice for playback**:
  * **LINP=1** ("standard non-transition mode = LINP set unless... LUT",
    buttons.a:1544-1551; ShortOutFader 1990-1997) means linear: the card uses
    its own line counter, and the header L values are ignored for display.
    **[M-H]**
  * **LINP=0** (DVE, LUT, readback) means the line comes from header L. **[H]**
  * Framestore interlaced line n = 2L+1 in fields I/III, 2L in fields II/IV
    (LUT ramp at n=504-507 ↔ L=252/253). **[H]**
* **Field / copy for playback**:
  * **AFP=1** = automatic. **AFP=0** = from the header F bits (G 13-12), used
    by readback (`ReadPart1` autocal.a:602, 668). **[M]**
  * For an RGB emulator: fields I/II use the normal copy and III/IV the
    complemented copy; showing the normal copy on every field gives the right
    picture.
* **Recording**:
  * **CERB** (G bit 15, *per line*) = record or write this line. **[H]**
  * **OBR** (EH0R bit 8) = the bank recorded or written: 0 = bank 0, 1 = bank 1
    (dswsprchng `or.l d4` with `ror.l #8,d4` = bank, cop.a:3820;
    `SetLoadBank` "load into DV0/DV1", buttons.a:1903-1930). **[H]**
  * **Reads to the Amiga come from bank NOT OBR**: `ReadPart1` sets OBR=1 for
    bank 0 (autocal.a:606-611); `SetSaveBank` sets "OBR → save DV0"
    (buttons.a:1944-1959). This closes the open item in the protocol doc.
    **[H]**
  * **LINR=1** = record lines by the card's own counter. **AFR=1** = record
    field automatic; AFR=0 = from header F (write lines set AFR=0). **[M]**
* **Grab** (`Grab1Bank`, autocal.a:1367-1430) **[H]**:
  1. Freeze (ABSE=0).
  2. OBR = bank, IS = source, LINP=LINR=BRIDGE=1. Send EH2 (EH0 always goes
     with any list).
  3. Point sprite 0 at **Sprite0Live (CERB=1 on every line) for 4 fields**.
     That records one full colour frame (fields I-IV) of IS into bank OBR.
  4. Freeze again and restore OBR and IS.
* **Live / freeze** (`VIDEOTYPE_*`, vthand.i:52-59; FreezeThawDVE
  vthand.txt:1670-1733) **[H]** for what the software sets:

| state | CERB (every line) | ABSE | BRIDGE |
|---|---|---|---|
| LIVE | 1 (Sprite0Live) | 1 | 1 |
| FREEZE8 | 0 | 1 | – |
| FREEZE4 | 0 | 0 | 0 |
| MIXED | 1 | 0 | – |

  * Live DVE: the card continuously digitises IS, and DAC0 shows the live
    (framestore-delayed) picture. Changing IS is done 3+ fields before an
    effect starts (`ChangeIS` keylogic.a:5136-5200), so the path has a few
    fields of latency. **[M]**
  * Leaving live with only analog sources shown: Prvw is put on IS and the
    software waits 8 fields "to flood both banks" (CookFreeze
    buttons.a:760-770). So with ABSE=1 recording alternates between both banks
    (FREEZE8 = an 8-field, two-bank freeze). **[M]**
  * BRIDGE's function is **[?]**. It is set for live, writes, reads and grabs,
    and cleared for freeze and key fades.
* **What to show** for emulation:
  * DAC0/DAC1 normally = playback of bank 0/1 at (n from LINP, copy from field).
  * In LIVE (ABSE=1, CERB=1) DAC0 = the IS-selected input, delayed. Pass-through
    is a fine first cut. Also write it into the banks so a later freeze shows
    the last frame.

---------------------------------------------------------------------------

## 8. PED, GAIN, DP/EP, TERM and the rest (Q6)

* **PED** (EH6A) / **GAIN** (EH6B): 8 bits, stored inverted. Pedestal and gain
  of the input digitiser, set by AutoCalibrate (autocal.a:1588-2078). Default
  $7F. They affect what gets *recorded*, not the outputs. **[M]**
* **EP** (External Phase) / **DP** (Digital Phase): 6 bits, EH5.
  `SetExternalPhase`/`SetDigitalPhase` (autocal.a:3193, 3515), with adjust
  texts "Analog (external) video phase" and "Digital (internal) video phase"
  (adjust.a:2513, 2525). Default 32. These are hue trims; ignore them. **[M]**
* **TERM1-4**: 75 Ω termination for inputs 1-4, from prefs
  (`spref_Termination`, pecomm.a:5038-5042). **[H]**
* **NOLOCK**: see section 2 (0 locks the header registers). **[M]**
* **NOPAIRS/LUST**, **SA**, **WERB**: write-path details. SA is cleared for sync
  writes and on the first EH0 when frozen. WERB is set for writes and cleared
  on frozen EH0. They do not affect what is displayed. **[?]**
* **NOSNOW**: 0 replaces the matte with snow. **[H]**

---------------------------------------------------------------------------

## 9. Emulation recipe

```
per line (decode from chipset capture, sprite words as on the wire):
  R,G,B,I = header words;  E = R>>12;  L = (I>>12)<<4 | (B>>12)
  cerb = G bit15
  if !locked and E != 0xF: latch EHn bits 11-8 of R,G,B,I (EH6: page = B bits 11-8)
      if EH4 and NOLOCK==0: locked = true          # [M]
  if cerb and !WERB-write-line: record IS-source line into bank OBR   # live/grab
  (write lines as already implemented: bank = OBR)

per pixel on active lines (card pixel x, 768 wide, 4fsc):
  src(v): VIDn -> input n;  DAC0/1 -> fs[bank0/1][line(n)][x];  ENCODER -> MATTE ? matte(MA,MB,MP,NOSNOW) : encode(amiga_rgb)
          (live DVE: DAC0 -> IS source)
  line(n): LINP ? card line counter : L  -> n = 2L+1 (fields I/III) or 2L
  A = src(AM); B = src(BM)
  k = CDS==CD     ? CD/255
      CDS==LUMKEY ? key(luma(src(LK)), LKA, LKB, NOKEYINVERT)
      CDS==DIB    ? dib(b1,b0)/3          * CD/255?   [L]
      CDS==DIBGR  ? dibgr(b1,b0,g1,r1)/15 * CD/255    [L]
      CDS==PIXELSW? (zd ? 0 : CD/255)                 [M/L]
      QUAF/...    : not specified
  PROGRAM = A*(1-k) + B*k
  PREVIEW = PV==7 ? PROGRAM : src(PV);  if USERON: overlay UI level (r1,g1) [L]
```

To get a first picture: the idle state is AM=BM=Main, CDS=CD, CD=$FF and
LINP=1, so Program = Main and Preview = PV. Frozen DV1/DV2 = banks 0/1 played
linearly.

---------------------------------------------------------------------------

## 10. Open items (need hardware or deeper RE)

1. Composite sample format. The scale is known: sync tip 4, black (7.5 IRE) 71,
   white 200, about 1.407 codes/IRE (`DMIN/DMAX/YOFFSET`, rgb2yiq.i). The 4fsc
   I/Q phase order per pixel is not: `YIQ2Composite`@135282 is object-only.
   yiq2comp.i says "Q from pixels 1,5,9", which hints s[4m]=Y±I,
   s[4m+1]=Y±Q... Calibrate the decoder against
   `NewTek/Framestore/ColorBars`.
2. Exact transfer curves: CD, DIB/DIBGR codes, LUMKEY with LKA/LKB.
3. Meaning of QUAF, TRANSQUAD, RGQUAD, BRIDGE, ABSE (details), LCE, LKEC,
   LKNOT, PVMUTE, LK=EXT, IS=EXT/MAIN feedback.
4. How the AA latch code (r2 vs r1) reaches the card (4.10).
5. What the card outputs on the ELH lines (21-26) and in the header pixel
   columns. Suggest normal picture.
6. DVE pixel addressing (warp/quad data in the bitplanes).

## 11. Source map

| topic | where |
|---|---|
| bit/field map, macros, lists | `old/switcher/INC/elh.i` |
| defaults | switcher.a:6984-7085; shutdown 958-990; fade data 5267-5333; FCounts 7456 |
| mask ↔ mux, cook routines | buttons.a:178-375, 638-1597, 1903-2010, 2124-2300 |
| transitions | keylogic.a PEtake 3063, PEtbar 3204, ProcessDuring 4225, GetFrom 4548, ProcessTakeUnTake 4787, ChangeIS 5136 |
| logic tables | binary `TSEab`@17ff90 … `LatchTDEonEncoderB`@180af6; layout in cgsupport.a:1850-1940 |
| per-field fader update | binary `HandleSwitcher1/TBar/Auto` @18192a/@1818ec/@181826 |
| sprites | binary `SpriteData`@17ac84 (Sprite0Live/Freeze, DVESprite1, VTSprite, ELHSprite, Sprite1Linear @17e1b4); vthand.txt ELH2Sprite0/1, Headers2Sprite0/1, FreezeThawDVE |
| writes / matte in write areas | cop.a:2440-2560, dswsprchng 3564-3900 |
| reads, grabs, calibration | autocal.a ReadPart1 564, Grab1Bank 1367, pedestal 1835-1910, phases 3193/3515 |
| matte | binary `SetMatteColor`@122dc8, `MT_BGColorFGs`@185d10; adjust.a:2056-2085 |
| keys / CG | cgsupport.a:900-1000, 1370-1720, 2700-3120; CG/src/edita.a:215-263 |
| palette bit semantics | tags.i:1990-2140 (PACO_*); cop.i:100-330 |
| AnimFX latch / pixelsw | animfxhandler.a:2625-2731, 3950-3962, 4100-4215, 4640-4690; cop.a:4660-4672 |
