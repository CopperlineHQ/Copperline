# NewTek Video Toaster Flyer clip format

Container, VTASC video coding and audio of Flyer clips, taken from NewTek's
1994-98 sources (released 2004). A reference implementation is in
`crates/flyerclip` (Rust, no dependencies).

**Path prefixes**
- `A/` = `fly/amiflyer/amiflyer/` (flyer.library and Amiga tools)
- `F/` = `fly/flyer/flyer/` (the Flyer's own 68000 firmware)
- `asm` = `A/src/flyerlibvtasc.asm` rev 1.5 (1995-11-22), the software model of the VTASC chips

**Markers**
- Anything without a marker was read directly from the source cited.
- **INFERRED** marks deductions from the source, not statements in it.
- **QUIRK** marks asm behaviour the crate reproduces or deliberately departs from (all quirks are collected in §3.10).

The VTASC chips are Xilinx bitstreams with no design files, so `asm` is the reference for the algorithm. The firmware does carry one piece of chip-generated data: the matte-black field (§5). It decodes with `asm`'s algorithm and is reproduced bit-for-bit by the crate's encoder. That confirms the bit order, the 760-sample line, the initial state and both predictors against the hardware.

---

## 1. Container

### 1.1 General

| Property | Value | Source |
|---|---|---|
| Unit of all offsets/lengths | 512-byte logical block (`LogBlkSize`, `LOGBLK_SIZE`) | `A/inc/flyerprivate.i:82`, `F/src/vid.c:2684` |
| Byte order | big-endian (68000); structs word-aligned (SAS/C), every header exactly 512 bytes | `A/inc/FlyerClipFmt.h:18-78` (sizes add up to 512) |
| Offsets | always relative: clip offsets from the CLIP block, frame offsets from the CFRM block, index offsets from the TAIL block. A clip is position-independent. | `F/src/clip.c:1111-1112`, `F/src/clip.c:578-583`, `F/src/vid.c:2769-2795` |
| Colour frame | 4 fields (I-IV) = one CFRM chunk; field n is frame n/4, slot n%4 | `F/inc/flyer.h:69` `FIELDSPERFRAME 4`; `F/src/clip.c:578` |
| Max frame size | `VBUFSIZE-1` = 0x33D blocks | `F/inc/flyer.h:122`, `F/inc/Clip.h:43` |

### 1.2 Layout of a recorded clip

This is how `DoRecMachine` writes it (`F/src/vid.c:2280-2725`) and how `ClipWriter` writes it:

```
blk 0            CLIP header
blk 1            CFRM frame 0  -+  header (1 blk)
                                |  audio: AudioChans x 4 fields x AudioLength blks (channel-major)
                                |  video field I, II, III, IV, back to back (VidLength blks each)
blk 1+len0       CFRM frame 1 ...
...
blk DataEnd=Tail TAIL header
  Tail+1         index table, NdxTabSize blks (i32 entries, 128 per blk)
  Tail+1+N       2 HEAD blocks (head list)
  Tail+3+N       40 user blocks (zero)
blk Length       (end of clip)
```

- Video starts at `frame + 1 + 12*AudioChans` (`F/src/vid.c:1453`). Each field begins on the block after the previous one ends; the DMA pointer simply carries on (`F/src/vid.c:1194-1195, 1228`).
- There is no other padding or alignment. A frame's `Length` = 1 + audio + Σ VidLength (`F/src/vid.c:1470`).
- Software-written clips (`OpenWriteField`/`CloseField`) differ:
  - There is a gap between `DataEnd` and `Tail`: a new clip is created with `Tail = 1+MAXFRAMESIZE` (`F/src/clip.c:321`) and grown with `ExtendClip` (`F/src/clip.c:784-884`).
  - Fields not yet written in the last frame are pointed at one shared 48-block matte-black area (`F/src/clip.c:692-727`).
  - `Fields` can be any count, not only a multiple of 4 (`F/src/clip.c:746-747`).
- Skeletal clips (`Tail = 0`, no index) are legal input. `MakeAudClip` writes one and asks the Flyer to "reprocess" it into a full clip (`A/src/MakeAudClip.c:116, 171-226`). Without a tail the frame finder refuses (`F/src/subs.c:332-334`), so cueing and playback need the tail.

### 1.3 ClipHeader ("CLIP", block 0)

From `A/inc/FlyerClipFmt.h:18-41`; firmware names from `F/inc/vid.h:255-277`.

| Off | Type | Name | Meaning / 4.x value |
|---|---|---|---|
| 0 | u32 | ID | `0x434C4950` "CLIP" (`FlyerClipFmt.h:83`) |
| 4 | u32 | Length | whole clip in blocks, incl. tail section. Recorded: `stopaddr-startaddr` (`vid.c:2685`) |
| 8 | u8 | Version | `FLYER_VERSION` = 4 (`F/inc/flyer.h:51`) |
| 9 | u8 | VidCmpVer | `VTASCversion` from the coder chip revision (`F/src/hard.c:331-334`): 1 = old D2, 2 = new D2/SN (`F/inc/vid.h:41-42`). 4.x boards: **2** (INFERRED from software clips `F/src/clip.c:311-314` and `MakeAudClip.c:111`) |
| 10 | u8 | AudVer | `AUDIO_VERSION` = 2 (`F/inc/Audio.h:30`) |
| 11 | u8 | VidGrade | 0 std, 1 HQ5, 2 HQ6 (`F/inc/vid.h:279-281`); see §1.8 |
| 12 | u32 | Fields | number of fields. Recorded = 4 × frames (`vid.c:2690`) |
| 16 | u32 | DataStart | blocks CLIP→first CFRM, always 1 (`vid.c:2691`) |
| 20 | u32 | DataEnd | blocks CLIP→block after last frame (`vid.c:2692`) |
| 24 | u32 | Tail | blocks CLIP→TAIL. Recorded = DataEnd (`vid.c:2693`). 0 = no index (`F/src/FILESYS.C:2932-2935`) |
| 28 | u8 | reserved2 | 0 |
| 29 | u8 | AudioLength | blocks per mono channel per field: 3 if audio, else 0 (`vid.c:2700-2713`; `MONOAUDFIELD` `F/inc/flyer.h:119`). The `FLYERVER3` build used 3×chans; 4.x does not define it (`F/inc/flyer.h:52`) |
| 30 | u8 | VidFlag | 1 = video present (`vid.c:2695-2698`) |
| 31 | u8 | AudioChans | mono channels 0..2 |
| 32 | u8 | Pedestal | `NTSC_BLANK` = 60 (`vid.c:2714`) |
| 33 | u8 | (reserved3) | firmware `ch_FIRset`: FIR preset in use when recorded (`vid.c:2715`) |
| 34 | u16[9] | CustCoefs | custom pre-comp FIR: 8 coefs + scale (`vid.c:2717-2719`, `struct FIRSET` `F/inc/Hard.h:28-31`) |
| 52 | u16[9] | CustInvCoefs | custom post-comp FIR |
| 70 | u16 | LogBlkSize | 512 |
| 72 | u8 | FIRROM | FIR map shape. Never set (0) by firmware writers |
| 73 | u8 | FIRRAM | ditto |
| 74 | u8[438] | reserved4 | 0 |

`DSP_Code/CroutonLib/src/filetypes.c:598-645` uses an older view of this header ("Modula-2 printout"). It only reads ID/VidFlag/AudioChans/Fields: VidFlag with `Fields == 4` is a Flyer **still**, VidFlag with `Fields > 4` is a video clip.

### 1.4 FrameHeader ("CFRM", one per colour frame)

From `A/inc/FlyerClipFmt.h:62-78`. The Amiga header calls bytes 28-223 `reserved3[196]`; the firmware (`F/inc/vid.h:224-253`) defines them as below.

| Off | Type | Name | Meaning |
|---|---|---|---|
| 0 | u32 | ID | `0x4346524D` "CFRM" |
| 4 | u32 | Length | blocks of this frame incl. header (`fh_chunklen`, `vid.c:1470`) |
| 8 | u8 | Version | 4 (`vid.c:2374`) |
| 9 | u8 | VidCmpVer | 2. Playback refuses a mismatch with the loaded chips (`vid.c:1895-1901`). This byte, not the CLIP one, picks old vs new D2 on decode (`F/src/clip.c:193-203`) |
| 10 | u8 | AudVer | 2 (picks the audio layout, §4) |
| 11 | u8 | reserved1 | |
| 12 | i32 | PrevFrame | blocks to previous CFRM (negative), 0 for frame 0 (`vid.c:2358-2361`) |
| 16 | i32 | NextFrame | blocks to next CFRM (= Length), 0 on the last frame (`vid.c:2363, 2580`). Playback stops at 0 (`vid.c:1995`) |
| 20 | u32 | FrameNumber | colour frame from 0 |
| 24 | u8 | reserved2 | |
| 25 | u8 | AudioLength | 3 or 0 |
| 26 | u8 | VidFlag | |
| 27 | u8 | AudioChans | |
| 28 | u16 | fh_phase | unused |
| 30 | u8 | fh__FIRtype | unused (writes commented out, `F/src/clip.c:1064-1066`) |
| 31 | u8 | pad | |
| 32 | u16[8] | fh__FIRcoefs | unused (`vid.c:2380` commented out) |
| 48 | i32[15] | fh_backptrs | §1.6 |
| 108 | u8[110] | reserved | |
| 218 | u16 | fh__Pragma | `$BEEF` enables the next 3 (code commented out, `vid.c:1915-1931`) |
| 220 | u16 | fh__Offset | |
| 222 | u8,u8 | CourseA/B | |
| 224 | Field[4] | fields | fields I..IV, 72 bytes each |

### 1.5 Field record (72 bytes, at 224 + 72·i)

From `A/inc/FlyerClipFmt.h:43-60`; firmware `struct FLDINFO`, `F/inc/vid.h:202-219`.

| Off | Type | Name | Meaning |
|---|---|---|---|
| 0 | u16 | VidStart | blocks from CFRM to this field's VTASC data (`vid.c:1194`, `F/src/clip.c:673`) |
| 2 | u16 | VidLength | blocks of data. 0 = missing (dropped) field (`vid.c:1327-1328`) |
| 4 | u8 | Tolerance | VTASC tolerance (§3.4). Hardware 0..3 (`VTEF_TOLMASK`, `F/inc/chips.h:320`); software clips to ≤7 (`asm:553-559`) |
| 5 | u8 | RndSize | dither bits. Always written = Tolerance (`vid.c:1304`, `F/src/clip.c:676`). Hardware uses `&3` (`vid.c:662`) |
| 6 | u8 | RndFreq | dither period − 1 (§3.6). Default 122 (`vid.c:533`) |
| 7 | u8 | RndSeed | dither seed. Hardware: +1 per field, skipping 0xFF (`vid.c:731-735`). Software: field % 255 (`F/src/clip.c:678`, `asm:199-203`) |
| 8 | u16 | (reserved1) | firmware `fi_aloc` (audio loc, unused; cleared `F/src/clip.c:1058`) |
| 10 | u16 | (reserved2) | firmware `fi_alen` |
| 12 | u8 | Flags | bit 0 `FIF_SUBNYQ`: field coded sub-Nyquist (`F/inc/vid.h:221`, `vid.c:1204-1210`) |
| 13 | u8 | reserved3 | |
| 14 | u8[18] | reserved4 | |
| 32 | u8 | FIRpreset | FIR set used on record (0 custom, 1-4 preset) (`vid.c:1197`) |
| 33 | u8 | SerType | serial device type (`SERDEVTYPE_*`, `A/inc/flyer.h:251-254`) |
| 34 | u8 | SerMake | |
| 35 | u8 | SerialDataLen | ≤ 36 |
| 36 | u8[36] | SerData | SMPTE/serial bytes captured during this field (`vid.c:1217-1226`) |

### 1.6 TAIL, index table, back pointers, head list

TAIL header (`F/inc/vid.h:283-304`). Offsets are from the TAIL block. Built at `vid.c:2610-2629`:

| Off | Type | Name | Recorded value |
|---|---|---|---|
| 0 | u32 | ID | `0x5441494C` "TAIL" |
| 4 | u32 | ChunkLen | 1 + NdxTabSize + 2 + 40 |
| 8 | u32 | Table | 1 |
| 12 | u16 | NdxTabSize | ⌈NdxCount/128⌉ (`STONESPERBLK`, `vid.h:313`; `vid.c:2757-2763`) |
| 14 | u16 | NdxCount | number of entries |
| 16 | u32 | MaxFrame | unused |
| 20 | u8[16] | | |
| 36 | u32 | HeadList | Table + NdxTabSize |
| 40 | u16 | HeadBlks | 2 (`HEADTABLESIZE`) |
| 42 | u8[32] | | |
| 74 | u32 | UserArea | HeadList + 2 |
| 78 | u16 | UserBlks | 40 (`USERAREASIZE`) |
| 80 | u8[432] | | |

- **Index entries** are i32 offsets `frame_block − tail_block`, so they are negative (`vid.c:2769-2795`). The entries are:
  - the CFRM of every frame with `frame % 16 == 0` ("milestones");
  - plus one final entry for the last frame (`vid.c:2382-2396, 2593-2597`).
  - A one-frame clip therefore has `[f0, f0]` (also built that way by `LinkInFrame`, `F/src/clip.c:965-1019`).
- **fh_backptrs[15]**: offset from this CFRM to the other frames of its 16-group, 0 where unset (`F/src/misc.asm:93-107`). For frame F = 16k + j:
  - j = 0, k > 0: `back[i]` = frame (F−1−i) for i = 0..14;
  - j > 0: `back[15−m]` = frame 16k+m for m = 1..j (the self entry is 0); the other entries are 0;
  - frame 0: all 0 (`vid.c:2382-2412`, `F/src/clip.c:911-956`).
- **FindFrame** (`F/src/subs.c:410-541`) looks up frame F like this:
  1. entry = F/16, plus 1 if F%16 ≠ 0;
  2. target = tail + index[entry];
  3. if F%16 ≠ 0: target += backptrs_of(target)[16 − F%16 − 1].
  This works for the final entry too.
- **HEAD blocks** (`F/inc/Heads.h:23-43`): "HEAD", Next, then 22 × 22-byte entries (`startfld, fldlength, mainblk, auxblk, blklength` u32; `drive, flags` u8), then 20 bytes of pad. The recorded state is block 0 `Next=1`, block 1 `Next=0`, and each block's entry[0].startfld = `LISTEND` 0xFFFFFFFF (`vid.c:2641-2678`, `F/inc/Lists.h:27`).
- **User area**: 40 zero blocks.

### 1.7 Fields to frames, and video lines

- Field n of a clip = slot n%4 of frame n/4. Slots are NTSC fields I-IV of a colour frame. Recording starts each frame on field I (`e_field` 1..4 wraps per frame, `vid.c:1342-1348`).
- Field I/III vs II/IV phase and line mapping are covered in §3.2.

### 1.8 VidGrade: std vs HQ5/HQ6

VidGrade changes only the field-size budget and the coder clock rates. The bitstream is identical.

| Grade | Nominal / max blocks per field | Data rate at nominal | Chip clocks rec M/P | Source |
|---|---|---|---|---|
| 0 std | 0x70 / 0x7A | 3.44 MB/s | 80 / 49 MHz | `vid.c:158-159, 382-384` |
| 1 HQ5 | 0x92 / 0x9C | 4.48 MB/s | 84 / 49 MHz | `vid.c:161-162, 387-389` |
| 2 HQ6 | 0xB5 / 0xBF | 5.55 MB/s | 91.9 / 71.5 MHz | `vid.c:166-167, 393-397` |

- The live grade comes from `vmaxlen` thresholds 0x7A/0xA0 (`vid.c:344-365`). Software-written clips take it from the field-size limit (`F/src/clip.c:240-248`).
- Firmware playback never reads `ch_vidgrade`. It is only reported (`F/src/FILESYS.C:2940-2941`) for UI labels (`Edit/Edit/src/controls.c:5713-5720`, `A/src/clipstats.c:110-117`).
- HQ5/HQ6 are EEPROM options (`FLYOPTF_NOT_HQ5`, `A/inc/flyer.h:501`; `F/src/ops.c:863-889`).

### 1.9 Version values of a 4.x clip

| Field | Value |
|---|---|
| Version | 4 |
| VidCmpVer | 2 (CLIP and every CFRM) |
| AudVer | 2 |
| VidGrade | 0/1/2 |
| Pedestal | 60 |
| LogBlkSize | 512 |
| ch_FIRset / FIRpreset | 2 by default (`vid.c:539`) |
| RndFreq | 122 |

These are the record/firmware sources of the era: `vid.c` rev 1.16 (1997-02), `F/inc/flyer.h` rev 1.10 (1998-04).

---

## 2. Clips as files

### 2.1 On the Flyer drive (FLY0: etc.)

The Flyer formats its SCSI drives with its own filesystem (`F/inc/filesys.h`, `F/src/FILESYS.C`). AmigaDOS sees each drive as a volume through the flyerfs handler (`A/src/flyerfs1.c`).

| Item | Detail | Source |
|---|---|---|
| Block 0 | ROOTHDR "ROOT". Offsets: flags 4 (FVIF_VIDEOREADY 1 / AUDIOREADY 2 / WRITEPROT 4), diskokay 5, blksize 6, volname[32] 12, freelist 44, headlist 48, hdrsarea 52, unused 56, dataarea 60, headspace 64, videoend 68, version 76 (=1), type 77 (=3), date 78, userblks 90, newheadlist 94, entries[100] 108 (hash), checksum 508 | `filesys.h:142-170, 44, 95-97` |
| Drive map | freelist ("FLST" blocks of 63 (start,len) extents) at block 1, max(⌈MB/63⌉,5) blocks; headlist and newheadlist, 50 blocks each (`HEADLISTSIZE`); file/dir header area, max(2·MB, 5000) blocks; data area up to `videoend`; a 1024-block chip-patch area at the end | `FILESYS.C:557-592`, `filesys.h:54-56, 207-230` |
| File header | "FILE": nexthash 4, parent 8, name[32] 12, comment[32] 44, type 77 (1), date 78, bits 90, start 102, bytes 106, bytes_hi 110, checksum 508 | `filesys.h:189-205` |
| Dir header | "FDIR": nexthash 4, parent 8, name 12, comment 44, flags 76, type 77 (2), date 78, bits 90, entries[100] 108 | `filesys.h:174-187` |
| Contiguity | a file is **one extent**: `start` block + byte length (64-bit). Clips are allocated contiguously and grown in place (`ExtendClip`) | `filesys.h:200-202`, `F/src/clip.c:784-884` |
| Names | ≤ 31 chars + NUL (32 bytes) for volume, file, dir and comment | `filesys.h:59-63` |
| Lookup | hash `h = (h*13 + toupper(c)) & 2047` over the name, then `% 100`, into root/dir `entries[]`, chained by `nexthash`; case-insensitive | `FILESYS.C:489-501` |
| Dates | `FlyerDate {days since 1978-01-01, minutes, ticks @50 Hz}` = AmigaDOS DateStamp | `filesys.h:52, 66-70` |
| bits | user-defined; flyerfs presents them as the DOS protection bits | `A/src/flyerfs1.c:306` |
| Checksums | written as 0, never computed | `FILESYS.C:1417` |
| Clip test | file whose first block has ID "CLIP" | `FILESYS.C:2921` |
| DOS view | file size = byte length; reads return the raw blocks | `A/src/flyerfs1.c:292-293` |

### 2.2 File forms on an Amiga volume

1. **One clip, copied through flyerfs.** The DOS file is the clip's blocks verbatim (CLIP header to end of tail section, `Length × 512` bytes). Because every offset is relative (§1.1), the bytes are self-contained.
   - Writing such a file back creates a Flyer file at a new LBA. `MakeAudClip` shows that writing clips this way is a supported path (`A/src/MakeAudClip.c:84-170`).
   - Whether a complete clip copied back needs "reprocessing" is open (§6). Skeletal clips do need it.
2. **Whole drive image (`Drive2File`).** Blocks 0..`videoend`, rounded up to a multiple of 4. The copied ROOT is marked `FVIF_WRITEPROT`. It was intended for CD-ROM (`A/src/Drive2File.c:126-166`).

There is no other backup or interchange format in the sources.

### 2.3 Recommended host layout for the emulator

```
FlyerDrives/
  Drive0/                    one folder = one Flyer SCSI drive (one FLYn: volume)
    .flyervol                volume name, FVIF flags (video+audio ready), format date, size in blocks
    Clip Name                exact clip bytes ("CLIP" ... tail), Length*512 bytes
    Clip Name.flymeta        optional sidecar: comment, FlyerDate, bits
    SubDir/ ...              Flyer directories
```

- **Names:** keep the Flyer name (≤31 chars). Uniqueness is case-insensitive, and names may not contain `/` (`FILESYS.C:495`) or `:`.
- **Mount-time LBAs:** synthesize ROOT/FILE/DIR headers in a virtual block space and give each host file **one contiguous LBA range** in the data area. The firmware addresses clips by absolute LBA:
  - `ClipAction.StartBlk/EndBlk` (`A/inc/flyer.h:333-334`);
  - the tail and frame finder (`F/src/subs.c:322-397`);
  - `ClipInfo.Start/EndBlk/IndexBlk` (`filesys.h:105-122`).
  Map block writes back to host-file offsets. Growing a clip needs free blocks after it, so leave slack after each extent.
- **Metadata kept beyond the bytes:** name, comment, type (file/dir), FlyerDate, bits (protection), byte length (for non-clip files whose last block is partial, `FILESYS.C:1300-1395` `fragbytes`), parent directory, volume name and flags.
- **Raw images:** also accept a `Drive2File` drive image as a read-only mount.

---

## 3. VTASC video coding

### 3.1 Pipeline

| Encode (CompLine, `asm:1725-1945`) | Decode (DecompLine, `asm:1967-2246`) |
|---|---|
| 1. 752 Toaster samples → 760-sample line with 4 pad samples each side set to `NTSC_BLACK` 71 (`asm:1733-1744`) | 1. VLC decode + predict (+ sub-Nyquist regen) → 760 samples |
| 2. FIR pre-comp, advance ½ sample (`asm:1752-1758`) | 2. subtract dither (`asm:2191-2226`) |
| 3. add dither (`asm:1761-1798`) | 3. FIR post-comp, retard ½ sample (`asm:2229-2235`) |
| 4. predict, quantize by tolerance, VLC-code (`asm:1802-1934`) | 4. return samples 4..755 (`asm:358-362`) |

All sample arithmetic is 8-bit and wraps mod 256 (`add.b`/`sub.b`).

### 3.2 Field geometry

| Quantity | Value | Source |
|---|---|---|
| Samples per coded line | 760 = 752 + 2×4 (`PIXELSPERLINE`) | `flyerprivate.i:282` |
| Lines returned per field | 242 (`TOASTERLINES`) | `A/inc/flyer.h:282`, `asm:351-353` |
| Coded lines, fields I/III | 243: user lines 0..241, then 1 black fill line | `asm:272-291`, `A/inc/flyer.h:283` |
| Coded lines, fields II/IV | 244: 1 black pad line, user 0..241, 1 fill line. The decoder throws coded line 0 away ("half line") | `asm:754-767, 778-795` |
| Line 0 of a field | NTSC line 21 of field I. The Toaster uses field lines 0..239 (lines 21-260) | `A/src/ReadField.c:173`, `A/src/ReadField2.c:121` |
| Lines in time order | consecutive scan lines of one field, top to bottom | INFERRED |
| Hardware sample window | 760 = Toaster's 752 plus 4 each side | INFERRED from `asm:1733` "fit NTSC pixels into wide760" |

- **Hardware line counts:** the matte (§5) proves that hardware D2 lines are 760 codes long. The number of lines the chips code per field, and whether the II/IV first line is a real half-line, are not stated. INFERRED: ≥243/244, as in software.
- **Subcarrier phase:**
  - a quad of samples is (Y−I, Y−Q, Y+I, Y+Q) on positive lines and the negation on negative ones (`asm:1673-1693`);
  - field f (0-3), line y is positive iff `(y + f + (f>>1))` is even (`asm:1649-1671`), so fields I/IV share a phase, as do II/III (`A/src/FlyerProc.c:429`).
- **Relation to the Toaster framestore:**
  - the framestore stores fields in the order I, III, II, IV (`A/src/ReadField.c:136-139`);
  - in Copperline's `composite.rs` terms, field f line y is picture line 2y + (f&1): the normal copy for f < 2, the chroma-complemented copy for f ≥ 2.
  - Levels are the same: blank 60, black 70/71, white 200. The Flyer's software uses 71 as black (`flyerprivate.i:278`).

### 3.3 Bitstream

- **Bit order:** one continuous bit stream per field, starting at the first byte of the field data. Bits fill each byte **LSB first**, and every code is emitted **LSB first** (OutputCode000 `asm:987-1036`; InputCode000 `asm:1196-1247`).
  - The '020 versions are byte-reversed internally but undo it when copying (`asm:1042-1071, 1357-1365, 1427-1435`), so they produce the same stream.
  - Read a k-bit code as `Σ bit[pos+i] << i`.
- **Lines:** there are no line or field markers. One code is read per coded sample, lines back to back.
- **End:** software pads with 0 bits up to a multiple of 1024 bytes (`BITBUFSIZE`, `asm:1085-1116`, `flyerprivate.i:274-275`), so software `VidLength` is even. A field longer than its limit is cut off at the limit (`asm:1453-1463`). Readers treat bits past the data as 0.
- **Code sizes:** the current size `msize` starts at 8 per field and carries across lines (`asm:725, 1934, 2243`). Per coded sample:

```
enc:  need = CodeMagSize[err]
      while msize < need: emit(EscapeCodes[msize], msize bits); msize += 1
      emit(err, msize bits);  msize = need
dec:  loop { c = read(msize); if c == EscapeCodes[msize] { msize += 1 } else break }
      if c has bit (msize-1) set and msize ≥ 2: c += SgnExtVals[msize]   (sign-extend)
      msize = CodeMagSize[c]
```

The escape code for size n is `1<<(n-1)`, the most negative n-bit value. So n bits carry −(2ⁿ⁻¹−1)..2ⁿ⁻¹−1.

| Table | Values | Source |
|---|---|---|
| EscapeCodes[0..8] | 00 01 02 04 08 10 20 40 80 | `asm:2284-2293` |
| SgnExtBits[0..8] | 0 0 1 2 3 4 5 6 7 | `asm:2295-2304` |
| SgnExtVals[0..8] | 00 00 FC F8 F0 E0 C0 80 00 | `asm:2306-2315` |

CodeMagTable (`asm:1489-1505`) is indexed by the error byte. It equals the smallest n with |v| ≤ 2ⁿ⁻¹−1 (0 → 1, −128 → 8):

```
     0 1 2 3 4 5 6 7 8 9 A B C D E F
0x   1 2 3 3 4 4 4 4 5 5 5 5 5 5 5 5
1x   6 (x16)
2x-3x 7 (x32)
4x-Bx 8 (x128)
Cx   8 7 7 7 7 7 7 7 7 7 7 7 7 7 7 7
Dx   7 (x16)
Ex   7 6 6 6 6 6 6 6 6 6 6 6 6 6 6 6
Fx   6 5 5 5 5 5 5 5 5 4 4 4 4 3 3 2
```

QUIRK: an error of −128 (0x80) needs 8 bits, but 0x80 is also the size-8 escape code. In the asm the encoder emits it and the decoder misreads it as an escape (it reads past the table). This can only happen at tolerance 0. See §3.10.

### 3.4 Prediction and tolerance

**Per-field state.** At the start of every field (`asm:698-731`):
- both 760-byte line buffers = `BORDERVALUE` 0x82;
- `msize` = 8, `preverr` = 0, `C4 = C5 = C6` = 0x82.

**State across lines.** Two line buffers alternate, so the buffer used for line k holds line k−2 (`asm:1928-1931, 2237-2240`). `preverr`, `C4..C6` and `msize` carry from line to line.

**Predictor** for sample x. The kernel (`asm:1845-1854`) is `t7` = the sample 2 lines up (same x) and `c4 c5 c6` = the previous three coded samples:

| Algo | VidCmpVer / Flags | guess |
|---|---|---|
| Old D2 (`ALGO_OLDD2` 0) | VidCmpVer 1 | `c4 − c5 + c6` |
| D2 (`ALGO_D2` 1) | 2, FIF_SUBNYQ clear | `t7 + preverr` |
| Sub-Nyquist (`ALGO_SN` 2) | 2, FIF_SUBNYQ set | `t7 + preverr` (coded samples only) |

Source: `asm:620-642, 1856-1869`; `F/src/clip.c:193-203`.

**Quantize** (encoder, `asm:1871-1888`), with t = min(Tolerance, 7):

```
err   = (i8)(pixel − guess) >> t          // asr.b: rounds toward −∞
c7    = guess + (err << t)                // reconstruction, stored in the line buffer
preverr = c7 − t7;  c4,c5,c6 = c5,c6,c7
```

The decoder does the same with `err` from the stream (`asm:2123-2147, 2172-2178`).

**Reconstruction error.** c7 = pixel − ((pixel−guess) mod 2ᵗ), so the error lies in [−(2ᵗ−1), 0]. That is lossless at t = 0. (Wrap-around below 0 is possible only for pixel < 2ᵗ−1.)

**Hardware tolerances.**
- D2 uses tolerance 0..1; SN uses 0..maxtol−4. The defaults are min 0 = (D2,0) and max 6 = (SN,2) (`vid.c:531-538, 3895-3939`).
- The tolerance level is `algo*4 + tol` (`vid.c:3922-3938`).
- Rate control steps the tolerance up or down each field from the field's size (`vid.c:1267-1306`).

### 3.5 Sub-Nyquist

**Which samples are coded.** On coded line k, sample x (0..759) is coded iff **(k + x) is even**. This holds for all four fields: the asm computes it as `(vt_y ^ x ^ field) & 1 == 0` with `vt_y` offset per field (`asm:1824-1841, 1999-2009`).
- Uncoded samples: the encoder stores them raw in the line buffer and never predicts from them; they emit no code (`asm:1836-1841`).
- 380 codes per line. Coded samples line up with coded samples 2 lines up, so prediction is unchanged.

**Decoder lag.** The decoder runs one line behind (`vt_y` starts at −1, or −2 for fields II/IV, `asm:774-795`). Output line y is assembled while coded line y+1 is being decoded:
- its coded samples come from the previous call;
- each missing sample m is regenerated from:
  - L = out[m−1] (0x82 if m = 0), R = out[m+1] (0x82 if m = 759);
  - A = the line above, values at m−2 and m (0x82 for the first line);
  - B = the line below, values at m−2 and m (0x82 if there is no line below).

Regeneration (`asm:2043-2074`):

```
d1  = L + R                                        // 2Y, 9-bit
up  = sx(A[m-2] − A[m]);  dn = sx(B[m-2] − B[m])   // 2C: samples 2 apart are 180° of subcarrier
v   = ((up + dn) >>ₐ 1 + d1) >>ₐ 1                  // 16-bit, arithmetic shifts
out[m] = (0 ≤ v ≤ 255) ? v : (d1 >> 1)
```

`sx(a−b)` is the byte difference, sign-extended **only if the subtraction borrowed**. It is exact for −128..255 and wraps for differences below −128.

QUIRK (out-of-bounds reads): for m = 0, 1 the asm reads 1-2 bytes before the line buffer. Those bytes are `vt_topline`/`vt_extraline` (values 1/2) or the tail of the other line buffer (`flyerprivate.i:332-334`). The crate models that memory exactly. It affects only samples that the post FIR spreads into output samples 0-3.

QUIRK (half-line parity): the asm encoder codes the pad line of fields II/IV with the opposite parity from the one its decoder uses (§3.10). The decoder then predicts the even coded lines of those fields from the pad line's neighbouring samples instead of the right ones. The resulting error runs down the affected columns. For a black pad line it is non-zero only where the pad line is not flat: near the line ends (FIR reads blanking) and at dither steps (INFERRED; not measured against the asm).

### 3.6 Dither (pseudo-random, subtractive)

**Size and period.**
- Dither size in bits = t (the tolerance). `StartVTASC` sets `vt_RndDither = tolbits` (`asm:558-559`). The hardware uses RndSize, which is always written equal to Tolerance (§1.5).
- t = 0 → no dither, and the generator does not run (`asm:1764-1765`).
- Period p = RndFreq + 1, as a byte (`asm:571-573`).

**Generator.**
- Seed (`InitRndSeed`, `asm:848-872`): s = RndSeed (0xFF → 0). Each bit i of s, if set, adds `7 << 3i` to a 24-bit register R.
- Next value (`GenRndNum`, `asm:886-935`): 4 times `R = (R << 1) | (R[23] == R[16])` (an XNOR feedback shift). The value is the low t bits of R, sign-extended (range −2ᵗ⁻¹..2ᵗ⁻¹−1). Only bits 23 and 16 feed back.

**Application** over each 760-sample line, after the FIR on encode and before the FIR on decode:

```
start of field: num = 0, cnt = 1
per sample:     s ±= num;  if cnt == p { num = gen(); cnt = 0 };  cnt += 1   (cnt is a byte)
```

The first p samples of a field get 0 ("THE NEW P-CODERS ... USE RND # 00 FOR FIRST GROUP", `asm:733-734`). The state runs continuously across lines.

**Alignment.** The encoder uses one 760-sample segment per coded line, pad and fill lines included. The crate decoder subtracts segment k from coded line k.

QUIRK: the asm decoder skips the segment of the discarded first line of fields II/IV (`asm:2192-2193` runs only when there is an output buffer). Its fields II/IV are therefore one segment out of step with its own encoder. The amplitude is ≤ 2ᵗ⁻¹, ±2 LSB at the hardware's maximum tolerance.

### 3.7 FIR pre/post compensation

`FIRfilterLine` (`asm:1521-1624`), applied to all 760 samples unless `FWF_NOFIR` (`A/inc/flyer.h:291`). With P(i) = line[i] for 0 ≤ i < 760, else `NTSC_BLANK` 60, and a = 0 to advance (encode) or −1 to retard (decode):

```
S   = Σ_{d=0..7} c[7−d] · (P(x+d+1+a) + P(x−d+a))      // c: 10-bit two's complement, c[7] = centre tap
s16 = (i16)(S >> 2)                                      // muls.w uses the low word
V   = (s16 · (scale >> 8)) >> 7                          // scale 0x200 → ×2/512 → net /256
out = ((u16)V > 255) ? 255 : V                           // unsigned compare: negatives become 255 (QUIRK)
```

- Coefficients are masked to 10 bits and sign-extended (`asm:677-696`).
- The filter reads the line and writes a temporary line, so it is not applied in place.

**Presets** (`FIRPRESETS[set-1][pre,post]`, `F/src/hard.c:94-108`). Order is c[0] (outermost) .. c[7] (centre); scale 0x200 for all:

| Set | Pre (record) | Post (play) |
|---|---|---|
| 1 "25" | 003 004 006 00D 00F 00B 019 033 | 026 3FE 3CA 00C 3E2 314 3C4 1CC |
| 2 "33" | 004 003 005 00D 00D 007 019 03A | 025 3D2 008 3E4 3D5 3FA 2E1 1ED ("33i tweaked"; old 33i: 015 3F6 3DC 010 3E9 357 3E4 163) |
| 3 "50" | 004 001 002 00C 00A 400 018 049 (0x400 masks to 0) | 400 3F5 017 3C3 003 016 334 164 |
| 4 "100" | 3FF 003 3FA 00A 3F0 01B 3CD 0A2 | same as pre |

**Which set is used.**
- Set 0 = custom: the Flyer's current custom set (`F/src/hard.c:485-533`). The crate uses the clip header's `CustCoefs`/`CustInvCoefs` for it.
- Software decoding uses the post set of the field's `FIRpreset` (`asm:645-664`, `F/src/clip.c:185`). The default encoder set is 2 (`asm:247`).
- The hardware **always plays back with set 2** (`vid.c:3977-3982`) and records with `p_firset` = 2 by default (`vid.c:539`).

**Measured responses** (INFERRED, from the coefficients, `tests/codec.rs::fir_preset_responses`):

| Set | Pre gain at 0.9 / 1.8 / 3.58 MHz | Pre×post gain at 0.9 / 1.8 / 3.58 MHz |
|---|---|---|
| 2 | 0.57 / 0.31 / 0.27 | 1.04 / 1.03 / 1.16 |
| 4 | ≈1 (pure ½-sample interpolation) | ≈1 |

So set 2's precompensation is a strong low-pass that the post filter undoes. The pair leaves chroma +16% hot.

**Map RAM.** The hardware maps the FIR's 12-bit output to 8 bits through a RAM table (`LoadFirMap`, `F/src/hard.c:608-669`):
- scale 1/2/4 → value/4, /2, /1, clipped to 255; values with bit 11 set → 0;
- shape 2 passes through `INVSIN_TABLE` (`F/src/hard.c:569-602`, 256 entries 0x1A..0xBC, an S-curve).
- The firmware loads scale 2 linear into all 8 banks (`F/src/hard.c:467-479`). The software model ignores the map. `FIRROM`/`FIRRAM` are always 0.

INVSIN_TABLE (`F/src/hard.c:569-602`), index 0..255, 32 per row:

```
1A 1A 1A 1A 1A 1A 1A 1A 1B 1B 1B 1B 1B 1C 1C 1C 1D 1D 1D 1E 1E 1F 1F 20 20 21 21 22 22 23 23 24
25 25 26 27 27 28 29 2A 2A 2B 2C 2D 2E 2E 2F 30 31 32 33 34 35 35 36 37 38 39 3A 3B 3C 3D 3E 3F
40 41 42 43 44 45 46 47 48 49 4A 4B 4C 4D 4E 4F 50 51 52 53 54 55 56 57 58 59 5A 5B 5C 5D 5E 5F
60 61 62 63 64 65 66 67 68 69 69 6A 6B 6C 6D 6E 6F 70 71 72 73 74 75 76 77 77 78 79 7A 7B 7C 7D
7E 7F 80 80 81 82 83 84 85 86 86 87 88 89 8A 8B 8B 8C 8D 8E 8F 8F 90 91 92 92 93 94 95 95 96 97
98 98 99 9A 9B 9B 9C 9D 9D 9E 9F 9F A0 A1 A1 A2 A2 A3 A4 A4 A5 A6 A6 A7 A7 A8 A8 A9 A9 AA AB AB
AC AC AD AD AE AE AF AF AF B0 B0 B1 B1 B2 B2 B2 B3 B3 B4 B4 B4 B5 B5 B5 B6 B6 B6 B7 B7 B7 B8 B8
B8 B8 B9 B9 B9 B9 B9 BA BA BA BA BA BB BB BB BB BB BB BB BC BC BC BC BC BC BC BC BC BC BC BC BC
```

### 3.8 Codec parameters from the headers

Decoding a field n uses these parameters (`GetCompInfo`, `F/src/clip.c:124-212`; `OpenReadField2`, `asm:80-166`):

| Parameter | Value |
|---|---|
| algo | from the CFRM's `VidCmpVer` and the field's `Flags` (§3.4) |
| tolerance | `Tolerance` |
| RndFreq | `RndFreq` |
| RndSeed | `RndSeed` |
| FIR | post set of `FIRpreset` |
| field parity | n & 3 |

- Data = `VidLength` blocks at CFRM + `VidStart`. The asm reads in 2-block chunks and may read past the end (`asm:1289-1377`).
- **Encoding defaults** (`DefaultCompression`, `asm:244-252`; `A/src/WriteField.c:94-100`): D2, tolerance 0, RndFreq 122, FIR set 2, seed field % 255.
- **Size limit:** given by `VidCompInfo.DataSize`, or by the grade when 0 (`F/src/clip.c:240-248, 523-526`).
- **Software rate control:** `FlyerProc` tries D2/0, D2/1, SN/0, SN/1 until the field fits (`A/src/FlyerProc.c:341-349`). `ClipWriter` does the same, adding SN/2.

### 3.9 Encoder/decoder per field (as implemented)

```
encode(field f, lines[0..242]):
  init (§3.4, §3.6);  k = 0
  if f odd: code(fill_black, k++)
  for y in 0..242: code(pad4(lines[y]), k++)
  while counted lines < 243: code(fill_black, k++)
  pad with 0 bits to a multiple of 1024 bytes

decode(field f):
  init;  vt_y = −(SN?1:0) − (f odd?1:0);  while vt_y ≠ 0: DecompLine(no output)
  for y in 0..242: DecompLine → 760 samples; subtract dither segment (y + (f&1)); post FIR; keep [4..756)
```

### 3.10 Quirks and deviations (crate vs asm)

| # | asm behaviour | Crate |
|---|---|---|
| 1 | Error −128 at tolerance 0 is emitted as 0x80 in 8 bits; the decoder takes it as an escape and desynchronises (`asm:1895-1911, 2093-2096`) | encoder codes ±127 (1 LSB error); decoder reads 0x80 at size 8 as −128 |
| 2 | Fields II/IV pad line coded with the opposite SN parity from what the decoder expects (`asm:754-767` vs `2000-2009`) | the pad line uses the decoder's parity |
| 3 | Decoder skips the dither segment of the discarded II/IV line (`asm:2192-2193`) | one segment per coded line on both sides |
| 4 | FIR negative results clip to 255; the 16-bit truncation of S>>2 can overflow on extreme edges | reproduced |
| 5 | SN regen reads 1-2 bytes before the line buffer for m = 0, 1 | reproduced (memory modelled) |
| 6 | `GenRndNum` loops 65536× for 8-bit dither | unreachable (`MAXTOLER` 7); crate returns the sign |

---

## 4. Audio

| Property | Value | Source |
|---|---|---|
| Sample | 16-bit, written by the 68k as a native `UWORD` → **big-endian**. Signed two's complement is INFERRED (`MakeAudClip` writes a wrapping sawtooth) | `A/src/MakeAudClip.c:49, 94-96` |
| Samples per field per channel | **735** | `A/src/MakeAudClip.c:43`; the DSP program loads the constant: `exe/2C/DSP.bin` word 630 = `0x402DF4` = ADSP-21xx `AY0 = 735` (decoding INFERRED) |
| Rate | 735 × 59.94 = 44 055.94 Hz = 44.1 kHz × 1000/1001 (INFERRED). The audio clock is 84 587 402 Hz = 1920 × 44 055.94 | `F/src/main.c:131`; marketing says "44.1 kHz" (§7) |
| Channels | 0, 1 or 2 mono channels (L, R) | `A/src/MakeAudClip.c:42` |
| Chunk | `AudioLength` (3) blocks = 1536 bytes = 768 slots per channel per field; samples 0..734 used, 735..767 zero | `F/inc/flyer.h:119`, `MakeAudClip.c:49, 158` |
| Position | starts at CFRM + 1, before the video | `F/src/vid.c:1453, 2439-2443` |
| AudVer 2 order | channel-major `L1 L2 L3 L4 R1 R2 R3 R4`: offset = 1 + (chan·4 + fld)·AudioLength | `F/src/Audio.c:1028-1044, 1095-1101` |
| AudVer 1 order | field-major `L1 R1 L2 R2 ...`: offset = 1 + (fld·chans + chan)·AudioLength | `F/src/Audio.c:1035-1039` |
| Audio-only clips | `VidFlag` 0, frames = header + 12·chans blocks, all field records zero | `MakeAudClip.c:109-147` |

**Converting from MP4.** Resample to 44 055.94 Hz, or equivalently to 735 samples per field (2940 per colour frame). The crate's `Clip::audio()` / `ClipWriter::push_frame` use interleaved i16, 4·735 samples per channel per frame.

---

## 5. Test vectors in the sources

The firmware builds a matte-black field that its chips decode (`CreateMatteField`, `F/src/vid.c:4227-4267`):
- 48 blocks (`MATTEBLKS`, `F/inc/flyer.h:129`), all zero bits except 8 bytes at 0x0000 and 8 at 0x00BF;
- new chips: `NEWBLACKSEQ`; old chips: `OLDBLACKSEQ` (`vid.c:144-151`).

```
NEWBLACKSEQ  @0000: C5 00 00 00 00 00 00 00   @00BF: A0 44 08 EE 00 00 00 00
OLDBLACKSEQ  @0000: C5 80 12 21 58 04 00 00   @00BF: 00 x8
```

Worked decode of the new-chip sequence (D2):

| Line | Bits | What happens |
|---|---|---|
| 0 | 773 | first sample: guess 0x82, code 0xC5 = −59 in 8 bits → 71; then 0 in 7 bits; then 758 codes of 1 bit |
| 1 | 760 | all zeros (bits 773..1532) |
| 2 | — | first sample: t7 = 71, preverr = −59, so guess = 12. Escapes of size 1..6 (21 bits) at bit 1533 = byte 0xBF bit 5, then +59 = 0x3B in 7 bits → bytes A0 44 08 EE |
| rest | — | zeros |

Result: every sample of every line is 71 (black). The old-chip sequence gives the same result with the c4−c5+c6 predictor.

Both sequences are tests in `tests/codec.rs`:
- decoding gives all 71;
- encoding a black field reproduces both sequences byte for byte, for all four fields.

No sample clips, CLIP/CFRM test data or other embedded VTASC data exist in the sources, and none were found on this machine.

---

## 6. Open questions

1. Lines per field the **hardware** codes, and whether fields II/IV really start with a half line (the software assumes 243/244 coded lines and discards II/IV line 0).
2. Hardware dither alignment on fields II/IV (quirk 3), and how the hardware handles error −128 at tolerance 0 (quirk 1).
3. Whether the Xilinx FIR path and map RAM match the software filter exactly (presets and scales agree; rounding unknown).
4. Audio signedness (INFERRED signed), and exactly 735 samples per field: the rate is consistent, but the DSP code was only checked for the constant.
5. Whether a complete clip copied back to a Flyer drive through flyerfs plays without `CAF_REPROCESS` (offsets are relative, so it should; heads lists may be rebuilt).
6. Exact alignment of the hardware's 760-sample window with the Toaster's 752 (asserted only by the asm comment).

## 7. Web search results

None documents the clip container or the VTASC bitstream; this spec appears to be the first.
- NewTek marketing text (VTASC = "Video Toaster Adaptive Statistical Coding", 0.7-8 MB/s, 752×480, 14.3 MHz 8-bit, 16-bit 44.1 kHz audio): https://discmaster.textfiles.com/file/19252/PC_PRESS.ISO/yuweb/ns/00000004/00000345.htm?html=true
- Hardware summary (ADSP2115 DSP, 44.1 kHz, three SCSI buses): https://amiga.resource.cx/exp/flyer
- Source release, Feb 2004 (openvideotoaster.org): https://amiga-news.de/en/news/AN-2004-02-00022-EN.html
- Flyer drives discussion (no format details): https://classiccmp.org/pipermail/cctalk/2020-October/056125.html
- Background: https://en.wikipedia.org/wiki/Video_Toaster, https://bigbookofamigahardware.com/bboah/Product.aspx?id=504
- A copy of the flyer.library autodoc is on an Amiga CD (fetch blocked by robots): https://discmaster.textfiles.com/file/21165/EnigmaAmiga112CD.iso/dalla rivista/news/documentazione_flyer/flyer.doc
