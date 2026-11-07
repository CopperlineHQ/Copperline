# ToasterCG key vs fill alignment on Copperline's Toaster

## Short answer

* **The fill is off, not the key.** Every 768-wide framestore write lands
  **16 card pixels to the left** in Copperline's framestore. That covers
  `FrameLoadNew`, key pages, and anything else that goes through
  `DoSafeWrite` with the zipper on.
* The alpha key from the Amiga is in the right place.
* **Cause:** Copperline ignores EH0 **LUST** ("left justify the write"). On AA,
  `DoSafeWrite` writes a 768-wide bitmap in two passes:
  1. A normal pass with LUST=1.
  2. A **zipper pass** that scrolls the bitmap **16 hi-res pixels left** and
     clears LUST, so the card stores it 16 samples further right.

  Copperline stores the second pass unshifted, over the whole line, so the
  shifted copy is what remains.
* The DIBGR decode is fine.

## 1. Measurements (all local files)

| what | mapping (picture column c, 752-space; c = card pixel − 12) | evidence |
|---|---|---|
| **Fill** (DV bank from file) | **c = file x − 16** | Cross-correlation of p03 and p04 against the Title1/Title2 FRAME0 planes: peak d=16 on both. key6 against LowerThird2 text: 16–17 on every row. |
| **Key** (Amiga alpha) | **c = alpha x** (±1) | s03 screenshot b0/b1 against Title1 alpha planes: screenshot x = alpha x − 4, which is canvas column x+12, card x+12, c = x. p03/p04 key-on mask against file alpha==3: peak d = 0/−1. |
| Authored offset inside the files | alpha ≈ fill **+3** | Left edges of alpha minus fill (>135). Stock LowerThird2 text: +3 (42 of 46 edges). Title1: +2/+3. The CG-made file matches NewTek's own. |
| **Net in the emulator** | key **≈19 px right** of the fill (16 + 3) | Row 318 of p03: the C stroke (file 280–283) is drawn at c=264–267 but the key starts at 283. White shows from 289, which is file 305 (the next stroke). Dark keyed fill shows 450–473: the fill's white ends at 465−16=449, and alpha 3 runs to 474. |
| Side effect | the right-edge garbage stripe in key6 (c ≥ 736) is file pixels **752–767** | Those are past the 752-wide picture. The −16 shift pulls them into view. |

* **DIBGR decode is correct.** The title rows of s03 carry only codes 0, 7, 11
  and 15, which are alpha 0/1/2/3 (r1=g1=1 with b1b0 = 1/2/3) under
  `AAFadeDIB0`. The resulting k = 0, 119, 187, 255 is monotonic. No
  interface-plane bits appear in the title area.
* **The alpha bitmap position is correct.** It sits at card x+12, the same
  origin as a left-justified write bitmap. It is the same in the cal run
  (ui6: band at screenshot 325, file 329).
* **Rows line up.** Picture row = body row + 43, and the fits use that.

## 2. Why: switcher source chain

1. **The load always zippers.** `FrameLoadNew` calls `DoSafeWrite` with
   `moveq #0,d5` (frameslice.a:2979-2981). `KeyPageLoad`/`SendBkBnd` do the
   same (3161-3163). The comment there reads: "D5 is cleared for AA … force
   zipper writing … bitmap needs to be 768 pixels wide".
2. **`DoSafeWrite` makes two passes** (cop.a:2570-2600). It runs
   `dswsub` with d7=0, then d7=1 when the row is over 92 bytes and d5=0.
3. **The zipper pass shifts the Amiga picture and clears LUST.**
   * The Amiga side: `AAbplcon1 (AAbmscroll-32)`, "shift left 16 HIRES
     pixels" (cop.a:2918-2920). The ECS twin adds 2 bytes to the plane
     pointers.
   * The card side: EH0 I `andi.w #~VTF_LUST` "zipper write!"
     (cop.a:3805-3808).
   * The bit: `VTB_LUST` = EH0 I bit 8, the same bit as NOPAIRS.
     old/switcher/INC/elh.i:44 says "LUST = left justify the write".
4. **The zipper area** is bitmap pixels ≥ 732 (the "garbage beyond the 732nd
   pixel" comment), which is framestore quads 186-189 (dve-spec 2.3). With
   LUST=0 the card stores the scrolled picture **16 samples right**, so
   bitmap pixel b still lands at sample b+12.
5. **Copperline's handling.** `store_write_line` (mod.rs:460-500) stores
   sample p from card pixel p whatever LUST says. The zipper pass therefore
   leaves sample p = bitmap pixel p+4, so file x ends up at sample x−4,
   which is c = x−16. This matches the measured fill offset.
6. **Why the read self-test and the `PIXEL0_LORES` calibration are not
   affected.** `TestReadScanLine` writes through `SendBytes2Toaster` with
   `moveq #1,d4 ;no zipper write` (autocal.a), a single LUST=1 pass.

## 3. Fix (minimal, mod.rs + regs.rs)

`src/toaster/regs.rs`, in `impl Regs`, next to `werb`/`lut`:

```rust
    /// LUST, "left justify the write" (EH0 I bit 8; NOPAIRS during
    /// playback; `elh.i`). `DoSafeWrite` clears it for the second, "zipper"
    /// pass of a 768-wide write, which shows the bitmap 16 hi-res pixels
    /// further left (cop.a `dswsub`, 2918-2920 and 3805-3808).
    pub fn lust(&self) -> bool {
        self.eh(0).i & 0x0100 != 0
    }
```

`src/toaster/mod.rs`, next to `PIXEL0_LORES`:

```rust
/// Samples a right-justified write (EH0 `LUST` clear: `DoSafeWrite`'s
/// zipper pass) stores to the right of where the Amiga shows them. The pass
/// scrolls the bitmap 16 hi-res pixels left (BPLCON1 `AAbmscroll-32`; plane
/// pointers +2 bytes on ECS) so its tail (from the 732nd pixel, framestore
/// quads 186-189) falls in the card's write window; the card puts it back.
const ZIPPER_SHIFT: usize = 16;
```

In `store_write_line`, replace the body from `let bank = …` through the
`col` computation:

```rust
        let bank = self.write_bank();
        // Right-justified (zipper) write: card pixel c carries sample c + 16.
        // Card pixels left of the bitmap (quad 0, the header sprite) carry
        // no data.
        let shift = if self.regs().lust() { 0 } else { ZIPPER_SHIFT };
        let x0 = Self::pixel0_column(field);
        let step = field.canvas_scale as i64;
        let store = self.frame_store.line_mut(bank, complemented, line);
        for (p, byte) in store.iter_mut().enumerate() {
            let src = match p.checked_sub(shift) {
                Some(c) if shift == 0 || c >= video::PICTURE_X0 => c,
                _ => continue,
            };
            let Ok(col) = usize::try_from(x0 + src as i64 * step) else {
                continue;
            };
            // … rest unchanged (row.get(col), lines_of, nibble merge)
```

**Why this is safe.**

* The zipper pass now rewrites samples 28..767 with exactly what pass 1
  stored (bitmap p−12), so it is idempotent.
* It does not touch samples < 28. Writing those would put header-sprite
  pixels into picture columns 8–15.
* `LUST=1` lines (every other write, the self-test, and the test helper
  `write_line` with `i: 0x0F00`) behave exactly as before.
* A one-line alternative is to skip `LUST`-clear write lines in `take_line`.
  It gives the same result today, because pass 1 already covers all 768
  samples. The shift is closer to what the hardware does.

**Expected after the fix:**

* Fill at c = x, key at c = x.
* What remains is the files' own +3 px alpha offset, the same on stock and
  CG-made pages. Do not compensate for it. There is no source evidence for a
  key/fill pipeline delay.
* The c ≥ 736 garbage stripe disappears.
* Stills now round-trip in place through `FMSV` (readback: sample 12 = file
  pixel 0) and reload.

**Unit test idea** (mod.rs tests, same style as `write_line`):

```rust
/// One DoSafeWrite pass: EH0 with LUST as given, the bitmap drawn `shift`
/// card pixels left of pixel 12; pixels that would fall left of card 12
/// are not drawn (hidden under the header sprite).
fn write_line_pass(card: &mut Toaster, line: usize, bytes: &[u8], lust: bool, shift: usize)
// EH0 header i = if lust { 0x0F00 } else { 0x0E00 }; then the four NH/NL/CH/CL
// lines with data.card_pixel(12 + k - shift, nibble) for 12 + k >= shift + 12.

#[test]
fn a_zipper_pass_stores_its_line_sixteen_samples_right() {
    let mut card = Toaster::new();
    let bytes: Vec<u8> = (0..756).map(|k| (40 + k % 211) as u8).collect();
    write_line_pass(&mut card, 200, &bytes, true, 0);   // pass 1
    write_line_pass(&mut card, 200, &bytes, false, 16); // zipper pass
    for k in [0, 4, 15, 16, 300, 731, 747, 755] {
        assert_eq!(card.frame_store_byte(0, false, 200, 12 + k), bytes[k], "bitmap px {k}");
    }
}
```

Today this test fails with `bytes[k + 16]`.

## 4. Other per-pixel modes

All of these read the Amiga through the same origin (`amiga_row` /
`pixel0_column` + `PICTURE_X0`), and that origin is right. None of them has
its own key/fill offset. The fix corrects the one thing they share: any DV
bank loaded from a file (`FrameLoadNew`, `LRGB`/`SendRGBPicture`, CG renders
via `DoSafeWrite`) was shown 16 px left, with file pixels 752–767 at the
right edge.

| mode | status |
|---|---|
| **Latch wipes** (AMWIPE/BMWIPE, `latch_codes`) | Codes are aligned. Only file-loaded DV sources were shifted. |
| **DIB wipes / TSE wipes** (`amiga_key` Dib) | Wipe shape is aligned. Same note as latch wipes. |
| **DVE map** (`DveMap::from_field`) | Not affected. It reads warp labels at card quad c and maps output quad c to samples 4Q..4Q+3, so the identity map equals linear playback. Live or grabbed banks (`grab` writes picture column x to sample x+12) were never shifted. A DVE on a file-loaded still was shifted 16 px, and that is fixed. |
