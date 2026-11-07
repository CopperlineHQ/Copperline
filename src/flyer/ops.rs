// SPDX-License-Identifier: GPL-3.0-or-later

//! The Flyer's command handlers: what its 68000 firmware (Flyer.bin) does
//! for each mailbox opcode, done in Rust instead of by running it.
//!
//! The card powers up in its boot ROM, which waits for flyer.library to
//! download Flyer.bin into SRAM and send FIRMWARE; from then on the
//! firmware's main loop serves the mailbox. Here FIRMWARE switches the
//! model from [`Cpu::Rom`] to [`Cpu::Running`] without looking at the code,
//! and the handlers below stand in for the firmware's. Their answers
//! (results, error codes, EEPROM contents) follow the firmware sources,
//! file and function named at each handler, so the library and the Toaster
//! software see what the firmware would tell them. The SCSI drives are
//! host folders ([`super::drive`], [`super::fs`]); a drive with no folder
//! does not answer. Commands the HLE does not model yet complete with
//! `OKAY` and one log line, so a caller is never left polling a slot that
//! never finishes.
//!
//! Commands are served synchronously, inside the Amiga's write of the
//! opcode word: the library writes it last and then polls for done, and
//! for GETCLOCK and CACHETEST it does so with interrupts off. A command
//! the firmware's task would wait in (DOSEARCH for the last frame to take,
//! FLYPLAY for a channel a jog session holds) stays in progress and is
//! tried again every field until it can finish.

use super::fs::Filesystem;
use super::link::FlyerLink;
use super::mailbox::{self, err, op, Slot, STAT_NEW};
use super::play::{self, Player, Request};
use crate::zorro_device::HostBeam;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// The beam line at which the field clock steps. The Flyer's clock runs
/// from its own top-of-field interrupt; seen from the Amiga, the switcher's
/// `SyncClocks` notes, it "usually changes about 40% down the field".
pub const FIELD_CLOCK_LINE: u32 = 105;

/// EEPROM words (93C46-type, 64 x 16 bits). Addresses from the firmware's
/// `flyer.h` (`NVOL_`).
pub mod nvol {
    pub const MAGIC: u8 = 0;
    pub const BOARDREV: u8 = 1;
    pub const CHIPREVS: u8 = 2;
    pub const SERLO: u8 = 3;
    pub const SERHI: u8 = 4;
    pub const APLAYOFF: u8 = 5;
    pub const BPLAYOFF: u8 = 6;
    pub const ARECOFF: u8 = 7;
    pub const BRECOFF: u8 = 8;
    pub const DACAPHASE: u8 = 9;
    pub const DACBPHASE: u8 = 10;
    pub const ADCPHASE: u8 = 11;
    pub const PLLNULL: u8 = 12;
    pub const PEDESTALA: u8 = 13;
    pub const PEDESTALB: u8 = 14;
    pub const SKEWBASELINE: u8 = 17;
    pub const OPTIONSLO: u8 = 18;
    pub const OPTIONSHI: u8 = 19;
    /// `NVOL_SETUPMAGIC`: the EEPROM has been set up.
    pub const SETUP_MAGIC: u16 = 0xBEEF;
    /// Words in the part.
    pub const WORDS: usize = 64;
}

/// The board's serial number, in the EEPROM. flyer.library maps it to a
/// chip set through `Flyer_Support/chipset.dat`, and fails with error 1 if
/// no range holds it. The installed ranges are 1C `0-8004800`, 2C
/// `8004801-8004900`, 2CA `8004901-8009900` and 2CC `8100000-8105000`; this
/// is a 2CC board, the last production run.
pub const SERIAL_NUMBER: u32 = 8_100_100;

/// The PCB revision in the EEPROM. The firmware only distinguishes
/// revision 1 (the first FPGA programming scheme) from the rest.
const BOARD_REVISION: u16 = 2;

/// User options (`NVOL_OPTIONS`, the Toaster's Flyer setup): the bits are
/// "not HQ5" (1), "stop on timing errors" (2), "not HQ6" (3) and drop-frame
/// detection (0). Factory default here: HQ5 and HQ6 allowed, nothing else.
const DEFAULT_OPTIONS: u32 = 0;

/// Calibration defaults the firmware writes to a blank EEPROM
/// (`GetDefaults`, `hard.c`).
const DEFAULT_PLAY_OFFSET: u16 = 0xFFDD;
const DEFAULT_REC_OFFSET: u16 = 21;
const DEFAULT_REC_SKEW: u16 = 0x78;
const NTSC_BLANK: u16 = 60;

/// Frequencies the firmware gives the four clock synthesizers at start
/// (`main.c`): 0 DMA, 1 audio, 2 M-coders, 3 P-coders.
const DEFAULT_CLOCKS: [u32; 4] = [28_000_000, 84_587_402, 65_000_000, 14_318_180];

/// FPGA chip numbers (`chips.h`): 1 skew, 2 timer/router, 3-4 P-coders,
/// 5-6 M-coders, 7 DMA, 8 audio DMA, 9 aligner.
const FPGA_CHIPS: u8 = 9;
/// Chips 3-6, the VTASC coders that change with play and record mode.
const VTASC_CHIPS: u16 = 0b111_1000;

/// Which program the Flyer's 68000 is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cpu {
    /// The boot ROM, waiting for Flyer.bin to be downloaded.
    Rom,
    /// Flyer.bin, serving the mailbox.
    Running,
}

/// The video engines' mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoMode {
    None,
    Play,
    Record,
}

/// The routing switches the Toaster software sets (TOASTMUX, INPUTSEL,
/// TERMINATE). Stored for the video model to come.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routing {
    /// Toaster input 3 shows Flyer channel A (else the external input).
    pub flyer_on_input3: bool,
    /// Toaster input 4 shows Flyer channel B.
    pub flyer_on_input4: bool,
    /// Toaster Preview shows the Flyer's camcorder input.
    pub camcorder_on_preview: bool,
    /// Record source: 0 camcorder, 1 S-VHS, 2 VID1, 3 VID2, 4 Main, 5 Preview.
    pub record_source: u8,
    /// Genlock reference: 0 Toaster Main, 1 Toaster VID1.
    pub sync_source: u8,
    /// Termination flags (TERMINATE).
    pub termination: u8,
}

impl Default for Routing {
    /// The firmware's `DFLT_ROUTING`: sync and record video from Toaster
    /// Main, inputs 3/4 and Preview passed through.
    fn default() -> Self {
        Self {
            flyer_on_input3: false,
            flyer_on_input4: false,
            camcorder_on_preview: false,
            record_source: 4,
            sync_source: 0,
            termination: 0,
        }
    }
}

/// Calibration values (READCALIB/WRITECALIB), loaded from the EEPROM when
/// the firmware starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calibration {
    pub play_offset: [u16; 2],
    pub rec_offset: [u16; 2],
    /// DAC A, DAC B and ADC phase: edge(2) << 7 | coarse(4) << 3 | fine(3).
    pub skew: [u16; 3],
    pub pedestal: [u16; 2],
}

/// The audio codec's input settings (CODECCTRL), kept for the record
/// panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Codec {
    /// Input source per side: 0 the RCA line in, 1 AUX1, 2 LINE2.
    pub source: [u8; 2],
    /// Input gain per side, 1-16 (0 mutes).
    pub gain: [u8; 2],
    /// AUX1 and AUX2 mix-in (`$80` mutes).
    pub mix: [u8; 2],
}

impl Default for Codec {
    /// Line 1 at gain index 8 (+12 dB), the firmware's `DFLTRECGAIN`.
    fn default() -> Self {
        Self {
            source: [0; 2],
            gain: [9; 2],
            mix: [0x80; 2],
        }
    }
}

/// What a command did when it was started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Finished with this error code: mark the slot done.
    Done(u8),
    /// Still running; the slot stays in progress (CACHETEST waiting for
    /// the Amiga to clear `cont`, a clip playing).
    Held,
    /// Waiting for something on the card: the slot stays in progress and
    /// the command is tried again every field.
    Waiting,
}

/// The Flyer's firmware state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Firmware {
    pub cpu: Cpu,
    /// The slot whose QUIT completed: the firmware reboots into the ROM
    /// once the library frees it (`Suicide_cmd`).
    quitting: Option<Slot>,
    /// The slot whose CACHETEST waits for `cont` to go to 0.
    cache_test: Option<Slot>,
    pub eeprom: Vec<u16>,
    /// The field clock (GETCLOCK): one count per video field.
    pub field_clock: u32,
    /// Where the beam was at the last tick, to count the field clock line.
    beam: Option<HostBeam>,
    /// The previous field's long-frame flag, to tell interlace; `None`
    /// when that field was not seen.
    last_field_long: Option<bool>,
    /// Toggles on each long field of an interlaced picture; set on field I
    /// of the four-field colour frame.
    colour_frame_field_one: bool,
    pub mode: VideoMode,
    /// Bit n: FPGA chip n is configured.
    pub fpgas: u16,
    /// The VTASC coder revision from the last P/M-coder download.
    pub vtasc_version: u8,
    pub dsp_running: bool,
    /// The four synthesizer frequencies actually set, in Hz.
    pub clocks: [u32; 4],
    pub routing: Routing,
    pub calibration: Calibration,
    /// The user options word (GETSETOPT), as in the EEPROM.
    pub options: u32,
    /// The six LEDs the firmware drives (FLOOBY item 14).
    pub leds: u8,
    /// Still mode (FLOOBY item 9): 1 field, 2 frame, 4 colour frame.
    still_mode: u8,
    /// Date from SETCLOCK: days, minutes, ticks (Amiga DateStamp), and the
    /// field clock when it was set.
    pub date: [u32; 3],
    date_clock: u32,
    /// VIDPARAM's bytes (compression tolerances, frequency, lengths, FIR).
    vid_params: [u8; 10],
    /// The serial-bus registers, for SBUSW/SBUSR (test tools).
    serial_bus: Vec<u8>,
    /// The drives and the firmware's filesystem state.
    pub fs: Filesystem,
    /// The sequencer, the clip player and the DSP.
    pub player: Player,
    pub codec: Codec,
    /// Opcodes already reported as not modelled.
    reported: u128,
    /// The ribbon to the Toaster (its inputs 3 and 4, the sound), when
    /// one is fitted. Not saved: a loaded state keeps the live one.
    #[serde(skip)]
    link: Option<Arc<FlyerLink>>,
    /// Field clocks stepped since the last [`Self::run_fields`].
    #[serde(skip)]
    fields_due: Vec<u32>,
    /// Commands waiting on the card ([`Outcome::Waiting`]), oldest first:
    /// their slot and opcode.
    #[serde(default)]
    waiting: Vec<(Slot, u16)>,
    /// The cutting room's list, between NEWCUTLIST and MAKESUBCLIPS.
    #[serde(default)]
    cut: Option<CutList>,
}

/// A clip being cut up (`NewCutList`, clip.c:1691): the master and the
/// sub-clips defined so far, in field order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CutList {
    drive: u8,
    /// The master's host file.
    master: PathBuf,
    /// Fields in the master.
    fields: u32,
    /// `CLF_DESTRUCT`: the master goes once its sub-clips are made.
    destructive: bool,
    subs: Vec<SubClip>,
}

/// One sub-clip (`AddSubClip`, clip.c:1748).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SubClip {
    drive: u8,
    /// `AF_VIDEO`, `AF_AUDIOL`, `AF_AUDIOR`.
    flags: u8,
    refnum: u16,
    start: u32,
    fields: u32,
}

/// Cut-list flags (`CLF_`, `Clip.h`) and clip-action flags (`AF_`,
/// `acts.h`).
const CLF_DESTRUCT: u8 = 1 << 0;
const AF_VIDEO: u8 = 1 << 0;
const AF_AUDIOL: u8 = 1 << 1;
const AF_AUDIOR: u8 = 1 << 2;

/// The name a sub-clip is made under until flyer.library renames it
/// (`MakeTempName`, clip.c): `_tmp_` and the reference number, three hex
/// digits.
fn temp_name(refnum: u16) -> String {
    format!("_tmp_{:03X}", refnum & 0xFFF)
}

impl Default for Firmware {
    fn default() -> Self {
        Self::new()
    }
}

impl Firmware {
    /// A Flyer at power-on: the boot ROM waiting for a download, with a
    /// set-up EEPROM.
    pub fn new() -> Self {
        Self {
            cpu: Cpu::Rom,
            quitting: None,
            cache_test: None,
            eeprom: factory_eeprom(),
            field_clock: 0,
            beam: None,
            last_field_long: None,
            colour_frame_field_one: false,
            mode: VideoMode::None,
            fpgas: 0,
            vtasc_version: 0,
            dsp_running: false,
            clocks: DEFAULT_CLOCKS,
            routing: Routing::default(),
            calibration: Calibration::default(),
            options: DEFAULT_OPTIONS,
            // "Flyer autobooted properly" (FlyerLEDs.doc).
            leds: 0x04,
            still_mode: 4,
            date: [0; 3],
            date_clock: 0,
            vid_params: [0; 10],
            serial_bus: vec![0; 256],
            fs: Filesystem::default(),
            player: Player::default(),
            codec: Codec::default(),
            reported: 0,
            link: None,
            fields_due: Vec::new(),
            waiting: Vec::new(),
            cut: None,
        }
    }

    /// Serve the drives from the folders in `folder` (the Toaster's
    /// `Flyer` folder): `FA0`, `FB0`, ...
    pub fn with_drives(mut self, folder: Option<PathBuf>) -> Self {
        self.fs = Filesystem::new(folder);
        self
    }

    /// Connect the ribbon to the Toaster.
    pub fn with_link(mut self, link: Option<Arc<FlyerLink>>) -> Self {
        self.link = link;
        self
    }

    pub fn link(&self) -> Option<&Arc<FlyerLink>> {
        self.link.as_ref()
    }

    /// Take over the ribbon of the card this one replaces (a loaded
    /// state), and tell the Toaster this card's routing.
    pub fn adopt_link(&mut self, link: Option<Arc<FlyerLink>>) {
        self.link = link;
        self.publish_routing();
    }

    /// The Amiga was reset: so is the card (its 68000 restarts in the boot
    /// ROM). The EEPROM keeps its contents; the SRAM is not touched.
    pub fn reset(&mut self) {
        let eeprom = std::mem::take(&mut self.eeprom);
        let field_clock = self.field_clock;
        let folder = self.fs.folder().map(PathBuf::from);
        let link = self.link.take();
        *self = Self::new().with_drives(folder).with_link(link);
        self.eeprom = eeprom;
        self.field_clock = field_clock;
        self.publish_routing();
    }

    /// Tell the Toaster which of its inputs the Flyer has.
    fn publish_routing(&self) {
        if let Some(link) = &self.link {
            link.set_routing(self.routing.flyer_on_input3, self.routing.flyer_on_input4);
        }
    }

    /// The Amiga wrote `len` bytes at window offset `off`; serve whatever
    /// that did to the mailbox.
    pub fn after_write(&mut self, sram: &mut [u8], off: usize, len: usize) {
        for touch in mailbox::touched_slots(off, len) {
            let slot = touch.slot;
            if touch.opcode {
                let status = slot.status(sram);
                if self.quitting == Some(slot) && status == mailbox::STAT_BLANK {
                    self.quitting = None;
                    self.reboot_into_rom();
                }
                if self.cache_test == Some(slot) && status != mailbox::STAT_INPROG {
                    self.cache_test = None;
                }
                if status == STAT_NEW {
                    self.start(sram, slot);
                }
            }
            if touch.cont && self.cache_test == Some(slot) && slot.cont(sram) == 0 {
                self.finish_cache_test(sram, slot);
            }
            if touch.cont && slot.status(sram) == mailbox::STAT_INPROG {
                // A clip's abort, pause or resume.
                let cont = slot.cont(sram);
                self.player
                    .cont_written(slot, cont, sram, self.link.as_deref());
                // A clip still waiting for its channel, aborted: it ends
                // having shown nothing.
                if cont == 0 {
                    if let Some(k) = self
                        .waiting
                        .iter()
                        .position(|&(s, o)| s == slot && o == op::FLYPLAY)
                    {
                        self.waiting.remove(k);
                        let r = Request::read(sram, play::action_of(sram, slot));
                        let first = if r.flags & play::caf::VIDEO != 0 {
                            r.vid_start
                        } else {
                            r.aud_start
                        };
                        mailbox::wr32(sram, r.action + play::ca::LAST_FIELD_DONE, first);
                        slot.set_error(sram, err::OKAY);
                        slot.mark_done(sram);
                    }
                }
            }
        }
    }

    /// Start the new command in `slot`, as the firmware's main loop does.
    fn start(&mut self, sram: &mut [u8], slot: Slot) {
        let opcode = slot.opcode(sram);
        if self.cpu == Cpu::Rom {
            // The boot ROM is not in NewTek's release; all the library
            // ever sends it is FIRMWARE (the library refuses other
            // commands until it has). Anything else gets BADCOMMAND rather
            // than a slot that never completes.
            slot.mark_busy(sram);
            let error = if opcode == op::FIRMWARE {
                self.boot(sram, slot);
                err::OKAY
            } else {
                log::warn!(
                    "flyer: {} sent while the boot ROM waits for Flyer.bin",
                    mailbox::opcode_name(opcode)
                );
                err::BADCOMMAND
            };
            slot.set_error(sram, error);
            slot.mark_done(sram);
            return;
        }
        slot.mark_busy(sram);
        let outcome = self.run(sram, slot, opcode);
        if self.diag() {
            log::info!(
                "flyer: slot {} {} ({:02X}) -> {:?} [{}]",
                slot.0,
                mailbox::opcode_name(opcode),
                opcode,
                outcome,
                hex(&sram[slot.base() + 4..slot.base() + mailbox::CMD_LEN])
            );
        }
        match outcome {
            Outcome::Done(error) => {
                slot.set_error(sram, error);
                slot.mark_done(sram);
            }
            Outcome::Waiting => self.waiting.push((slot, opcode)),
            Outcome::Held => {}
        }
    }

    /// Try the waiting commands again, in the order they came.
    fn retry_waiting(&mut self, sram: &mut [u8]) {
        let mut k = 0;
        while k < self.waiting.len() {
            let (slot, opcode) = self.waiting[k];
            // The Amiga gave up on it (freed or reused the slot).
            if slot.status(sram) != mailbox::STAT_INPROG || slot.opcode(sram) != opcode {
                self.waiting.remove(k);
                continue;
            }
            match self.run(sram, slot, opcode) {
                Outcome::Waiting => k += 1,
                outcome => {
                    self.waiting.remove(k);
                    if self.diag() {
                        log::info!(
                            "flyer: slot {} {} -> {:?} (after waiting)",
                            slot.0,
                            mailbox::opcode_name(opcode),
                            outcome
                        );
                    }
                    if let Outcome::Done(error) = outcome {
                        slot.set_error(sram, error);
                        slot.mark_done(sram);
                    }
                }
            }
        }
    }

    fn diag(&self) -> bool {
        crate::envcfg::flag("COPPERLINE_DIAG_FLYER")
    }

    /// The boot ROM ran the downloaded Flyer.bin: its start-up, as far as
    /// the Amiga can tell (`main.c`, `GetDefaults` in `hard.c`). The real
    /// firmware's main loop then marks slot 0 done as its "I'm running"
    /// handshake; the library only ever sends FIRMWARE through slot 0, so
    /// marking the FIRMWARE slot done is the same thing.
    fn boot(&mut self, sram: &mut [u8], slot: Slot) {
        let length = slot.arg32(sram, 8);
        let go = slot.arg32(sram, 4);
        log::info!("flyer: Flyer.bin started ({length} bytes at SRAM ${go:05X})");
        self.cpu = Cpu::Running;
        self.mode = VideoMode::None;
        self.field_clock = 0;
        self.clocks = DEFAULT_CLOCKS;
        self.routing = Routing::default();
        self.still_mode = 4;
        // GetDefaults: a never set-up EEPROM gets the defaults, then
        // everything is read back from it.
        if self.ee(nvol::MAGIC) != nvol::SETUP_MAGIC {
            write_default_calibration(&mut self.eeprom);
        }
        self.options =
            (u32::from(self.ee(nvol::OPTIONSHI)) << 16) | u32::from(self.ee(nvol::OPTIONSLO));
        self.calibration = Calibration {
            play_offset: [self.ee(nvol::APLAYOFF), self.ee(nvol::BPLAYOFF)],
            rec_offset: [self.ee(nvol::ARECOFF), self.ee(nvol::BRECOFF)],
            skew: [
                self.ee(nvol::DACAPHASE),
                self.ee(nvol::DACBPHASE),
                self.ee(nvol::ADCPHASE),
            ],
            pedestal: [self.ee(nvol::PEDESTALA), self.ee(nvol::PEDESTALB)],
        };
        // InitFileSys and DriveSurvey.
        self.fs.boot(sram);
        self.player = Player::default();
        self.waiting.clear();
        self.publish_routing();
    }

    /// QUIT's `ColdBoot`: back into the boot ROM.
    fn reboot_into_rom(&mut self) {
        log::info!("flyer: Flyer.bin quit, back in the boot ROM");
        self.cpu = Cpu::Rom;
        self.mode = VideoMode::None;
        self.cache_test = None;
        self.leds = 0x04;
        self.player = Player::default();
        self.waiting.clear();
    }

    fn ee(&self, addr: u8) -> u16 {
        self.eeprom[usize::from(addr) % nvol::WORDS]
    }

    fn ee_write(&mut self, addr: u8, value: u16) {
        self.eeprom[usize::from(addr) % nvol::WORDS] = value;
    }

    /// Run one command in the firmware.
    fn run(&mut self, sram: &mut [u8], slot: Slot, opcode: u16) -> Outcome {
        use Outcome::Done;
        match opcode {
            // Null_cmd; Firmware ("Code already running!"); the release
            // firmware is built without DEBUG, which leaves RunTest_cmd
            // (CALLMOD) and DebugFlags_cmd empty; LoadVid is absent from
            // shipping builds.
            // WriteProt_cmd is "currently unimplemented" in the firmware.
            op::NULL
            | op::FIRMWARE
            | op::CALLMOD
            | op::DEBUGFLAGS
            | op::LOADVID
            | op::WRITEPROT => Done(err::OKAY),
            op::PGMFPGA => Done(self.pgm_fpga(sram, slot)),
            op::SBUSW => {
                let addr = slot.arg8(sram, 4);
                self.serial_bus[usize::from(addr)] = slot.arg8(sram, 5);
                Done(err::OKAY)
            }
            op::SBUSR => {
                let addr = slot.arg8(sram, 4);
                slot.set_arg8(sram, 5, self.serial_bus[usize::from(addr)]);
                Done(err::OKAY)
            }
            op::CPUW | op::CPUR => Done(self.cpu_access(sram, slot, opcode == op::CPUW)),
            op::GETCLOCK => {
                slot.set_arg32(sram, 4, self.field_clock);
                Done(err::OKAY)
            }
            op::DSPBOOT => {
                // DSPboot loads, verifies and starts the ADSP-2115; with
                // no DSP to fail, it always succeeds.
                self.dsp_running = true;
                Done(err::OKAY)
            }
            op::VIDPARAM => {
                for (i, b) in self.vid_params.iter_mut().enumerate() {
                    *b = slot.arg8(sram, 4 + i);
                }
                Done(err::OKAY)
            }
            op::PLAYMODE => Done(self.set_mode(VideoMode::Play)),
            op::RECMODE => Done(self.set_mode(VideoMode::Record)),
            op::NOMODE => {
                // NoMode_cmd also unconfigures the four VTASC coders; the
                // library downloads the play or record pair next.
                self.fpgas &= !VTASC_CHIPS;
                Done(self.set_mode(VideoMode::None))
            }
            op::TOASTMUX => {
                // ToasterMux: nonzero puts the Flyer's channel on the input.
                self.routing.flyer_on_input3 = slot.arg8(sram, 4) != 0;
                self.routing.flyer_on_input4 = slot.arg8(sram, 5) != 0;
                self.routing.camcorder_on_preview = slot.arg8(sram, 6) != 0;
                self.publish_routing();
                Done(err::OKAY)
            }
            op::INPUTSEL => {
                self.routing.record_source = slot.arg8(sram, 4);
                self.routing.sync_source = slot.arg8(sram, 5);
                Done(err::OKAY)
            }
            op::TERMINATE => {
                self.routing.termination = slot.arg8(sram, 4);
                Done(err::OKAY)
            }
            // No A/B heads are needed when clips are host files.
            op::NEWHEADLIST | op::ENDHEADLIST | op::MAKEHEAD | op::VOIDHEAD | op::VOIDALL => {
                Done(err::OKAY)
            }
            op::SCSIRST | op::SCSIINIT => {
                self.fs.survey();
                Done(err::OKAY)
            }
            op::FINDDRV => Done(self.fs.find_drives(sram, slot)),
            op::CPUDMA => Done(self.fs.cpu_dma(sram, slot)),
            op::FIRXCHG => Done(self.fs.fir_exchange(sram, slot)),
            op::FLOOBY => Done(self.flooby(sram, slot)),
            op::QUIT => {
                // Suicide_cmd marks itself done, then waits for the library
                // to free the slot before rebooting.
                self.quitting = Some(slot);
                Done(err::OKAY)
            }
            op::SETCLOCK => {
                self.date = [
                    slot.arg32(sram, 4),
                    slot.arg32(sram, 8),
                    slot.arg32(sram, 12),
                ];
                self.date_clock = self.field_clock;
                Done(err::OKAY)
            }
            op::WRITECALIB => Done(self.write_calib(sram, slot)),
            op::READCALIB => Done(self.read_calib(sram, slot)),
            op::EEWRITE => {
                let (addr, data) = (slot.arg8(sram, 4), slot.arg16(sram, 6));
                self.ee_write(addr, data);
                Done(err::OKAY)
            }
            op::EEREAD => {
                let addr = slot.arg8(sram, 4);
                slot.set_arg16(sram, 6, self.ee(addr));
                Done(err::OKAY)
            }
            op::RESETFLYER => {
                self.player.reset(sram, self.link.as_deref());
                Done(err::OKAY)
            }
            op::FLYPLAY => self.fly_play(sram, slot),
            op::SRCHARGS => self.search_args(sram, slot),
            op::DOSEARCH => {
                let action = play::action_of(sram, slot);
                let link = self.link.clone();
                step(
                    self.player
                        .do_search(slot, sram, action, self.field_clock, link.as_deref()),
                )
            }
            op::NEWSEQ => {
                self.player.new_sequence();
                Done(err::OKAY)
            }
            op::ADDSEQCLIP => Done(self.add_sequence_clip(sram, slot)),
            op::ENDSEQ => {
                // Nothing to arrange; the progress fraction (0.32) is full.
                slot.set_arg32(sram, 28, u32::MAX);
                Done(err::OKAY)
            }
            op::PLAYSEQ => Done(if self.mode == VideoMode::Play {
                let base = slot.arg32(sram, 4);
                self.player
                    .play_sequence(base, self.field_clock, sram, self.link.as_deref())
            } else {
                err::WRONGMODE
            }),
            op::CHGAUDIO => {
                let r = Request::read(sram, play::action_of(sram, slot));
                self.player.change_audio(&r);
                Done(err::OKAY)
            }
            op::AUDEKEY => {
                self.player.add_envelope_key(
                    slot.arg32(sram, 4),
                    slot.arg32(sram, 8),
                    [slot.arg16(sram, 14), slot.arg16(sram, 16)],
                    [slot.arg16(sram, 18), slot.arg16(sram, 20)],
                );
                Done(err::OKAY)
            }
            op::CODECCTRL => Done(self.codec_ctrl(sram, slot)),
            // No SMPTE reader is fitted: the time code reads as zeros.
            op::GETSMPTE => Done(err::OKAY),
            op::SETCLOCKGEN => {
                let (clock, freq) = (slot.arg8(sram, 4), slot.arg32(sram, 6));
                Done(self.set_clock_freq(clock, freq))
            }
            op::TBC => Done(self.tbc(sram, slot)),
            op::GETSETOPT => Done(self.get_set_options(sram, slot)),
            op::CACHETEST => self.start_cache_test(sram, slot),
            // The firmware's FuncTable has `Unknown` here.
            op::CMPGRIPS
            | op::EXAMINE
            | op::CHANGEMODE
            | op::OPENREADFLD
            | op::READLINE
            | op::WRITELINE
            | op::SETFILLCOLOR
            | op::SKIPLINES
            | op::SELFTEST => Done(err::BADCOMMAND),
            _ if opcode >= op::COUNT => Done(err::BADCOMMAND),
            _ if is_drive_command(opcode) => Done(self.drive_command(sram, slot, opcode)),
            _ => {
                self.report_unmodelled(opcode);
                Done(err::OKAY)
            }
        }
    }

    /// The clip a ClipAction names: `None` for no name, else its host file
    /// and what it holds, or the error the firmware's `GetNamedClipInfo`
    /// gives.
    fn named_clip(&mut self, sram: &[u8], action: usize) -> Result<Option<play::ClipInfo>, u8> {
        let volume = mailbox::rd32(sram, action + play::ca::VOLUME) as usize;
        let drive = mailbox::rd8(sram, volume + 5);
        let path = super::fs::read_cstr(sram, mailbox::rd32(sram, volume) as usize);
        if super::drive::strip_device(&path).is_empty() {
            return Ok(None);
        }
        let host = self.fs.clip_file(drive, &path)?;
        self.player
            .clip_info(&host)
            .map(Some)
            .ok_or(super::fs::ERR_BADVIDHDR)
    }

    /// FLYPLAY (`Play1Clip_cmd`): play one clip. The command stays in
    /// progress until the clip is done or aborted.
    fn fly_play(&mut self, sram: &mut [u8], slot: Slot) -> Outcome {
        let action = play::action_of(sram, slot);
        // "Not timely anymore, but at least let'em know we started."
        slot.set_cont(sram, 2);
        let r = Request::read(sram, action);
        let volume = mailbox::rd32(sram, action + play::ca::VOLUME) as usize;
        if (r.flags & play::caf::VIDEO != 0 && usize::from(r.channel) >= play::VIDEO_CHANNELS)
            || mailbox::rd8(sram, volume + 5) >= super::fs::DRIVES
        {
            return Outcome::Done(err::BADPARAM);
        }
        if self.mode != VideoMode::Play {
            return Outcome::Done(err::WRONGMODE);
        }
        let clip = match self.named_clip(sram, action) {
            Ok(Some(clip)) => clip,
            Ok(None) => return Outcome::Done(err::BADPARAM),
            Err(e) => return Outcome::Done(e),
        };
        log::info!(
            "flyer: play {} on {} from field {} for {} (go {})",
            clip.path.display(),
            if r.flags & play::caf::VIDEO != 0 {
                ["A", "B"][usize::from(r.channel) % 2]
            } else {
                "audio"
            },
            r.vid_start,
            r.vid_count,
            r.go_clock
        );
        // A jog session holds the channel: FLYPLAY waits for it to close
        // (the firmware's channel semaphore), or fails if asked to.
        if r.flags & play::caf::VIDEO != 0 && self.player.channel_held(r.channel) {
            return if r.permiss & play::CAPF_ERRIFBUSY != 0 {
                Outcome::Done(play::ERR_CHANINUSE)
            } else {
                Outcome::Waiting
            };
        }
        let link = self.link.clone();
        match self
            .player
            .play_clip(slot, r, clip, self.field_clock, sram, link.as_deref())
        {
            Ok(()) => Outcome::Held,
            Err(e) => Outcome::Done(e),
        }
    }

    /// SRCHARGS (`SearchArgs`): open (1), change the sound of (2) or close
    /// (0) the jog/shuttle session on the clip the ClipAction names.
    fn search_args(&mut self, sram: &mut [u8], slot: Slot) -> Outcome {
        let action = play::action_of(sram, slot);
        let flag = slot.arg8(sram, 8);
        let r = Request::read(sram, action);
        let link = self.link.clone();
        step(match flag {
            0 => self
                .player
                .close_search(&r, self.field_clock, link.as_deref()),
            1 if !self.player.search_open() => {
                let volume = mailbox::rd32(sram, action + play::ca::VOLUME) as usize;
                if mailbox::rd8(sram, volume + 5) >= super::fs::DRIVES {
                    return Outcome::Done(err::BADPARAM);
                }
                match self.named_clip(sram, action) {
                    Ok(clip) => self.player.open_search(&r, clip),
                    Err(e) => play::Step::Done(e),
                }
            }
            1 => self.player.open_search(&r, None),
            _ => self.player.change_search(&r, self.field_clock),
        })
    }

    /// ADDSEQCLIP (`AddSequenceClip`): one clip of the sequence.
    fn add_sequence_clip(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let action = play::action_of(sram, slot);
        let r = Request::read(sram, action);
        if self.diag() {
            log::info!("flyer: ADDSEQCLIP {r:?}");
        }
        match self.named_clip(sram, action) {
            Ok(clip) => self.player.add_sequence_clip(r, clip),
            Err(e) => e,
        }
    }

    /// CODECCTRL (`CODEC_cmd`, `Audio.c`): set the codec's inputs, or read
    /// its overrange bits or 8-bit peak meters. The meters show the
    /// Toaster's sound, the nearest thing to a line input there is.
    fn codec_ctrl(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        const SENSE: u8 = 1 << 0;
        const SETGAIN: u8 = 1 << 1;
        const SETSRC: u8 = 1 << 2;
        const SETMIX: u8 = 1 << 3;
        const SENSE8: u8 = 1 << 4;
        let ctrl = slot.arg32(sram, 4) as usize;
        let oper = slot.arg8(sram, 8);
        let rd = |off: usize| mailbox::rd8(sram, ctrl + off);
        if oper & SETGAIN != 0 {
            self.codec.gain = [rd(6), rd(7)];
        }
        if oper & SETSRC != 0 {
            self.codec.source = [rd(4), rd(5)];
        }
        if oper & SETMIX != 0 {
            self.codec.mix = [rd(8), rd(10)];
        }
        let peak = self.link.as_ref().map_or((0.0, 0.0), |l| l.line_peak());
        if oper & SENSE != 0 {
            // Two bits of overrange per side: 3 is clipping.
            let over = |p: f32| if p >= 0.99 { 3 } else { 0 };
            mailbox::wr8(sram, ctrl + 2, over(peak.0));
            mailbox::wr8(sram, ctrl + 3, over(peak.1));
        } else if oper & SENSE8 != 0 {
            let level = |p: f32| (p.clamp(0.0, 1.0) * 255.0).round() as u8;
            mailbox::wr8(sram, ctrl + 2, level(peak.0));
            mailbox::wr8(sram, ctrl + 3, level(peak.1));
        }
        err::OKAY
    }

    /// Play the fields whose clock stepped since the last call: video to
    /// the Toaster, a field of sound, commands that finished.
    pub fn run_fields(&mut self, sram: &mut [u8]) {
        // Called at every instruction boundary: nothing to do between
        // fields, and that is decided before anything is touched.
        if self.fields_due.is_empty() {
            return;
        }
        if self.cpu != Cpu::Running {
            self.fields_due.clear();
            return;
        }
        let link = self.link.clone();
        for clock in std::mem::take(&mut self.fields_due) {
            self.player.field(clock, sram, link.as_deref());
            if !self.waiting.is_empty() {
                self.retry_waiting(sram);
            }
        }
    }

    /// Log the first use of an opcode the HLE does not model yet.
    fn report_unmodelled(&mut self, opcode: u16) {
        let bit = 1u128 << (opcode & 0x7F);
        if self.reported & bit == 0 {
            self.reported |= bit;
            log::warn!(
                "flyer: {} ({opcode:02X}) is not emulated yet",
                mailbox::opcode_name(opcode)
            );
        }
    }

    /// A storage command: to a drive that is not there it times out (the
    /// firmware's SCSI selection does), to one that is it goes to the
    /// filesystem.
    fn drive_command(&mut self, sram: &mut [u8], slot: Slot, opcode: u16) -> u8 {
        // Not modelled, and a success that did nothing would mislead: the
        // editor's destructive Cut Clip deletes the source's icons after a
        // cut that "worked", and the drive tools read empty SCSI answers
        // as a drive. These fail the way a board without the feature would.
        match opcode {
            op::NEWCUTLIST => return self.new_cut_list(sram, slot),
            op::ADDSUBCLIP => return self.add_sub_clip(sram, slot),
            op::MAKESUBCLIPS => return self.make_sub_clips(sram, slot),
            op::READTEST => {
                self.report_unmodelled(opcode);
                return err::CMDFAILED;
            }
            op::SCSIDIRECT | op::SCSICALL => {
                self.report_unmodelled(opcode);
                return err::SELTIMEOUT;
            }
            _ => {}
        }
        let Some(drive) = Filesystem::target(opcode, sram, slot) else {
            self.report_unmodelled(opcode);
            return err::OKAY;
        };
        if drive >= super::fs::DRIVES {
            return err::BADPARAM;
        }
        if !self.fs.present(drive) {
            if self.diag() {
                log::info!(
                    "flyer: {} -> no drive {} (SELTIMEOUT)",
                    mailbox::opcode_name(opcode),
                    super::drive::drive_name(drive)
                );
            }
            return err::SELTIMEOUT;
        }
        if let Some(error) = self.fs.run(sram, slot, opcode) {
            return error;
        }
        self.report_unmodelled(opcode);
        err::OKAY
    }

    /// NEWCUTLIST (`NewCutList`, clip.c:1691): open the cutting room on
    /// the clip the ClipAction names (`+4`), with `CLF_` flags at `+8`.
    fn new_cut_list(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        self.cut = None;
        let action = slot.arg32(sram, 4) as usize;
        let flags = slot.arg8(sram, 8);
        let volume = mailbox::rd32(sram, action + play::ca::VOLUME) as usize;
        let drive = mailbox::rd8(sram, volume + 5);
        let path = super::fs::read_cstr(sram, mailbox::rd32(sram, volume) as usize);
        let master = match self.fs.clip_file(drive, &path) {
            Ok(m) => m,
            Err(e) => return e,
        };
        let mut head = [0u8; flyerclip::BLOCK];
        let header = std::fs::File::open(&master)
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head))
            .ok()
            .and_then(|()| flyerclip::ClipHeader::parse(&head).ok());
        let Some(header) = header else {
            return super::fs::ERR_BADVIDHDR;
        };
        if self.diag() {
            log::info!(
                "flyer: NEWCUTLIST {} ({} fields){}",
                master.display(),
                header.fields,
                if flags & CLF_DESTRUCT != 0 {
                    ", destructive"
                } else {
                    ""
                }
            );
        }
        self.cut = Some(CutList {
            drive,
            master,
            fields: header.fields,
            destructive: flags & CLF_DESTRUCT != 0,
            subs: Vec::new(),
        });
        err::OKAY
    }

    /// ADDSUBCLIP (`AddSubClip`, clip.c:1748): drive `+4`, `AF_` flags
    /// `+5`, reference number `+6`, first field `+8`, fields `+12`.
    fn add_sub_clip(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let Some(cut) = self.cut.as_mut() else {
            return err::CMDFAILED;
        };
        let start = slot.arg32(sram, 8);
        if start > cut.fields {
            return err::BADPARAM;
        }
        // "Ensure not past end (but forgive -- sloppy UI is calling this!)"
        let fields = slot.arg32(sram, 12).min(cut.fields - start);
        let sub = SubClip {
            drive: if cut.destructive {
                cut.drive
            } else {
                slot.arg8(sram, 4)
            },
            flags: slot.arg8(sram, 5),
            refnum: slot.arg16(sram, 6),
            start,
            fields,
        };
        log::debug!("flyer: ADDSUBCLIP {sub:?}");
        let at = cut.subs.partition_point(|s| s.start <= sub.start);
        cut.subs.insert(at, sub);
        err::OKAY
    }

    /// MAKESUBCLIPS (`MakeSubClips`, clip.c:1914): with `+4` set, make
    /// every sub-clip as a new clip named `_tmp_<ref>` in the root of its
    /// drive (flyer.library then renames each to the name the user gave),
    /// and on a destructive cut take the master away; either way the list
    /// is closed. The first error is returned; a sub-clip that failed is
    /// not made.
    fn make_sub_clips(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let Some(cut) = self.cut.take() else {
            return err::OKAY;
        };
        if slot.arg8(sram, 4) == 0 {
            return err::OKAY;
        }
        let bytes = match std::fs::read(&cut.master) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("flyer: reading {}: {e}", cut.master.display());
                return err::CMDFAILED;
            }
        };
        let master = match flyerclip::Clip::read(&bytes) {
            Ok(c) => c,
            Err(_) => return super::fs::ERR_BADVIDHDR,
        };
        let mut first_error = err::OKAY;
        let mut made = 0;
        for sub in &cut.subs {
            let parts = flyerclip::cut::Parts {
                video: sub.flags & AF_VIDEO != 0,
                audio: sub.flags & (AF_AUDIOL | AF_AUDIOR) != 0,
            };
            let error = match flyerclip::cut::sub_clip(&master, sub.start, sub.fields, parts) {
                Ok(clip) => match self
                    .fs
                    .write_root_file(sub.drive, &temp_name(sub.refnum), &clip)
                {
                    Ok(()) => {
                        made += 1;
                        err::OKAY
                    }
                    Err(e) => e,
                },
                Err(_) => err::BADPARAM,
            };
            if error != err::OKAY {
                log::warn!(
                    "flyer: sub-clip {} not made (error {error:#04X})",
                    sub.refnum
                );
                if first_error == err::OKAY {
                    first_error = error;
                }
            }
        }
        log::info!("flyer: cut {} into {made} clip(s)", cut.master.display());
        if cut.destructive && first_error == err::OKAY {
            if let Err(e) = self.fs.retire_file(cut.drive, &cut.master) {
                first_error = e;
            }
        }
        first_error
    }

    /// PGMFPGA (`PgmFPGA`, `hard.c`): configure one FPGA, or a pair of
    /// identical ones, from a bitstream in SRAM.
    fn pgm_fpga(&mut self, sram: &[u8], slot: Slot) -> u8 {
        let data = slot.arg32(sram, 4) as usize;
        let length = slot.arg32(sram, 8) as usize;
        let chip = slot.arg8(sram, 12);
        let dual = slot.arg8(sram, 13) != 0;
        let revision = slot.arg8(sram, 14);
        if !(1..=FPGA_CHIPS).contains(&chip) {
            return err::BADPARAM;
        }
        let last = if dual { chip + 1 } else { chip };
        // A chip that is sent nothing, or a bitstream running off the
        // SRAM, never raises DONE: the firmware unconfigures it again.
        if length == 0 || data.saturating_add(length) > mailbox::SRAM_SIZE || last > FPGA_CHIPS {
            for n in chip..=last.min(FPGA_CHIPS) {
                self.fpgas &= !(1 << n);
            }
            return err::CMDFAILED;
        }
        for n in chip..=last {
            self.fpgas |= 1 << n;
        }
        if (3..=6).contains(&chip) {
            self.vtasc_version = revision;
        }
        err::OKAY
    }

    /// CPUW/CPUR: a word access anywhere in the Flyer CPU's map. Only the
    /// SRAM is modelled; elsewhere writes are dropped and reads give 0.
    fn cpu_access(&mut self, sram: &mut [u8], slot: Slot, write: bool) -> u8 {
        let addr = slot.arg32(sram, 4) & 0x00FF_FFFF;
        let in_sram = (mailbox::SRAM_CPU_BASE..mailbox::SRAM_CPU_BASE + mailbox::SRAM_SIZE as u32)
            .contains(&addr);
        let off = (addr - mailbox::SRAM_CPU_BASE.min(addr)) as usize;
        if write {
            if in_sram {
                let value = slot.arg16(sram, 8);
                mailbox::wr16(sram, off, value);
            }
        } else {
            let value = if in_sram { mailbox::rd16(sram, off) } else { 0 };
            slot.set_arg16(sram, 8, value);
        }
        err::OKAY
    }

    fn set_mode(&mut self, mode: VideoMode) -> u8 {
        if mode != self.mode {
            log::info!("flyer: {mode:?} mode");
        }
        self.mode = mode;
        err::OKAY
    }

    /// FLOOBY (`SetFloobyDust`, `ops.c`): miscellaneous settings.
    fn flooby(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let chan = slot.arg8(sram, 4);
        let item = slot.arg8(sram, 5);
        let value = slot.arg32(sram, 6);
        match item {
            // SetClockFreq's result is not passed on here.
            7 => {
                self.set_clock_freq(chan, value);
                err::OKAY
            }
            9 => match value as u8 {
                mode @ (1 | 2 | 4) => {
                    self.still_mode = mode;
                    err::OKAY
                }
                _ => err::BADPARAM,
            },
            14 => {
                let leds = value as u8 & 0x3F;
                if leds != self.leds {
                    log::debug!("flyer: LEDs {}", led_pattern(leds));
                }
                self.leds = leds;
                err::OKAY
            }
            // Debug help, genlock calibration and exchange, test bars,
            // skew and SCSI format: nothing the HLE has hardware for.
            _ => err::OKAY,
        }
    }

    /// SETCLOCKGEN (`SetClockFreq`, `hard.c`): program one ICD2061-type
    /// synthesizer. The firmware searches the dividers for the closest
    /// legal VCO setting, and fails when none is in range; the frequency
    /// kept is the one the chip would really make.
    fn set_clock_freq(&mut self, clock: u8, freq: u32) -> u8 {
        match synthesize(clock, freq) {
            Some(hz) => {
                if let Some(c) = self.clocks.get_mut(usize::from(clock)) {
                    *c = hz;
                }
                err::OKAY
            }
            None => err::CMDFAILED,
        }
    }

    /// WRITECALIB (`WriteCalib`, `ops.c`).
    fn write_calib(&mut self, sram: &[u8], slot: Slot) -> u8 {
        let item = slot.arg16(sram, 4);
        let value = slot.arg16(sram, 6);
        let save = slot.arg8(sram, 8) != 0;
        let c = &mut self.calibration;
        let mut saves: Vec<(u8, u16)> = Vec::new();
        match item {
            9 => {
                c.play_offset[0] = value;
                saves.push((nvol::APLAYOFF, value));
            }
            10 => {
                c.play_offset[1] = value;
                saves.push((nvol::BPLAYOFF, value));
            }
            11 | 12 => {
                c.rec_offset = [value; 2];
                saves.push((nvol::ARECOFF, value));
                saves.push((nvol::BRECOFF, value));
            }
            0..=8 => {
                let chan = usize::from(item / 3);
                let old = c.skew[chan];
                let (mut edge, mut coarse, mut fine) = ((old >> 7) & 3, (old >> 3) & 0xF, old & 7);
                match item % 3 {
                    0 => edge = value,
                    1 => coarse = value,
                    _ => fine = value,
                }
                let skew = (edge << 7) | (coarse << 3) | fine;
                c.skew[chan] = skew;
                let addr = [nvol::DACAPHASE, nvol::DACBPHASE, nvol::ADCPHASE][chan];
                saves.push((addr, skew));
            }
            13 => {
                c.pedestal[0] = value;
                saves.push((nvol::PEDESTALA, value));
            }
            14 => {
                c.pedestal[1] = value;
                saves.push((nvol::PEDESTALB, value));
            }
            _ => {}
        }
        if save {
            for (addr, value) in saves {
                self.ee_write(addr, value);
            }
        }
        err::OKAY
    }

    /// READCALIB (`ReadCalib`, `ops.c`). An unknown item leaves the value
    /// as the library cleared it.
    fn read_calib(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let item = slot.arg16(sram, 4);
        let c = &self.calibration;
        let value = match item {
            9 => Some(c.play_offset[0]),
            10 => Some(c.play_offset[1]),
            11 => Some(c.rec_offset[0]),
            12 => Some(c.rec_offset[1]),
            0..=8 => {
                let skew = c.skew[usize::from(item / 3)];
                Some(match item % 3 {
                    0 => (skew >> 7) & 3,
                    1 => (skew >> 3) & 0xF,
                    _ => skew & 7,
                })
            }
            13 => Some(c.pedestal[0]),
            14 => Some(c.pedestal[1]),
            _ => None,
        };
        if let Some(value) = value {
            slot.set_arg16(sram, 6, value);
        }
        err::OKAY
    }

    /// TBC (`TBC_cmd`, `hard.c`) with no TBC module fitted: the status
    /// reads 0, and setting modes or adjustments fails.
    fn tbc(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        const TBCOP_MODES: u8 = 1 << 1;
        const TBCOP_ADJUST: u8 = 1 << 2;
        const TBCOP_STATUS: u8 = 1 << 0;
        let ctrl = slot.arg32(sram, 4) as usize;
        let oper = slot.arg8(sram, 8);
        if oper & (TBCOP_MODES | TBCOP_ADJUST) != 0 {
            mailbox::wr8(sram, ctrl, 0);
            return err::CMDFAILED;
        }
        if oper & TBCOP_STATUS != 0 {
            mailbox::wr8(sram, ctrl, 0);
        }
        err::OKAY
    }

    /// GETSETOPT (`GetSetOptions`, `ops.c`): the options word, saved in
    /// the EEPROM when it changes.
    fn get_set_options(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        if slot.arg8(sram, 8) != 0 {
            let options = slot.arg32(sram, 4);
            if options != self.options {
                self.ee_write(nvol::OPTIONSHI, (options >> 16) as u16);
                self.ee_write(nvol::OPTIONSLO, options as u16);
                self.options = options;
                log::info!("flyer: options {options:08X}");
            }
        }
        slot.set_arg32(sram, 4, self.options);
        err::OKAY
    }

    /// CACHETEST (`CacheTest_cmd`, `amishar.c`): the library checks that
    /// its CPU does not cache the Flyer's SRAM. The firmware writes
    /// `$FEEDF00D` at `+28`, busy-waits with interrupts off until the Amiga
    /// clears `cont`, then writes `$12345678`; the Amiga, having written
    /// `$DEADD00D` there itself, must read the new value straight back.
    fn start_cache_test(&mut self, sram: &mut [u8], slot: Slot) -> Outcome {
        slot.set_arg32(sram, 28, 0xFEED_F00D);
        if slot.cont(sram) == 0 {
            slot.set_arg32(sram, 28, 0x1234_5678);
            return Outcome::Done(err::OKAY);
        }
        self.cache_test = Some(slot);
        Outcome::Held
    }

    fn finish_cache_test(&mut self, sram: &mut [u8], slot: Slot) {
        self.cache_test = None;
        slot.set_arg32(sram, 28, 0x1234_5678);
        slot.mark_done(sram);
    }

    /// Follow the beam: step the field clock each time it passes
    /// [`FIELD_CLOCK_LINE`]. The bus ticks devices at every instruction
    /// boundary, so normally at most one field goes by between calls; a
    /// longer gap (the CPU stopped) still counts every field.
    pub fn advance_beam(&mut self, beam: HostBeam) {
        let Some(last) = self.beam.replace(beam) else {
            return;
        };
        if (beam.frame, beam.vpos) < (last.frame, last.vpos) {
            // The machine was reset or a state loaded: count from here.
            self.last_field_long = None;
            return;
        }
        let past = |b: &HostBeam| u64::from(b.vpos >= FIELD_CLOCK_LINE);
        // The fields whose clock line went by since the last call.
        let first = last.frame + past(&last);
        let end = beam.frame + past(&beam);
        let count = end.saturating_sub(first);
        // Only the fields seen at a tick have a known long-frame flag.
        let long = |f: u64| {
            if f == beam.frame {
                Some(beam.long_frame)
            } else if f == last.frame {
                Some(last.long_frame)
            } else {
                None
            }
        };
        if count == 0 {
            return;
        }
        self.step_field_clock(long(first));
        if count > 2 {
            self.field_clock = self.field_clock.wrapping_add((count - 2) as u32);
            self.last_field_long = None;
        }
        if count > 1 {
            self.step_field_clock(long(end - 1));
        }
    }

    /// One field: `topoffield` in the firmware's `vid.c`. On field I of
    /// the colour frame the clock is forced to a multiple of four. The
    /// Amiga has no colour frame, so field I is every other long field of
    /// an interlaced picture; without interlace there is no alignment.
    fn step_field_clock(&mut self, long_frame: Option<bool>) {
        self.field_clock = self.field_clock.wrapping_add(1);
        self.align_colour_frame(long_frame);
        // At most a few fields wait: a longer gap has nothing worth playing.
        if self.fields_due.len() < 4 {
            self.fields_due.push(self.field_clock);
        }
    }

    fn align_colour_frame(&mut self, long_frame: Option<bool>) {
        let Some(long_frame) = long_frame else {
            self.last_field_long = None;
            return;
        };
        let interlaced = self.last_field_long == Some(!long_frame);
        self.last_field_long = Some(long_frame);
        if interlaced && long_frame {
            self.colour_frame_field_one = !self.colour_frame_field_one;
            if self.colour_frame_field_one && self.field_clock & 3 != 0 {
                self.field_clock &= !3;
            }
        }
    }
}

/// A player step as a command outcome.
fn step(step: play::Step) -> Outcome {
    match step {
        play::Step::Done(e) => Outcome::Done(e),
        play::Step::Wait => Outcome::Waiting,
    }
}

/// The storage and playback commands that go to a SCSI drive.
fn is_drive_command(opcode: u16) -> bool {
    matches!(
        opcode,
        op::SRCHARGS
            | op::DOSEARCH
            | op::FLYPLAY
            | op::FLYRECORD
            | op::COPYDATA
            | op::SCSICALL
            | op::READSIZE
            | op::READ10
            | op::WRITE10
            | op::DRIVEINFO
            | op::LOCATE
            | op::FREEGRIP
            | op::COPYGRIP
            | op::PARENT
            | op::DIRLIST
            | op::FILEINFO
            | op::FILEOPEN
            | op::FILECLOSE
            | op::FILESEEK
            | op::FILEREAD
            | op::FILEWRITE
            | op::CREATEDIR
            | op::DELETE
            | op::RENAME
            | op::RENAMEDISK
            | op::FORMAT
            | op::DEFRAG
            | op::SETBITS
            | op::SETDATE
            | op::SETCOMMENT
            | op::WRITEPROT
            | op::MAKEFILE
            | op::COPYCLIP
            | op::READTEST
            | op::WRITETEST
            | op::SCSIDIRECT
            | op::OPENWRITEFLD
            | op::CLOSEFIELD
            | op::GETCOMPINFO
            | op::STRIPAUDIO
            | op::NEWCUTLIST
            | op::ADDSUBCLIP
            | op::MAKESUBCLIPS
            | op::ADDSEQCLIP
            | op::LOCATEFLD
            | op::FILEEXTEND
            | op::FILEHSREAD
            | op::FILEHSWRITE
    )
}

/// A set-up EEPROM as a board leaves the factory: the serial number and
/// the calibration defaults, everything else erased (`$FFFF`).
fn factory_eeprom() -> Vec<u16> {
    let mut ee = vec![0xFFFF; nvol::WORDS];
    ee[usize::from(nvol::BOARDREV)] = BOARD_REVISION;
    ee[usize::from(nvol::CHIPREVS)] = 0;
    ee[usize::from(nvol::SERLO)] = SERIAL_NUMBER as u16;
    ee[usize::from(nvol::SERHI)] = (SERIAL_NUMBER >> 16) as u16;
    ee[usize::from(nvol::SKEWBASELINE)] = 0;
    ee[usize::from(nvol::OPTIONSLO)] = DEFAULT_OPTIONS as u16;
    ee[usize::from(nvol::OPTIONSHI)] = (DEFAULT_OPTIONS >> 16) as u16;
    write_default_calibration(&mut ee);
    ee
}

/// What `GetDefaults` writes to an EEPROM without the set-up magic.
fn write_default_calibration(ee: &mut [u16]) {
    let mut set = |addr: u8, value: u16| ee[usize::from(addr)] = value;
    set(nvol::MAGIC, nvol::SETUP_MAGIC);
    set(nvol::APLAYOFF, DEFAULT_PLAY_OFFSET);
    set(nvol::BPLAYOFF, DEFAULT_PLAY_OFFSET);
    set(nvol::ARECOFF, DEFAULT_REC_OFFSET);
    set(nvol::BRECOFF, DEFAULT_REC_OFFSET);
    set(nvol::DACAPHASE, 0);
    set(nvol::DACBPHASE, 0);
    set(nvol::ADCPHASE, DEFAULT_REC_SKEW);
    set(nvol::PEDESTALA, NTSC_BLANK);
    set(nvol::PEDESTALB, NTSC_BLANK);
    let _ = nvol::PLLNULL;
}

/// The frequency, in Hz, an ICD2061-type synthesizer makes for a request
/// of `freq` Hz on output `clock`, or `None` when `SetClockFreq` finds no
/// legal setting. Transcribed from `hard.c`: a 14.31818 MHz reference,
/// prescale 1x (or 2x/4x on the odd outputs), VCO 50-120 MHz in 10 Hz units
/// divided by `2^mux`, P 4-130, Q from the reference's legal range.
pub fn synthesize(clock: u8, freq: u32) -> Option<u32> {
    const REFCLK: u64 = 1_431_818;
    const QLO: u64 = REFCLK.div_ceil(100_000);
    const QHI: u64 = REFCLK / 20_000;
    let freq = freq / 10;
    let maxpre = if clock & 1 != 0 { 2 } else { 1 };
    let mut best: Option<(u64, u64, u64, u64, u64)> = None; // err, pre, mux, p, q
    for pre in 1..=maxpre {
        let scref = REFCLK << pre;
        let scref2 = scref >> 1;
        for mux in 0..=7u32 {
            // A C ULONG shift: bits shifted out are lost.
            let target = u64::from(freq << mux);
            if !(5_000_000..=12_000_000).contains(&target) {
                continue;
            }
            for q in QLO..=QHI {
                let p = (target * q + scref2) / scref;
                if !(4..=130).contains(&p) {
                    continue;
                }
                let made = scref * p / q;
                let error = target.abs_diff(made);
                if best.is_none_or(|b| error < b.0) {
                    best = Some((error, pre, u64::from(mux), p, q));
                }
            }
        }
    }
    let (_, pre, mux, p, q) = best?;
    let vco = (REFCLK << pre) * p / q;
    Some(((vco * 10) >> mux) as u32)
}

/// The six controllable LEDs as FlyerLEDs.doc draws them (LED 1 first).
fn led_pattern(leds: u8) -> String {
    (0..6)
        .map(|n| if leds & (1 << n) != 0 { 'x' } else { '-' })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .chunks(4)
        .map(|c| c.iter().map(|b| format!("{b:02X}")).collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flyer::mailbox::{STAT_DONE, STAT_INPROG};

    /// A Flyer with Flyer.bin running and an SRAM to talk to it through.
    fn running() -> (Firmware, Vec<u8>) {
        let mut fw = Firmware::new();
        let mut sram = vec![0u8; mailbox::SRAM_SIZE];
        let done = send(
            &mut fw,
            &mut sram,
            Slot(0),
            op::FIRMWARE,
            &[(4, 0x18000), (8, 96092)],
        );
        assert_eq!(done, err::OKAY);
        assert_eq!(fw.cpu, Cpu::Running);
        (fw, sram)
    }

    /// Issue a command the way flyer.library does: take the slot, clear it,
    /// fill in long arguments, write the opcode word last. Returns the
    /// error byte; the slot must be done.
    fn send(
        fw: &mut Firmware,
        sram: &mut [u8],
        slot: Slot,
        opcode: u16,
        args: &[(usize, u32)],
    ) -> u8 {
        issue(fw, sram, slot, opcode, |sram| {
            for &(off, value) in args {
                slot.set_arg32(sram, off, value);
            }
        });
        assert_eq!(
            slot.status(sram),
            STAT_DONE,
            "{}",
            mailbox::opcode_name(opcode)
        );
        slot.error(sram)
    }

    fn issue(
        fw: &mut Firmware,
        sram: &mut [u8],
        slot: Slot,
        opcode: u16,
        fill: impl FnOnce(&mut [u8]),
    ) {
        let base = slot.base();
        slot.set_opcode_word(sram, 0x0001);
        fw.after_write(sram, base, 2);
        sram[base + 2..base + 32].fill(0);
        fill(sram);
        slot.set_opcode_word(sram, opcode | STAT_NEW);
        fw.after_write(sram, base, 2);
    }

    /// The library frees a finished slot.
    fn free(fw: &mut Firmware, sram: &mut [u8], slot: Slot) {
        slot.set_opcode_word(sram, 0);
        fw.after_write(sram, slot.base(), 2);
    }

    #[test]
    fn the_boot_rom_runs_flyer_bin_on_firmware() {
        let mut fw = Firmware::new();
        let mut sram = vec![0u8; mailbox::SRAM_SIZE];
        assert_eq!(fw.cpu, Cpu::Rom);
        // Until Flyer.bin runs, nothing else is served.
        assert_eq!(
            send(&mut fw, &mut sram, Slot(0), op::EEREAD, &[]),
            err::BADCOMMAND
        );
        free(&mut fw, &mut sram, Slot(0));
        assert_eq!(
            send(
                &mut fw,
                &mut sram,
                Slot(0),
                op::FIRMWARE,
                &[(4, 0x18000), (8, 96092)]
            ),
            err::OKAY
        );
        assert_eq!(fw.cpu, Cpu::Running);
        assert_eq!(Slot(0).opcode_word(&sram), op::FIRMWARE | STAT_DONE);
        // Once it runs, FIRMWARE is a no-op.
        free(&mut fw, &mut sram, Slot(0));
        assert_eq!(
            send(&mut fw, &mut sram, Slot(0), op::FIRMWARE, &[]),
            err::OKAY
        );
        assert_eq!(fw.cpu, Cpu::Running);
    }

    #[test]
    fn a_taken_slot_is_not_a_command() {
        let (mut fw, mut sram) = running();
        Slot(2).set_opcode_word(&mut sram, 0x0001);
        fw.after_write(&mut sram, Slot(2).base(), 2);
        assert_eq!(Slot(2).opcode_word(&sram), 0x0001);
        // Writing arguments does not start anything either.
        Slot(2).set_arg32(&mut sram, 4, op::GETCLOCK as u32 | 0x4000);
        fw.after_write(&mut sram, Slot(2).base() + 4, 4);
        assert_eq!(Slot(2).opcode_word(&sram), 0x0001);
    }

    #[test]
    fn a_command_clears_a_stale_error_and_completes_in_its_own_slot() {
        let (mut fw, mut sram) = running();
        let slot = Slot(5);
        issue(&mut fw, &mut sram, slot, op::NULL, |sram| {
            slot.set_error(sram, 0x33)
        });
        assert_eq!(slot.opcode_word(&sram), op::NULL | STAT_DONE);
        assert_eq!(slot.error(&sram), err::OKAY);
        assert_eq!(Slot(0).opcode_word(&sram), op::FIRMWARE | STAT_DONE);
    }

    /// A running Flyer with drive FA0 holding `Master`, a clip of
    /// `frames` colour frames; the ClipAction at 0x2000 names it.
    fn cutting_room(frames: u32) -> (tempfile::TempDir, Firmware, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let fa0 = dir.path().join("Flyer/FA0");
        std::fs::create_dir_all(&fa0).unwrap();
        crate::flyer::clip::tests::write_test_clip(&fa0.join("Master"), frames);
        let (fw, mut sram) = running();
        let mut fw = Firmware {
            fs: Filesystem::new(Some(dir.path().join("Flyer"))),
            ..fw
        };
        fw.fs.survey();
        const ACTION: usize = 0x2000;
        const VOLUME: usize = 0x2100;
        const NAME: usize = 0x2200;
        mailbox::wr32(&mut sram, ACTION + play::ca::VOLUME, VOLUME as u32);
        mailbox::wr32(&mut sram, VOLUME, NAME as u32);
        mailbox::wr8(&mut sram, VOLUME + 5, 0);
        sram[NAME..NAME + 15].copy_from_slice(b"FlyerA0:Master\0");
        (dir, fw, sram)
    }

    fn sub(fw: &mut Firmware, sram: &mut [u8], flags: u8, refnum: u16, start: u32, n: u32) -> u8 {
        let e = send(
            fw,
            sram,
            Slot(1),
            op::ADDSUBCLIP,
            &[
                (4, (u32::from(flags) << 16) | u32::from(refnum)),
                (8, start),
                (12, n),
            ],
        );
        free(fw, sram, Slot(1));
        e
    }

    #[test]
    fn the_cutting_room_makes_sub_clips_under_temporary_names() {
        let (dir, mut fw, mut sram) = cutting_room(5);
        let slot = Slot(1);
        assert_eq!(
            send(&mut fw, &mut sram, slot, op::NEWCUTLIST, &[(4, 0x2000)]),
            err::OKAY
        );
        free(&mut fw, &mut sram, slot);
        // Picture and sound from field 4 for 8 fields; sound alone from 0.
        assert_eq!(
            sub(
                &mut fw,
                &mut sram,
                AF_VIDEO | AF_AUDIOL | AF_AUDIOR,
                1,
                4,
                8
            ),
            err::OKAY
        );
        assert_eq!(
            sub(&mut fw, &mut sram, AF_AUDIOL | AF_AUDIOR, 2, 0, 4),
            err::OKAY
        );
        assert_eq!(
            send(&mut fw, &mut sram, slot, op::MAKESUBCLIPS, &[(4, 1 << 24)]),
            err::OKAY
        );
        free(&mut fw, &mut sram, slot);
        let fa0 = dir.path().join("Flyer/FA0");
        let one = std::fs::read(fa0.join("_tmp_001")).unwrap();
        let one = flyerclip::Clip::read(&one).unwrap();
        assert_eq!(one.field_count(), 8);
        let master = std::fs::read(fa0.join("Master")).unwrap();
        let master = flyerclip::Clip::read(&master).unwrap();
        assert_eq!(
            one.decode_field(0).unwrap(),
            master.decode_field(4).unwrap()
        );
        let two = std::fs::read(fa0.join("_tmp_002")).unwrap();
        let two = flyerclip::Clip::read(&two).unwrap();
        assert_eq!(two.header.vid_flag, 0);
        assert_eq!(two.audio().1[..], master.audio().1[..4 * 735 * 2]);
        // The master stays: the cut was not destructive.
        assert!(fa0.join("Master").is_file());
    }

    #[test]
    fn a_destructive_cut_moves_its_master_to_the_trash() {
        let (dir, mut fw, mut sram) = cutting_room(3);
        let slot = Slot(1);
        let flags = u32::from(CLF_DESTRUCT) << 24;
        assert_eq!(
            send(
                &mut fw,
                &mut sram,
                slot,
                op::NEWCUTLIST,
                &[(4, 0x2000), (8, flags)]
            ),
            err::OKAY
        );
        free(&mut fw, &mut sram, slot);
        assert_eq!(sub(&mut fw, &mut sram, AF_VIDEO, 7, 0, 12), err::OKAY);
        assert_eq!(
            send(&mut fw, &mut sram, slot, op::MAKESUBCLIPS, &[(4, 1 << 24)]),
            err::OKAY
        );
        let flyer = dir.path().join("Flyer");
        assert!(flyer.join("FA0/_tmp_007").is_file());
        assert!(!flyer.join("FA0/Master").exists());
        assert!(flyer.join("Trash/Master").is_file());
    }

    #[test]
    fn an_abandoned_cut_list_makes_nothing() {
        let (dir, mut fw, mut sram) = cutting_room(2);
        let slot = Slot(1);
        send(&mut fw, &mut sram, slot, op::NEWCUTLIST, &[(4, 0x2000)]);
        free(&mut fw, &mut sram, slot);
        sub(&mut fw, &mut sram, AF_VIDEO, 1, 0, 4);
        assert_eq!(
            send(&mut fw, &mut sram, slot, op::MAKESUBCLIPS, &[]),
            err::OKAY
        );
        assert!(!dir.path().join("Flyer/FA0/_tmp_001").exists());
        // With no list open, a sub-clip has nowhere to go.
        free(&mut fw, &mut sram, slot);
        assert_eq!(sub(&mut fw, &mut sram, AF_VIDEO, 1, 0, 4), err::CMDFAILED);
    }

    #[test]
    fn raw_scsi_and_read_tests_fail_rather_than_pretend() {
        let (mut fw, mut sram) = running();
        let slot = Slot(3);
        for (opcode, expected) in [
            (op::READTEST, err::CMDFAILED),
            (op::SCSIDIRECT, err::SELTIMEOUT),
            (op::SCSICALL, err::SELTIMEOUT),
        ] {
            assert_eq!(send(&mut fw, &mut sram, slot, opcode, &[]), expected);
            free(&mut fw, &mut sram, slot);
        }
    }

    #[test]
    fn eeprom_holds_a_serial_number_in_the_2cc_range_of_chipset_dat() {
        let (mut fw, mut sram) = running();
        // InitFlyers: ReadEE(4) is the high word, ReadEE(3) the low.
        send(&mut fw, &mut sram, Slot(0), op::EEREAD, &[(4, 4 << 24)]);
        let hi = Slot(0).arg16(&sram, 6);
        free(&mut fw, &mut sram, Slot(0));
        send(&mut fw, &mut sram, Slot(0), op::EEREAD, &[(4, 3 << 24)]);
        let lo = Slot(0).arg16(&sram, 6);
        let serial = (u32::from(hi) << 16) | u32::from(lo);
        assert_eq!(serial, SERIAL_NUMBER);
        // The installed chipset.dat ranges (chipset.asc).
        let ranges = [
            ("1C", 0, 8_004_800),
            ("2C", 8_004_801, 8_004_900),
            ("2CA", 8_004_901, 8_009_900),
            ("2CC", 8_100_000, 8_105_000),
        ];
        let set = ranges
            .iter()
            .find(|(_, lo, hi)| (*lo..=*hi).contains(&serial))
            .map(|(name, ..)| *name);
        assert_eq!(set, Some("2CC"));
        free(&mut fw, &mut sram, Slot(0));
        send(&mut fw, &mut sram, Slot(0), op::EEREAD, &[(4, 0)]);
        assert_eq!(Slot(0).arg16(&sram, 6), nvol::SETUP_MAGIC);
    }

    #[test]
    fn eeprom_writes_read_back_and_survive_a_reset() {
        let (mut fw, mut sram) = running();
        send(
            &mut fw,
            &mut sram,
            Slot(1),
            op::EEWRITE,
            &[(4, 40 << 24), (6, 0x1234 << 16)],
        );
        free(&mut fw, &mut sram, Slot(1));
        fw.reset();
        assert_eq!(fw.cpu, Cpu::Rom);
        assert_eq!(fw.ee(40), 0x1234);
    }

    #[test]
    fn quit_reboots_into_the_rom_once_the_slot_is_freed() {
        let (mut fw, mut sram) = running();
        assert_eq!(send(&mut fw, &mut sram, Slot(0), op::QUIT, &[]), err::OKAY);
        // Suicide_cmd waits for the library before rebooting.
        assert_eq!(fw.cpu, Cpu::Running);
        free(&mut fw, &mut sram, Slot(0));
        assert_eq!(fw.cpu, Cpu::Rom);
        // ProgramChips then downloads Flyer.bin again.
        assert_eq!(
            send(&mut fw, &mut sram, Slot(0), op::FIRMWARE, &[]),
            err::OKAY
        );
        assert_eq!(fw.cpu, Cpu::Running);
    }

    #[test]
    fn cache_test_follows_the_library_handshake() {
        let (mut fw, mut sram) = running();
        let slot = Slot(0);
        // CacheTest: cont = 1, then the opcode.
        issue(&mut fw, &mut sram, slot, op::CACHETEST, |sram| {
            slot.set_cont(sram, 1)
        });
        assert_eq!(slot.status(&sram), STAT_INPROG);
        assert_eq!(slot.arg32(&sram, 28), 0xFEED_F00D);
        // The Amiga writes its own value, then clears cont.
        slot.set_arg32(&mut sram, 28, 0xDEAD_D00D);
        fw.after_write(&mut sram, slot.base() + 28, 4);
        assert_eq!(slot.status(&sram), STAT_INPROG);
        slot.set_cont(&mut sram, 0);
        fw.after_write(&mut sram, slot.base() + 2, 1);
        assert_eq!(slot.arg32(&sram, 28), 0x1234_5678);
        assert_eq!(slot.opcode_word(&sram), op::CACHETEST | STAT_DONE);
        assert_eq!(slot.error(&sram), err::OKAY);
        free(&mut fw, &mut sram, slot);
        assert_eq!(fw.cache_test, None);
    }

    #[test]
    fn pgm_fpga_checks_the_chip_number_and_the_bitstream() {
        let (mut fw, mut sram) = running();
        let pgm = |fw: &mut Firmware,
                   sram: &mut Vec<u8>,
                   data: u32,
                   len: u32,
                   chip: u8,
                   dual: u8,
                   rev: u8| {
            let e = send(
                fw,
                sram,
                Slot(0),
                op::PGMFPGA,
                &[
                    (4, data),
                    (8, len),
                    (12, u32::from_be_bytes([chip, dual, rev, 0])),
                ],
            );
            free(fw, sram, Slot(0));
            e
        };
        assert_eq!(pgm(&mut fw, &mut sram, 0x600, 2800, 1, 0, 0), err::OKAY);
        assert_eq!(fw.fpgas, 1 << 1);
        assert_eq!(pgm(&mut fw, &mut sram, 0x600, 14484, 3, 1, 7), err::OKAY);
        assert_eq!(fw.fpgas, (1 << 1) | (1 << 3) | (1 << 4));
        assert_eq!(fw.vtasc_version, 7);
        assert_eq!(pgm(&mut fw, &mut sram, 0x600, 100, 0, 0, 0), err::BADPARAM);
        assert_eq!(pgm(&mut fw, &mut sram, 0x600, 100, 10, 0, 0), err::BADPARAM);
        assert_eq!(pgm(&mut fw, &mut sram, 0x600, 0, 2, 0, 0), err::CMDFAILED);
        assert_eq!(
            pgm(&mut fw, &mut sram, 0x3_FF00, 0x200, 2, 0, 0),
            err::CMDFAILED
        );
        // NOMODE unconfigures the VTASC coders.
        send(&mut fw, &mut sram, Slot(0), op::NOMODE, &[]);
        assert_eq!(fw.fpgas, 1 << 1);
        assert_eq!(fw.mode, VideoMode::None);
    }

    #[test]
    fn modes_follow_play_record_and_none() {
        let (mut fw, mut sram) = running();
        send(&mut fw, &mut sram, Slot(0), op::PLAYMODE, &[]);
        assert_eq!(fw.mode, VideoMode::Play);
        free(&mut fw, &mut sram, Slot(0));
        send(&mut fw, &mut sram, Slot(0), op::RECMODE, &[]);
        assert_eq!(fw.mode, VideoMode::Record);
    }

    #[test]
    fn set_clock_gen_makes_the_firmware_default_frequencies() {
        // The firmware's own start-up values are all reachable.
        for (clock, &hz) in DEFAULT_CLOCKS.iter().enumerate() {
            let made = synthesize(clock as u8, hz).unwrap();
            let error = made.abs_diff(hz);
            assert!(error < hz / 1000, "clock {clock}: {hz} -> {made}");
        }
        // Too fast for any divider.
        assert_eq!(synthesize(0, 200_000_000), None);
        assert_eq!(synthesize(0, 0), None);
        // The library writes the clock byte at +4 and the speed long at +6.
        let (mut fw, mut sram) = running();
        issue(&mut fw, &mut sram, Slot(0), op::SETCLOCKGEN, |sram| {
            Slot(0).set_arg8(sram, 4, 2);
            Slot(0).set_arg32(sram, 6, 65_000_000);
        });
        assert_eq!(Slot(0).error(&sram), err::OKAY);
        assert!(fw.clocks[2].abs_diff(65_000_000) < 65_000);
        free(&mut fw, &mut sram, Slot(0));
        issue(&mut fw, &mut sram, Slot(0), op::SETCLOCKGEN, |sram| {
            Slot(0).set_arg8(sram, 4, 0);
            Slot(0).set_arg32(sram, 6, 400_000_000);
        });
        assert_eq!(Slot(0).error(&sram), err::CMDFAILED);
    }

    #[test]
    fn getclock_reports_the_field_clock() {
        let (mut fw, mut sram) = running();
        fw.field_clock = 1234;
        send(&mut fw, &mut sram, Slot(3), op::GETCLOCK, &[]);
        assert_eq!(Slot(3).arg32(&sram, 4), 1234);
    }

    #[test]
    fn the_field_clock_steps_once_per_field_at_its_line() {
        let mut fw = Firmware::new();
        let beam = |frame: u64, vpos: u32, long_frame: bool| HostBeam {
            vpos,
            long_frame,
            frame,
        };
        // Without interlace, so no colour-frame alignment.
        fw.advance_beam(beam(10, 50, true));
        assert_eq!(fw.field_clock, 0);
        fw.advance_beam(beam(10, FIELD_CLOCK_LINE - 1, true));
        assert_eq!(fw.field_clock, 0);
        fw.advance_beam(beam(10, FIELD_CLOCK_LINE, true));
        assert_eq!(fw.field_clock, 1);
        fw.advance_beam(beam(10, 250, true));
        assert_eq!(fw.field_clock, 1);
        // Into the next field, before and past the line.
        fw.advance_beam(beam(11, 3, true));
        assert_eq!(fw.field_clock, 1);
        fw.advance_beam(beam(11, 200, true));
        assert_eq!(fw.field_clock, 2);
        // A long gap counts every field.
        fw.advance_beam(beam(15, 200, true));
        assert_eq!(fw.field_clock, 6);
        // Passing the line of the next field, from above it.
        fw.advance_beam(beam(16, 10, true));
        fw.advance_beam(beam(17, 10, true));
        assert_eq!(fw.field_clock, 7);
        // A reset beam just restarts the count.
        fw.advance_beam(beam(0, 200, true));
        assert_eq!(fw.field_clock, 7);
        fw.advance_beam(beam(1, 200, true));
        assert_eq!(fw.field_clock, 8);
    }

    #[test]
    fn interlaced_fields_keep_the_clock_on_the_colour_frame() {
        let mut fw = Firmware::new();
        fw.field_clock = 6;
        let mut long = true;
        let mut at_field_one = Vec::new();
        fw.advance_beam(HostBeam {
            vpos: 0,
            long_frame: long,
            frame: 0,
        });
        for n in 0..16u64 {
            fw.advance_beam(HostBeam {
                vpos: 200,
                long_frame: long,
                frame: n,
            });
            if long && fw.colour_frame_field_one {
                at_field_one.push(fw.field_clock);
            }
            let frame = n + 1;
            long = !long;
            fw.advance_beam(HostBeam {
                vpos: 0,
                long_frame: long,
                frame,
            });
        }
        assert!(at_field_one.len() >= 3);
        assert!(at_field_one.iter().all(|c| c % 4 == 0), "{at_field_one:?}");
        // Without interlace (every field long) the clock just counts.
        let mut fw = Firmware::new();
        fw.advance_beam(HostBeam {
            vpos: 0,
            long_frame: true,
            frame: 0,
        });
        for f in 0..8 {
            fw.advance_beam(HostBeam {
                vpos: 200,
                long_frame: true,
                frame: f,
            });
            fw.advance_beam(HostBeam {
                vpos: 0,
                long_frame: true,
                frame: f + 1,
            });
        }
        assert_eq!(fw.field_clock, 8);
    }

    #[test]
    fn options_are_kept_in_the_eeprom() {
        let (mut fw, mut sram) = running();
        // Get.
        send(
            &mut fw,
            &mut sram,
            Slot(0),
            op::GETSETOPT,
            &[(4, 0xAAAA_AAAA)],
        );
        assert_eq!(Slot(0).arg32(&sram, 4), DEFAULT_OPTIONS);
        free(&mut fw, &mut sram, Slot(0));
        // Set: "not HQ6".
        send(
            &mut fw,
            &mut sram,
            Slot(0),
            op::GETSETOPT,
            &[(4, 8), (8, 1 << 24)],
        );
        assert_eq!(Slot(0).arg32(&sram, 4), 8);
        assert_eq!(fw.ee(nvol::OPTIONSLO), 8);
        assert_eq!(fw.ee(nvol::OPTIONSHI), 0);
        // A restarted Flyer.bin reads them back.
        fw.reset();
        free(&mut fw, &mut sram, Slot(0));
        send(&mut fw, &mut sram, Slot(0), op::FIRMWARE, &[]);
        assert_eq!(fw.options, 8);
    }

    #[test]
    fn calibration_reads_the_eeprom_defaults_and_saves_writes() {
        let (mut fw, mut sram) = running();
        let read = |fw: &mut Firmware, sram: &mut Vec<u8>, item: u16| {
            send(
                fw,
                sram,
                Slot(0),
                op::READCALIB,
                &[(4, u32::from(item) << 16)],
            );
            let v = Slot(0).arg16(sram, 6);
            free(fw, sram, Slot(0));
            v
        };
        assert_eq!(read(&mut fw, &mut sram, 9), DEFAULT_PLAY_OFFSET);
        assert_eq!(read(&mut fw, &mut sram, 11), DEFAULT_REC_OFFSET);
        assert_eq!(read(&mut fw, &mut sram, 13), NTSC_BLANK);
        // ADC phase $78: edge 0, coarse 15, fine 0.
        assert_eq!(read(&mut fw, &mut sram, 6), 0);
        assert_eq!(read(&mut fw, &mut sram, 7), 15);
        assert_eq!(read(&mut fw, &mut sram, 8), 0);
        // Set DAC B fine phase to 5 and save it.
        send(
            &mut fw,
            &mut sram,
            Slot(0),
            op::WRITECALIB,
            &[(4, (5 << 16) | 5), (8, 1 << 24)],
        );
        free(&mut fw, &mut sram, Slot(0));
        assert_eq!(read(&mut fw, &mut sram, 5), 5);
        assert_eq!(fw.ee(nvol::DACBPHASE), 5);
        // Without "save" only the live value changes.
        send(
            &mut fw,
            &mut sram,
            Slot(0),
            op::WRITECALIB,
            &[(4, (13 << 16) | 70), (8, 0)],
        );
        free(&mut fw, &mut sram, Slot(0));
        assert_eq!(read(&mut fw, &mut sram, 13), 70);
        assert_eq!(fw.ee(nvol::PEDESTALA), NTSC_BLANK);
    }

    #[test]
    fn tbc_reports_no_module() {
        let (mut fw, mut sram) = running();
        sram[0x800] = 0xFF;
        // Status only.
        assert_eq!(
            send(
                &mut fw,
                &mut sram,
                Slot(0),
                op::TBC,
                &[(4, 0x800), (8, 1 << 24)]
            ),
            err::OKAY
        );
        assert_eq!(sram[0x800], 0);
        free(&mut fw, &mut sram, Slot(0));
        // The switcher's start-up STATUS|MODES|ADJUST.
        sram[0x800] = 0xFF;
        assert_eq!(
            send(
                &mut fw,
                &mut sram,
                Slot(0),
                op::TBC,
                &[(4, 0x800), (8, 7 << 24)]
            ),
            err::CMDFAILED
        );
        assert_eq!(sram[0x800], 0);
    }

    #[test]
    fn flooby_sets_leds_and_checks_still_modes() {
        let (mut fw, mut sram) = running();
        let flooby = |fw: &mut Firmware, sram: &mut Vec<u8>, item: u8, value: u32| {
            let e = send(
                fw,
                sram,
                Slot(0),
                op::FLOOBY,
                &[(4, u32::from(item) << 16), (6, value)],
            );
            free(fw, sram, Slot(0));
            e
        };
        assert_eq!(flooby(&mut fw, &mut sram, 14, 0x5A), err::OKAY);
        assert_eq!(fw.leds, 0x1A);
        assert_eq!(led_pattern(0x1A), "-x-xx-");
        assert_eq!(flooby(&mut fw, &mut sram, 9, 2), err::OKAY);
        assert_eq!(flooby(&mut fw, &mut sram, 9, 3), err::BADPARAM);
    }

    #[test]
    fn routing_switches_are_stored() {
        let (mut fw, mut sram) = running();
        send(
            &mut fw,
            &mut sram,
            Slot(0),
            op::TOASTMUX,
            &[(4, 0x0100_0000)],
        );
        assert!(fw.routing.flyer_on_input3 && !fw.routing.flyer_on_input4);
        free(&mut fw, &mut sram, Slot(0));
        send(
            &mut fw,
            &mut sram,
            Slot(0),
            op::INPUTSEL,
            &[(4, 0x0201_0000)],
        );
        assert_eq!(fw.routing.record_source, 2);
        assert_eq!(fw.routing.sync_source, 1);
    }

    #[test]
    fn no_drives_answer_yet() {
        let (mut fw, mut sram) = running();
        assert_eq!(
            send(&mut fw, &mut sram, Slot(0), op::DRIVEINFO, &[]),
            err::SELTIMEOUT
        );
        free(&mut fw, &mut sram, Slot(0));
        sram[Slot(0).base() + 6] = 0xFF;
        assert_eq!(
            send(
                &mut fw,
                &mut sram,
                Slot(0),
                op::FINDDRV,
                &[(4, 0x0008_0000)]
            ),
            err::OKAY
        );
        assert_eq!(Slot(0).arg8(&sram, 6), 0);
        free(&mut fw, &mut sram, Slot(0));
        assert_eq!(
            send(
                &mut fw,
                &mut sram,
                Slot(0),
                op::FINDDRV,
                &[(4, 0x0018_0000)]
            ),
            err::BADPARAM
        );
    }

    #[test]
    fn unknown_opcodes_are_badcommand_like_the_firmware() {
        let (mut fw, mut sram) = running();
        for opcode in [
            op::CMPGRIPS,
            op::EXAMINE,
            op::SELFTEST,
            op::OPENREADFLD,
            0x6C,
            0x3FFF,
        ] {
            assert_eq!(
                send(&mut fw, &mut sram, Slot(1), opcode, &[]),
                err::BADCOMMAND
            );
            free(&mut fw, &mut sram, Slot(1));
        }
        // Real but not yet modelled: answered OKAY.
        assert_eq!(
            send(&mut fw, &mut sram, Slot(1), op::CODECCTRL, &[]),
            err::OKAY
        );
    }

    #[test]
    fn cpu_access_reaches_the_sram_through_its_flyer_address() {
        let (mut fw, mut sram) = running();
        send(
            &mut fw,
            &mut sram,
            Slot(0),
            op::CPUW,
            &[(4, 0x00C0_1000), (8, 0xBEEF << 16)],
        );
        assert_eq!(mailbox::rd16(&sram, 0x1000), 0xBEEF);
        free(&mut fw, &mut sram, Slot(0));
        send(&mut fw, &mut sram, Slot(0), op::CPUR, &[(4, 0x00C0_1000)]);
        assert_eq!(Slot(0).arg16(&sram, 8), 0xBEEF);
        free(&mut fw, &mut sram, Slot(0));
        send(&mut fw, &mut sram, Slot(0), op::CPUR, &[(4, 0x00A2_0000)]);
        assert_eq!(Slot(0).arg16(&sram, 8), 0);
    }
}
