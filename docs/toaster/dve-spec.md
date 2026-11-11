# Video Toaster 4000: digital video effects (DVE / "TDE") for emulation

Scope: every effect that moves or resizes the picture (Trajectory, Push-Pull,
Digital, Blinds, "Crop To Center", the ANIM fly/flip/sphere effects). Builds on
`video-path-spec.md` (§2 line layout, §4.6 logic tables, §7 framestore) and
`toaster-protocol.md` (input lines, card pixels). Nothing there is repeated.

Confidence: **[H]** source states it or code leaves no other reading; **[M]**
strongly implied by how the code uses it; **[L]** inference; **[?]** silent.

Source paths: `S/` = `vt/full/switcher/SRC`, `P/` = `vt/old/switcher/Pre2-4-94/src`
(4.0 versions of `dynam.a`, `eflib.a`, not in the 2004 SRC set), `I/` =
`vt/old/switcher/INC`. `@xxxxxx` = address in `Switcher.main.AA` linked at
$100000 (`re/aa_syms.json`). Effect files: `4000FX/<cat>/<name>` under
`amiga/boot-real/NewTek/Effects`. Helper scripts used here: `vt/dve-tools/`
(`algo.py` decodes ALGO equation tables, `ilbm.py` decodes ANIM DVE frames,
`crud.py` CrUD tags, `vtdis.py` disassembles the AA binary).

---------------------------------------------------------------------------

## 0. The mechanism in one paragraph

The card has no geometry engine. With **LINP = 0** the framestore is read
through an address the Amiga supplies on every line: the **line** comes from
the header line number L (sprite 1, as before), and the **horizontal source**
comes from the bitplanes: every quad (4 card pixels = 1 colour-subcarrier
cycle) carries an 8-bit **source-quad number Q** on the B and I lines, exactly
like the read-back labels. Output at card quad c = framestore quad Q(c) of line
L. Q = $FF is the "transparent digital quad" (QUAF): with CDS = QUAF the fader
shows A there. All scaling, moving, flipping, tiling and mosaic is the
software choosing L per line and Q per quad; algorithmic effects use one row of
Q for the whole rectangle, ANIM effects store a full Q bitmap and an L table
per frame. **[H]** for the software side (sections below), **[M]** for the
card's interpretation (it is the only reading under which the software's
identity frames, read-back and border/key tricks all work).

---------------------------------------------------------------------------

## 1. Vertical: which framestore line each output line shows (Q1)

| item | rule | conf | source |
|---|---|---|---|
| Line select | LINP=0 ⇒ framestore line from header L = `I[15:12]<<4 \| B[15:12]` of **the same display line** | H (software) / M (same-line) | LINP cleared: `P/dynam.a:366-371` (ALGO), `S/animfxhandler.a:4110-4115` (ANIM); identity frames below |
| Sprite-1 words on DVE lines | `B = (L&15)<<12`, `I = (L>>4)<<12`, bits 11-0 = 0 (`B2SNORM`=0) | H | `Bytes2Sprite1`@10eab8; `I/vthand.i:92-96` |
| Sprite line ↔ display line | sprite line y (0..240) = display line 21+y (`SPRITE1CTRL241`) | H | `I/vthand.i:78`; ANIM sprite planes start `$152D0683` |
| Identity | L = y. The last frame of every ANIM "In" effect has L=y for y=2..238 (decoded: Block In, Fly In Left), same as `Sprite1Linear` | H | `dve-tools/ilbm.py` on `4000FX/Digital/Block In` |
| Field / copy | unchanged: AFP=1, field I/III read framestore line 2L+1, II/IV read 2L; III/IV use the complemented copy | M | `video-path-spec.md` §7 |
| **Parity rule** | L ≡ y (mod 2) on every line, always | H | `ScaleCalc`@11a4f4 (+1 to srcAxis if parity differs), `ScaleYOut`@11a862 (odd y: `bset #0`; even y: `+1, bclr #0`); `InitDVESprite1Buff`@10ef54 (pairs even/odd); 100 % of lines in 7 decoded ANIMs |
| Why parity | moving a field line by an even number of lines keeps NTSC chroma phase (227.5 cycles/line), so the stored normal copy is still right | L | inference |
| Background lines | L = 238/239 alternating (y even → 238), with an all-QUAF bitmap row | H | `P/dynam.a:476` ("overscan color"); `I/croutons.i:249` `BGCOLORY 238`; ANIM frame 0 |
| Vertical border lines | L = 252 = framestore lines 504-507, which `RestoreBorderColor`@122fe2 fills with the border YIQ in both banks (`WriteYIQBlock` line $1F8, 4 lines) | H | `I/croutons.i:251`; disasm @123002-123038 |
| Coordinate constants | source lines SYMIN 2..SYMAX 238 ("technically 239"), dest DYMIN 1 ("avoid 1/2 scan line") .. DYMAX 240 ("only on field II") | H | `I/croutons.i:132-139`; hardware hack `P/dynam.a:549-557` |

Header kinds on DVE lines: sprite 0 is `Sprite0Live` (R=$F000, G=$F000: CERB=1,
the emulator's `LineKind::Write` with `write_mode()` false = record) or
`Sprite0Freeze` (G=$7000 → `Dummy`). On the first field of an effect the top
lines carry ELHs (EH4, EH2, EH1, [EH68], EH0; `P/dynam.a:812-819` `elhsets`)
whose B/I nibbles still hold the DVE L (`ELH2Sprite1`). **Every header kind
must yield L; ELH lines are display lines too.** **[H]**

---------------------------------------------------------------------------

## 2. Horizontal: the quad address (Q2)

### 2.1 Encoding on the wire **[H]**
* Per quad, Q is sent MSB first across the quad's 4 pixels: **I line = Q7..Q4,
  B line = Q3..Q0**. `Bytes2BitMaps`@10f50a with `InitBytes2BmLUT`@10ef7e stores
  the *complement* (plane 0 = ~low nibble, plane 1 = ~high nibble), and the DVE
  palette complements back: `InitDVECopList1` low-nibble colours
  color0..3 = $110/$010/$100/$000 (`S/cop.a:434-437`). Same as the read-back
  path (`ReadColorMap`, `S/autocal.a:725`), which the emulator already decodes
  (`quad_label`, mod.rs).
* DVE coplist: 2 planes, hi-res, same DDFSTRT/BPLCON1 as the write and read
  bitmaps (`S/cop.a:489-498`), **bpl1mod = bpl2mod = -96**: one row of Q is
  repeated on every line; copper waits switch between a "null" row (all
  background code) and the effect row (`SetUpDVECopList1`, `S/cop.a:593-742`).
  ANIM effects instead display a full bitmap (different Q per line).

### 2.2 PACO_Warp table (ANIM/ILBM palettes, `I/tags.i:2009, 2073-2076`) **[H]**

| name | colour low nibble | bitplane index in DVE palette | wire B | wire I | contributes Q bits |
|---|---|---|---|---|---|
| `PACO_Warp0` | $110 (r0,g0) | 0 | 1 | 1 | lo=1, hi=1 |
| `PACO_Warp1` | $010 (g0) | 1 | 0 | 1 | lo=0, hi=1 |
| `PACO_Warp2` | $100 (r0) | 2 | 1 | 0 | lo=1, hi=0 |
| `PACO_Warp3` | $000 | 3 | 0 | 0 | lo=0, hi=0 |

So pixel j of a quad carries bit (3-j) of each nibble. ANIM DVE files use
exactly this palette (CMAP $010101/$000100/$010001/$000000, decoded). There is
**no increment/hold/skip code**: it is a direct per-quad address.

### 2.3 Coordinates **[H]** (`I/croutons.i:23-29, 41-76`, `I/rect.i:35-36`)
* Amiga bitmap quad k (k = 0..191) sits at **card quad k+3** (bitmaps start at
  card pixel 12; `toaster-protocol.md`, mod.rs `PIXEL0_LORES`).
* Software identity: `DIGOFFSET = -3`, dest k shows source Q = k+3. Decoded
  last frame of Block In / Fly In Left: Q(k) = k+3 for k = 3..182.
  Read-back uses the same numbering (`InitReadScanLineBMs` starts at quad 3,
  `S/autocal.a:200-203`; `TestReadScanLine` compares byte k with written byte k,
  `S/autocal.a:3041-3100`).
* Hence in card coordinates: **output card quad c shows framestore quad Q(c)**,
  i.e. card pixel 4c+j ← framestore sample 4·Q(c)+j. **[M]** (no extra pipeline
  offset; otherwise every DVE would jump at its first/last field).
* Valid source quads SXMIN 5 .. SXMAX 189 (186-189 = "zipper"); dest 0..183
  (`QUADSWIDE` 184). Quads beyond, and dest k 184-191, carry background codes.
* Left-edge quirk: `ScaleXOut` replaces special codes (>189) in dest quads 0-2
  by the nearest valid address to their right (@11a7e0-11a810); identity frames
  show Q = 5,5,5 in dest quads 0-2. The right-edge twin is dead code. Emulation
  needs nothing for this. **[H]**
* Moving by whole quads keeps chroma phase (4 samples = 1 subcarrier cycle at
  4fsc), which is why addressing is per quad. **[M]**

### 2.4 Special Q values

| Q | used as | DAC output | conf | evidence |
|---|---|---|---|---|
| 0-191 | framestore quad (5-189 used) | samples 4Q..4Q+3 of line | H/M | above |
| 192-231, $F0-$F7 | not used on purpose; stray negative source addresses appear at picture edges (`Split Corners In` frame 2: 242-247 = source −14..−9 in dest quads 0-5) | **black** | L | |
| $E8-$EF (232-239) | hardware colour quads: borders (`_BCPalette`@15bdb8 byte 0), key backgrounds (232 black when keying on black, 233 white when keying on white, `P/dynam.a:443-449`) | constant colour, YIQ below | M | only lines 504-507 are ever written with a border colour, so per-quad borders on picture lines and the 232/233 luma-key backgrounds must be card-generated |
| $F8-$FE (248-254) | TRANSQUAD drop-shadow quads (always on L=238 lines) | **black** where opaque (§3) | L | Fly In Left, Sphere, Split Corners ANIMs |
| $FE | `NULLQUADIN`, "don't record" in geometry-on-input (unused by 4000FX) | n/a | H | `ScaleXIn`@11aab4 |
| $FF | `QUAFBYTE`, transparent quad; all background | **black** if ever shown (TDEtoTDE squeezes show it with CDS=CD) | H use / L colour | `I/rect.i:36` |

`_BCPalette` (border index → quad code, Y I Q; Y on the 71=black..200=white
scale, I/Q signed) **[H]** for the table, **[M]** that the card's colour quads
have these values:

| code | Y | I | Q | colour (TB_BorderC) |
|---|---|---|---|---|
| $E8 | $47 | 0 | 0 | black (0) |
| $E9 | $C8 | 0 | 0 | white (1) |
| $EA | $9D | +31 | −30 | yellow (5) |
| $EB | $8A | −58 | −20 | cyan (7) |
| $EC | $80 | −27 | −51 | green (3) |
| $ED | $6E | +27 | +51 | magenta (6) |
| $EE | $64 | +58 | +20 | red (2, default) |
| $EF | $51 | −31 | +30 | blue (4) |

($E9-$EF in bar order white, yellow, cyan, green, magenta, red, blue: a bar
generator. `croutons.i:252` "WHITE SET 238 ;white quad ???" disagrees and is
marked uncertain by its author.)

### 2.5 Not used by any 4000FX effect (implement later if ever)
* **Pairs** (`NOPAIRS`=0, `DigitalPairs` tag, `HandleDVE2`, `Bytes2BitMapsDVE2`
  @10f58a): a second address stream on planes 3/4 at half-quad resolution.
  `HandleDVE1` always sets NOPAIRS (`P/dynam.a:371`). **[H]**
* **Geometry on input** (`DEB_GEOONINPUT`, LINR cleared, `ScaleXIn/YIn`
  invert the map: the Amiga gives destination quad/line per *incoming* quad/line,
  $FE / L=$FC = don't write). No 4000FX effect sets it. **[H]**

---------------------------------------------------------------------------

## 3. QUAF, TRANSQUAD, RGQUAD (Q3)

Key-mode names (`I/tags.i:622-646`): QUAF = "Digital 280ns" (per quad),
TRANSQUAD = DG = "QUAFalpha" = "2Level70ns" (2 levels per pixel), RGQUAD =
"DRGquad" = "256Level280ns". **[H]** names.

| CDS | bus layout used | k (B weight, 0..255) per pixel | conf |
|---|---|---|---|
| 0 QUAF | A = background, B = DVE (DAC) | `Q == $FF ? 0 : CD` | M |
| 2 TRANSQUAD | A = DVE, B = background | transparent pixel ? `CD` : 0, where transparent = `Q >= $F8 && (Q >> (3-j)) & 1` (j = pixel in quad) | M (polarity, threshold), L (CD scaling) |
| 6 RGQUAD | unused | unknown; perhaps an 8-bit per-quad level on the R/G lines | ? |

Evidence:
* QUAF background code is $FF in every effect (ALGO `backgroundColor` 255, ANIM
  frames); TDEon/TDEoff put the DVE on B (§4). `Expand Fade In` / `Squeeze Fade
  Out` ramp CD 0→255 with CDS=QUAF (TELH1 equation, `DEB_DOELH1`): the DVE fades
  over the background, so CD scales the non-QUAF quads. **[H]** data, **[M]** model.
* Converting TRANSQUAD to QUAF swaps AM and BM ("loose the shadow",
  `S/animfxhandler.a:4128-4146`): opposite polarity. **[H]**
* TRANSQUAD ANIMs draw shadows as codes 248-254 (interior 248 = %1000, edges
  249-254), 255 outside. Reading the B-line bit per pixel as "transparent"
  gives 75 % dither coverage inside, lighter at the edges; the other polarity
  would be nonsense. QUAF ANIMs never contain 248-254. **[M]**
* The opaque shadow pixels show the DAC output for Q=$F8-$FE: assumed black **[L]**.

LUMKEY during a DVE (TDEon/TDEoff "+key" tables): LK = OLay = DAC0, CDS=LUMKEY;
the background is drawn with Q=232 (black) or 233 (white) so it keys out
(`P/dynam.a:443-449`). The emulator's luma keyer must see the *transformed*
DAC0. **[H]**

---------------------------------------------------------------------------

## 4. Per-field sequence, buses and banks (Q4)

Logic tables decoded from the 4.2 binary (layout: `video-path-spec.md` §4.6;
source form `P/eflib.a:1097-1608, 3718-3852`). "During" row, AM/BM/PV/IS/LK/CDS:

| logic (ButtonELHlogic) | live | frozen | rows swapped at stage start |
|---|---|---|---|
| TDEon (2) | Main/OLay/Prvw/OLay/OLay/QUAF | IS unchanged | OLay←Prvw (live DVE) |
| TDEoff (3) | same | same | OLay←Main, Main←Prvw |
| TDEtoTDE (5), 2 stages split at $7FFF | Main/Main/Prvw/Main/Main/CD | | full-screen DVE of Main, sources flip at mid (MIDFLOP) |
| TDtoBtoTD (6), 2 stages | Main/OLay/Prvw/OLay/OLay/QUAF | | matte, then new source |
| TDEfx (19) | Main/Main/Prvw/Main/Main/CD | | full-screen DVE, no take |
| QuafAlphaOn/Off/Temp (17/18/28) | **OLay/Main**/Prvw/OLay/OLay/TRANSQUAD | | as TDEon/TDEoff |

* Row masks for "live DVE" carry DV0|DV1|VIDn: `Mask2BM` → **DAC0**, `Mask2IS` →
  **VIDn** (`S/buttons.a:178-300`, `ISLIVEDVEON` `I/instinct.i:4697`). So during
  TDEon/TDEoff: A = background (analog), B = DAC0 = framestore bank 0 recording
  IS = the moving source, played through the address map. **[H]**
* Frozen variant: OLay row = frozen DV0 or DV1 → BM = DAC0 or DAC1; no recording.
  Apply the address map to whichever DAC is selected. **[M]**
* OBR (`S/keylogic.a:4418-4434`): OBR = 1 if the OLay row's (else Main's)
  frozen source is DV0, else 0 (LUT keeps it). Live DVE: OBR=0, CERB=1 on every
  line, ABSE=1 ⇒ IS recorded into bank 0, DAC0 plays it. **[H]**
* `EKE_LIVEIFANALOGS`: if no row uses a DV source the effect forces LIVE
  (`S/keylogic.a:3390-3415`). `ChangeIS` is called with 2 fields (6 for
  geometry-on-input) at each stage start (`P/dynam.a:744-750`). **[H]**
* Per field (`HandleDVE1Auto/TBar` `P/dynam.a:161-329`, `HandleDVE1` 330,
  `ShowOneDVEFrame` 601): compute t; `ProcessDuring` (stage, buses, OBR); first
  field: init background row/sprite, LINP=0, NOPAIRS=1, ELHs at the top; every
  field: new L table (`Bytes2Sprite1`) and effect row (`Bytes2BitMaps`),
  coplist with interface (AA) installed for the next field. One geometry per
  field. At the end `ProcessTake` restores AM=BM=Main, CDS=CD, LINP=1. **[H]**

---------------------------------------------------------------------------

## 5. Which effects, file formats, animation (Q5)

### 5.1 Census of `4000FX` (558 croutons scanned with `crud.py`) **[H]**
* **ALGO, AlgoFXtype 0 "StandardDVE"**: 93 (TDEon 44, TDEoff 36, TDEtoTDE 7,
  TDtoBtoTD 6): all of Push-Pull, Blinds/Slats (except Split Corners), the
  algorithmic Trajectory (Curving/Drop/Spiral/Snaky/Tumble/Ping Pong/Kicked
  Puppy/Take off/Fly In Top), Digital Expand*/Squeeze*/Stretch*/Zoom/Mosaic*/
  Infinite*/Flip H/V Matte/Fold/Roll/Slide, Wipes/Crop To Center, Wipe Border
  Center. AlgoFXtype 3 "CustomDVE": Special/Positionable Expand/Shrink.
* **ANIM DigitalFX**: QUAF 32 (Block/Diag/Slats/Raster/Grid/Screen Falling/
  TV Shut Off/Peel Fly Away/TwoFall/Spheres Join Up/Screech/Snap Off R/SkyLine,
  Overlays/Glitch* = TDEfx), TRANSQUAD 22 (Trajectory Fly/Flip In/Out, Sphere
  In/Out, Cube Out, Split Corners). DIB+latch ANIMs with DigitalFX (Hand Pull,
  Warp To Globe, Photo Fly Out, Flip Chrome Edge, Flip Out Bottom2, Infinite
  Video Space, Fly Over Crumpled Edge) combine the DVE with a latch/DIB key.
* PagePeel/* are **latch** ANIMs (LatchAM, DIB), not DVE.
* ALGO types 1/4 (StandardWipe/CustomWipe) are DIB wipes, not DVE.

### 5.2 ALGO croutons
File = `FORM ILBM` (icon) + `FORM CrUD`: `TYPE 'ALGO'`, `LIBS` (effects.library
LVO −816 = the crouton-type dispatcher @14a6d4, which sends ALGO to
`AlgoFXHandler`@149154, `AlgoFX.o`), `TAGS`. Tags used: `AlgoFXtype`($63),
`ButtonELHlogic`($07), `Equations` table ($1C), F-count tags ($55-$59). **[H]**

`StandardDVE` = `ProcessEffect(HandleDVE1TBar | HandleDVE1Auto, logic =
EffectsLogic[ButtonELHlogic−1], data = Equations)` (AlgoFX.o disasm). Equations
table (data = table + its first longword, `S/keylogic.a:3355-3371`;
`InitDynamic` `P/dynam.a:1743`):

```
ULONG offset_to_equations (=10)   ULONG Modes1 (DEF_* flags, I/rect.i:342-369)   UWORD NumHor
repeat: UWORD struct_id (0 = end)
        repeat per field in struct order: UWORD eq_type (0 = end of struct), params
struct_id: 1-9 ScaleDataX1-9, 10 ScaleDataY, 11/12 PixelizeX/Y, 13-21 Wipe1DataX1-9,
           22 Wipe1DataY, 23 TELH1 (CD,LKA,LKB), 24 TELH2 (AM,BM,IS,PV,LK,CDS), 25 TELH3,
           26-29 Fader/LumKey/TBarTime/BMposition          (P/eflib.a:363-450)
eq_type:   2 const WORD; 3 linear{init,speed,min,max} 16.16; 4 accel; 5 harmonic; 6 delayed
           linear; 7 events; 8 segments; 9-17 delta*; 18 multiDelta; 19 multiSum; 20 multiSegment
           (sizes: P/dynam.a:1609-1740)
ScaleData field order: borderUpper borderLower position srcMin srcMax srcAxis destMin destMax
           destAxis effectSize effectColor borderColor backgroundColor overscanColor (I/rect.i:39-103)
```
Time t: 0..$10000 per field (`TValSec·$10000/TValMax` for the T-bar, field
count for AUTO); `linear` = init + speed·t, magnitude clamped (`LinearMotion`
@11acf0). MIDFLOP doubles t and reverses after the middle; REVERSE uses 1−t.

Example `Push-Pull/Pull Off L-R` (decoded): X position = 192·t; srcMin 5,
srcMax 189, srcAxis 97, dest 0..183, destAxis 94, size 185, bg $FF; Y identity
(2..238 → 1..240, bg 238). `ScaleCalc`/`ScaleXOut` give, at t = ½: dest
k = 0..97 → $FF, k = 98..183 → Q = k−93. In card pixels: **x ≥ 404 shows
source x−384, x < 404 shows A**.

Per-rectangle row: `Q(k) = int(srcInit + ½ + (k−destInit)·srcInc)`,
srcInc = srcSize/effectSize, plus optional border (`borderColor` replaced by the
global border quad $E8+i at the first field, `P/dynam.a:460-470`) and
background fill (`P/dynam.a:460-472` for the border code); Y: `L(y) = int(...)`,
parity forced, clamped (`ScaleYOut`).
Mosaic/Tile post-process the buffers (`MosaicX/Y`@11b612/@11b6f6,
`TileX/Y`@181dd4/@181e4a).

### 5.3 ANIM DVE croutons **[H]**
* 768×241, 3 planes, ByteRun1 first frame, then NewTek op-7 deltas (ANHD
  `bits`: $40/$50 byte horiz./vert., $00/$10 word, $01/$11 long; interleave 2).
  Format, per plane: signed count (byte for byte mode, word otherwise; 0 = end),
  WORD skip in units, count>0 = literals, count<−1 = run of −count, −1 = skip
  only (`DecompressAnim7`@13d2cc and `Decompress*`@13d5e4-13dcc4). Implemented
  in `dve-tools/ilbm.py`.
* Plane 0/1 + palette = Q per quad (§2.2). **Plane 2 is the sprite-1 DMA image**:
  `bm_Planes[depth−1]` becomes `SSBM_Sprite1` and depth drops by one
  (`S/animfxhandler.a:3893-3906`): long `$152D0683`, then 241 × (DATA, DATB) =
  L per display line.
* Tags: `DigitalFX`=1 (LINP cleared), `KeyMode` 0 (QUAF) or 2 (TRANSQUAD),
  logic 2/3/17/18/19.

---------------------------------------------------------------------------

## 6. Emulation algorithm (Q6)

Inputs per line: the header words (any kind) → L; the line's pixels → Q per
card quad; latched ELH registers; the two banks. Works in the emulator's
existing frame composition (`video.rs`: picture row y ↔ framestore line
fl = y+1 ↔ Amiga line `21 + (fl>>1)`; card pixel = picture x + 12).

```
dve_active = !LINP && !LUT                      // EH0; LUT mode is not a DVE

fn dve_line(vpos) -> (L, [Q; 192]):             // from the field's FieldPixels
    h = header(vpos)                            // ANY kind: ELH, CERB/"Write", Dummy
    L = (h.i >> 12) << 4 | (h.b >> 12)
    for c in 0..192: Q[c] = quad_label(row(vpos), column(card pixel 4c))   // I=hi, B=lo, MSB first
    // no header on the line (outside 21..261): treat every Q as $FF

fn dac_picture_dve(src: picture of bank b or, when live, of the IS input):
    for y in 0..480:
        fl = y + 1;  f = fl & 1                  // 1 = field I/III row
        (L, Q) = dve_line(21 + (fl >> 1))
        sy = 2*L + f - 1                         // source picture row (PICTURE_LINE0 = 1)
        for x in 0..752:
            p = x + 12; c = p >> 2; j = p & 3; q = Q[c]
            out[y][x] = if q < 192:  src[sy][4*q + j - 12]     // black if outside the picture
                        elif $E8 <= q <= $EF: yiq_to_rgb(BCPALETTE[q])
                        else: black
            qmap[y][x] = (q, j)                  // for the key below

fader_key (per pixel, B weight):
    CDS=QUAF      : k = (q == $FF) ? 0 : CD
    CDS=TRANSQUAD : k = (q >= $F8 && (q >> (3-j)) & 1 == 1) ? CD : 0
    CDS=LUMKEY    : as now, but on the transformed LK source
    CDS=CD        : k = CD                       // TDEtoTDE/TDEfx: DAC shows the map incl. black $FF
    (when !dve_active, QUAF/TRANSQUAD keep today's flat CD)
```

Notes:
* Do the remap in decoded RGB space (decode the bank once, then index), so
  chroma phase needs no care. Remapping raw composite samples also works: whole
  quads and the parity rule keep the phase.
* Apply the map in `dac_picture` for both banks and for the live path, so AM, BM,
  PV and LK all see it. On the IS mux (`Role::Is`, feedback) the map does not
  apply: recording is linear (LINR=1) in every 4000FX effect.
* Use the same field's headers for both picture rows of a line pair (the
  emulator composes per frame; the DVE changes every field, so a 1-field lag on
  one parity is harmless).

---------------------------------------------------------------------------

## 7. Tests

In the emulator (switcher 4.2 running):
1. **Identity**: select `Push-Pull/Push On L-R`, T-bar one notch from the bottom,
   then take: the last DVE field and the first linear field must match pixel for
   pixel. Confirms no horizontal or vertical offset.
2. **Pull Off L-R at T-bar ½**: at card pixel p ≥ 404 Program = outgoing
   sample p−384, p < 404 = incoming; every DVE line's decoded row is Q = $FF for
   card quads 3..100, Q = c−96 for card quads 101..186, $FF for 187..194;
   L(y) = y.
3. `Digital/Expand Fade In`: CD rises with t under QUAF (fade plus expand).
4. `Push-Pull/Push On Border L-R`: side borders ($E8+i) match the top/bottom
   border (L=252 rows) for each of the 8 border colours.
5. `Trajectory/Fly In Left` mid-way: picture with a dithered dark shadow below it.
6. Self-check of decoding: `python3 dve-tools/ilbm.py`-based dump (see
   `anim_frames`, `quads_row`) of the ANIM file vs. the Q/L the emulator decodes
   from the Amiga picture on the same frame.

On real hardware (settles the [L] items): capture Program with the T-bar parked
at ½ in (2) (horizontal offset), in (5) (shadow colour and density), and with a
copy of `Push On L-R` whose `backgroundColor` constant (the WORD after type 2
in the X struct, `algo.py` shows it) is changed to 200, 240, 250 or 255 and
`ButtonELHlogic` set to 5 (TDEtoTDE, CDS=CD, so the DAC output for that code is
shown unkeyed): tells what the card outputs for unlisted codes.

---------------------------------------------------------------------------

## 8. Certain vs inferred

| claim | conf |
|---|---|
| Q encoding (I=hi, B=lo, MSB first, true value on the wire), Warp table | H |
| L from header on DVE lines, parity rule, background L=238/239, border L=252 | H |
| Card output at card quad c = framestore quad Q(c), same display line | M |
| Every algorithmic and ANIM DVE reduces to per-line (L, Q row) | H |
| QUAF: Q=$FF shows A, CD scales the DVE | M |
| TRANSQUAD per-pixel transparency for Q ≥ $F8, polarity | M; CD scaling L |
| $E8-$EF colour quads with `_BCPalette` YIQ | M |
| DAC output black for $FF, $F0-$FE, 192-231 | L |
| RGQUAD | ? |
| Same map applied to DAC1 when a frozen DV2 is on the DVE bus | M |
