# Peripherals and expansion

## Zorro autoconfig (`zorro.rs`)

The `ZorroChain` implements the Zorro II/III autoconfig protocol --
nibble-encoded config ROMs in the `$E80000` window, base-address
assignment, shut-up, chain advance, and power-on reset. Boards are
described by data (`BoardSpec`) rather than a trait; the built-in fast and
Z3 RAM options and user `[[zorro]]` metadata boards all build the same
specs. The user-facing guide, including the metadata file format and the
autoconfig walk-through, is [](../zorro).

## Fat Gary and Ramsey (`gary.rs`, `ramsey.rs`)

The A3000 and A4000 profiles fit the big-box motherboard pair: Fat Gary,
the bus controller, and Ramsey, the memory controller. Both answer in the
`$DE0000` page Gary decodes, with an address decode cruder than a register
map suggests: only the byte lane and two address bits matter (lanes 0-2
are Gary's, lane 3 is Ramsey's), and the whole layout repeats every `$100`
to the end of the page, so every register is mirrored many times over --
the Ramsey version register Kickstart reads at `$DE0043` answers equally
at `$DE0047` or `$DE0143`. Diagnostic tools reading addresses that look
like nothing in particular are reading a mirror.

Gary's three registers are single read/write bits in bit 7: TIMEOUT
(`$DE0000`, whether an unanswered bus cycle produces BERR or DSACK), TOENB
(`$DE0001`, timeout enable), and COLDBOOT (`$DE0002`, the power-up flag
the OS clears on warm reboot). None of them change emulated behaviour --
bus timeouts are not modelled; an unanswered cycle floats -- but the
read/write COLDBOOT bit is what identification tools use to detect a Fat
Gary, and without one they never go looking for the Ramsey behind it.

Ramsey drives the motherboard fast RAM (`[memory] motherboard`): a 32-bit
local bank ending at `$08000000` and growing downward, so a full 16 MiB
reaches `$07000000`. Its two byte-wide registers sit on lane 3: the
control register at `$DE0003` (refresh rate, page/burst/skip modes, DRAM
geometry) and the read-only version register at `$DE0043` -- `$0D` for
the A3000's Ramsey-04, `$0F` for the A4000's Ramsey-07, a distinction
that matters because the two parts disagree about control bit 4
(Ramsey-04 DRAM width versus Ramsey-07 cycle-skip mode). Refresh and the
speed modes have no observable effect in an emulator that never loses a
DRAM cell, but the bits store and read back because Kickstart and the
diagnostic tools write a mode and spin until they read it back, and the
geometry bits are seeded to describe DRAM parts matching the fitted RAM
size so sizing probes agree with the RAM that answers. Only the register
file lives in `ramsey.rs`; the RAM bank itself is `memory::Memory::mb_ram`.

Beyond Ramsey's four banks the big-box memory map reserves
`$04000000`-`$06FFFFFF` for motherboard RAM expansion; on the A4000
profile the bank keeps growing downward through it, up to 64 MiB at
`$04000000`, still sized by Kickstart's top-down probe (the control
register keeps describing the fully populated 1Mx4 geometry -- it has
no way to say more). The complementary `[memory] accelerator` bank
models CPU-slot local RAM: it starts at `$08000000` and grows upward
through the coprocessor-slot space, up to 128 MiB at `$10000000` where
Zorro III space begins, gated only on a 32-bit CPU
(`memory::Memory::accel_ram`).

## Gayle IDE (`gayle.rs`)

A600/A1200 machines get the Gayle gate array: the ID register at
`$DE1000`, the IDE task file at `$DA0000` (byte registers on the odd word
half, 4-byte stride), and the IDE interrupt and status bits. Drives are
raw flat HDF images with an RDB inside, opened read/write; PIO transfers
complete synchronously within the access. One hardware subtlety worth
knowing: Gayle byte-swaps the IDE bus, so IDENTIFY data words are
low-byte-first while sector data passes through untouched -- Kickstart
3.1 expects exactly this. The absent-slave behaviour follows the
WinUAE-verified model so device scans terminate correctly.

### Gayle PCMCIA slot (`gayle.rs`, `pcmcia.rs`)

Gayle's PCMCIA side is modelled from the Commodore register map as
captured by Linux's `amigayle.h` (disassembled from card.resource) and
WinUAE's `gayle.cpp`; nothing in it is keyed to a driver.

Registers, all on the even byte of their A12 page:

| Address | Register | Model |
|---|---|---|
| `$DA8000` | card status | read: the slot pins -- CCDET (bit 6), BVD1/SC (5), BVD2/DA (4), WR (3), BSY/IRQ (2) -- OR-ed with the bits last written, plus live IDE INTRQ on bit 7. An empty socket reads with every pin bit clear. Write: bit 0 DIS disables the slot (the windows unmap and the pins read as an empty socket; both edges latch a card-detect change), bit 1 DAEN, and bits 7-2 read back as written (card.resource's `CardMiscControl` write-protect override lands on bit 3) |
| `$DA9000` | interrupt change | a latch per pin, set whenever the sampled pin differs from the last value shown (an insertion or removal latches every pin that moved); the IDE bit is the INTRQ edge; BSY/IRQ is also re-latched while the pin stays high, since IREQ# is a level. Write-to-clear with AND semantics, except bits 1:0 (RESET, BERR from the preliminary datasheet) which are set by writing them: both together reset the card's configuration, RESET alone reboots the machine on the next card-detect change (BERR alone is logged, not modelled) |
| `$DAA000` | interrupt enable | bits 7-2 admit the matching latches; bit 1 BVD_LEV and bit 0 BSY_LEV pick INT6 over INT2 for the battery and busy/IRQ sources |
| `$DAB000` | config | programming voltage (bits 1-0) and access speed (bits 3-2), stored, four bits readable |

Routing: IDE and WR changes drive INT2 (PORTS); card detect always drives
INT6 (EXTER); BVD1/BVD2 and BSY/IRQ follow their level bits. Both lines are
levels into Paula, re-asserted each tick while a latched, enabled source
stands. The bus re-samples the pins after every insert, eject, and card
access (`Bus::pcmcia_sync_pins`), which is how a CF card's IREQ# reaches the
status register.

Address windows, decoded by the CPU after the plain-memory regions so an
autoconfigured Zorro II RAM board always wins the address:

| Window | Cycle |
|---|---|
| `$600000`-`$9FFFFF` | common memory (4 MiB) |
| `$A00000`-`$A1FFFF` | attribute memory: the CIS on even bytes (odd bytes mirror), a CF card's configuration registers at `$200`-`$206` |
| `$A20000`-`$A2FFFF` | I/O, 16-bit and even 8-bit registers |
| `$A30000`-`$A3FFFF` | I/O, odd 8-bit registers (A0 forced high: `$A30000+2n` is register `2n+1`) |
| `$A40000`-`$A7FFFF` | card reset: a write asserts RESET (a CF card drops its configuration and resets its ATA function), a read releases it |

Gayle cross-wires the byte lanes, so a byte at card address N is the byte at
CPU address N and a 16-bit register reads with its bytes exchanged -- the
same convention as the Gayle IDE data port, which is why the CF card drives
the shared `ata.rs` engine unchanged (IDENTIFY words arrive swapped, sector
data in natural order).

**The fast-RAM rule.** The common window is Zorro II space. Fast RAM
autoconfigures from `$200000`, so more than 4 MiB reaches `$600000`;
`Config::pcmcia_slot_shadowed` decides this at machine build, the emulator
warns, and `Gayle::set_slot_shadowed` makes the slot read as empty and
decode nothing, as on a real A1200 with an 8 MiB Zorro II expansion. Other
Zorro II RAM that lands in the window shadows it at the decode level by
construction (autoconfig RAM is classified before the slot).

**CompactFlash card** (`pcmcia::CfCard`): one `AtaBus` with the drive in
slot 0 behind the CF register layout. The Configuration Option Register
(attribute `$200`) selects it: index 0 (power-on) memory-mapped, with the
16-byte task-file block at the start of every 2 KiB of common memory and
`$400`-`$7FF` a window on the data register; index 1 contiguous I/O (A3-A0
decoded); index 2/3 PC primary/secondary I/O (`$1F0`/`$3F6`, `$170`/`$376`).
Block offsets 8/9 repeat the data register, `$D` error/feature, `$E`
alternate status/device control. In an I/O configuration the registers
leave common memory, and INTRQ (masked by nIEN) drives the BSY/IRQ pin.
COR bit 7 is a soft reset. The CIS is the CF specification's example
layout (device, JEDEC, VERS_1, FUNCID fixed disk, FUNCE ATA, CONFIG at
`$200` with last index 3, one CFTABLE_ENTRY per index). Pins: CCDET, BVD1,
BVD2 (STSCHG#/SPKR inactive), WR (CF cards are never write-protected).

**SRAM card** (`pcmcia::SramCard`): up to 4 MiB of RAM in the common window
(undecoded beyond its size), a CIS built the way WinUAE established
card.resource needs -- CISTPL_DEVICE (DTYPE_SRAM, 100 ns, WPS from the
switch, the size in the tuple's unit-count/unit-size encoding, hence the
size rule in the configuration guide), DEVICEGEO, VERS_1, FUNCID memory,
MANFID -- and pins CCDET, BVD1, BVD2 (battery good), WR from the switch.
Kickstart adds a card present at boot as credit-card RAM. A backing file
is written back about once an emulated second when dirty, on eject, and
on drop; the RAM itself travels inside save states.

The card lives in `Bus::pcmcia` (its own `PCMC` state chunk); Gayle holds
only the register file, the sampled pins, and the slot flags. Runtime
insert and eject (`Bus::pcmcia_insert`/`pcmcia_eject`, the window's PCMCIA
Card menu, the `pcmcia.*` control-protocol methods) go through the same
pin sampling as a boot-time card, so the guest sees a real card-detect
change.

Observed on Kickstart 3.1 (40.068, A1200) through the control protocol:
with a 2 MiB SRAM card present at boot, exec's MemList gains a header at
`$600200` (card.resource adds the card as credit-card RAM, keeping the
first `$200` bytes for the card's own header) and card.resource writes
`$DA9000` with the RESET bit set -- which is why a real A600/A1200 reboots
when a memory card is pulled, and why `pcmcia.eject` on that machine
resets it. A CF card hot-inserted afterwards has its four latched changes
(CCDET/BVD1/BVD2/WR) acknowledged by the ROM's INT6 handler within a
second, and an empty socket reading all-zero pins boots cleanly. Register
semantics and CIS layouts were cross-checked against WinUAE's model; tests
in `gayle.rs` (status/change/enable/config bits,
INT2/INT6 routing, DIS, RESET/BERR, shadowing), `pcmcia.rs` (window
decode, CIS, SRAM size encoding, backing file), and `bus/tests.rs` (CF
task-file mapping per configuration, IREQ# to INT2, reset register,
eject to INT6, SRAM window, shadowing).

Either drive slot may instead be an ATAPI CD-ROM (a `.cue`/`.iso`/`.nrg`/`.chd`
image): `ata.rs`'s task-file engine drives the PACKET (0xA0) command,
handing 12-byte CDBs to the same bus-agnostic SCSI-2 CD-ROM command engine
(`scsi/cd.rs`'s `ScsiCdRom`) the `[scsi]` host adapters use, so the read
family, TOC/sub-channel queries, mode pages, and CD-DA playback all behave
identically over ATAPI PACKET or a WD33C93 SCSI bus.

## A4000 motherboard IDE (`ide_a4000.rs`)

The A4000 profile decodes the same ATA task file (`ata.rs`) at `$DD2020`
with no gate array in front of it -- the layout Kickstart's own
`scsi.device` probes, with the Gayle-style 4-byte register stride, the
control block one A12 page up at `$DD3038`, and an interrupt status byte
at `$DD3020` whose bit 7 is the drive's INTRQ. Unlike Gayle there is no
interrupt-change latch: INTRQ feeds INT2 directly and the driver drops it
by reading the status register. Drives come from the same `[ide]`
section as Gayle machines, ATAPI CD-ROMs included -- same `ata.rs` engine,
same PACKET protocol.

## SCSI controllers (`a2091.rs`, `a4091.rs`, `sdmac.rs`, `scsi.rs`)

The `[scsi]` option attaches one of three host adapters, selected by its
`controller` key: the Zorro II A2091 (the default), the Zorro III A4091,
or the A3000's motherboard Super DMAC. All three drive the same SCSI-2
target layer in `scsi.rs`.

### A2091 (`a2091.rs`)

The A2091 is a Zorro II device board pairing the Commodore DMAC (rev 02
modeled) with a WD33C93A SBIC, plus the board's autoboot ROM whose
`scsi.device` drives them. The autoconfig
identity comes from the DMAC -- Commodore West Chester (514), product 3,
`ERTF_DIAGVALID` with `er_InitDiagVec` pointing at `$2000` -- while the
ROM supplies the DiagArea and the driver. Copperline defaults to its bundled
clean-room open ROM; `rom`/`rom_odd` override it with merged or split EPROM
dumps (interleaved U13-first).

Board window layout: ISTR `$40`, CNTR `$42`, WTC `$80/$82`, ACR
`$84/$86` (low bit forced even), DAWR `$8E`, the WD33C93 SASR/auxiliary
status at `$90/$91` and data port at `$92/$93`, the ST_DMA/SP_DMA/CINT/
FLUSH strobes at `$E0/$E2/$E4/$E8` (read- or write-triggered), and the
boot ROM repeating from `$2000` to the end of the 64K window. Unpopulated
decode below the ROM reads as floating bus (`$FF`): the boot ROM's drive
probe ANDs the A590 XT-interface bytes at `$A1/$A3/$A5/$A7` and only
takes the SCSI-only path when they all read `$FF` -- zeros wedge it
polling a phantom XT drive.

The WD33C93A model covers both ways drivers run the bus, verified against
the real 7.0 boot ROM booting a Workbench install end-to-end:

- the **Select-and-Transfer** combination command (full transaction in
  one command, status byte landing in the Target LUN register, CSR
  `$16` then the `$85` disconnect interrupt), including the short-data
  pause (`$4B`, command phase `$46`) and resume that real targets force
  on MODE SENSE-style reads; and
- the **manual path** the 7.0 ROM uses: Select-with-ATN posting CSR
  `$11` then service-required `$88|phase`, identify message and CDB via
  Transfer Info (with the single-byte-transfer modifier), phase-qualified
  completions (`CSR_XFER_DONE | next phase`), message-in pausing with
  `$20` until Negate ACK releases the target to disconnect.

Data phases run through the DMAC handshake (a word per DMAC cycle into
chip, slow, or Zorro RAM with the 24-bit ACR auto-incrementing) or
through the PIO data register with DBR. Like the Gayle model, transfers
complete within the access; completion interrupts are delivered after a
short emulated delay, and INT2 is the level `CNTR_INTEN && ISTR &
(INTS|E_INT)` fed to Paula's PORTS latch each tick. DMAC bus-master
cycles are not yet arbitrated against the CPU (TODO in `a2091.rs`).

The bundled driver executes inquiry, sense, and mode commands using
asynchronous PIO. Sector transfers use the DMAC when the buffer address and
length are even-aligned and reside entirely below 16 MiB; unaligned or
high-memory transfers are bounced through Chip RAM. Transfers program the ACR,
start the DMAC, and complete via a shared `INTB_PORTS` interrupt handler that
queues command-complete and disconnect status pairs.

### A4091 (`a4091.rs`)

The A4091 is a Zorro III SCSI-2 controller carrying an NCR 53C710 and a
nibble-wide autoboot ROM. Within its 16M window, `$000000-$7FFFFF` is the
boot ROM presented nibble-wide (expansion.library reassembles the
DiagArea with DAC_NIBBLEWIDE, and the ROM's own relocator copies the
driver the same way), `$800000` is the 53C710 register file (only the low
6 address bits decode, so it mirrors across the window -- the driver
relies on the `+$40` shadow as a cache write-allocate workaround), and
`$8C0003` reads the DIP-switch byte (host ID, termination, negotiation
enables). A DSP write starts the 53C710's SCRIPTS processor, whose phase
engine executes the driver's SCRIPTS programs against the disk targets.
The autoconfig identity is Commodore product 84 with `er_InitDiagVec`
`$0200`.

### A3000 Super DMAC (`sdmac.rs`)

The SDMAC is the SCSI DMA controller on the A3000 motherboard, not a
Zorro board: a register file at `$DD0000` (repeating at `$DD0100` -- the
"ALT" shadow that write-through tools use to defeat CPU write buffering)
that owns the DMA FIFO and interrupt plumbing and maps a WD33C93's
register file into a select latch and data port. It is the same layering
as the A2091 -- two front-ends onto the one `Wd33c93` core -- differing in
the register map, the ISTR bits, and a 32-bit DMA address counter
(physically in Ramsey) instead of the Zorro II DMAC's 24-bit one.
Kickstart's built-in `scsi.device` drives the pair directly, so there is
no boot ROM to configure.

### Shared drive backend

All IDE, SCSI, and copperhf drives share the `harddrive.rs` sector backend:
raw HDF images, bare partition hardfiles wrapped in a synthesized RDB
(bootable `DHn` named after the unit), gzip-compressed hardfiles (`.hdz`,
sniffed by gzip magic and unpacked by `gzip.rs` into memory at open time
because deflate has no random access, which is what makes their writes
session-only), CHD hard-disk images (below), and host directories built
into in-memory FFS or OFS volumes by
`dirfs.rs` (FFS by default; `filesystem = "ofs"` on the drive picks OFS,
the one every Kickstart from 1.2 onward can read with no guest-side
setup -- FFS needs a handler loaded from disk or an RDB `FileSystemHeader`
chain, neither of which Copperline bundles). The volume label defaults to
the directory name, or a `name` override configured on the drive. The
SCSI-2 target layer in
`scsi.rs` answers INQUIRY, MODE SENSE pages 3/4, READ CAPACITY,
READ/WRITE(6)/(10), REQUEST SENSE, and the no-op housekeeping commands,
with sense state kept per target.

`HardDriveImage::write_protected` says whether the backing refuses writes
(a CHD with no overlay, a read-only netplay session copy, a host disk
attached read-only); a refused write comes back `PermissionDenied`, which
the SCSI target reports as DATA PROTECT / WRITE PROTECTED (and shows as
the WP bit in the MODE SENSE header), copperhf as `TDERR_WriteProt` with
`CHF_UNIT_RDONLY`/`TD_PROTSTATUS` set, and the ATA core as an aborted
command (ATA has no write-protect status). The filesystem turns those into
its own write-protect error instead of a disk fault.

#### CHD hard-disk images (`harddrive/chd.rs`)

A hard-disk CHD -- MAME's compressed container as `chdman createhd` writes
it from an HDF: compressed hunks of whole 512-byte sectors and one `GDDD`
metadata entry, `CYLS:401,HEADS:16,SECS:32,BPS:512.` -- is read through
the same `chd` crate as the CD backend in `cdrom/chd.rs`, with the same
raw-header pre-validation (the crate sizes its hunk map and codec buffers
straight from the header's words with no v5 bounds checks, so a hostile
124-byte header is refused before it reaches the allocator) and the same
single-hunk decompression cache; the sector arithmetic is LBA to
hunk/offset with no track layout. The header's logical size fixes the
sector count; `GDDD` is parsed only to insist on 512-byte sectors, and CD
track tags make the open fail with "attach it as a CD image". The
`MComprHD` magic is sniffed by content like the gzip one, so the file's
name is irrelevant to the open. Delta CHDs (a parent SHA-1 in the header)
are refused: nothing about the convert-your-own-HDF use case needs them.

`harddrive::chd::media_kind` classifies a `.chd` for the configuration
(`config::is_cd_image_path`), the launcher, and the window's drop handler
by walking only the header and the metadata chain -- never the hunk map --
so the launcher can ask about a path on every redraw; `is_hard_disk_chd`
caches the verdict against the file's size and mtime. A `.chd` that cannot
be read keeps its traditional reading as a CD image, so the open that
follows reports the real problem.

The `chd` crate is read-only and nothing rewrites compressed hunks in
place (MAME writes only uncompressed CHDs, which forfeit the compression
that is the point), so guest writes go to a **copy-on-write overlay
sidecar**, `<image>.wov`, created beside the image on first attach. A read
checks the overlay index before decompressing; a write lands in the
overlay only, and the CHD is never opened for writing. The format:

```text
offset  size  field
0       8     magic "CLWOV001"
8       4     sector size, u32 little-endian (512)
12      8     image sectors, u64 little-endian (the CHD's logical size / 512)
20      20    the CHD header's SHA-1, so an overlay is refused on any other image
40      8     reserved, zero
48      ...   records: u64 LE LBA, then the sector's 512 bytes, repeated
```

Records are an append-only log: every write adds one, and the scan at open
rebuilds the LBA-to-record index so that the last record for a sector
wins. Nothing is ever overwritten in place, which is what makes the
recovery rule hold: a record cut short by a crash is dropped at the next
open, leaving the sector's previous record (or the CHD itself) in force,
whereas overwriting a record would leave a full-length one holding half of
each version that a scan could not tell from a sound one. Rewriting the
same sector therefore leaves superseded records behind, so an open whose
live records account for less than half the file rewrites it compacted,
through the same temporary-file-and-rename the state restore uses. Every
write reaches the file as it happens (the `File` is unbuffered), which is
the eject/exit flush. An overlay that names another image (SHA-1, sector
count) or is not one at all is left alone and the disk attaches
write-protected, as it does when the sidecar cannot be created; deleting
the `.wov` returns the disk to the pristine image. A netplay session copy
decompresses the whole image into memory instead and never touches a
sidecar, like the gzip form.

The overlay is machine state: `HardDriveImageState` carries every
overlaid sector (`chd_overlay`), and loading a state rewrites the sidecar
to exactly that set by building a sibling file and renaming it over the
old one, so a restore that fails partway leaves the previous sidecar
intact rather than a half-written disk. A resumed run sees the disk as it
was when the state was taken -- unlike an HDF, whose file contents are deliberately not
part of the state (`docs/internals/savestate.md`).

The drive controllers latch read/write activity, which the bus drains to
light the status-bar HDD LED; the LED holds for a short minimum period so
brief accesses stay visible. Gayle, the A4000 IDE, the A2091, the SDMAC,
and the lide-compatible board (below) report activity today; the A4091
shows the LED but does not latch activity into it yet.

## lide.device-compatible Zorro II IDE (`ide_zorro.rs`)

`[lide]` attaches a Zorro II IDE board compatible with LIV2's
actively-maintained open-source `lide.device`, in three AutoConfig
personalities selected by `board`: **RIPPLE** (mfg `0x144A`/product 7, two
ATA channels), **RIDE** (mfg `0x144A`/product 9, one channel, sharing
RIPPLE's ROM image and register layout), and **AT-Bus 2008** (mfg
`0x082C`/product 6, one channel, the register model shared by that board's
whole clone family). All three reuse the front-end-agnostic ATA core in
`ata.rs` (the same one Gayle and the A4000 IDE port use) and the shared
drive backend above; the new work is entirely in the board's own address
decode, since none of the three personalities resemble Gayle's 4-byte task
file. Drive slots may be ATA hard disks or, since `ata.rs` gained ATAPI
PACKET support, `.cue`/`.iso`/`.nrg` (or CD-holding `.chd`) CD-ROM images.

**Register decode.** Each ATA channel occupies a 4K block of the board
window, with register index `(offset >> 9) & 7` -- ATA A0-A2 are wired to
CPU A9-A11, so a register answers throughout its 512-byte slot, which is
what lets the driver bulk-transfer a sector with `movem.l`. Every register
but the 16-bit data port sits on the *upper* byte lane (even addresses,
D15-D8); the odd lane floats. RIPPLE's two channels sit at window offset
`$1000`/control `$5000` (channel 0) and `$2000`/`$6000` (channel 1) -- two
chip selects per physical connector, task file and control block, per the
RTL's own decode; RIDE and AT-Bus 2008 have one channel at `$1000`, control
block at `$2000` (so `$2C00` is the alternate-status register the driver's
channel-autodetect polls against `$1E00`). A channel with **no drives
attached at all** floats every register, not only status: `AtaBus::read_reg`
only special-cases status/alt-status for "no drive selected", so the
front-end checks `AtaBus::any_drive_attached` itself -- without it, an
empty channel's device/head register reads a hard zero, which real
`lide.device` reads as "a device answered" and polls forever waiting for
it to respond. (Found and fixed by booting a downloaded `lide.rom` under
RIPPLE with only channel 0 populated -- see `ide_zorro.rs`'s tests and
module docs.)

**ROM window and banking.** The flash is byte-wide, so a 32K bank fills 64K
of window at even addresses (stride 2; the odd lane on AT-Bus 2008, whose
`er_InitDiagVec` is `1` rather than `8` for exactly this reason). Before
the first write anywhere in the window, ROM covers the whole window (bank
selected by address bit 16 -- bank 1 being the optional CD filesystem);
that first write latches `ide_enabled`, after which ROM remains only in
the upper 64K (RIPPLE also keeps it in the low 64K wherever address bits
12 and 13 agree, which is exactly where the register blocks above are
not). The bank register is written anywhere in `$8000-$FFFF`: RIPPLE has
two banks and it is write-only; RIDE has four and reads back with
`otherram_en`/`maprom_en` on the next nibble down. AT-Bus 2008 has no latch
and no banking: its image sits on the odd lane across the whole window,
always. None of the three boards wire an interrupt line -- `lide.device`
is a purely polling driver.

A fitted board with no `rom`/`rom_bank2` named defaults to Copperline's own
bundled ROMs (`assets/lide/`, `src/config/resolve.rs`,
`resolve_bundled_lide_rom`): RIPPLE/RIDE get `lide.rom`, AT-Bus 2008 gets
its own `lide-atbus.rom`. The two are not interchangeable -- upstream links
them with different scripts (`bootrom/rom.ld` puts a 4-byte `"LIV2"` header
before the bootloader; `bootrom/atbusrom.ld` starts the bootloader at offset
0), matching the different `diag_vec` `BoardSpec::lide` already carries per
personality above (`0x0008` vs `0x0001`). `cdfs.rom` (the CD-filesystem
bank) only defaults on RIPPLE/RIDE, which have the flash banking to put it
in. `rom = ""` opts out into hardware-only mode: no DiagArea, no autoboot,
`diag_vec` absent from the `BoardSpec`, but drives still answer once a
disk-loaded driver finds them.

All three personalities have booted a real `lide.rom`/`lide-atbus.rom`
release end-to-end to a real Workbench (Kickstart 1.3 and 3.1, `--cpu
68020`): AutoConfig, the DiagArea, `lide.device` loading as a resident
module, finding an attached drive, and mounting a boot node from a real
RDB image.

AT-Bus 2008's ROM and register blocks share address space by byte lane
(ROM odd, registers even, per above), including inside the control block
at `$2000`: the board's read dispatch used to match the control block
first regardless of lane, so odd-lane reads there -- exactly where the
boot ROM's chainloader fetches its relocatable driver payload -- floated
as an unpopulated register instead of reaching ROM. `lide.device` never
loaded and the machine never got past the "insert disk" screen; RIPPLE and
RIDE were unaffected, since their ROM sits on the even lane, clear of any
register. Fixed by checking the ROM lane ahead of the register-block
dispatch in `IdeZorro::read()` (see `ide_zorro.rs`'s tests).

## SF2000 accelerator Zorro II SD card controller (`sf2000sd.rs`, `sdcard.rs`)

`[sf2000sd]` attaches the SF2000 accelerator's SD card controller: mfg
`0x144A`/product 11 (the same manufacturer ID `lide` uses), a 64K Zorro II
I/O window over an SPI-mode SD card, register-compatible with the upstream
RTL (`sdcard.v`/`shifter.v`/`fifo.v`/`tx_cpu_buf.v`/`rx_cpu_buf.v`). Split
across two files the way `ide_zorro.rs`/`ata.rs` are: `sf2000sd.rs` is the
Zorro board (register file, ROM overlay, `ZorroDevice` impl), `sdcard.rs` is
the SD-over-SPI protocol engine, wrapping the same `HardDriveImage` sector
backend `ata.rs`/`a2091.rs` use -- an SD card image is handled exactly like
a `[lide]`/`[copperhf]` hardfile (RDB images, bare partition hardfiles with
a synthesized RDB, gzip-compressed images).

**Register decode.** The whole 64K window mirrors one 32-byte register block
(`off & 0x1F`), word-addressed: `$00` CLKDIV, `$02` SLAVE_SEL, `$04`
CARD_DET, `$06` STATUS, `$08` SHIFT_CTRL (mode + receive length), `$0A`
INTREQ, `$0C` INTENA, `$0E` INTACT, `$10`-`$1E` the TX/RX data port (byte
access: upper lane only, matching `ide_zorro.rs`'s task-file convention).
Full bit layout is in `sf2000sd.rs`'s module documentation.

**No cycle-accurate SPI timing.** `CLKDIV` paces real SCLK bit timing on
hardware; a polled register protocol has no need for that to behave
correctly (`ide_zorro.rs`'s task-file registers are likewise instant rather
than ATA-bus-timed), so it is stored/read back faithfully but never used to
delay anything -- every SPI byte-time (`SdCard::clock_byte`) resolves
synchronously. What *is* modelled faithfully is the FIFO backpressure
contract driver code loops on: the RX queue is capacity-34 (32-entry FIFO +
2-stage CPU buffer, matching the RTL); a `SHIFT_CTRL` receive request tops
it up to capacity immediately and tracks the remainder, refilling one byte
per drained read until exhausted, so STATUS's busy bit stays observably set
across any request bigger than 34 bytes.

**The `clock_byte` model.** Every SPI byte-time is bidirectional on the real
bus even though `sdcard.v`'s TX/RX/BOTH shifter "mode" is a local FPGA
buffering convenience, not a bus-level distinction: in TX mode the shifter
still receives a byte from the card each byte-time, it just discards it
instead of pushing it to the RX FIFO; in RX mode it still drives real clock
edges, it just always sends `0xFF` filler. `SdCard::clock_byte` models the
one true primitive -- advance the card's command/response state machine by
one byte, in both directions at once -- and `Sf2000Sd` calls it once per
byte-time regardless of which RTL mode is active, discarding the reply on a
TX-only byte exactly as the real shifter does.

**SD-over-SPI protocol.** `sdcard.rs` implements the command set verified
against Mike Stirling's `sd.c` (`k1208-drivers`, also used by the `spisd2`
Amiga driver): CMD0/CMD8/CMD55+ACMD41/CMD58 (the init handshake), CMD9/CMD10
(CSD/CID -- CMD9 in particular is load-bearing: without it a driver has no
way to learn capacity, and its bit layout was checked field-for-field
against `sd_parse_csd`), CMD13 (status), CMD16 (accepted no-op, block length
is always 512), CMD17/CMD24 (single-block read/write), CMD18/CMD25/CMD12
(multi-block read/write/stop -- not an edge case: any trackdisk-style
request wider than one sector takes this path, since `device.c` passes
`io_Length >> 9` straight through as the sector count), ACMD23 (its result
is never checked by the driver, so it falls through to the catch-all
"unknown command" response harmlessly), CMD59 (accepted no-op -- CRC
checking is never enforced, including on CMD0/CMD8's normally-mandatory
fixed CRC bytes, a deliberately permissive choice: friendlier for driver
bring-up than a strict card). `[[host_disk]]` passthrough and a launcher UI
entry are not implemented yet. Presented throughout as a block-addressed
(SDHC-style) card via CMD8/ACMD41's HCS bit and CMD58's OCR CCS bit, so a
real driver always addresses it by block number -- matching
`HardDriveImage`'s own `u64` LBA unit directly.

CMD18's block stream and CMD25's block-accepting loop are each modelled as
their own `Activity` state in `sdcard.rs` (`StreamingRead`/`AwaitWriteToken`
with a `multi` flag) rather than as one-shot replies: the driver interleaves
these with an unknown number of per-block round trips before finally
stopping (CMD12 for a read, the `0xFD` STOP_TRAN token for a write), so the
card has to keep responding correctly for as long as the driver keeps going,
including recognizing a CMD12 frame arriving in place of the next block's
start token.

**ROM overlay.** Gated the same way `ide_zorro.rs`'s RIPPLE/RIDE personalities
are: before the first write anywhere in the window, the whole 64K reads as a
flat ROM image (no odd/even lane split, no banking -- simpler than lide's
boards, since the RTL available here is a development build with no boot
ROM wired up at all, so there is no real register/lane interplay to match);
that first write latches the interface live, and from then on the whole
window is the register file, with no ROM visible anywhere (unlike RIPPLE,
which keeps ROM in part of its post-latch window). `er_InitDiagVec` is
`0x0001`. `rom` absent (or `""`) is hardware-only mode: registers are live
immediately, no autoboot. Unlike `[lide]`'s `rom`, there is no bundled
default -- this ROM is the SF2000 firmware author's, not Copperline's to
ship.

## Host filesystem service (`filesys.rs`)

`[[filesys]]` mounts export host directories as live AmigaDOS volumes
(`HOSTFS0:` ... up to 8 mounts), with no disk image in between -- distinct
from the `dirfs.rs` path above, which snapshots a directory into an
in-memory FFS or OFS volume behind a virtual drive. The guest side is a tiny
handler (see `guest/services/`) mapped into the Copperline services board
with a mount table and a hand-built DiagArea. DiagPoint only patches a
Romtag into the retained diag copy; Kickstart's cold-start resident scan
calls its rt_Init once DOS-list surgery is actually safe (doing it
straight out of raw DiagPoint context corrupts Kickstart 1.3's own boot),
and rt_Init builds one DeviceNode per mount and `AddBootNode`s it (at the
mount's configured boot priority), so DOS mounts the devices at boot. The
handler probes the library versions at runtime and falls back to the
1.3-era calls on Kickstart 1.3: `AddDosNode` for a non-boot mount, and a
hand-built BootNode `Enqueue()`d on `eb_MountList` (mirroring the
V34-era A590/A2091 boot ROM recipe) for a bootable one, so `bootpri`
boots the machine from a hostfs volume on 1.3 exactly as it does on
2.0+. V34's own boot-time handler startup carries BCPL process
parameters rather than a V36 `ACTION_STARTUP` (`dp_Arg3` is NULL; the
handler locates its unit through `dp_Arg2`'s `FileSysStartupMsg`
instead). The handler forwards
every DosPacket to the host through a doorbell register in the board's
MMIO window: writing the packet APTR to `REG_DOSPKT` services the packet
synchronously inside the register write, so `dp_Res1`/`dp_Res2` and the
result registers are filled before the next guest instruction runs. All
`ACTION_*` semantics -- reads, writes, create/rename/delete, directory
walks, protection, comments, datestamps -- are implemented host-side
against the real filesystem, with results written straight into guest
memory.

Each mount unit owns its own bank of longword registers in the window
(layout shared with the guest via `guest/services/copperline_board.h`):
`REG_MSGPORT` publishes the handler process's MsgPort while the unit is
live, and `REG_RESULT`/`REG_ARG` tell the handler what to do with the
packet it just rang in (reply it, `AddDosEntry` a host-built volume
DosList node, or exit on `ACTION_DIE`). One handler process runs per
unit against its own bank, so mounts never synchronize with each other.
A single global `DIAG_DOORBELL` strobe carries the expansion-init work,
which runs before any handler process exists. That init strobe is also
where `[machine] rom_scsi_device_disable` takes effect: the board's
DiagPoint culls the ROM's `scsi.device` resident tag (`romtags.rs`),
which is why setting the flag instantiates the services board even with
no `[[filesys]]` mounts configured.

These longword registers are written with a single `move.l` in the guest
ROM/handler, but on a 68000/68010 that compiles to two word-sized bus
cycles (high word, then low word -- a real 16-bit-bus artifact the CPU
core reproduces). The board fires each doorbell (`DIAG_DOORBELL`,
`REG_DOSPKT`, `REG_MSGPORT`) on whichever write actually completes the
value -- a single 4-byte access on a 32-bit bus, or the low word of a
split pair on a 16-bit one -- reading the result back out of the already-
latched window image rather than trusting the write that triggered it.

Amiga attributes a host filesystem cannot hold live in UAE-style `.uaem`
sidecar files (read when present, written back on change, hidden from
guest listings); the delete-protection bit is honoured on
`ACTION_DELETE_OBJECT`. Filenames map between host UTF-8 and guest
Latin-1, hiding names with no Latin-1 spelling; host symlinks are
followed (the guest cannot create one, so a symlink is the host user
deliberately grafting a tree into the mount), while path escapes that a
guest could construct on its own (`..`, embedded separators) are
blocked. A `readonly`
mount refuses writes with the standard write-protection error.

## uaelib trap (`uaelib.rs`)

WinUAE's boot ROM ("rtarea") provides a service trap at `rtarea_base + 0xFF60`
called by guest utilities and cross-compiler templates (`uae-configuration`,
`vscode-amiga-debug` helpers `warpmode()`, `KPrintF()`, `debug_*()`). Copperline
provides a compatible ABI at `$F0FF60` (see
[Direct launching](../guide/run.md#uaelib-trap)).

- **Bus-level implementation**: Rather than hooking CPU opcodes directly,
  Copperline decodes a 32-byte ROM region in the memory map (`cpu.rs`): `JSR` to
  an internal handler, `MOVE.L A7,(doorbell)`, `MOVE.L (result),D0`, `RTS`,
  `RTS`. The entry word is `0x4EB9`. Arguments are read from the guest stack at
  `A7 + 8 + 4n` through the CPU address mask; D0 and CCR are the only modified
  registers.
- **Doorbell synchronization**: Longword writes to the doorbell register trigger
  synchronous processing on completion (`completes_long_reg`), latching the
  result for the subsequent read. The region is cache-inhibited.
- **Memory hierarchy**: `classify_plain_memory` decodes RAM and ROM with higher
  priority, so CDTV extended ROM at `$F00000` naturally covers this region.
- **Function dispatch**: Function 13 (WinUAE `ExitEmu`, `uae_quit()`) latches
  an exit request the frontend takes at the next frame boundary and ends the
  session on (exit status 0, or 3 after a failed screenshot expectation; see
  `verdict.rs`); function 82 parses `"key value"` pairs and handles
  `warp`; function 86 prints log strings to stdout and queues `event.debug`
  events; function 88 manages the resource registry, idle time accounting, and
  the 768x576 debug overlay. Unhandled functions return 0. The exit latch is
  host-side and, like the warp latch, is not carried by a save state.
- **State serialization**: Trap state, resource registries, and overlay lists
  are serialized as part of `Bus` state (save-state version 76).

## Freezer cartridge (`cartridge.rs`)

The freezer cartridge models an Action Replay-style system monitor mapped in
memory and triggered via level-7 NMI (see
[Configuration](../guide/configuration.md#freezer-cartridge)).
The bundled implementation uses HRTMon 2.39 assembled for the UAE cartridge
target (`assets/hrtmon/hrtmon.rom`).

- **Memory mapping**: Maps a 1 MiB bank at `$A10000` containing monitor code,
  stack, and workspace RAM. The bank is present at all times and preserved in
  save states.
- **Configuration header**: The header block at `+20`..`+72` configures monitor
  runtime settings (`mon_size`, screen colors, hardware detection flags).
  These are populated on reset from active emulator hardware.
- **Register shadows**: Because custom chipset registers are write-only, the bus
  maintains a 512-byte shadow of custom registers and CIA registers
  (`write_custom_word_from`, `custom_read`). On freeze, shadows are copied into
  the cartridge bank (`$A9F000` for custom registers, `$A9E000`/`$A9D000` for
  CIAs) allowing the monitor to inspect and restore state.
- **Entry mechanism**: `Cartridge::freeze` updates register shadows, writes the
  level-7 autovector (VBR + `$7C`) pointing to monitor entry, and raises an NMI.
  The 68000 enters the monitor on the next instruction boundary, and the
  interrupt acknowledge consumes the request.
- **State serialization**: Cartridge memory, register shadows, and interrupt
  state are serialized in `Bus` state (save-state version 75).

## A2065 Ethernet (`a2065.rs`, `net/`)

The `[a2065]` option fits a Commodore A2065: a Zorro II board carrying an
Am7990 LANCE and 32 KiB of on-board RAM, driven by the AmigaOS SANA-II
`a2065.device`. Unlike the DMAC boards the LANCE never masters the Amiga
bus: its init block, descriptor rings, and packet buffers all live in the
board's own RAM, which the CPU reaches through the board window, so the
board is self-contained and owns a host `NetBackend` (`net/`) for real
frames. The LANCE engine models the Am7990 programming surface a real
driver exercises: TX and RX buffer chaining (STP..ENP spans across
descriptors), the stored FCS trailer (MCNT counts it; drivers read the
payload as `MCNT - 4`), the init-block MODE gates (DTX/DRX and the LOOP
internal-loopback self-test SANA-II drivers run at power-up), and MISS on
an RX ring overrun.

The `nat` backend (`net/nat/`, `net-nat` build feature) is a slirp-style
userspace NAT: a dedicated `a2065-nat` thread owns a smoltcp interface
that terminates ARP and the guest's TCP on the virtual gateway
(10.0.2.2, DNS forwarder 10.0.2.3, guest 10.0.2.15/24), splices each TCP
flow onto a non-blocking host socket, NATs UDP per flow, resolves DNS
through the host's own resolver, and answers BOOTP/DHCP and ICMP echo at
frame level. Frames cross to the emulated NIC over bounded channels that
drop on overflow, so the emulator thread never blocks on the host
network. Networking is inherently non-deterministic, so a fitted NIC
breaks byte-identical replay while traffic flows; save states record only
the chosen backend and bring up a fresh one on load (flows die; the
guest's TCP retransmits). The board and backend story, including the WASM
plugin `net` capability, is covered in [](../zorro).

The `bridge` backend (`net/bridge/`, `net-bridge` build feature) uses the
same bounded worker boundary but carries unmodified Ethernet frames to a
selected physical adapter: AF_PACKET on Linux, system libpcap/BPF on macOS,
and runtime-loaded Npcap on Windows. A platform filter and a second software
guard admit only the guest station address and multicast/broadcast, while
guest-source capture echo is discarded. The LANCE's init-block PADR updates
that filter. Linux's companion process owns only `CAP_NET_RAW`, validates an
interface request, and passes a bound descriptor with `SCM_RIGHTS`; it never
handles a frame. Backend construction is fallible so bridge errors abort
machine startup or state restoration rather than changing connectivity.

## HostSocket (`hostsocket.rs`, `crates/hostsocket-plugin/`, `guest/hostsocket/`)

The `[hostsocket]` option fits the bundled HostSocket board: guest-facing
`bsdsocket.library` backed by a smoltcp TCP/IP stack on the host, so socket
applications run with no guest network stack at all. It is deliberately
*not* a native device like the A2065 but a WASM plugin board hosted by
`wasmboard.rs`, with its module and guest stub ROM embedded in the binary
(`hostsocket.rs` holds the bytes and expands the config section into an
ordinary plugin-board entry whose module path is a sentinel the plugin host
and save-state restore resolve). The plugin boundary is what makes the
board save-state-clean: the entire TCP/IP stack -- smoltcp interface,
socket set, fd table, DNS state -- lives in the module's linear memory,
which snapshots and restores byte-for-byte like Amiga RAM; a native port
would have to hand-serialize live smoltcp state, which smoltcp does not
support. The guest side (`guest/hostsocket/`) installs the library via an
`rt_Init`-deferred Romtag (safe on real Kickstart 1.3/3.1 and AROS) and
stages each LVO through a Forbid-bracketed register-window RPC, with a
wake-queue interrupt path for blocking calls -- the same host-does-the-work
pattern as the services board's hostfs handler. The board reuses the shared
`NetBackend`s above through the plugin `net` capability; `loopback` is
deterministic, `nat`/`bridge` are not. `gethostbyname()` defaults to the
plugin ABI's `resolve` capability under `net = "nat"`/`"bridge"`
(`resolve_start`/`resolve_poll` in `wasmboard.rs`, `register_host_fns`),
resolving via the host's own OS resolver on a short-lived background
thread -- the same `getaddrinfo`-on-a-thread shape the NAT DNS forwarder
above already uses, reused directly (`net::nat::dns::resolve_a`) rather than
reimplemented -- so it works out of the box under `net = "bridge"` with no
`dns_server` hand-configured to match the LAN. `[hostsocket] resolver =
"dns"` opts back into the board speaking DNS itself over that same `net`
traffic, to target a specific server instead of the host's own resolver.
The library's own LVO table covers the real bsdsocket_lib.sfd order from
`socket()` all the way to the table's real end at LVO -858 (confirmed
against Olaf Barthel's own authoritative `.sfd`, not just the -30..-300
range Phase 4 originally shipped, and not just the AmiTCP-4.0-compatible
subset through `ObtainServerSocket` at -696) -- `inet_aton`/`inet_ntop`/
`inet_pton`, `In_LocalAddr`/`In_CanForward`, the `setservent`/`setprotoent`/
`setnetent` iterator families, and Roadshow's own resolver-family extension
(`getaddrinfo`/`getnameinfo`/`gai_strerror`/`freeaddrinfo`, plus the
reentrant `gethostbyname_r`/`gethostbyaddr_r`) all get real bodies too,
while the LVOs with no equivalent in this project's model (raw packet
capture, host routing tables, live interface reconfiguration, direct BSD
mbuf-chain manipulation, Roadshow's own internal global-data-access
functions) stay `_hs_stub` rather than jumping off the end of the table
(see `guest/hostsocket/entry.s`'s own jump-table comment for the full
accounting).

## zz9k crypto board (`zz9k.rs`, `crates/zz9k-plugin/`)

The `[zz9k]` option fits the bundled ZZ9000 SDK crypto board: a
register-compatible subset of the MNT ZZ9000's SDK v2 service platform
(CORE + MEMORY + CRYPTO) whose crypto runs host-side on pure-Rust
RustCrypto inside the plugin, so the zz9000-sdk's unmodified Amiga
software -- transport library, tools, accelerated AmiSSL -- offloads
TLS-era crypto at host speed. Like HostSocket it is a bundled WASM plugin
board with a path-sentinel module, but unlike every other bundled board it
autoconfigs under MNT's own manufacturer ID (0x6D6E, product 4/3): the
SDK's `FindConfigDev` probe is the detection mechanism, so compatibility
*is* the identity. The whole board -- registers, ring mailbox, and the
shared-buffer heap the guest copies payloads through -- is one byte array
in the plugin's linear memory; there is no DMA, no network, and no host
randomness (key-exchange scalars always come from the guest), which keeps
the board pure compute and therefore deterministic, replay-safe, and
save-state-exact including mid-operation (pending completions carry
remaining-colour-clock counters, never host time). Requests are picked up
by the plugin's tick scanning the request ring -- the SDK's Zorro II
transport never rings the doorbell -- computed at dispatch, and completed
after a deterministic latency table, one request per tick so no single
wasm call approaches the plugin fuel budget. The register/opcode contract,
the pinned zz9000-sdk revision, and every firmware-latitude choice are
specified in [](zz9k.md).

## CDTV (`cdtv.rs`, `cdrom.rs`)

The CDTV model pairs the DMAC (which autoconfigs ahead of the Zorro chain,
as on the real machine -- the CDTV firmware requires the DMAC to be the
first configured board) with a Matshita drive speaking its fixed-length
command/response protocol: seek, read, play (LSN/MSF/track), status, SubQ,
and TOC queries, with responses delivered byte-by-byte with STEN pulses.
Data sectors DMA onto the system bus at the 24-bit ACR address -- chip,
slow, or Zorro board RAM, like the A2091's DMAC; Kickstart allocates the
CD buffers in fast RAM when a board is fitted -- paced at single speed and
raising the DMAC interrupt on completion. The 256 KiB extended ROM sits at
`$F00000`.

## CD32 Akiko (`akiko.rs`)

Akiko sits at `$B80000` with its `$C0CACAFE` ID: the chunky-to-planar
converter, the I2C lines to the 24C08 NVRAM EEPROM (persisted to the
`[cd] nvram` file), and the CD command/response rings talking to a Chinon
drive model (stop, pause, seek/play/read, LED, SubQ, status). Data sectors
stream as 2352-byte raw frames at 75 (or 150 at 2x) sectors/second; CD
audio mixes into the host output, and both light the blue CD LED. The
512 KiB extended ROM sits at `$E00000`, and the CD32 pad protocol drives
port 2.

Every sector the pickup delivers, data read or CD-DA play alike, comes with
its 96-byte subcode frame: with the subcode flag enabled Akiko DMAs it into
the misc page's subcode area (alternating 128-byte halves, `$FFFF $0000` end
marker, offset register advanced by 100) and raises the subcode interrupt.
Image formats carry no subchannel data, so the Q channel is regenerated from
the TOC as an ADR 1 position packet (control, track, index, track-relative
and absolute MSF in BCD, CRC) with P and R-W blank. That stream is what the
Kickstart `cd.device` turns into `CD_ADDFRAMEINT` server calls and
`CD_QCODEMSF`/`CD_QCODELSN` positions, and the drive's SubQ command reports
the same position while a play is running or paused (regression example:
Liberation's CD32 intro busy-waits on its first frame interrupt after
`CD_PLAYTRACK`).

### CD32 Full Motion Video module (`cd32_fmv.rs`)

The CD32 profile fits a 1 MiB Zorro II FMV cartridge by default using the
bundled open ROM; top-level `fmv_rom` overrides it and an empty value removes
it. The module is the first autoconfig board, normally at `$200000`
(manufacturer 514, product
`$6A`, serial `$0028001E`). Its window follows the physical decode: 256 KiB ROM at
`+$000000`, board status/control at `+$040000`, LSI L64111 MPEG Layer II audio
at `+$050000`, the C-Cube CL450 bitstream port at `+$060000`, CL450 registers
at `+$070000`, and 512 KiB module RAM at `+$080000`.

The guest module ROM remains responsible for reading sectors through Akiko
and programming both chips. Copperline implements their register, command,
FIFO, interrupt, SCR/PTS, presentation, and audio-buffer contracts; MPEG-1
video is decoded through CopperlineHQ's safe pure-Rust `plmpeg` core and
MPEG-1 Layer II audio through Symphonia. The production player never programs
Denise/Lisa's digital genlock controls: the cartridge keys the dark native RGB
level in its analogue output path, which the renderer models alongside
explicit chipset genlock transparency while preserving the module's border
and blanking controls. The CL450's interpolated 704-pixel output line maps to
the captured TV aperture rather than the deeper native overscan framebuffer;
the later TV presentation therefore retains both edges of the 352-pixel MPEG
source instead of applying a second, one-sided crop. The decoder's partial
bitstream, prediction frames, and presentation state are serialized directly,
so a resumed headless run produces the same frames without retaining the
already-decoded program stream.

`fmv-rom/` builds the bundled open 256 KiB image. Its DiagArea installs three
residents under CD32 Kickstart 3.1: `cd32mpeg.device`, `videocd.library`, and
a version 41 `cdstrap`. The device worker configures the host `cd.device` for
2328-byte Mode-2 sectors, reads them in chronological LSN order with standard
`CD_READ`, separates system/PES data, and feeds the two decoder ports. The
library temporarily selects 2048-byte sectors, reads White Book INFO.VCD and
ENTRIES.VCD at LSN 150/151, obtains track boundaries with `CD_TOCLSN`, and
restores the prior drive configuration before returning. The strap replaces
the CD32 extended ROM's lower-version resident in place, claims only White
Book media, and chains the displaced init entry for normal game discs. A
claimed disc starts a controller-driven task which lists the parsed tracks,
submits asynchronous `PLAYLSN`, and aborts it on Blue before hiding the
decoder overlay and redrawing the menu. Under AROS PR 1089 the system-ROM MPEG
device is used instead and the cartridge diagnostic is deliberately skipped
to keep the legacy Commodore ROM from replacing AROS's `cd.device`; that also
means the cartridge library and player are not installed on AROS. Both paths
use the cartridge's empty CL450 container. Starting
`CPU_CONTROL` is the boundary at which Copperline marks its command-level
CL450 model ready, so no proprietary microcode is copied or executed.

The matching AROS PR 1089, merged upstream on 2026-09-01, includes
Copperline's ordering fix as commit `64eb7ed1`.
Akiko allocates the highest armed PBX slot first; when a high slot is re-armed
before an older low-slot sector is consumed, slot-number order is no longer
arrival order. The driver sorts each CDXL snapshot by the raw sector MSF,
preserving an exact chronological Mode-2 stream without changing Akiko's
hardware arbitration.

The optical sector clock and the firmware command transport are separate:
sector payloads retain their physical 75/150 Hz cadence, while the drive's
cached TOC is returned over the command ring at 600 packets/second. This lets
CDStrap receive a coherent TOC during its first media probe, including on
CD32 discs mastered with the older CDTV trademark boot layout. Two mechanism
behaviours gate that transport, both pinned against a real CD32 (a filmed
cold boot plus the `tools/cd32-probe` rows): a freshly loaded disc pays the
mechanism's spin-up before the first lead-in dump delivers entries (~8.8 s
from a cold power-on, ~3.5 s for a change on a warm drive; a guest reset
keeps the disc spinning, so warm reboots skip it), and an in-flight dump is
finished before the drive acts on the next queued command. Together these
hold the KS driver's first TOC transaction -- and every io queued behind
it, the boot screen's one-shot CD_CHANGESTATE included -- open until the
disc is genuinely readable, which is why a real CD32 with a bootable disc
inserted at power-on goes straight from the Kickstart grey screen to the
boot display without ever starting the fly-in show, and why the emulated
cold boot now reaches the startup-sequence within 50 ms of the filmed real
machine (14.36 s vs 14.31 s). The hold is the drive's, not Akiko's: the
TX DMA keeps draining the guest's command ring into the drive's receive
buffer whatever the drive is doing, and the drive parses commands out of
that buffer one at a time between dumps. That distinction matters
because Kickstart's driver queues a 3-byte LED packet for every TOC
entry it receives (and an unpause right behind the request itself): on a
disc with more than about 25 tracks those packets outrun a 256-byte ring
that nothing consumes, and the lapped bytes then parse as garbage after
the dump: the driver's LED toggles and its unpause come back as
checksum-error replies, which the real machine never produces (observed
with Pinball Illusions CD32, 39 tracks, whose boot showed four such
replies before the first data read). A data locate
also pays the tray mechanism's real seek time, calibrated against a real
CD32 with `tools/cd32-probe`: a +500-sector hop costs 253 ms, +1000 costs
301 ms, and long strokes flatten out around 1.4 s (WinUAE bills a single
sector slot). Only a seamless continuation of the running stream skips the
relocate. The measured 2x sequential rate (150.0 sectors/s) matches the
model exactly.

READ DATA's end MSF is exclusive. At that boundary the PBX path retains one
final position-bearing raw frame, matching the sector already buffered by a
continuous physical read. The ROM driver uses its on-disc MSF to detect when
the next filesystem request is outside the current stream, stop it, and seek
to the new LSN; dropping the buffered boundary frame instead would leave the
driver asleep waiting for a PBX interrupt that can never arrive.

The drive protocol is cross-checked against both ROM drivers known to have
run on real hardware, Kickstart's cd.device and AROS's, which pinned down
three behaviours where the two disagree with older emulator lore: the drive
microcontroller answers a command about a millisecond after its last byte
rather than inside the guest's register write (drivers arm their
completion interrupt in that window), and an enabled TX-DMA completion is
presented before the corresponding RX completion on the half-duplex drive
link; the TOC dump streams the track
entries before the A0/A1/A2 session entries, since a parser may treat the
lead-out entry as end-of-TOC; and the media-status packet reports a present
disc as `$83` (Kickstart masks the byte with 3, AROS compares it whole --
only `$83` satisfies both). The CDINTREQ status read returns the raw
request latches; CDINTENA gates only the INT2 line. Direct-drive software
depends on the raw view -- Jim Power's CD32 loader polls DRIVEXMIT before
each PIO command byte with that source never enabled -- and the completion
bits stay latched until the matching comparator register is rewritten, so
an INT2 server that reads CDINTREQ on every chain entry must ignore
sources it has not armed (an earlier Copperline masked the read to protect
AROS's server from its own stale latches; that server is fixed instead,
and the bundled ROM carries the fix). Akiko's DMA engines drive a full 24-bit address bus (the
address registers mask to `$00FFF000`), so the rings and sector buffers
resolve through every RAM bank in the low 16 MB -- Zorro II fast RAM
included, which is where AROS places its `MEMF_24BITDMA` allocations when
fast RAM exists -- not just chip RAM. The command/status comparator indices
are eight-bit and the DMA addresses fold to their 256-byte pages on every
access, TX and RX alike: Kickstart's command producer uses eight-bit index
arithmetic, so a packet whose bytes straddle index `$FF` wraps to the start
of the TX page (a register trace of the boot screen's LED packets shows the
straddling packet's checksum at page offset 0 -- carrying the address into
the following page instead reads unrelated memory and fails the packet's
checksum, and the resulting error reply aborts the driver's TOC read).
The next parsed packet rebases the counter to its visible index, even if the
producer queued several packets under one comparator value.

`cdrom.rs` parses cue sheets (single- or multi-file; MODE1/2048,
MODE1/2352, MODE2/2336, MODE2/2352, and AUDIO tracks;
`PREGAP`/`POSTGAP` as unstored zero-fill
extents, like a CHD's gaps) for both machines and the SCSI/ATAPI drives,
and lays every `FILE` out as a run of extents over a byte-addressed
source. Nero NRG images use the same extent model after their 32- or
64-bit CUE/DAO or ETN footer has supplied the track offsets and stored
pregaps. A raw read from a cooked MODE1/2048 source synthesizes the complete
2352-byte frame: sync and BCD MSF header, EDC, the reserved bytes, and both
Reed-Solomon P/Q parity fields. MODE2/2336 similarly gains its omitted sync,
BCD MSF, and mode header without disturbing the stored XA subheader, payload,
EDC, or parity. Akiko therefore presents the same raw-sector
shape to CDXL players whether their video disc is a bare ISO or BIN/CUE. A
`BINARY` source is the file itself; a `WAVE` or `MP3` source
(`cdrom/audio.rs`) presents the decoded audio as CD-DA sectors --
588 stereo frames per sector, the last sector zero-padded, other sample
rates linearly interpolated in integer arithmetic -- so the layout code
sees only sector bytes. Decoding is on demand: a WAV is random access
(`cdrom/wav.rs`, via `hound`); an MP3 (`cdrom/mp3.rs`, Symphonia's
decoder behind the `cd-mp3` feature) is indexed at load without decoding
(frames located by header, ID3v2 skipped, a Xing/Info frame dropped, a
LAME tag's encoder delay and padding trimmed) and then decoded by a
cursor that follows sequential reads. A jump warms a fresh decoder up on
as many earlier frames as it takes to refill the Layer III bit reservoir
(511 bytes of main data for MPEG-1, 255 for MPEG-2/2.5 -- sized in bytes
from the frame index, since a frame at the bottom of the MPEG-2 range
carries only a byte or two of main data) plus the one-frame-deep
overlap-add and synthesis state, so a sector decodes to the same bytes
whichever way the cursor reached it; that is what keeps a run resumed
from a save state byte-identical to an uninterrupted one, and a unit
test holds it to that down to 8 kbps streams.
A save state records each file's path, format, and sector byte length and
reopens (re-indexes) it on load.

## RTC (`rtc.rs`)

An MSM6242-compatible register view at `$DC0000`, present on machines
configured with `rtc = true`. Reads reflect host time; guest writes only
affect the emulated latch/control state, never the host clock.

The part is four bits wide and wired to the low byte lane alone, so it answers
on odd addresses while the even lane floats with the bus -- with or without a
chip in the socket, since nothing else drives it either. The register select is
A2-A5, so A1 does not reach the decode and each register answers at both of its
odd bytes (register 0 at `+1` and `+3`, register 1 at `+5` and `+7`, ...);
AmigaOS uses `$DC0000 + N * 4 + 3` by convention, not because the part is deaf
at `+1`. Writes take the same lanes as reads.
With `rtc = false` the page still answers the cycle, and the odd lane reads
back `$40` (measured on real A500 hardware) rather than floating. That
distinction matters: every OS clock probe -- AROS `battclock.resource`,
1.3's `SetClock`, 2.0+'s `battclock.resource` -- decides a clock is present
by writing a control nibble and reading it back, so a lane floating to the
last value on the data bus eventually echoes the write and hands the guest
an imaginary clock, and then an imaginary date.

## Input (`gamepad.rs`, window input paths)

Host keyboard events translate to Amiga raw codes and feed a 6500/1
keyboard-MCU model (`chipset/keyboard.rs`) that clocks each event into
CIA-A bit by bit over the emulated KCLK/KDAT lines: 60 us bit cells,
the KDAT handshake after every byte (the MCU samples the line within
microseconds and accepts any deliberate pulse, so software that reads
the keyboard with a brief handshake -- e.g. Pinball Dreams at ~13.5 us
-- works, not just the boot ROM's longer pulse), lost-sync recovery
(lone sync bits, $F9, retransmission), the $FD/$FE power-up stream,
the $78/KCLK-low reset protocol behind Ctrl+Amiga+Amiga, Caps Lock's
keyboard-owned LED toggle, a 10-event type-ahead buffer with $FA
overflow, and ghost suppression on the real A500 key matrix (the seven
qualifiers are on dedicated lines and never ghost). The protocol was
cross-checked against real-hardware-validated replacement keyboard
firmware. Mouse deltas
feed the JOY0DAT quadrature counters. Gamepads are read through `gilrs`
with its bundled SDL controller database enabled: a recognised pad
resolves through a fixed standard layout, overridden per-UUID by the
calibration described in [](../guide/ui), which records raw event codes
(one per control, plus an optional alternate per direction so a stick
and a d-pad can both steer) and is the only path for unrecognised pads.
A direction pair recorded on the two ends of one raw axis is reported
as that stick's deflection as well, which is what the gamepad-mouse
device paces the pointer by. On CD32 machines the pad
output is serialized through the CD32 pad protocol instead of the plain
digital joystick lines, modelled after the pad's 4021 shift register:
in load mode the register's output follows Blue continuously (which is
how Blue doubles as the plain second button on POTxY), and while the
register is clocking each shifted bit reflects only its own button
line, a held Blue included.

The window layer has one host-source policy for the emulated port-2
joystick/CD32 pad: gamepad (the default) or keyboard. Keyboard mode
skips gamepad polling for port-2 input; gamepad mode disables keyboard
joystick capture so the mapped keys take the normal Amiga keyboard path.
(The old auto-detect mode has been removed; `"auto"` in a config parses
as a backward-compatibility alias for gamepad.) Both sources ultimately
call the port-indexed `InputState::set_joystick`
and `set_cd32_buttons` helpers, so JOY1DAT, /FIR1, POT1Y/POTGOR, and
the CD32 serial bits remain hardware-derived.

The two game ports are joined by the parallel-port four-player adapter's
sockets (ports 3 and 4, `InputState::parallel_joysticks`) when
`[parallel] device = "joystick-adapter"` fits it. The adapter is wiring,
not a peripheral: each direction switch shorts one CIA-A port-B data pin to
ground (D0-D3 port 3, D4-D7 port 4), port 3's fire shorts the Centronics
SEL line (CIA-B PA2), port 4's fire shorts BUSY (PA0) and either second
button shorts POUT (PA1) -- the assignment WinUAE's
`handle_parport_joystick` models, which the four-player titles were
written against. The bus overlays the pull-downs on the CIA reads only
for pins the guest has left as inputs (DDR bit clear), so a printer driver
driving the port as outputs is unaffected. The host routing
(`host_routing_for_ports`) queues the sockets behind the game ports for
the pad and keyboard mappings; the recorder, `--joy-after`'s PORT token
and the control protocol address them as ports 3 and 4.

A `lightpen` port device models the pen/gun's two signals. The
photodetector pulls the port's pin 6 (/FIRx) low as the beam sweeps past
it, and on the board that pin is Agnus's LP input -- port 1's on the A1000
(detected by the WCS at $FC0000), port 2's on the A500 and every later
Amiga -- so `Bus::light_pen_wired_port` decides whether a fitted pen
reaches the chip at all. The pen's glass position is kept in rendered-field
coordinates (the space `sprite_framebuffer_origin`, `--mouse-to-after` and
`input.mouse_to` share); at every frame start the bus maps it back
through the renderer's comparator origin
(`bitplane::framebuffer_beam_position`) to the beam line and colour clock
that paints that pixel and hands Agnus the target, and Agnus fires the
latch as its counters sweep past that exact clock (`light_pen_sweep`),
once per field, honouring BPLCON0 LPEN and ECS BEAMCON0 LPENDIS as
before. The Denise output-pipeline delay is not subtracted -- a real pen
reads late by the same clocks, and pen software calibrates it away. The
tip switch / trigger is the port's third-button line (POTxX, read through
POTGOR), because /FIRx is the pulse line and cannot also hold a switch;
`set_mouse_button` index 0 and `set_joystick`'s fire both close it on a
pen port. In the window the pen follows the uncaptured host pointer, traced
back from the canvas pixel through the display copy
(`canvas_source_point`) and the field placement
(`FieldPlacement::field_point`); headless, `--pen-after` and the control
protocol's `input.pen` set the position directly.

Keyboard joystick emulation is deliberately a host input source, not a
guest-keyboard behaviour. When active, the winit key handler consumes the
mapped host keys before rawkey translation: cursor keys drive directions,
Right Ctrl/Right Alt drive fire, and the CD32 extras are C/X/D/S/Return/Z/A.
Each alias is tracked independently before resolving to a single joystick
state, so releasing one fire alias does not clear fire while another alias
is still held. Releases for keys already captured as joystick controls are
also swallowed if the source mode changes before key-up, preventing stray
Amiga rawkey releases.

## Audio output (`audio.rs`)

`AudioSink` abstracts the host boundary: a cpal live sink, a WAV-file sink
(`--audio-wav`), and a null sink (`--noaudio`). Paula renders in emulated
time; the live sink resamples and buffers against wall-clock. The
`CPAL_*` lead/prebuffer/stale-drop targets in `audio.rs` are fixed rather
than adaptive (currently a 131072-frame ring, a ~150 ms prebuffer equal to
the ~150 ms steady lead, and a ~300 ms stale-drop threshold at 44.1 kHz).
Playback starts only after the first audible frames have filled that
prebuffer, so silent boot/load periods do not queue seconds of zeros. If the
cpal callback later drains the queue completely, it stops playback, outputs
silence, and waits for the same prebuffer depth before restarting. While an
already-started queue is merely below target, the sink reports the missing
buffer depth as extra live-audio lead so the real-time pacer runs ahead and
restores the cushion without forcing a host-side silence gap first.

The live queue is host presentation state, not Paula state. A save-state or
reverse-debug timeline jump keeps the restored Paula/CD/floppy mixer state but
discards queued cpal frames from the abandoned timeline, then rebuilds the live
prebuffer from the restored emulated audio stream. Offline WAV capture is not
affected by any of this buffering policy.

Two profiling knobs cover the audio/pacing boundary, both emitting one
`info` line per second:

- `COPPERLINE_AUDIO_PROFILE=1` -- live-audio queue depth and the cpal
  callback counters (callbacks, callback frames, estimated device CCK,
  plus cumulative underrun/overrun/stale-frame totals). The cpal callback
  itself never logs; it only updates atomic counters under this flag.
- `COPPERLINE_REAL_PACING_PROFILE=1` -- the real-speed pacing line:
  retired instructions, raw `m68k` cycles, chip-bus wait CCK, device CCK,
  CPU chip-bus slots, host sleep count/time, and wall-time late
  count/time. Kept separate so CPU/device pacing can be measured without
  enabling the lower-level cpal counters.

Default live-audio warnings are emitted from the producer side at the same
one-second cadence, and only when an underrun, overrun, or stale-frame
counter is nonzero.

(serial-sink)=

## Serial (`serial.rs`)

Paula's SERDAT transmit path lands on a `SerialSink`. The default
`StdoutSink` prints to the host terminal -- this
is how DiagROM's diagnostic stream and the `timing-test/` results are
captured in terminals and CI logs. `TcpSerialSink` bridges the port to a
listening TCP socket (`[serial] mode = "tcp"`, one client at a time) or
dials out to a remote endpoint (`mode = "tcp-connect"` with `connect =
"host:port"` -- the BBS-client wiring), and `PtySerialSink` bridges to a
host pseudo-terminal pair (`mode = "pty"`, Unix only); all are
bidirectional, so an `AUX:` shell on the Amiga side gives a remote
AmigaDOS console. The browser build swaps in a channel-backed sink that
the page bridges to a WebSocket.

`DeviceSerialSink` (`serial/device.rs`, `mode = "device"`, behind the
`host-serial` feature) is a real host serial port on the same trait: the
`serialport` crate FluxBridge already uses, opened 8N1 with OS flow
control off, so the guest owns the handshake exactly as on a real machine.
The sink is written against its own small `HostSerialPort` trait (read
with a bounded timeout, write, the four line settings, the four modem
inputs, discard, clone), which the crate backend implements and an
in-memory fake stands in for under test, so the whole sink -- threads,
unplug, reopen -- is unit-tested with no hardware. Two background threads
own the host I/O: a reader blocking on the port with a 20 ms timeout, which
samples CTS/DSR/DCD/RI after every return (so a line change is seen within
one timeout) and publishes them as `SerialControlLines` bits in an atomic
the bus reads on each CIA-B PRA read; and a writer draining a 256-byte
bounded queue, which the emulation thread blocks on only when it outruns
the wire (an unthrottled run is paced by the port, as a real machine would
be). The guest's settings follow it onto the port: `baud_changed` maps
the SERPER-derived rate onto the nearest standard rate within 3%
(`host_baud_for`; an off-grid rate is passed through for the driver to
judge), `write_word` infers one or two stop bits from the SERDAT word's
bits above the data (Paula has no stop-bit register; the guest writes
them), and `set_control_outputs` mirrors `/DTR` and `/RTS` onto the pins.
A 9-bit word loses its ninth bit (host UARTs carry eight); a received
byte comes back with bit 8 clear. RI is read but has no CIA pin to land
on, so it is only logged. A read or write error is treated as the adapter
gone: the sink detaches (lines float high like an unplugged cable, output
is dropped) and the reader retries the same path every second, reapplying
the settings and DTR/RTS when it reopens. The sink is a live host boundary
(see [architecture](architecture.md#determinism-and-the-host-boundary)):
`reset_after_timeline_jump` drops the host bytes queued on the abandoned
timeline and keeps the port, and `Bus::adopt_host_resources` then pushes
the restored CIA-B outputs and, through `Paula::republish_serial_line_rate`,
the restored SERPER rate onto it.

A `SerialSink` that can *produce* input must override
`has_pending_input` alongside `read_byte`/`read_word`:
Paula's per-tick UART step takes an idle fast path that skips the receiver
entirely while it reports false -- the TCP and pty sinks poll a counter
there, never a syscall.

The sink is also the device on the far end of the RS-232 cable, so it owns
the handshake inputs. `SerialSink::control_lines` reports DSR, CTS, and
carrier detect as asserted-or-not (`SerialControlLines`); the bus samples
it on every guest read of CIA-B PRA and overlays PA3-5 with the levels the
motherboard's inverting 1489 receivers would present (asserted = pin low,
undriven = pulled high), leaving pins the guest has switched to outputs
CIA-driven -- the same shape as the Centronics status overlay on PA0-2.
The guest's `/DTR` (PA7) and `/RTS` (PA6) outputs are the CIA's own pins,
readable by a host bridge through `Cia::port_a_pins`. The default is an
unplugged cable (every input high), which is what the inert and MIDI sinks
keep; `StdoutSink` is a ready device with no carrier; `TcpSerialSink`
is a modem whose carrier follows the live connection (an atomic flag the
acceptor/reader thread maintains, so the PRA read never touches the
writer lock); `PtySerialSink` is a null-modem peer with its port open;
`ChannelSerialSink` starts ready-without-carrier and lets the frontend set
the lines (`ChannelSerialHandle::set_carrier`, exported to the browser as
`serial_set_carrier`); `DeviceSerialSink` reports the real wire. The lines
are host-side state like the bytes
themselves -- never serialized, never part of the deterministic timeline.
Paula has no framing-error or parity hardware: a received word always
carries its stop bit(s) set, and `serial.device` computes parity in
software, so neither needs a model here.

CCP serial observability is a host-side tap beside `SerialSink`, not another
serial device. When a control connection subscribes, each successfully
completed transmit word is copied into a 4,096-entry `VecDeque`; the normal
sink receives the same word immediately. Overflow evicts and counts the
oldest observation, so a debugger cannot back-pressure Paula. The tap is
skipped by serde and carried across state loads with the live serial/audio
sinks; disconnecting or unsubscribing removes it.

## MIDI serial bridge (`midi/`)

`[serial] mode = "midi"` (or `--midi-out`/`--midi-in`) bridges Paula's
serial port to host MIDI, behind the optional `midi` cargo feature -- a
plain build compiles none of it and the mode falls back with a clear
message. The whole thing hangs off one `SerialSink`, `MidiSerialSink`, so
the emulator core is unchanged from any other serial target.

The load-bearing detail is that byte timing survives to the wire. Paula
stamps each transmitted byte with the emulated colour clock it left on
(`SerialTimeAnchor`); `MidiSerialSink` maps that to a host `Instant` and
asks the backend to *schedule* the message for that instant rather than
send it now, so a frame's worth of bytes flushed together still leaves at
the original spacing. Two host-agnostic pieces sit above the backend: a
`MidiFramer` reassembles the single-byte serial stream into whole MIDI
messages (a receiver rejects lone data bytes), tracking running status and
SysEx and passing interleaved real-time bytes straight through; and Active
Sensing (`0xFE`) is forwarded by default -- a real Amiga passes it down the
wire -- and only dropped under `COPPERLINE_MIDI_STRIP_ACTIVE_SENSE=1`.
Input arrives on a lock-free SPSC ring the receiver drains on its idle
fast path, so the poll never locks.

The host connection lives behind the `MidiBackend` trait, chosen by
`cfg(target_os)`: macOS drives CoreMIDI (`coremidi.rs`), Linux the ALSA
sequencer (`alsa.rs`), and Windows WinMM (`winmm.rs`); any other target gets
`stub.rs`, which enumerates nothing and refuses to open. Each backend links its
platform library directly with no wrapper crate, and each maps the
scheduled send onto that platform's timed-delivery primitive: a CoreMIDI
packet timestamp, an ALSA real-time queue event, or -- since WinMM carries no
timestamp -- a scheduler thread that fires each message when it comes due. A
new backend implements `send`/`set_output`/`set_input`/`current_output`/`current_input`
plus free `enumerate`/`open`; nothing else changes. The raw FFI is
layout-sensitive -- CoreMIDI packs its packet list to 4 bytes, the ALSA
`snd_seq_event_t` scheduling helpers are header-only inlines whose field writes
are replicated by hand, and WinMM's `MIDIHDR` is packed -- so the mirrors are
pinned with compile-time layout assertions and want checking against live MIDI,
not just review.

On macOS the process holds exactly one `MIDIClient`, created at first use
(enumeration included) and never disposed. CoreMIDI's link to the MIDIServer
daemon is per-process and does not recover: the daemon exits a few seconds
after the system-wide last client is disposed, and a process whose link dies
that way cannot create a client again until it is relaunched. The one held
client keeps the daemon running for the app's lifetime; each machine's
backend owns only its ports.

Two debug knobs help tell a dead path from a routing one:
`COPPERLINE_MIDI_DEBUG=1` reports per-second tx/rx byte counts and the
first bytes sent (no tx while a song plays means the guest is not driving
serial, i.e. the fault is upstream of the bridge); `=2` decodes every
message in each direction. `COPPERLINE_MIDI_IMMEDIATE=1` bypasses
scheduling and sends each message for immediate delivery, to separate a
timing problem from a connection one.

## Parallel port peripherals (`parallel.rs`, `sampler.rs`)

The Centronics port's peripheral boundary is the `ParallelPort` trait.
CIA-A port B (`$BFE101`) carries the eight data pins and CIA-A's `PC`
output is the active-low printer strobe: the bus forwards each strobe
with the physical pin levels, and a peripheral that accepts the byte
returns true, which the bus turns into the printer's active-low `/ACK`
edge on CIA-A FLAG. An input peripheral instead drives the data pins
itself on every CIA-A port-B read. The status lines BUSY, POUT, and SEL
are CIA-B port A pins 0-2, peripheral-driven inputs with motherboard
pull-ups. The default null peripheral is an unplugged cable: it neither
acknowledges nor drives any pin, and the pulled-up status lines read all
high.

`[parallel] device = "joystick-adapter"` is not a `ParallelPort` peripheral
at all: the four-player adapter's switches live in the deterministic
`InputState` (see the Input section above) and the bus overlays them on
the CIA reads, so save states and reverse replay carry them like any
other controller.

`[parallel] device = "printer"` captures strobed bytes to the configured
output file (`FileParallelPort`), holding the status lines at
ready-online levels (SEL high, BUSY and POUT low) -- without those,
`parallel.device` polls BUSY forever and never sends a byte. `device =
"sampler"` fits the classic mono 8-bit parallel-port digitizer
(AMAS/DSS-class, modelled on the open-amiga-sampler schematics): a host
capture stream fills a ring in real time and each port-B read returns
the sample for the elapsed *emulated* time, so recordings line up
however fast or slow the Amiga polls. Samples are 8-bit offset-binary
(128 = silence), host left and right are summed to the mono input, and
the preamp gain is clamped to +/-24 dB. `COPPERLINE_SAMPLER_DEBUG=1`
logs the captured input level about once a second -- a CLI VU meter for
checking the host microphone is feeding the port.
