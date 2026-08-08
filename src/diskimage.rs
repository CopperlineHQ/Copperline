// SPDX-License-Identifier: GPL-3.0-or-later

//! Making fresh Amiga disk images: floppies (ADF) and hard drives (HDF).
//!
//! Nothing here touches the emulated machine. It is a workshop: the
//! launcher's *Create Image* page drives it, and what comes out is meant to
//! be usable anywhere -- another emulator, or a real Amiga through a CF
//! card -- so every structure is written the way AmigaDOS writes it rather
//! than the way any one emulator happens to accept.
//!
//! Two independent sources were used to pin the layouts down, and they
//! agree: `amitools` (cnvogelg), which states the block structures
//! directly, and WinUAE's `disk.cpp`, which writes the same bytes with the
//! constants folded in. Where WinUAE has a magic number the derivation is
//! noted beside it, because a magic number nobody can re-derive is how
//! these formats get quietly wrong.
//!
//! A big image is created sparse by default -- the length is set and only
//! the blocks that carry structure are written -- so a 2 GB hard drive
//! costs a few kilobytes of writes and whatever the host filesystem
//! chooses to allocate. That is `std` alone on Unix, where setting the
//! length of a file leaves a hole; NTFS allocates unless a file is marked
//! sparse first, so [`mark_sparse`] is the one platform call here.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

/// Every Amiga block device this module writes uses 512-byte blocks.
pub const BLOCK_BYTES: usize = 512;
/// Longs per block: the unit AmigaDOS block fields are indexed in.
const BLOCK_LONGS: usize = BLOCK_BYTES / 4;

/// Blocks AmigaDOS keeps at the front of a volume for the boot block.
/// Two is what every Amiga tool writes, but the figure is stated per
/// partition in the Rigid Disk Block, so a drive may name another.
pub const RESERVED_BLOCKS: u32 = 2;

/// The most blocks anything AmigaDOS describes can hold. Every block
/// number it uses -- a volume's root, each bitmap page, every file header,
/// and the Rigid Disk Block's own extents -- is a 32-bit field, so blocks
/// `0..=u32::MAX` is the whole of what can be named, however large the file
/// is. Both a partition table and a filesystem run out of room here.
const MAX_BLOCKS: u64 = 1 << 32;

/// The largest image anything on it can describe: 2 TiB with 512-byte
/// blocks. An image can be made larger than this only if it is left both
/// unpartitioned and unformatted -- a bare drive, for the Amiga's own tools
/// to divide into partitions each within the limit.
pub const MAX_RDB_BYTES: u64 = MAX_BLOCKS * BLOCK_BYTES as u64;

/// A double-density floppy: 80 cylinders, 2 sides, 11 sectors.
pub const FLOPPY_DD_BLOCKS: u64 = 80 * 2 * 11;
/// A high-density floppy doubles it.
pub const FLOPPY_HD_BLOCKS: u64 = FLOPPY_DD_BLOCKS * 2;

/// Which floppy a fresh image should be.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Density {
    /// 880 KB, the drive every Amiga has.
    #[default]
    Dd,
    /// 1.76 MB, the A4000/A3000 drive and later externals.
    Hd,
}

impl Density {
    pub const ALL: [Density; 2] = [Density::Dd, Density::Hd];

    pub fn blocks(self) -> u64 {
        match self {
            Density::Dd => FLOPPY_DD_BLOCKS,
            Density::Hd => FLOPPY_HD_BLOCKS,
        }
    }

    pub fn bytes(self) -> u64 {
        self.blocks() * BLOCK_BYTES as u64
    }

    /// Tracks on the disk: one per cylinder per side, which is what the
    /// extended container writes a record for.
    pub fn tracks(self) -> u64 {
        self.blocks() / u64::from(self.sectors_per_track())
    }

    /// Sectors per track, which the extended container records per track.
    fn sectors_per_track(self) -> u32 {
        match self {
            Density::Dd => 11,
            Density::Hd => 22,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Density::Dd => "DD (880K)",
            Density::Hd => "HD (1.76M)",
        }
    }
}

/// How a floppy image is wrapped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Container {
    /// A plain sector image: every block in order, nothing else. What
    /// almost everything means by "ADF".
    #[default]
    Adf,
    /// The `UAE-1ADF` extended container: a header, then one record per
    /// track. It can hold tracks a sector image cannot describe, which is
    /// why copy-protected disks are dumped into it. A blank one is still a
    /// blank AmigaDOS disk -- the container is what differs, not the
    /// filesystem.
    ExtendedAdf,
}

impl Container {
    pub const ALL: [Container; 2] = [Container::Adf, Container::ExtendedAdf];

    pub fn label(self) -> &'static str {
        match self {
            Container::Adf => "Standard ADF",
            Container::ExtendedAdf => "Extended ADF",
        }
    }

    /// How many bytes the container adds around the sectors: the tag, the
    /// track count, and one record per track. A plain sector image adds
    /// nothing, so the file is exactly the disk.
    pub fn overhead(self, density: Density) -> u64 {
        match self {
            Container::Adf => 0,
            // 8-byte tag, 4-byte header, then 12 bytes per track.
            Container::ExtendedAdf => 12 + 12 * density.tracks(),
        }
    }

    /// The extension a fresh image of this container wants.
    pub fn extension(self) -> &'static str {
        "adf"
    }
}

/// What a volume's directory handling supports, beyond the plain
/// original.
///
/// This is not a set of independent flags, which is the easy mistake: bits
/// 1 and 2 of the `DOS\x0N` tag are one two-bit number, so a volume is
/// plain, *or* international, *or* dircache, *or* long-name -- and each of
/// the later three implies the ones before it. AmigaDOS agrees by
/// inspection rather than by masking: `is_dircache` is true for exactly
/// DOS4 and DOS5.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Variant {
    /// The original. Case folding has the accented-character bug.
    #[default]
    Plain,
    /// International case folding, which fixes it. Needs Kickstart 2.0.
    Intl,
    /// Directory cache blocks, for faster listing; international too.
    /// Needs Kickstart 3.0.
    DirCache,
    /// Long file names, and international. Needs a filesystem that knows
    /// DOS6/DOS7, which no stock Kickstart provides.
    LongName,
}

impl Variant {
    pub const ALL: [Variant; 4] = [
        Variant::Plain,
        Variant::Intl,
        Variant::DirCache,
        Variant::LongName,
    ];

    /// The two-bit field this occupies in the tag.
    fn bits(self) -> u32 {
        match self {
            Variant::Plain => 0,
            Variant::Intl => 1,
            Variant::DirCache => 2,
            Variant::LongName => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Variant::Plain => "Plain",
            Variant::Intl => "International",
            Variant::DirCache => "Dir cache",
            Variant::LongName => "Long names",
        }
    }

    /// Whether the DOS type folds case the international way.
    ///
    /// True of the intl variant, and of dircache and longname *whether or
    /// not* the intl bit is set: DOS4 is `ofs+intl+dircache` and DOS6 is
    /// `ofs+intl+longname`, so international is not an option alongside
    /// them but a property they carry. This is the rule AmigaDOS itself
    /// applies (and the one amitools' `is_intl` states), and the reason
    /// the field is two bits rather than three flags.
    pub fn is_intl(self) -> bool {
        !matches!(self, Variant::Plain)
    }

    /// Whether the two directory schemes can be held at once: they cannot,
    /// being two values of one two-bit field.
    pub fn is_dircache(self) -> bool {
        matches!(self, Variant::DirCache)
    }

    pub fn is_longname(self) -> bool {
        matches!(self, Variant::LongName)
    }
}

/// The AmigaDOS filesystem a volume is formatted with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileSystem {
    /// Fast filesystem: data blocks carry no header, so they hold 512
    /// bytes rather than 488. Kickstart 1.3 can read FFS from a hard
    /// drive but not from a floppy, which is why OFS is still the safe
    /// choice for a disk meant to boot anywhere.
    pub ffs: bool,
    pub variant: Variant,
}

impl FileSystem {
    pub const OFS: FileSystem = FileSystem {
        ffs: false,
        variant: Variant::Plain,
    };
    pub const FFS: FileSystem = FileSystem {
        ffs: true,
        variant: Variant::Plain,
    };

    /// The `DOS\x0N` longword this filesystem is tagged with: the variant
    /// in bits 1 and 2, the fast filesystem in bit 0.
    pub fn dos_type(self) -> u32 {
        0x444F5300 | (self.variant.bits() << 1) | u32::from(self.ffs)
    }

    /// How the picker names it, in the `DOSn` terms the Amiga world uses.
    pub fn label(self) -> String {
        format!(
            "DOS{} ({}{})",
            self.variant.bits() * 2 + u32::from(self.ffs),
            if self.ffs { "FFS" } else { "OFS" },
            match self.variant {
                Variant::Plain => "",
                Variant::Intl => " + intl",
                Variant::DirCache => " + dircache",
                Variant::LongName => " + longname",
            }
        )
    }

    /// Every tag, DOS0 through DOS7, in that order.
    pub fn all() -> impl Iterator<Item = FileSystem> {
        Variant::ALL
            .into_iter()
            .flat_map(|variant| [false, true].map(|ffs| FileSystem { ffs, variant }))
    }
}

/// What to make a floppy image into.
#[derive(Debug, Clone)]
pub struct FloppySpec {
    pub density: Density,
    pub container: Container,
    /// `None` leaves the image blank: no boot block, no filesystem. An
    /// unformatted disk, which the guest can format itself.
    pub filesystem: Option<FileSystem>,
    /// Write boot code, so the disk boots rather than merely mounting. A
    /// formatted disk with no boot code is what a save disk is.
    pub bootable: bool,
    /// Volume name, as `Workbench` or `Empty` would appear on the desktop.
    pub label: String,
}

impl Default for FloppySpec {
    fn default() -> Self {
        Self {
            density: Density::Dd,
            container: Container::Adf,
            filesystem: Some(FileSystem::OFS),
            bootable: false,
            label: "Empty".to_string(),
        }
    }
}

/// The pretend cylinders/surfaces/sectors a drive reports.
///
/// Nothing physical depends on these -- an image has no platters -- but the
/// Rigid Disk Block states partitions in *cylinders*, so the geometry sets
/// the granularity a partition can start and end on, and it is what the
/// Amiga's filesystem sees when it asks the device about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub cylinders: u32,
    pub surfaces: u32,
    pub sectors: u32,
}

impl Geometry {
    /// Blocks in one cylinder, the unit a partition is measured in.
    pub fn cylinder_blocks(self) -> u64 {
        u64::from(self.surfaces) * u64::from(self.sectors)
    }

    pub fn blocks(self) -> u64 {
        self.cylinder_blocks() * u64::from(self.cylinders)
    }

    pub fn bytes(self) -> u64 {
        self.blocks() * BLOCK_BYTES as u64
    }

    /// A geometry holding at least `bytes`, in the shape Copperline's own
    /// hardfile support already synthesizes: 16 surfaces of 32 sectors, which
    /// is a 256 KB cylinder. Round up, so the image is never smaller than
    /// asked for.
    ///
    /// The alternative convention -- 63 sectors with surfaces stepped by size
    /// tier, as PC BIOS geometry does -- exists, and real drives report it.
    /// It is not used here: an image is not a real drive, a 256 KB cylinder
    /// gives finer partition granularity than a 1 MB one, and matching what
    /// this emulator already writes for plain hardfiles means an image made
    /// here and a hardfile grown here describe their partitions the same
    /// way.
    pub fn for_size(bytes: u64) -> Geometry {
        let surfaces = crate::harddrive::RDB_HEADS;
        let sectors = crate::harddrive::RDB_SPT;
        let cyl_blocks = u64::from(surfaces) * u64::from(sectors);
        let blocks = bytes.div_ceil(BLOCK_BYTES as u64);
        // A drive with no partition table may be any size the host will
        // hold; one with a table is checked when it is written, where
        // refusing can say so, rather than quietly coming out smaller than
        // was asked for.
        let cylinders = blocks.div_ceil(cyl_blocks).clamp(2, u32::MAX as u64);
        Geometry {
            cylinders: cylinders as u32,
            surfaces,
            sectors,
        }
    }
}

/// How a hard drive image is partitioned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Partitioning {
    /// A Rigid Disk Block at the front, then one partition. What a real
    /// Amiga writes, and what makes the image self-describing: any machine
    /// or emulator reads the partition table off the disk itself.
    #[default]
    Rdb,
    /// No partition table at all. With a filesystem it is the classic
    /// single-partition hardfile, which an emulator mounts by being told
    /// its geometry; with none it is a blank drive, exactly like fitting a
    /// brand new mechanism -- the Amiga's own HDToolBox partitions and
    /// formats it.
    None,
}

impl Partitioning {
    pub const ALL: [Partitioning; 2] = [Partitioning::Rdb, Partitioning::None];

    pub fn label(self) -> &'static str {
        match self {
            Partitioning::Rdb => "RDB",
            Partitioning::None => "None",
        }
    }
}

/// What to make a hard drive image into.
#[derive(Debug, Clone)]
pub struct HardSpec {
    /// Size asked for, in bytes. The image comes out at the next whole
    /// cylinder, never smaller.
    pub bytes: u64,
    /// `None` derives a geometry from the size.
    pub geometry: Option<Geometry>,
    pub partitioning: Partitioning,
    /// `None` leaves the volume unformatted for the guest to format.
    pub filesystem: Option<FileSystem>,
    /// The device the partition mounts as. Ignored without a partition
    /// table, where the emulator names the mount instead.
    pub device: String,
    /// Volume name, once formatted.
    pub label: String,
    /// Whether the partition offers itself as a boot candidate.
    pub bootable: bool,
    /// Where it ranks among boot candidates (`de_BootPri`): the higher of
    /// two bootable partitions goes first. Ignored when not bootable.
    pub boot_pri: i8,
    /// Blocks at the front of the partition the filesystem never uses.
    /// [`RESERVED_BLOCKS`] unless there is a reason to say otherwise.
    pub reserved: u32,
    /// What the drive says it is: the three strings HDToolBox shows as
    /// Drive and Type. `None` lets the drive name itself from its size.
    pub identity: Option<crate::harddrive::RdbIdentity>,
    /// Mark the file itself read-only on this computer once written, so
    /// nothing can change the image by accident. Not an Amiga flag -- the
    /// Rigid Disk Block has no such thing -- but the host's own.
    pub read_only: bool,
    /// Leave the file's unwritten blocks as holes, rather than claiming
    /// the whole of it on the host up front.
    ///
    /// The image's *contents* are the same either way -- a hole reads as
    /// zeros, so writing either to a card gives identical bytes. What
    /// differs is when the space is taken. Sparse makes a large image
    /// instantly and costs nothing until it is used, which is what almost
    /// everyone wants; the price is that a host drive with less room than
    /// the image claims fails part-way through a *guest* write rather than
    /// here, and a file filled in piecemeal can end up fragmented.
    /// Claiming it up front trades the wait for both.
    pub sparse: bool,
}

impl Default for HardSpec {
    fn default() -> Self {
        Self {
            bytes: 100 * 1024 * 1024,
            geometry: None,
            partitioning: Partitioning::Rdb,
            filesystem: Some(FileSystem::FFS),
            device: "DH0".to_string(),
            label: "Empty".to_string(),
            bootable: true,
            boot_pri: 0,
            reserved: RESERVED_BLOCKS,
            identity: None,
            read_only: false,
            sparse: true,
        }
    }
}

/// What was written, for the line the launcher reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub bytes: u64,
    /// The geometry the image describes, for a hard drive.
    pub geometry: Option<Geometry>,
}

// --- block plumbing -------------------------------------------------------

fn put_long(block: &mut [u8], long: usize, value: u32) {
    block[long * 4..long * 4 + 4].copy_from_slice(&value.to_be_bytes());
}

fn get_long(block: &[u8], long: usize) -> u32 {
    u32::from_be_bytes(block[long * 4..long * 4 + 4].try_into().expect("4 bytes"))
}

/// The AmigaDOS block checksum: with the checksum field zeroed, the block's
/// longs sum to zero, so the field holds the two's complement of the rest.
/// Every structure here uses it -- only the field's position differs.
fn checksum(block: &mut [u8], chk_long: usize) {
    put_long(block, chk_long, 0);
    let mut sum = 0u32;
    for i in 0..BLOCK_LONGS {
        sum = sum.wrapping_add(get_long(block, i));
    }
    put_long(block, chk_long, 0u32.wrapping_sub(sum));
}

/// A BCPL string: a length byte, then the characters, in a fixed field.
/// The rest of the field is cleared, so a shorter name never leaves the
/// tail of a longer one behind it.
fn put_bstr(block: &mut [u8], long: usize, max: usize, s: &str) {
    let bytes: Vec<u8> = s.bytes().take(max).collect();
    let at = long * 4;
    block[at..at + 1 + max].fill(0);
    block[at] = bytes.len() as u8;
    block[at + 1..at + 1 + bytes.len()].copy_from_slice(&bytes);
}

/// Seconds since the Amiga epoch, 1 January 1978, split the way a
/// timestamp field wants: whole days, minutes past midnight, and ticks of a
/// fiftieth of a second.
fn amiga_timestamp() -> (u32, u32, u32) {
    const EPOCH_1978: u64 = 252_460_800;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(EPOCH_1978)
        .saturating_sub(EPOCH_1978);
    let days = secs / 86_400;
    let rest = secs % 86_400;
    (days as u32, (rest / 60) as u32, ((rest % 60) * 50) as u32)
}

fn put_timestamp(block: &mut [u8], long: usize, ts: (u32, u32, u32)) {
    put_long(block, long, ts.0);
    put_long(block, long + 1, ts.1);
    put_long(block, long + 2, ts.2);
}

/// The boot code AmigaDOS puts on a bootable disk: open `dos.library` and
/// hand its resident tag back, which is what strap then runs. The two
/// variants differ because a fast-filesystem disk has to bring the
/// filesystem in from `expansion.library` first.
///
/// These are the bytes every Amiga's `install` command writes and every
/// emulator reproduces; the checksum over them is recomputed here rather
/// than baked in, so a mistake shows up as an unbootable disk rather than a
/// silently wrong constant.
const BOOT_CODE_OFS: [u8; 49] = [
    0x43, 0xFA, 0x00, 0x18, 0x4E, 0xAE, 0xFF, 0xA0, 0x4A, 0x80, 0x67, 0x0A, 0x20, 0x40, 0x20, 0x68,
    0x00, 0x16, 0x70, 0x00, 0x4E, 0x75, 0x70, 0xFF, 0x60, 0xFA, 0x64, 0x6F, 0x73, 0x2E, 0x6C, 0x69,
    0x62, 0x72, 0x61, 0x72, 0x79, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00,
];
const BOOT_CODE_FFS: [u8; 84] = [
    0x43, 0xFA, 0x00, 0x3E, 0x70, 0x25, 0x4E, 0xAE, 0xFD, 0xD8, 0x4A, 0x80, 0x67, 0x0C, 0x22, 0x40,
    0x08, 0xE9, 0x00, 0x06, 0x00, 0x22, 0x4E, 0xAE, 0xFE, 0x62, 0x43, 0xFA, 0x00, 0x18, 0x4E, 0xAE,
    0xFF, 0xA0, 0x4A, 0x80, 0x67, 0x0A, 0x20, 0x40, 0x20, 0x68, 0x00, 0x16, 0x70, 0x00, 0x4E, 0x75,
    0x70, 0xFF, 0x4E, 0x75, 0x64, 0x6F, 0x73, 0x2E, 0x6C, 0x69, 0x62, 0x72, 0x61, 0x72, 0x79, 0x00,
    0x65, 0x78, 0x70, 0x61, 0x6E, 0x73, 0x69, 0x6F, 0x6E, 0x2E, 0x6C, 0x69, 0x62, 0x72, 0x61, 0x72,
    0x79, 0x00, 0x00, 0x00,
];

/// The boot block: the `DOS` tag and its flags, and on a bootable volume
/// the code that loads the filesystem.
///
/// The checksum spans both boot blocks on a real volume, but the boot code
/// written here fits the first, and the second stays zero -- which
/// contributes nothing to a sum. An unformatted volume gets no boot block
/// at all, so this is only reached when there is a filesystem to tag.
fn boot_block(fs: FileSystem, bootable: bool, root: u64) -> [u8; BLOCK_BYTES] {
    let mut b = [0u8; BLOCK_BYTES];
    put_long(&mut b, 0, fs.dos_type());
    if bootable {
        put_long(&mut b, 2, root as u32);
        let code: &[u8] = if fs.ffs {
            &BOOT_CODE_FFS
        } else {
            &BOOT_CODE_OFS
        };
        b[12..12 + code.len()].copy_from_slice(code);
        // The boot block's own checksum carries the end-around carry that
        // the block checksum does not: it is the one AmigaDOS structure
        // summed as ones' complement.
        put_long(&mut b, 1, 0);
        let mut sum = 0u32;
        for i in 0..BLOCK_LONGS {
            let (next, carry) = sum.overflowing_add(get_long(&b, i));
            sum = if carry { next + 1 } else { next };
        }
        put_long(&mut b, 1, !sum);
    }
    b
}

/// Somewhere numbered blocks can be put. A floppy is built whole in memory
/// and a hard drive is written through a file handle, but a volume is laid
/// out the same way either side of that difference.
trait Blocks {
    fn put(&mut self, block: u64, data: &[u8]) -> io::Result<()>;
}

/// The whole image in memory, for a floppy.
struct Memory<'a>(&'a mut [u8]);

impl Blocks for Memory<'_> {
    fn put(&mut self, block: u64, data: &[u8]) -> io::Result<()> {
        let at = block as usize * BLOCK_BYTES;
        self.0[at..at + BLOCK_BYTES].copy_from_slice(data);
        Ok(())
    }
}

/// A window on a file starting at some block, for a hard drive partition.
struct FileWindow<'a> {
    file: &'a mut File,
    first: u64,
}

impl Blocks for FileWindow<'_> {
    fn put(&mut self, block: u64, data: &[u8]) -> io::Result<()> {
        self.file
            .seek(SeekFrom::Start((self.first + block) * BLOCK_BYTES as u64))?;
        self.file.write_all(data)
    }
}

/// Write an empty AmigaDOS volume over `blocks` blocks: the boot block, the
/// root block, and a bitmap marking everything but those free.
///
/// Block numbers are the volume's own, so this serves a floppy (where they
/// are the image's) and a partition (where they are relative to where it
/// starts) without knowing which it is writing.
fn format_volume(
    out: &mut dyn Blocks,
    blocks: u64,
    reserved: u64,
    fs: FileSystem,
    bootable: bool,
    label: &str,
) -> io::Result<()> {
    // A volume numbers its own blocks in 32-bit fields -- the root block's
    // bitmap pointers, every file header -- so past this the pointers
    // written below would silently truncate and address the wrong blocks.
    if blocks > MAX_BLOCKS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "an AmigaDOS volume cannot be larger than {} -- \
                 leave it unformatted and partition it on the Amiga",
                crate::config::format_size(MAX_RDB_BYTES as usize)
            ),
        ));
    }
    // Two reserved blocks is the floor because the boot block is two blocks
    // long: with fewer, the second half of it would be inside the bitmap and
    // the filesystem would hand it out. The ceiling keeps the root block --
    // halfway through the volume -- outside the reserved run, which every
    // subtraction below assumes.
    if reserved < u64::from(RESERVED_BLOCKS) || reserved > blocks / 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{reserved} reserved blocks does not fit a {blocks}-block volume \
                 (needs at least {RESERVED_BLOCKS}, at most {})",
                blocks / 2
            ),
        ));
    }
    // AmigaDOS puts the root block in the middle of the volume, so a seek
    // to any file is at most half a disk away. Halfway through the whole
    // volume, not through its usable part: the reserved blocks count, and
    // every tool that reads a volume locates the root this way rather than
    // trusting the pointer the boot block carries.
    let root = blocks / 2;
    out.put(0, &boot_block(fs, bootable, root))?;
    // The boot block is two blocks long; any further reserved blocks are
    // left as they lie.
    out.put(1, &[0u8; BLOCK_BYTES])?;

    // --- the bitmap ---
    // One bit per block outside the reserved ones, set when the block is
    // free. Bits are packed into big-endian longs, least significant bit
    // first, and each bitmap block spends its first long on a checksum.
    let bits = blocks - reserved;
    let longs = bits.div_ceil(32);
    let longs_per_block = (BLOCK_LONGS - 1) as u64;
    let bitmap_blocks = longs.div_ceil(longs_per_block);
    // The root block holds 25 bitmap pointers; past that they continue in
    // extension blocks, each spending its last long on the next one.
    const PTRS_IN_ROOT: u64 = 25;
    let ptrs_per_ext = (BLOCK_LONGS - 1) as u64;
    let ext_blocks = bitmap_blocks
        .saturating_sub(PTRS_IN_ROOT)
        .div_ceil(ptrs_per_ext);

    // Everything the volume's own structure occupies sits directly after
    // the root block, in the order the root block will point at it.
    let first_ext = root + 1;
    let first_bitmap = first_ext + ext_blocks;
    let used: Vec<u64> = std::iter::once(root)
        .chain(first_ext..first_ext + ext_blocks)
        .chain(first_bitmap..first_bitmap + bitmap_blocks)
        .collect();

    // Build the bitmap as one run of bits, then cut it into blocks.
    let mut bits_data = vec![0xFFu8; (longs * 4) as usize];
    // Bits past the end of the volume must read as allocated, or the
    // filesystem will hand out blocks that are not there.
    for bit in bits..longs * 32 {
        clear_bit(&mut bits_data, bit);
    }
    for blk in &used {
        clear_bit(&mut bits_data, blk - reserved);
    }
    for (i, chunk) in bits_data
        .chunks(longs_per_block as usize * 4)
        .enumerate()
        .take(bitmap_blocks as usize)
    {
        let mut b = [0u8; BLOCK_BYTES];
        b[4..4 + chunk.len()].copy_from_slice(chunk);
        checksum(&mut b, 0);
        out.put(first_bitmap + i as u64, &b)?;
    }

    // --- bitmap extension blocks ---
    // Each holds pointers to bitmap blocks and then the next extension.
    for i in 0..ext_blocks {
        let mut b = [0u8; BLOCK_BYTES];
        let from = PTRS_IN_ROOT + i * ptrs_per_ext;
        for slot in 0..ptrs_per_ext {
            let which = from + slot;
            if which >= bitmap_blocks {
                break;
            }
            put_long(&mut b, slot as usize, (first_bitmap + which) as u32);
        }
        let next = if i + 1 < ext_blocks {
            (first_ext + i + 1) as u32
        } else {
            0
        };
        put_long(&mut b, BLOCK_LONGS - 1, next);
        // An extension block carries no checksum: it is all pointers.
        out.put(first_ext + i, &b)?;
    }

    // --- the root block ---
    let mut b = [0u8; BLOCK_BYTES];
    let ts = amiga_timestamp();
    put_long(&mut b, 0, 2); // T_SHORT
    put_long(&mut b, 3, (BLOCK_LONGS - 56) as u32); // hash table size
    put_long(&mut b, BLOCK_LONGS - 50, 0xFFFF_FFFF); // bitmap is valid
    for slot in 0..PTRS_IN_ROOT {
        if slot >= bitmap_blocks {
            break;
        }
        put_long(
            &mut b,
            BLOCK_LONGS - 49 + slot as usize,
            (first_bitmap + slot) as u32,
        );
    }
    if ext_blocks > 0 {
        put_long(&mut b, BLOCK_LONGS - 24, first_ext as u32);
    }
    put_timestamp(&mut b, BLOCK_LONGS - 23, ts); // last modified
    put_bstr(&mut b, BLOCK_LONGS - 20, 30, label);
    put_timestamp(&mut b, BLOCK_LONGS - 10, ts); // last altered
    put_timestamp(&mut b, BLOCK_LONGS - 7, ts); // created
    put_long(&mut b, BLOCK_LONGS - 1, 1); // ST_ROOT
    checksum(&mut b, 5);
    out.put(root, &b)?;
    Ok(())
}

/// Mark a block allocated. Bit `n` lives in long `n / 32`, at bit `n % 32`
/// counting from the least significant end of that big-endian long -- so in
/// byte terms it is the long's byte `3 - (n % 32) / 8`.
fn clear_bit(data: &mut [u8], bit: u64) {
    let long = (bit / 32) as usize;
    let within = (bit % 32) as u32;
    let at = long * 4;
    if at + 4 > data.len() {
        return;
    }
    let value = u32::from_be_bytes(data[at..at + 4].try_into().expect("4 bytes"));
    data[at..at + 4].copy_from_slice(&(value & !(1 << within)).to_be_bytes());
}

// --- floppies -------------------------------------------------------------

/// Ask the host to leave a file's unwritten extents as holes.
///
/// Setting the length of a file is enough on Unix: the tail reads as zeros
/// and nothing is committed until it is written to. NTFS is the other way
/// round -- a file is dense unless it has been marked sparse -- so a 100 GB
/// image would otherwise be 100 GB of real writes on Windows however the
/// Sparse image box is set.
///
/// Best effort, and deliberately silent: a filesystem that cannot do it
/// (FAT32 on a card, say) still gets a correct image, just a fully
/// allocated one, and the write fails with the host's own out-of-space
/// error if there is not room for it.
#[cfg(windows)]
fn mark_sparse(file: &File) {
    use std::os::windows::io::AsRawHandle;
    // FSCTL_SET_SPARSE. No input or output buffer: the call is the request.
    const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
    let mut returned = 0u32;
    unsafe {
        windows_sys::Win32::System::IO::DeviceIoControl(
            file.as_raw_handle() as _,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(not(windows))]
fn mark_sparse(_file: &File) {}

/// Write an image beside its destination, and move it into place only once
/// it is whole.
///
/// A half-written image is worse than none: it sits there under the name
/// the user chose, the right length, and mounts as garbage. Writing
/// straight to that name would also destroy whatever was already there
/// before knowing whether the new one can be finished at all -- and the
/// write runs on a worker, so quitting part-way through is an ordinary
/// thing to do rather than a crash.
///
/// So the bytes go to a sibling of the destination -- same directory,
/// therefore same filesystem, therefore the rename at the end is atomic --
/// and nothing but that sibling is ever removed. An interrupted write
/// leaves the temporary file behind and the destination untouched.
fn writing_image<T>(path: &Path, write: impl FnOnce(File) -> io::Result<T>) -> io::Result<T> {
    // A read-only file is one somebody meant to keep -- quite possibly an
    // image this workshop marked itself. Renaming over it would succeed on
    // Unix, where permission to replace a file belongs to its directory
    // rather than to the file, so it is refused here instead.
    if std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "that file is read only",
        ));
    }
    let temp = partial_path(path);
    let file = File::create(&temp)?;
    let made = write(file).and_then(|made| {
        std::fs::rename(&temp, path)?;
        Ok(made)
    });
    if made.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    made
}

/// Where an image is assembled before it is moved into place: beside the
/// destination, named after it, and out of the way of a real image.
fn partial_path(path: &Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_string());
    path.with_file_name(format!(".{name}.copperline-partial"))
}

/// How large the finished file will be, which for the extended container
/// is the disk plus the records that describe it.
pub fn floppy_bytes(spec: &FloppySpec) -> u64 {
    spec.density.bytes() + spec.container.overhead(spec.density)
}

/// Write a fresh floppy image.
pub fn create_floppy(path: &Path, spec: &FloppySpec) -> io::Result<Created> {
    // Built whole in memory first: a floppy is small, and a volume that
    // cannot be laid out should fail before anything is on disk.
    let blocks = spec.density.blocks();
    let mut image = vec![0u8; (blocks * BLOCK_BYTES as u64) as usize];
    if let Some(fs) = spec.filesystem {
        format_volume(
            &mut Memory(&mut image),
            blocks,
            u64::from(RESERVED_BLOCKS),
            fs,
            spec.bootable,
            &spec.label,
        )?;
    }
    writing_image(path, |mut file| {
        match spec.container {
            Container::Adf => file.write_all(&image)?,
            Container::ExtendedAdf => write_extended_adf(&mut file, &image, spec.density)?,
        }
        file.flush()?;
        Ok(Created {
            bytes: file.metadata()?.len(),
            geometry: None,
        })
    })
}

/// The `UAE-1ADF` container: an eight-byte tag, a track count, a table of
/// per-track records, then the tracks themselves.
///
/// Each record is type, length and bit length. Type 1 is a raw MFM track and
/// type 0 an AmigaDOS one; a freshly made disk holds sectors, so every track
/// is written as AmigaDOS with its bit length set, which is what a reader
/// uses to tell a decodable track from a captured one.
fn write_extended_adf(file: &mut File, image: &[u8], density: Density) -> io::Result<()> {
    let track_bytes = density.sectors_per_track() as usize * BLOCK_BYTES;
    let tracks = image.len() / track_bytes;
    file.write_all(b"UAE-1ADF")?;
    let mut header = [0u8; 4];
    header[2..4].copy_from_slice(&(tracks as u16).to_be_bytes());
    file.write_all(&header)?;
    for _ in 0..tracks {
        let mut record = [0u8; 12];
        // Type 0: an AmigaDOS track, stored as its sectors.
        record[4..8].copy_from_slice(&(track_bytes as u32).to_be_bytes());
        record[8..12].copy_from_slice(&((track_bytes * 8) as u32).to_be_bytes());
        file.write_all(&record)?;
    }
    file.write_all(image)
}

// --- hard drives ----------------------------------------------------------

/// Write a fresh hard drive image.
///
/// A sparse image is created at its full length with only the blocks that
/// carry structure written, so the host filesystem is left to fill the rest
/// in as it is used. A blank unpartitioned drive therefore costs almost
/// nothing, which is the point: it is a drive with nothing on it yet.
/// Clearing [`HardSpec::sparse`] walks the whole file instead, which takes
/// as long as writing that many bytes takes.
pub fn create_hard(path: &Path, spec: &HardSpec) -> io::Result<Created> {
    let geometry = spec
        .geometry
        .unwrap_or_else(|| Geometry::for_size(spec.bytes));
    // A Rigid Disk Block names blocks in 32-bit fields, so past 2 TiB it
    // cannot describe the drive it is on. Truncating the figures would
    // produce a table that reads as valid and points at the wrong place.
    if spec.partitioning == Partitioning::Rdb && geometry.blocks() > MAX_BLOCKS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "a partition table cannot describe more than {} -- \
                 make it smaller, or choose no partitioning",
                crate::config::format_size(MAX_RDB_BYTES as usize)
            ),
        ));
    }
    // The table lives in cylinder 0 and the partition starts at cylinder 1,
    // so a drive needs a second cylinder to have anything to partition --
    // and that first cylinder needs room for both the RDSK block and the
    // PART block after it. A one-block cylinder would put PART at the
    // partition's own first block, where formatting the volume writes over
    // it and an unformatted image points the partition at its own table.
    if spec.partitioning == Partitioning::Rdb
        && (geometry.cylinders < 2 || geometry.cylinder_blocks() < 2)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "a partition table needs two cylinders of at least two blocks; \
                 {} cylinders of {} x {} is too small",
                geometry.cylinders, geometry.surfaces, geometry.sectors
            ),
        ));
    }
    // A cylinder is stated in 32-bit fields too, and every drive numbers
    // at least two of them -- so for a partitioned drive the check above
    // has already caught this. It is here for the unpartitioned case,
    // where saying which figures are impossible beats letting the host
    // report that it cannot make a file that size.
    if geometry.cylinder_blocks() > u32::MAX as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} surfaces of {} sectors is more than one cylinder can hold",
                geometry.surfaces, geometry.sectors
            ),
        ));
    }
    let made = writing_image(path, |file| write_hard(file, spec, geometry))?;
    if spec.read_only {
        // Once it is in place and finished: a read-only file cannot be
        // renamed over anything on Windows, and there is nothing to protect
        // until the image is whole.
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(made)
}

fn write_hard(mut file: File, spec: &HardSpec, geometry: Geometry) -> io::Result<Created> {
    if spec.sparse {
        // Before the length is set, which is the point at which a
        // filesystem decides whether to commit the space.
        mark_sparse(&file);
    }
    file.set_len(geometry.bytes())?;
    if !spec.sparse {
        // Walk the file writing zeros, so the host commits the space now
        // and says so now if it has not got it.
        let zeros = vec![0u8; 1 << 20];
        let mut left = geometry.bytes();
        file.seek(SeekFrom::Start(0))?;
        while left > 0 {
            let chunk = left.min(zeros.len() as u64) as usize;
            file.write_all(&zeros[..chunk])?;
            left -= chunk as u64;
        }
    }

    match spec.partitioning {
        Partitioning::Rdb => {
            let cyl_blocks = geometry.cylinder_blocks();
            // The RDB owns cylinder 0; the partition takes the rest, as
            // HDToolBox lays a drive out.
            let low_cyl = 1;
            let high_cyl = geometry.cylinders - 1;
            let dos_type = spec.filesystem.unwrap_or(FileSystem::FFS).dos_type();
            write_rdb(
                &mut file,
                geometry,
                &spec.device,
                dos_type,
                spec.bootable,
                spec.boot_pri,
                spec.reserved,
                spec.identity
                    .clone()
                    .unwrap_or_else(|| crate::harddrive::default_rdb_identity(geometry.bytes())),
            )?;
            if let Some(fs) = spec.filesystem {
                let blocks = u64::from(high_cyl - low_cyl + 1) * cyl_blocks;
                format_volume(
                    &mut FileWindow {
                        file: &mut file,
                        first: u64::from(low_cyl) * cyl_blocks,
                    },
                    blocks,
                    u64::from(spec.reserved),
                    fs,
                    // A hard drive partition boots from its Rigid Disk
                    // Block, not from boot code on the volume: the boot
                    // flag lives in the partition entry. Writing floppy
                    // boot code here would only be misleading.
                    false,
                    &spec.label,
                )?;
            }
        }
        Partitioning::None => {
            if let Some(fs) = spec.filesystem {
                format_volume(
                    &mut FileWindow {
                        file: &mut file,
                        first: 0,
                    },
                    geometry.blocks(),
                    u64::from(spec.reserved),
                    fs,
                    false,
                    &spec.label,
                )?;
            }
        }
    }
    file.flush()?;
    Ok(Created {
        bytes: geometry.bytes(),
        geometry: Some(geometry),
    })
}

/// Write the Rigid Disk Block and its one partition entry.
///
/// The layout matches what this emulator already synthesizes for plain
/// hardfiles (see [`crate::harddrive`]), so an image made here and a
/// hardfile grown here describe themselves identically.
fn write_rdb(
    file: &mut File,
    geometry: Geometry,
    device: &str,
    dos_type: u32,
    bootable: bool,
    boot_pri: i8,
    reserved: u32,
    identity: crate::harddrive::RdbIdentity,
) -> io::Result<()> {
    let cyl_blocks = geometry.cylinder_blocks();

    let mut rdsk = [0u8; BLOCK_BYTES];
    rdsk[0..4].copy_from_slice(b"RDSK");
    put_long(&mut rdsk, 1, 64); // size in longs
    put_long(&mut rdsk, 3, 7); // host id
    put_long(&mut rdsk, 4, BLOCK_BYTES as u32);
    put_long(&mut rdsk, 5, 0x17); // last disk, last LUN, last unit
    put_long(&mut rdsk, 6, !0); // no bad-block list
    put_long(&mut rdsk, 7, 1); // partition list starts at block 1
    put_long(&mut rdsk, 8, !0); // no filesystem headers
    put_long(&mut rdsk, 9, !0); // no drive init code
    for long in 10..16 {
        put_long(&mut rdsk, long, !0);
    }
    put_long(&mut rdsk, 16, geometry.cylinders);
    put_long(&mut rdsk, 17, geometry.sectors);
    put_long(&mut rdsk, 18, geometry.surfaces);
    put_long(&mut rdsk, 19, 1); // interleave
    put_long(&mut rdsk, 20, geometry.cylinders); // park cylinder
    put_long(&mut rdsk, 24, !0); // write precomp
    put_long(&mut rdsk, 25, !0); // reduced write current
    put_long(&mut rdsk, 26, 3); // step rate
    put_long(&mut rdsk, 32, 0); // RDB occupies blocks 0..
    put_long(&mut rdsk, 33, (cyl_blocks - 1) as u32); // ..to the end of cylinder 0
    put_long(&mut rdsk, 34, 1); // first partitionable cylinder
    put_long(&mut rdsk, 35, geometry.cylinders - 1);
    put_long(&mut rdsk, 36, cyl_blocks as u32);
    put_long(&mut rdsk, 38, (cyl_blocks - 1) as u32); // highest RDB block
    crate::harddrive::put_rdb_identity(&mut rdsk, &identity);
    checksum(&mut rdsk, 2);
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&rdsk)?;

    let mut part = [0u8; BLOCK_BYTES];
    part[0..4].copy_from_slice(b"PART");
    put_long(&mut part, 1, 64);
    put_long(&mut part, 3, 7); // host id
    put_long(&mut part, 4, !0); // no next partition
    put_long(&mut part, 5, u32::from(bootable)); // PBFB_BOOTABLE
    put_bstr(&mut part, 9, 31, device);
    // The DOS environment vector: how the filesystem should see the
    // partition. Sizes are in blocks, the span in cylinders.
    put_long(&mut part, 32, 16); // entries that follow
    put_long(&mut part, 33, (BLOCK_LONGS) as u32); // longs per block
    put_long(&mut part, 34, 0); // sector origin
    put_long(&mut part, 35, geometry.surfaces);
    put_long(&mut part, 36, 1); // blocks per sector
    put_long(&mut part, 37, geometry.sectors);
    put_long(&mut part, 38, reserved);
    put_long(&mut part, 39, 0); // pre-allocated blocks
    put_long(&mut part, 40, 0); // interleave
    put_long(&mut part, 41, 1); // low cylinder
    put_long(&mut part, 42, geometry.cylinders - 1);
    put_long(&mut part, 43, 30); // buffers
    put_long(&mut part, 44, 0); // any memory for buffers
    put_long(&mut part, 45, 0x00FF_FFFF); // max transfer
    put_long(&mut part, 46, 0x7FFF_FFFE); // address mask
                                          // A partition that is not a boot candidate is ranked at the sentinel
                                          // the rest of Copperline uses for "never", so nothing ever strap-ranks
                                          // it however the flag is read.
    put_long(
        &mut part,
        47,
        if bootable {
            i32::from(boot_pri) as u32
        } else {
            i32::from(crate::config::BOOT_PRI_NEVER) as u32
        },
    );
    put_long(&mut part, 48, dos_type);
    checksum(&mut part, 2);
    file.seek(SeekFrom::Start(BLOCK_BYTES as u64))?;
    file.write_all(&part)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary file that takes itself away again, whether the test it
    /// belongs to passed, failed or panicked part-way through.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(what: &str) -> Scratch {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("after 1970")
                .as_nanos();
            Scratch(std::env::temp_dir().join(format!("copperline-diskimage-{nanos}-{what}")))
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn read(&self) -> Vec<u8> {
            std::fs::read(&self.0).expect("the image was written")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            // A read-only image has to be let go of before it can be
            // removed on the platforms that care.
            if let Ok(meta) = std::fs::metadata(&self.0) {
                let mut perms = meta.permissions();
                if perms.readonly() {
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                    let _ = std::fs::set_permissions(&self.0, perms);
                }
            }
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn block(image: &[u8], n: u64) -> &[u8] {
        let at = n as usize * BLOCK_BYTES;
        &image[at..at + BLOCK_BYTES]
    }

    /// The block checksum holds when the longs sum to zero -- which is the
    /// property AmigaDOS checks, so it is the property worth asserting
    /// rather than a recomputation of the same arithmetic.
    fn sums_to_zero(b: &[u8]) -> bool {
        let mut sum = 0u32;
        for i in 0..BLOCK_LONGS {
            sum = sum.wrapping_add(get_long(b, i));
        }
        sum == 0
    }

    /// The three things a DOS tag can say about itself, and which of them
    /// can be true together. Dircache and longname are two values of one
    /// field, so never both; and each is international in its own right,
    /// whether or not the intl bit happens to be set.
    #[test]
    fn a_dos_tag_is_international_by_implication() {
        for variant in Variant::ALL {
            assert!(
                !(variant.is_dircache() && variant.is_longname()),
                "{variant:?}: a tag cannot be both"
            );
            assert_eq!(
                variant.is_intl(),
                variant != Variant::Plain,
                "{variant:?}: everything but plain folds case the intl way"
            );
        }
        // Which is to say: DOS4..DOS7 are international although only
        // DOS2/DOS3 carry the bit that says so on its own.
        for fs in FileSystem::all() {
            let tag = fs.dos_type() & 0xFF;
            let bit_set = tag & 0b10 != 0;
            assert_eq!(fs.variant.is_intl(), bit_set || tag >= 4, "DOS{tag}");
        }
    }

    /// Bits 1 and 2 of the tag are one two-bit number, not two flags, so
    /// the eight tags run DOS0..DOS7 in order and nothing else is
    /// expressible.
    #[test]
    fn the_dos_tags_run_dos0_to_dos7_in_order() {
        let tags: Vec<u32> = FileSystem::all().map(FileSystem::dos_type).collect();
        assert_eq!(tags, (0..8).map(|n| 0x444F5300 | n).collect::<Vec<_>>());

        assert_eq!(FileSystem::OFS.dos_type(), 0x444F5300);
        assert_eq!(FileSystem::FFS.dos_type(), 0x444F5301);
        // Dircache is DOS4/DOS5 -- not DOS6, which is what treating the
        // two bits as independent international and dircache flags would
        // produce, and which is really the long-name tag.
        assert_eq!(
            FileSystem {
                ffs: false,
                variant: Variant::DirCache
            }
            .dos_type(),
            0x444F5304
        );
        assert_eq!(
            FileSystem {
                ffs: false,
                variant: Variant::LongName
            }
            .dos_type(),
            0x444F5306
        );
        // Each names itself after its own tag.
        for fs in FileSystem::all() {
            let n = fs.dos_type() & 0xFF;
            assert!(
                fs.label().starts_with(&format!("DOS{n}")),
                "{} does not name its own tag",
                fs.label()
            );
        }
    }

    #[test]
    fn a_blank_floppy_is_the_right_size_and_all_zero() {
        for density in Density::ALL {
            let s = Scratch::new("blank");
            let made = create_floppy(
                s.path(),
                &FloppySpec {
                    density,
                    filesystem: None,
                    ..Default::default()
                },
            )
            .expect("created");
            assert_eq!(made.bytes, density.bytes());
            assert!(
                s.read().iter().all(|b| *b == 0),
                "unformatted disk is dirty"
            );
        }
        // The sizes are the ones every tool expects, to the byte.
        assert_eq!(Density::Dd.bytes(), 901_120);
        assert_eq!(Density::Hd.bytes(), 1_802_240);
    }

    /// The layout WinUAE writes with folded-in constants, re-derived here:
    /// root at half the volume, bitmap right after it, and in the bitmap
    /// block the byte at offset 0x72 (DD) holding 0x3f, because bits 14 and
    /// 15 of long 27 are the root and bitmap blocks.
    #[test]
    fn a_formatted_floppy_matches_the_known_layout() {
        for (density, root, magic_at) in
            [(Density::Dd, 880u64, 0x72usize), (Density::Hd, 1760, 0xdc)]
        {
            let s = Scratch::new("format");
            create_floppy(
                s.path(),
                &FloppySpec {
                    density,
                    filesystem: Some(FileSystem::OFS),
                    bootable: false,
                    label: "Empty".into(),
                    ..Default::default()
                },
            )
            .expect("created");
            let image = s.read();

            let boot = block(&image, 0);
            assert_eq!(&boot[0..4], b"DOS\x00");

            let r = block(&image, root);
            assert_eq!(get_long(r, 0), 2, "root is not T_SHORT");
            assert_eq!(get_long(r, BLOCK_LONGS - 1), 1, "root is not ST_ROOT");
            assert_eq!(get_long(r, 3), 72, "hash table size");
            assert_eq!(get_long(r, BLOCK_LONGS - 50), 0xFFFF_FFFF, "bitmap flag");
            assert_eq!(
                get_long(r, BLOCK_LONGS - 49),
                (root + 1) as u32,
                "bitmap pointer"
            );
            assert!(sums_to_zero(r), "root checksum");
            // The volume name, as a BCPL string.
            assert_eq!(r[432], 5);
            assert_eq!(&r[433..438], b"Empty");

            let bm = block(&image, root + 1);
            assert!(sums_to_zero(bm), "bitmap checksum");
            assert_eq!(
                bm[magic_at],
                0x3f,
                "{}: root and bitmap are not the two allocated blocks",
                density.label()
            );
            // Everything else in the bitmap is free.
            let free: usize = bm[4..].iter().map(|b| b.count_ones() as usize).sum();
            let blocks = density.blocks();
            assert_eq!(
                free,
                (blocks - u64::from(RESERVED_BLOCKS) - 2) as usize,
                "{}: wrong number of free blocks",
                density.label()
            );
        }
    }

    #[test]
    fn a_bootable_floppy_carries_boot_code_that_checksums() {
        for fs in [FileSystem::OFS, FileSystem::FFS] {
            let s = Scratch::new("boot");
            create_floppy(
                s.path(),
                &FloppySpec {
                    filesystem: Some(fs),
                    bootable: true,
                    ..Default::default()
                },
            )
            .expect("created");
            let image = s.read();
            let boot = block(&image, 0);
            assert_eq!(get_long(boot, 0), fs.dos_type());
            assert_eq!(get_long(boot, 2), 880, "root pointer in boot block");
            assert_ne!(get_long(boot, 3), 0, "no boot code written");
            // The boot block sums with an end-around carry, and a valid one
            // comes out as all ones.
            let mut sum = 0u32;
            for i in 0..BLOCK_LONGS {
                let (next, carry) = sum.overflowing_add(get_long(boot, i));
                sum = if carry { next + 1 } else { next };
            }
            assert_eq!(sum, u32::MAX, "{}: boot checksum", fs.label());
        }
        // A disk that is formatted but not bootable -- a save disk -- has
        // the tag and nothing after it.
        let s = Scratch::new("savedisk");
        create_floppy(
            s.path(),
            &FloppySpec {
                filesystem: Some(FileSystem::OFS),
                bootable: false,
                ..Default::default()
            },
        )
        .expect("created");
        let boot = s.read()[..BLOCK_BYTES].to_vec();
        assert_eq!(&boot[0..4], b"DOS\x00");
        assert!(
            boot[4..].iter().all(|b| *b == 0),
            "boot code on a save disk"
        );
    }

    #[test]
    fn an_extended_adf_carries_a_track_table() {
        let s = Scratch::new("ext");
        create_floppy(
            s.path(),
            &FloppySpec {
                container: Container::ExtendedAdf,
                filesystem: Some(FileSystem::OFS),
                ..Default::default()
            },
        )
        .expect("created");
        let image = s.read();
        assert_eq!(&image[0..8], b"UAE-1ADF");
        let tracks = u16::from_be_bytes([image[10], image[11]]);
        assert_eq!(tracks, 160, "80 cylinders, two sides");
        // Every record describes an AmigaDOS track of one track's sectors.
        let track_bytes = 11 * BLOCK_BYTES as u32;
        for t in 0..tracks as usize {
            let at = 12 + t * 12;
            let record = &image[at..at + 12];
            assert_eq!(get_long(record, 0), 0, "track {t} is not AmigaDOS");
            assert_eq!(get_long(record, 1), track_bytes);
            assert_eq!(get_long(record, 2), track_bytes * 8);
        }
        // The sectors follow, and are the same volume a plain ADF holds.
        let data = &image[12 + tracks as usize * 12..];
        assert_eq!(data.len(), Density::Dd.bytes() as usize);
        assert_eq!(&data[0..4], b"DOS\x00");
    }

    #[test]
    fn a_geometry_covers_the_size_asked_for() {
        for bytes in [
            1024u64,
            10 * 1024 * 1024,
            100 * 1024 * 1024,
            2 * 1024 * 1024 * 1024,
        ] {
            let g = Geometry::for_size(bytes);
            assert!(g.bytes() >= bytes, "{bytes} rounded down to {}", g.bytes());
            // Never more than a cylinder of slack, once past the two-
            // cylinder floor a partitioned image needs (one for the RDB,
            // one to put a partition in).
            let cylinder = g.cylinder_blocks() * BLOCK_BYTES as u64;
            assert!(
                g.bytes() - bytes < cylinder || g.cylinders == 2,
                "{bytes} rounded up to {} with {} cylinders",
                g.bytes(),
                g.cylinders
            );
            assert_eq!(g.blocks(), g.bytes() / BLOCK_BYTES as u64);
        }
    }

    #[test]
    fn an_rdb_image_mounts_through_our_own_hardfile_parser() {
        let s = Scratch::new("rdb");
        let made = create_hard(
            s.path(),
            &HardSpec {
                bytes: 8 * 1024 * 1024,
                device: "DH0".into(),
                label: "Work".into(),
                filesystem: Some(FileSystem::FFS),
                ..Default::default()
            },
        )
        .expect("created");
        let geometry = made.geometry.expect("hard drives report a geometry");
        let image = s.read();

        let rdsk = block(&image, 0);
        assert_eq!(&rdsk[0..4], b"RDSK");
        assert!(sums_to_zero(rdsk), "RDSK checksum");
        assert_eq!(get_long(rdsk, 16), geometry.cylinders);
        assert_eq!(get_long(rdsk, 17), geometry.sectors);
        assert_eq!(get_long(rdsk, 18), geometry.surfaces);

        let part = block(&image, 1);
        assert_eq!(&part[0..4], b"PART");
        assert!(sums_to_zero(part), "PART checksum");
        assert_eq!(part[36], 3);
        assert_eq!(&part[37..40], b"DH0");
        assert_eq!(get_long(part, 48), FileSystem::FFS.dos_type());
        assert_eq!(get_long(part, 41), 1, "partition starts after the RDB");
        assert_eq!(get_long(part, 42), geometry.cylinders - 1);

        // The volume itself starts at the partition, not at the image.
        let first = u64::from(get_long(part, 41)) * geometry.cylinder_blocks();
        assert_eq!(&block(&image, first)[0..4], b"DOS\x01");

        // The strongest check available: the emulator's own hardfile
        // support has to recognise it, since that is what will mount it.
        let opened = crate::harddrive::HardDriveImage::open(s.path(), "DH0", "ide", None, 0)
            .expect("our own parser opens it");
        assert!(
            opened.has_own_rdb(),
            "our parser did not find the partition table we wrote, and \
             synthesized one over the top instead"
        );
    }

    #[test]
    fn a_refused_image_leaves_no_file_behind() {
        // A reserved count that cannot fit, and a drive too large for a
        // partition table: both are caught, and neither leaves the name the
        // user chose sitting on a half-written image.
        let s = Scratch::new("refused");
        let err = create_hard(
            s.path(),
            &HardSpec {
                bytes: 4 * 1024 * 1024,
                filesystem: Some(FileSystem::FFS),
                reserved: 90_000,
                ..Default::default()
            },
        )
        .expect_err("a reserved run larger than the volume is refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!s.path().exists(), "the refused image was left behind");

        let err = create_hard(
            s.path(),
            &HardSpec {
                bytes: MAX_RDB_BYTES + BLOCK_BYTES as u64,
                filesystem: None,
                ..Default::default()
            },
        )
        .expect_err("past 2 TB an RDB cannot describe the drive");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!s.path().exists());

        // The same size with no partition table is not turned away here:
        // nothing has to describe it in 32-bit fields. Whether the host can
        // hold a file that big is its own business -- a filesystem with no
        // sparse files (or a small disk under one) says so, and that is a
        // different answer from this module's refusal.
        let made = create_hard(
            s.path(),
            &HardSpec {
                bytes: MAX_RDB_BYTES + BLOCK_BYTES as u64,
                partitioning: Partitioning::None,
                filesystem: None,
                ..Default::default()
            },
        );
        match made {
            Ok(_) => assert!(s.path().exists()),
            Err(e) => assert_ne!(
                e.kind(),
                io::ErrorKind::InvalidInput,
                "an unpartitioned drive was refused for a limit it does not have"
            ),
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_file_we_could_not_even_open_is_left_alone() {
        // The cleanup only ever removes an image this module created. A
        // path that could not be opened holds somebody else's file, and
        // deleting it to report a failure would destroy their data.
        use std::os::unix::fs::PermissionsExt;
        let s = Scratch::new("notours");
        std::fs::write(s.path(), b"somebody else's file").unwrap();
        std::fs::set_permissions(s.path(), std::fs::Permissions::from_mode(0o400)).unwrap();

        let err = create_floppy(s.path(), &FloppySpec::default())
            .expect_err("a read-only path cannot be written");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(s.path().exists(), "their file was deleted");
        assert_eq!(std::fs::read(s.path()).unwrap(), b"somebody else's file");

        std::fs::set_permissions(s.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn a_failed_write_leaves_the_file_that_was_already_there() {
        // The destination is not touched until the image is whole: it may
        // hold an image the user still wants, and a write that fails --
        // or a session that quits -- must not take it with them.
        let s = Scratch::new("keepold");
        std::fs::write(s.path(), b"the image that was already there").unwrap();
        let err = create_hard(
            s.path(),
            &HardSpec {
                bytes: 4 * 1024 * 1024,
                filesystem: Some(FileSystem::FFS),
                reserved: 90_000,
                ..Default::default()
            },
        )
        .expect_err("a reserved run larger than the volume is refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read(s.path()).unwrap(),
            b"the image that was already there",
            "the old image was destroyed by a write that never succeeded"
        );
        // And nothing is left lying beside it either.
        assert!(!partial_path(s.path()).exists());

        // A write that does succeed replaces it.
        create_hard(
            s.path(),
            &HardSpec {
                bytes: 4 * 1024 * 1024,
                filesystem: Some(FileSystem::FFS),
                ..Default::default()
            },
        )
        .expect("created");
        assert_eq!(std::fs::metadata(s.path()).unwrap().len(), 4 * 1024 * 1024);
        assert!(!partial_path(s.path()).exists());
    }

    #[test]
    fn a_cylinder_too_small_to_hold_the_table_is_refused() {
        // The RDB owns cylinder 0 and the partition starts at cylinder 1,
        // so the first cylinder has to have room for RDSK *and* the PART
        // block after it. One block to a cylinder would put PART at the
        // partition's own first block, and formatting would write over it.
        let s = Scratch::new("tinycyl");
        for (cylinders, surfaces, sectors) in [(1000, 1, 1), (1, 16, 32), (1000, 2, 1)] {
            let made = create_hard(
                s.path(),
                &HardSpec {
                    geometry: Some(Geometry {
                        cylinders,
                        surfaces,
                        sectors,
                    }),
                    partitioning: Partitioning::Rdb,
                    filesystem: None,
                    ..Default::default()
                },
            );
            if surfaces * sectors >= 2 && cylinders >= 2 {
                made.expect("two cylinders of two blocks is the floor");
            } else {
                let err = made.expect_err("{cylinders}x{surfaces}x{sectors} cannot hold a table");
                assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            }
        }
    }

    #[test]
    fn an_extended_image_is_as_large_as_it_says_it_will_be() {
        // The container writes a tag, a track count and a record per track
        // around the sectors, so the file is larger than the disk -- and
        // the figure the launcher checks free space against has to be the
        // file's, not the disk's.
        for density in Density::ALL {
            for container in Container::ALL {
                let spec = FloppySpec {
                    density,
                    container,
                    ..Default::default()
                };
                let s = Scratch::new("extsize");
                let made = create_floppy(s.path(), &spec).expect("created");
                let on_disk = std::fs::metadata(s.path()).unwrap().len();
                assert_eq!(on_disk, made.bytes);
                assert_eq!(
                    floppy_bytes(&spec),
                    on_disk,
                    "{density:?}/{container:?}: predicted size"
                );
            }
        }
    }

    #[test]
    fn a_stated_reserved_count_reaches_both_the_table_and_the_bitmap() {
        // The Rigid Disk Block carries the figure, and the filesystem
        // inside the partition has to agree with it: reserved blocks are
        // outside the bitmap entirely, so a bitmap built for two while the
        // table says four hands out blocks the boot block is sitting on.
        let s = Scratch::new("reserved");
        create_hard(
            s.path(),
            &HardSpec {
                bytes: 8 * 1024 * 1024,
                device: "DH0".into(),
                label: "Work".into(),
                filesystem: Some(FileSystem::FFS),
                reserved: 5,
                ..Default::default()
            },
        )
        .expect("created");
        let image = s.read();

        let part = block(&image, 1);
        assert_eq!(get_long(part, 38), 5, "de_Reserved");

        // Every reserved block is marked in use, and the first block past
        // them is free. Bit `n` stands for block `n + reserved`.
        let geometry = Geometry::for_size(8 * 1024 * 1024);
        let first = u64::from(get_long(part, 41)) * geometry.cylinder_blocks();
        let root = block(
            &image,
            first
                + (u64::from(get_long(part, 42) - get_long(part, 41) + 1)
                    * geometry.cylinder_blocks())
                    / 2,
        );
        let bitmap = u64::from(get_long(root, BLOCK_LONGS - 49));
        let map = block(&image, first + bitmap);
        // Bit 0 of the map is the first non-reserved block, and nothing
        // before it is described at all.
        let long0 = get_long(map, 1);
        assert_eq!(long0 & 1, 1, "the block after the reserved run is free");
    }

    #[test]
    fn an_unpartitioned_blank_drive_is_entirely_empty() {
        let s = Scratch::new("blankhd");
        let made = create_hard(
            s.path(),
            &HardSpec {
                bytes: 4 * 1024 * 1024,
                partitioning: Partitioning::None,
                filesystem: None,
                ..Default::default()
            },
        )
        .expect("created");
        let image = s.read();
        assert_eq!(image.len() as u64, made.bytes);
        assert!(
            image.iter().all(|b| *b == 0),
            "a brand new drive has nothing on it"
        );
    }

    #[test]
    fn an_unpartitioned_formatted_drive_is_one_volume_from_block_zero() {
        let s = Scratch::new("hardfile");
        create_hard(
            s.path(),
            &HardSpec {
                bytes: 4 * 1024 * 1024,
                partitioning: Partitioning::None,
                filesystem: Some(FileSystem::FFS),
                label: "Spare".into(),
                ..Default::default()
            },
        )
        .expect("created");
        let image = s.read();
        assert_eq!(&image[0..4], b"DOS\x01");
        let blocks = image.len() as u64 / BLOCK_BYTES as u64;
        let root = block(&image, blocks / 2);
        assert!(sums_to_zero(root));
        assert_eq!(root[432], 5);
        assert_eq!(&root[433..438], b"Spare");
    }

    /// A volume large enough to need more bitmap blocks than the root can
    /// point at has to chain them through extension blocks. This is the
    /// case a small test disk never reaches and a real drive always does.
    #[test]
    fn a_large_volume_chains_its_bitmap_through_extension_blocks() {
        // 25 pointers in the root cover 25 * 127 * 32 blocks, so anything
        // past about 50 MB needs at least one extension block.
        let s = Scratch::new("bigbitmap");
        create_hard(
            s.path(),
            &HardSpec {
                bytes: 200 * 1024 * 1024,
                partitioning: Partitioning::None,
                filesystem: Some(FileSystem::FFS),
                ..Default::default()
            },
        )
        .expect("created");
        let image = s.read();
        let blocks = image.len() as u64 / BLOCK_BYTES as u64;
        let root_blk = blocks / 2;
        let root = block(&image, root_blk);
        assert!(sums_to_zero(root), "root checksum");

        let ext_blk = get_long(root, BLOCK_LONGS - 24);
        assert_ne!(ext_blk, 0, "no extension block for a bitmap this large");

        // Walk the whole chain and collect every bitmap block it names.
        let mut pointers: Vec<u32> = (0..25)
            .map(|i| get_long(root, BLOCK_LONGS - 49 + i))
            .filter(|p| *p != 0)
            .collect();
        let mut next = ext_blk;
        while next != 0 {
            let ext = block(&image, u64::from(next));
            for i in 0..BLOCK_LONGS - 1 {
                let p = get_long(ext, i);
                if p != 0 {
                    pointers.push(p);
                }
            }
            next = get_long(ext, BLOCK_LONGS - 1);
        }

        // Every bitmap block is present, checksums, and they are a
        // contiguous run: no pointer left dangling at zero.
        let bits = blocks - u64::from(RESERVED_BLOCKS);
        let wanted = bits.div_ceil(32).div_ceil((BLOCK_LONGS - 1) as u64);
        assert_eq!(pointers.len() as u64, wanted, "bitmap blocks accounted for");
        for p in &pointers {
            assert!(
                sums_to_zero(block(&image, u64::from(*p))),
                "bitmap block {p} checksum"
            );
        }
        let mut sorted = pointers.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            pointers.len(),
            "a bitmap block is named twice"
        );
    }

    /// Blocks past the end of the volume must read as allocated, or the
    /// filesystem hands out space that is not there.
    #[test]
    fn the_bitmap_never_offers_a_block_past_the_end() {
        let s = Scratch::new("tail");
        // A block count that is not a multiple of 32, so the last long is
        // partly off the end of the volume.
        create_hard(
            s.path(),
            &HardSpec {
                bytes: 3 * 1024 * 1024 + 512,
                partitioning: Partitioning::None,
                filesystem: Some(FileSystem::FFS),
                ..Default::default()
            },
        )
        .expect("created");
        let image = s.read();
        let blocks = image.len() as u64 / BLOCK_BYTES as u64;
        let root = block(&image, blocks / 2);
        let first_bitmap = get_long(root, BLOCK_LONGS - 49);

        let bits = blocks - u64::from(RESERVED_BLOCKS);
        let longs_per_block = (BLOCK_LONGS - 1) as u64;
        // Read a bit the way the writer addresses it: long `bit / 32` of
        // the run, least significant bit first within that long.
        let is_free = |bit: u64| {
            let long = bit / 32;
            let bm = block(&image, u64::from(first_bitmap) + long / longs_per_block);
            let value = get_long(bm, 1 + (long % longs_per_block) as usize);
            value >> (bit % 32) & 1 == 1
        };

        let longs = bits.div_ceil(32);
        for bit in bits..longs * 32 {
            assert!(
                !is_free(bit),
                "block {bit} is past the end of the volume but reads as free"
            );
        }
        let free = (0..bits).filter(|b| is_free(*b)).count() as u64;
        // Everything is free except the root block, its bitmap blocks and
        // any extension blocks chaining them.
        let bitmap_blocks = longs.div_ceil(longs_per_block);
        let ext = bitmap_blocks.saturating_sub(25).div_ceil(longs_per_block);
        assert_eq!(free, bits - 1 - bitmap_blocks - ext, "free block count");
    }
}
