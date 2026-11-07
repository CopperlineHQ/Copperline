// SPDX-License-Identifier: GPL-3.0-or-later

//! On-disk header blocks of a Flyer clip. Every header is one 512-byte
//! logical block, big-endian (68000), word-aligned as SAS/C laid it out.
//!
//! Field names follow `amiflyer/inc/FlyerClipFmt.h`; where the Flyer's own
//! firmware (`flyer/inc/vid.h`) gives a byte the Amiga header calls
//! "reserved", the firmware meaning is used and noted.

/// Logical block size (`LOGBLK_SIZE`, flyerprivate.i:82; `ch_blksize = 512`, vid.c:2684).
pub const BLOCK: usize = 512;

pub const ID_CLIP: u32 = 0x434C_4950; // "CLIP"
pub const ID_CFRM: u32 = 0x4346_524D; // "CFRM"
pub const ID_TAIL: u32 = 0x5441_494C; // "TAIL"
pub const ID_HEAD: u32 = 0x4845_4144; // "HEAD"

/// `FLYER_VERSION` (flyer/inc/flyer.h:51).
pub const FLYER_VERSION: u8 = 4;
/// `VTASC_OLDD2` / `VTASC_NEWD2` (flyer/inc/vid.h:41-42).
pub const VTASC_OLDD2: u8 = 1;
pub const VTASC_NEWD2: u8 = 2;
/// `AUDIO_VERSION` (flyer/inc/Audio.h:30).
pub const AUDIO_VERSION: u8 = 2;

/// `FIF_SUBNYQ` (vid.h:221): field coded with the sub-Nyquist algorithm.
pub const FIF_SUBNYQ: u8 = 1;

/// `ClipHeader.VidGrade` (`GRADE_*`, vid.h:279-281).
pub const GRADE_STD: u8 = 0;
pub const GRADE_HQ5: u8 = 1;
pub const GRADE_HQ6: u8 = 2;

pub(crate) fn get_u16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
pub(crate) fn get_u32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
pub(crate) fn put_u16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_be_bytes());
}
pub(crate) fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_be_bytes());
}
fn arr<const N: usize>(b: &[u8], o: usize) -> [u8; N] {
    let mut a = [0u8; N];
    a.copy_from_slice(&b[o..o + N]);
    a
}

/// Errors from header parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderError {
    Short,
    BadId { expected: u32, found: u32 },
}

fn need(b: &[u8]) -> Result<(), HeaderError> {
    if b.len() < BLOCK {
        Err(HeaderError::Short)
    } else {
        Ok(())
    }
}

/// The "CLIP" master header, block 0 of every clip (FlyerClipFmt.h:18-41,
/// firmware `struct CLIPHDR`, vid.h:255-277).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipHeader {
    pub id: u32,                  // 0   "CLIP"
    pub length: u32,              // 4   clip length in blocks, all components
    pub version: u8,              // 8   Flyer version (4)
    pub vid_cmp_ver: u8,          // 9   VTASC chip version (2)
    pub aud_ver: u8,              // 10  audio format version (2)
    pub vid_grade: u8,            // 11  GRADE_STD/HQ5/HQ6
    pub fields: u32,              // 12  fields in clip
    pub data_start: u32,          // 16  blocks: clip hdr -> first frame hdr
    pub data_end: u32,            // 20  blocks: clip hdr -> last data blk + 1
    pub tail: u32,                // 24  blocks: clip hdr -> TAIL (0 = no index)
    pub reserved2: u8,            // 28
    pub audio_length: u8,         // 29  blocks per mono channel per field (3)
    pub vid_flag: u8,             // 30  1 = has video
    pub audio_chans: u8,          // 31  mono channels
    pub pedestal: u8,             // 32  playback pedestal (60)
    pub fir_set: u8,              // 33  Amiga "reserved3"; firmware ch_FIRset
    pub cust_coefs: [u16; 9],     // 34  custom pre-comp FIR (8 coefs + scale)
    pub cust_inv_coefs: [u16; 9], // 52 custom post-comp FIR
    pub log_blk_size: u16,        // 70  512
    pub fir_rom: u8,              // 72
    pub fir_ram: u8,              // 73
    pub reserved4: [u8; 438],     // 74..512
}

impl Default for ClipHeader {
    fn default() -> Self {
        ClipHeader {
            id: ID_CLIP,
            length: 0,
            version: FLYER_VERSION,
            vid_cmp_ver: VTASC_NEWD2,
            aud_ver: AUDIO_VERSION,
            vid_grade: GRADE_STD,
            fields: 0,
            data_start: 1,
            data_end: 1,
            tail: 0,
            reserved2: 0,
            audio_length: 0,
            vid_flag: 1,
            audio_chans: 0,
            pedestal: 60,
            fir_set: 2,
            cust_coefs: [0; 9],
            cust_inv_coefs: [0; 9],
            log_blk_size: BLOCK as u16,
            fir_rom: 0,
            fir_ram: 0,
            reserved4: [0; 438],
        }
    }
}

impl ClipHeader {
    pub fn parse(b: &[u8]) -> Result<Self, HeaderError> {
        need(b)?;
        let id = get_u32(b, 0);
        if id != ID_CLIP {
            return Err(HeaderError::BadId {
                expected: ID_CLIP,
                found: id,
            });
        }
        let mut cust_coefs = [0u16; 9];
        let mut cust_inv_coefs = [0u16; 9];
        for i in 0..9 {
            cust_coefs[i] = get_u16(b, 34 + 2 * i);
            cust_inv_coefs[i] = get_u16(b, 52 + 2 * i);
        }
        Ok(ClipHeader {
            id,
            length: get_u32(b, 4),
            version: b[8],
            vid_cmp_ver: b[9],
            aud_ver: b[10],
            vid_grade: b[11],
            fields: get_u32(b, 12),
            data_start: get_u32(b, 16),
            data_end: get_u32(b, 20),
            tail: get_u32(b, 24),
            reserved2: b[28],
            audio_length: b[29],
            vid_flag: b[30],
            audio_chans: b[31],
            pedestal: b[32],
            fir_set: b[33],
            cust_coefs,
            cust_inv_coefs,
            log_blk_size: get_u16(b, 70),
            fir_rom: b[72],
            fir_ram: b[73],
            reserved4: arr(b, 74),
        })
    }

    pub fn to_bytes(&self) -> [u8; BLOCK] {
        let mut b = [0u8; BLOCK];
        put_u32(&mut b, 0, self.id);
        put_u32(&mut b, 4, self.length);
        b[8] = self.version;
        b[9] = self.vid_cmp_ver;
        b[10] = self.aud_ver;
        b[11] = self.vid_grade;
        put_u32(&mut b, 12, self.fields);
        put_u32(&mut b, 16, self.data_start);
        put_u32(&mut b, 20, self.data_end);
        put_u32(&mut b, 24, self.tail);
        b[28] = self.reserved2;
        b[29] = self.audio_length;
        b[30] = self.vid_flag;
        b[31] = self.audio_chans;
        b[32] = self.pedestal;
        b[33] = self.fir_set;
        for i in 0..9 {
            put_u16(&mut b, 34 + 2 * i, self.cust_coefs[i]);
            put_u16(&mut b, 52 + 2 * i, self.cust_inv_coefs[i]);
        }
        put_u16(&mut b, 70, self.log_blk_size);
        b[72] = self.fir_rom;
        b[73] = self.fir_ram;
        b[74..].copy_from_slice(&self.reserved4);
        b
    }
}

/// Per-field record inside a frame header, 72 bytes (FlyerClipFmt.h:43-60,
/// firmware `struct FLDINFO`, vid.h:202-219).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldInfo {
    pub vid_start: u16,      // 0  blocks: frame hdr -> this field's video
    pub vid_length: u16,     // 2  blocks of video for this field
    pub tolerance: u8,       // 4  VTASC tolerance (bits dropped)
    pub rnd_size: u8,        // 5  dither amplitude in bits
    pub rnd_freq: u8,        // 6  dither period - 1
    pub rnd_seed: u8,        // 7  dither seed
    pub aud_start: u16,      // 8  Amiga "reserved1"; firmware fi_aloc (unused)
    pub aud_length: u16,     // 10 Amiga "reserved2"; firmware fi_alen (unused)
    pub flags: u8,           // 12 FIF_SUBNYQ
    pub reserved3: u8,       // 13
    pub reserved4: [u8; 18], // 14
    pub fir_preset: u8,      // 32 FIR set used on record (0 custom, 1-4 preset)
    pub ser_type: u8,        // 33
    pub ser_make: u8,        // 34
    pub serial_len: u8,      // 35
    pub serial: [u8; 36],    // 36..72 SMPTE/serial bytes captured this field
}

impl Default for FieldInfo {
    fn default() -> Self {
        FieldInfo {
            vid_start: 0,
            vid_length: 0,
            tolerance: 0,
            rnd_size: 0,
            rnd_freq: 0,
            rnd_seed: 0,
            aud_start: 0,
            aud_length: 0,
            flags: 0,
            reserved3: 0,
            reserved4: [0; 18],
            fir_preset: 0,
            ser_type: 0,
            ser_make: 0,
            serial_len: 0,
            serial: [0; 36],
        }
    }
}

pub const FIELD_INFO_SIZE: usize = 72;

impl FieldInfo {
    pub fn parse(b: &[u8]) -> Self {
        FieldInfo {
            vid_start: get_u16(b, 0),
            vid_length: get_u16(b, 2),
            tolerance: b[4],
            rnd_size: b[5],
            rnd_freq: b[6],
            rnd_seed: b[7],
            aud_start: get_u16(b, 8),
            aud_length: get_u16(b, 10),
            flags: b[12],
            reserved3: b[13],
            reserved4: arr(b, 14),
            fir_preset: b[32],
            ser_type: b[33],
            ser_make: b[34],
            serial_len: b[35],
            serial: arr(b, 36),
        }
    }
    pub fn write(&self, b: &mut [u8]) {
        put_u16(b, 0, self.vid_start);
        put_u16(b, 2, self.vid_length);
        b[4] = self.tolerance;
        b[5] = self.rnd_size;
        b[6] = self.rnd_freq;
        b[7] = self.rnd_seed;
        put_u16(b, 8, self.aud_start);
        put_u16(b, 10, self.aud_length);
        b[12] = self.flags;
        b[13] = self.reserved3;
        b[14..32].copy_from_slice(&self.reserved4);
        b[32] = self.fir_preset;
        b[33] = self.ser_type;
        b[34] = self.ser_make;
        b[35] = self.serial_len;
        b[36..72].copy_from_slice(&self.serial);
    }
    pub fn is_sub_nyquist(&self) -> bool {
        self.flags & FIF_SUBNYQ != 0
    }
}

/// The "CFRM" header at the start of every colour frame (4 fields)
/// (FlyerClipFmt.h:62-78; firmware `struct FRAMEHDR`, vid.h:224-253 names
/// the bytes the Amiga header lumps into `reserved3[196]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub id: u32,                // 0   "CFRM"
    pub length: u32,            // 4   blocks in this frame incl. header
    pub version: u8,            // 8
    pub vid_cmp_ver: u8,        // 9
    pub aud_ver: u8,            // 10
    pub reserved1: u8,          // 11
    pub prev_frame: i32,        // 12  blocks from this hdr to previous (0 first)
    pub next_frame: i32,        // 16  blocks from this hdr to next (0 last)
    pub frame_number: u32,      // 20  colour frame number from 0
    pub reserved2: u8,          // 24
    pub audio_length: u8,       // 25  blocks per mono channel per field
    pub vid_flag: u8,           // 26
    pub audio_chans: u8,        // 27
    pub phase: u16,             // 28  fh_phase (unused)
    pub fir_type: u8,           // 30  fh__FIRtype (unused)
    pub pad3: u8,               // 31
    pub fir_coefs: [u16; 8],    // 32 fh__FIRcoefs (unused)
    pub back_ptrs: [i32; 15],   // 48 offsets to the previous frames of this 16-group
    pub reserved: [u8; 110],    // 108
    pub pragma: u16,            // 218 fh__Pragma ($BEEF = valid; unused)
    pub pragma_offset: u16,     // 220
    pub course_a: u8,           // 222
    pub course_b: u8,           // 223
    pub fields: [FieldInfo; 4], // 224..512
}

impl Default for FrameHeader {
    fn default() -> Self {
        FrameHeader {
            id: ID_CFRM,
            length: 0,
            version: FLYER_VERSION,
            vid_cmp_ver: VTASC_NEWD2,
            aud_ver: AUDIO_VERSION,
            reserved1: 0,
            prev_frame: 0,
            next_frame: 0,
            frame_number: 0,
            reserved2: 0,
            audio_length: 0,
            vid_flag: 1,
            audio_chans: 0,
            phase: 0,
            fir_type: 0,
            pad3: 0,
            fir_coefs: [0; 8],
            back_ptrs: [0; 15],
            reserved: [0; 110],
            pragma: 0,
            pragma_offset: 0,
            course_a: 0,
            course_b: 0,
            fields: [FieldInfo::default(); 4],
        }
    }
}

impl FrameHeader {
    pub fn parse(b: &[u8]) -> Result<Self, HeaderError> {
        need(b)?;
        let id = get_u32(b, 0);
        if id != ID_CFRM {
            return Err(HeaderError::BadId {
                expected: ID_CFRM,
                found: id,
            });
        }
        let mut fir_coefs = [0u16; 8];
        for (i, c) in fir_coefs.iter_mut().enumerate() {
            *c = get_u16(b, 32 + 2 * i);
        }
        let mut back_ptrs = [0i32; 15];
        for (i, p) in back_ptrs.iter_mut().enumerate() {
            *p = get_u32(b, 48 + 4 * i) as i32;
        }
        let mut fields = [FieldInfo::default(); 4];
        for (i, f) in fields.iter_mut().enumerate() {
            *f = FieldInfo::parse(&b[224 + FIELD_INFO_SIZE * i..]);
        }
        Ok(FrameHeader {
            id,
            length: get_u32(b, 4),
            version: b[8],
            vid_cmp_ver: b[9],
            aud_ver: b[10],
            reserved1: b[11],
            prev_frame: get_u32(b, 12) as i32,
            next_frame: get_u32(b, 16) as i32,
            frame_number: get_u32(b, 20),
            reserved2: b[24],
            audio_length: b[25],
            vid_flag: b[26],
            audio_chans: b[27],
            phase: get_u16(b, 28),
            fir_type: b[30],
            pad3: b[31],
            fir_coefs,
            back_ptrs,
            reserved: arr(b, 108),
            pragma: get_u16(b, 218),
            pragma_offset: get_u16(b, 220),
            course_a: b[222],
            course_b: b[223],
            fields,
        })
    }

    pub fn to_bytes(&self) -> [u8; BLOCK] {
        let mut b = [0u8; BLOCK];
        put_u32(&mut b, 0, self.id);
        put_u32(&mut b, 4, self.length);
        b[8] = self.version;
        b[9] = self.vid_cmp_ver;
        b[10] = self.aud_ver;
        b[11] = self.reserved1;
        put_u32(&mut b, 12, self.prev_frame as u32);
        put_u32(&mut b, 16, self.next_frame as u32);
        put_u32(&mut b, 20, self.frame_number);
        b[24] = self.reserved2;
        b[25] = self.audio_length;
        b[26] = self.vid_flag;
        b[27] = self.audio_chans;
        put_u16(&mut b, 28, self.phase);
        b[30] = self.fir_type;
        b[31] = self.pad3;
        for (i, c) in self.fir_coefs.iter().enumerate() {
            put_u16(&mut b, 32 + 2 * i, *c);
        }
        for (i, p) in self.back_ptrs.iter().enumerate() {
            put_u32(&mut b, 48 + 4 * i, *p as u32);
        }
        b[108..218].copy_from_slice(&self.reserved);
        put_u16(&mut b, 218, self.pragma);
        put_u16(&mut b, 220, self.pragma_offset);
        b[222] = self.course_a;
        b[223] = self.course_b;
        for (i, f) in self.fields.iter().enumerate() {
            f.write(&mut b[224 + FIELD_INFO_SIZE * i..224 + FIELD_INFO_SIZE * (i + 1)]);
        }
        b
    }
}

/// The "TAIL" header (firmware `struct TAILHDR`, vid.h:283-304). Offsets
/// below are relative to the TAIL block itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailHeader {
    pub id: u32,              // 0  "TAIL"
    pub chunk_len: u32,       // 4  blocks in tail section incl. this header
    pub table: u32,           // 8  -> index table (always 1)
    pub ndx_tab_size: u16,    // 12 index table blocks
    pub ndx_count: u16,       // 14 index entries
    pub max_frame: u32,       // 16 unused
    pub reserved1: [u8; 16],  // 20
    pub head_list: u32,       // 36 -> head list blocks
    pub head_blks: u16,       // 40 (2)
    pub reserved2: [u8; 32],  // 42
    pub user_area: u32,       // 74 -> user blocks
    pub user_blks: u16,       // 78 (40)
    pub reserved3: [u8; 432], // 80..512
}

impl TailHeader {
    pub fn parse(b: &[u8]) -> Result<Self, HeaderError> {
        need(b)?;
        let id = get_u32(b, 0);
        if id != ID_TAIL {
            return Err(HeaderError::BadId {
                expected: ID_TAIL,
                found: id,
            });
        }
        Ok(TailHeader {
            id,
            chunk_len: get_u32(b, 4),
            table: get_u32(b, 8),
            ndx_tab_size: get_u16(b, 12),
            ndx_count: get_u16(b, 14),
            max_frame: get_u32(b, 16),
            reserved1: arr(b, 20),
            head_list: get_u32(b, 36),
            head_blks: get_u16(b, 40),
            reserved2: arr(b, 42),
            user_area: get_u32(b, 74),
            user_blks: get_u16(b, 78),
            reserved3: arr(b, 80),
        })
    }

    /// Tail laid out the way `DoRecMachine` builds it (vid.c:2613-2629).
    pub fn new(ndx_blks: u16, ndx_count: u16) -> Self {
        TailHeader {
            id: ID_TAIL,
            chunk_len: 1 + ndx_blks as u32 + HEADTABLESIZE as u32 + USERAREASIZE as u32,
            table: 1,
            ndx_tab_size: ndx_blks,
            ndx_count,
            max_frame: 0,
            reserved1: [0; 16],
            head_list: 1 + ndx_blks as u32,
            head_blks: HEADTABLESIZE,
            reserved2: [0; 32],
            user_area: 1 + ndx_blks as u32 + HEADTABLESIZE as u32,
            user_blks: USERAREASIZE,
            reserved3: [0; 432],
        }
    }

    pub fn to_bytes(&self) -> [u8; BLOCK] {
        let mut b = [0u8; BLOCK];
        put_u32(&mut b, 0, self.id);
        put_u32(&mut b, 4, self.chunk_len);
        put_u32(&mut b, 8, self.table);
        put_u16(&mut b, 12, self.ndx_tab_size);
        put_u16(&mut b, 14, self.ndx_count);
        put_u32(&mut b, 16, self.max_frame);
        b[20..36].copy_from_slice(&self.reserved1);
        put_u32(&mut b, 36, self.head_list);
        put_u16(&mut b, 40, self.head_blks);
        b[42..74].copy_from_slice(&self.reserved2);
        put_u32(&mut b, 74, self.user_area);
        put_u16(&mut b, 78, self.user_blks);
        b[80..].copy_from_slice(&self.reserved3);
        b
    }
}

/// `HEADTABLESIZE`, `USERAREASIZE`, `STONESPERBLK` (vid.h:313,320-321).
pub const HEADTABLESIZE: u16 = 2;
pub const USERAREASIZE: u16 = 40;
pub const STONESPERBLK: usize = 128;
/// `LISTEND` (flyer/inc/Lists.h:27).
pub const LISTEND: u32 = 0xFFFF_FFFF;

/// One empty "HEAD" block of the tail's head list (`struct PRIVHEADSBLOCK`,
/// Heads.h:38-43, built at vid.c:2648-2662).
pub fn head_block(last: bool) -> [u8; BLOCK] {
    let mut b = [0u8; BLOCK];
    put_u32(&mut b, 0, ID_HEAD);
    put_u32(&mut b, 4, if last { 0 } else { 1 });
    put_u32(&mut b, 8, LISTEND); // ph_Heads[0].phe_startfld
    b
}
