// SPDX-License-Identifier: GPL-3.0-or-later

//! Commodore CD32 Full Motion Video module.
//!
//! The cartridge is a 1 MiB Zorro II board normally autoconfigured at
//! $200000: 256 KiB ROM, a small control window, an LSI L64111 MPEG audio
//! decoder, a C-Cube CL450 MPEG video decoder, and 512 KiB RAM.  The guest
//! ROM owns CD sector reads through Akiko and feeds the two decoder ports;
//! this module therefore models the cartridge rather than recognizing any
//! particular disc or title.

use crate::audio::resample::Resampler;
use crate::audio::MIX_SAMPLE_RATE;
use crate::chipset::paula::{CdAudioRing, PAULA_CLOCK_HZ};
use crate::zorro_device::{DeviceHost, ZorroDevice};
use anyhow::{bail, Result};
use plmpeg::{
    DecodeStatus, Decoder as Mpeg1Decoder, Frame as Mpeg1Frame, Sequence as Mpeg1Sequence,
};
use std::collections::VecDeque;
use std::sync::Arc;
use symphonia_bundle_mp3::MpaDecoder;
use symphonia_core::audio::{Audio, GenericAudioBufferRef};
use symphonia_core::codecs::audio::well_known::CODEC_ID_MP2;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia_core::packet::Packet;
use symphonia_core::units::{Duration, Timestamp};

pub const ROM_BYTES: usize = 0x4_0000;
const RAM_BYTES: usize = 0x8_0000;
const ROM_BASE: u32 = 0x000000;
const IO_BASE: u32 = 0x040000;
const L64111_BASE: u32 = 0x050000;
const CL450_DATA: u32 = 0x060000;
const CL450_BASE: u32 = 0x070000;
const RAM_BASE: u32 = 0x080000;
const BANK_MASK: u32 = 0x0F0000;

// Cartridge I/O/status bits.
const IO_CL450_IRQ: u16 = 0x8000;
const IO_L64111_IRQ: u16 = 0x4000;
const IO_CL450_VIDEO: u16 = 0x4000;
const IO_CL450_FIFO_STATUS: u16 = 0x0800;
const IO_L64111_MUTE: u16 = 0x0200;

// L64111 register indices.
const A_DATA: usize = 0;
const A_CONTROL1: usize = 1;
const A_CONTROL2: usize = 2;
const A_CONTROL3: usize = 3;
const A_INT1: usize = 4;
const A_INT2: usize = 5;
const A_PARAM1: usize = 9;
const A_PARAM2: usize = 10;
const A_PARAM3: usize = 11;
const A_PRESENT1: usize = 12;
const A_PRESENT2: usize = 13;
const A_PRESENT3: usize = 14;
const A_PRESENT4: usize = 15;
const A_PRESENT5: usize = 16;
const A_CB_STATUS: usize = 18;
const A_CB_WRITE: usize = 19;
const A_CB_READ: usize = 20;

const AUDIO_FRAME_DETECT: u16 = 0x04;
const AUDIO_PTS_AVAILABLE: u16 = 0x20;
const AUDIO_SYNC: u16 = 0x10;
const SYSTEM_SYNC: u16 = 0x08;
const AUDIO_NEW_FRAME: u16 = 0x01;

// CL450 direct-access registers (byte offsets).
const CMEM_DATA: usize = 0x02;
const CPU_CONTROL: usize = 0x20;
const CPU_PC: usize = 0x22;
const CPU_IADDR: usize = 0x3E;
const CPU_IMEM: usize = 0x42;
const CPU_TMEM: usize = 0x46;
const CPU_INT: usize = 0x54;
const HOST_NEWCMD: usize = 0x56;
const CPU_INTENB: usize = 0x26;
const CPU_TADDR: usize = 0x38;
const CMEM_CONTROL: usize = 0x80;
const CMEM_STATUS: usize = 0x82;
const CMEM_DMACTRL: usize = 0x84;
const HOST_RADDR: usize = 0x88;
const HOST_RDATA: usize = 0x8C;
const HOST_CONTROL: usize = 0x90;
const HOST_SCR0: usize = 0x92;
const HOST_SCR1: usize = 0x94;
const HOST_SCR2: usize = 0x96;
const HOST_INTVECW: usize = 0x98;
const HOST_INTVECR: usize = 0x9C;
const DRAM_REFCNT: usize = 0xAC;
const VID_CONTROL: usize = 0xEC;
const VID_REGDATA: usize = 0xEE;

const CL_SET_BLANK: u16 = 0x030F;
const CL_SET_BORDER: u16 = 0x0407;
const CL_SET_COLOR_MODE: u16 = 0x0111;
const CL_SET_INTERRUPT_MASK: u16 = 0x0104;
const CL_SET_THRESHOLD: u16 = 0x0103;
const CL_SET_VIDEO_FORMAT: u16 = 0x0105;
const CL_SET_WINDOW: u16 = 0x0406;
const CL_DISPLAY_STILL: u16 = 0x000C;
const CL_PAUSE: u16 = 0x000E;
const CL_PLAY: u16 = 0x000D;
const CL_SCAN: u16 = 0x000A;
const CL_SINGLE_STEP: u16 = 0x000B;
const CL_SLOW_MOTION: u16 = 0x0109;
const CL_ACCESS_SCR: u16 = 0x8312;
const CL_FLUSH_BITSTREAM: u16 = 0x8102;
const CL_INQUIRE_BUFFER_FULLNESS: u16 = 0x8001;
const CL_NEW_PACKET: u16 = 0x0408;
const CL_RESET: u16 = 0x8000;

const CL_INT_RDY: u16 = 1 << 10;
const CL_INT_PIC_D: u16 = 1 << 6;
const CL_INT_SEQ_V: u16 = 1 << 3;
const CL_INT_UND: u16 = 1 << 8;
const CL_HMEM_INT_STATUS: usize = 0x0A;

const CL_DRAM_H_SIZE: usize = 0x12;
const CL_DRAM_V_SIZE: usize = 0x13;
const CL_DRAM_PICTURE_RATE: usize = 0x14;
const CL_DRAM_TIME_CODE_0: usize = 0x17;
const CL_DRAM_TIME_CODE_1: usize = 0x18;
const CL_DRAM_VER: usize = 0xA0;
const CL_DRAM_PID: usize = 0xA1;

const CL450_MPEG_BUFFER_SIZE: usize = 65_536;
const CL450_VIDEO_BUFFERS: usize = 8;
const CL450_IMEM_WORDS: usize = 1024;
const CL450_TMEM_WORDS: usize = 128;
const CL450_HMEM_WORDS: usize = 16;
const CL450_VID_REGS: usize = 16;
const CL450_NEWPACKET_BUFFER_SIZE: usize = 32;
const MAX_AUDIO_STREAM_BYTES: usize = 1024 * 1024;
const MAX_PENDING_GOPS: usize = 64;

/// A decoded frame in Copperline's packed little-endian RGBA layout.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FmvFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<Vec<u32>>,
}

/// Cheap immutable frame snapshot carried to the render worker.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FmvPresentation {
    pub enabled: bool,
    pub blank: bool,
    /// Packed 0x00RRGGBB CL450 border latch.
    pub border: u32,
    pub generation: u64,
    pub frame: Option<FmvFrame>,
}

impl PartialEq for FmvPresentation {
    fn eq(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && self.blank == other.blank
            && self.border == other.border
            && self.generation == other.generation
            && self.frame.as_ref().map(|f| (f.width, f.height))
                == other.frame.as_ref().map(|f| (f.width, f.height))
    }
}

impl Eq for FmvPresentation {}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct GopMarker {
    offset: u64,
    hours: u8,
    minutes: u8,
    seconds: u8,
    pictures: u8,
}

enum VideoEvent {
    NeedInput,
    Sequence {
        width: u32,
        height: u32,
        frame_period: u32,
    },
    Gop {
        hours: u8,
        minutes: u8,
        seconds: u8,
        pictures: u8,
    },
    Frame(FmvFrame),
}

/// The CL450-facing MPEG-1 decoder. The decoder's reference pictures and
/// partial bit buffer serialize directly, so save-state restore neither
/// retains nor replays the compressed program stream.
#[derive(serde::Serialize, serde::Deserialize)]
struct VideoDecoder {
    decoder: Mpeg1Decoder,
    observed_sequence: Option<Mpeg1Sequence>,
    gop_scan_tail: Vec<u8>,
    stream_bytes: u64,
    pending_gops: VecDeque<GopMarker>,
    pending_frame: Option<FmvFrame>,
}

impl VideoDecoder {
    fn new() -> Self {
        Self {
            decoder: Mpeg1Decoder::new(),
            observed_sequence: None,
            gop_scan_tail: Vec::new(),
            stream_bytes: 0,
            pending_gops: VecDeque::new(),
            pending_frame: None,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn offer(&mut self, bytes: Vec<u8>) {
        if let Err(error) = self.decoder.push(&bytes) {
            log::warn!("CD32 FMV: resetting MPEG-1 decoder after input error: {error}");
            self.reset();
            if let Err(error) = self.decoder.push(&bytes) {
                log::warn!("CD32 FMV: MPEG-1 decoder rejected fresh input: {error}");
                return;
            }
        }
        self.scan_gops(&bytes);
    }

    fn scan_gops(&mut self, bytes: &[u8]) {
        let old_tail = self.gop_scan_tail.len();
        let base = self.stream_bytes.saturating_sub(old_tail as u64);
        let mut scan = std::mem::take(&mut self.gop_scan_tail);
        scan.extend_from_slice(bytes);
        for (at, code) in start_codes(&scan) {
            if code != 0xB8 || at + 8 > scan.len() || at + 8 <= old_tail {
                continue;
            }
            let bits = u32::from_be_bytes(scan[at + 4..at + 8].try_into().unwrap());
            if self.pending_gops.len() == MAX_PENDING_GOPS {
                self.pending_gops.pop_front();
            }
            self.pending_gops.push_back(GopMarker {
                offset: base + at as u64,
                hours: ((bits >> 26) & 0x1F) as u8,
                minutes: ((bits >> 20) & 0x3F) as u8,
                seconds: ((bits >> 13) & 0x3F) as u8,
                pictures: ((bits >> 7) & 0x3F) as u8,
            });
        }
        self.stream_bytes = self.stream_bytes.saturating_add(bytes.len() as u64);
        let tail = scan.len().saturating_sub(7);
        self.gop_scan_tail.extend_from_slice(&scan[tail..]);
    }

    fn metadata_event(&mut self) -> Option<VideoEvent> {
        if let Some(sequence) = self.decoder.sequence() {
            if self.observed_sequence != Some(sequence) {
                self.observed_sequence = Some(sequence);
                return Some(VideoEvent::Sequence {
                    width: sequence.width as u32,
                    height: sequence.height as u32,
                    frame_period: sequence.frame_rate.period_27mhz(),
                });
            }
        }
        let stream_position = self.decoder.stream_position();
        if let Some(gop) = self
            .pending_gops
            .pop_front_if(|gop| gop.offset <= stream_position)
        {
            return Some(VideoEvent::Gop {
                hours: gop.hours,
                minutes: gop.minutes,
                seconds: gop.seconds,
                pictures: gop.pictures,
            });
        }
        None
    }

    fn parse(&mut self) -> VideoEvent {
        if let Some(frame) = self.pending_frame.take() {
            return VideoEvent::Frame(frame);
        }
        if let Some(event) = self.metadata_event() {
            return event;
        }

        let status = match self.decoder.decode() {
            Ok(status) => status,
            Err(error) => {
                log::warn!("CD32 FMV: resetting malformed MPEG-1 stream: {error}");
                self.reset();
                return VideoEvent::NeedInput;
            }
        };
        if status == DecodeStatus::FrameReady {
            self.pending_frame = self.decoder.frame().and_then(Self::copy_display_frame);
        }
        if let Some(event) = self.metadata_event() {
            return event;
        }
        self.pending_frame
            .take()
            .map_or(VideoEvent::NeedInput, VideoEvent::Frame)
    }

    fn copy_display_frame(frame: &Mpeg1Frame) -> Option<FmvFrame> {
        let width = frame.width;
        let height = frame.height;
        if width == 0 || height == 0 || width > 1920 || height > 1080 {
            return None;
        }
        let y_stride = frame.y.width;
        let c_stride = frame.cb.width;
        let c_height = frame.cb.height;
        if frame.y.data.len() < y_stride.saturating_mul(frame.y.height)
            || frame.cb.data.len() < c_stride.saturating_mul(c_height)
            || frame.cr.data.len() < c_stride.saturating_mul(c_height)
            || frame.cr.width != c_stride
            || frame.cr.height != c_height
        {
            return None;
        }
        let mut pixels = Vec::with_capacity(width * height);
        for py in 0..height {
            let cy = (py / 2).min(c_height.saturating_sub(1));
            for px in 0..width {
                let cx = (px / 2).min(c_stride.saturating_sub(1));
                pixels.push(ycbcr_to_rgba(
                    frame.y.data[py * y_stride + px],
                    frame.cb.data[cy * c_stride + cx],
                    frame.cr.data[cy * c_stride + cx],
                ));
            }
        }
        Some(FmvFrame {
            width: width as u32,
            height: height as u32,
            pixels: Arc::new(pixels),
        })
    }
}

fn ycbcr_to_rgba(y: u8, cb: u8, cr: u8) -> u32 {
    let c = i32::from(y).saturating_sub(16);
    let d = i32::from(cb) - 128;
    let e = i32::from(cr) - 128;
    let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
    let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
    let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
    u32::from_le_bytes([r, g, b, 0xFF])
}

fn start_codes(data: &[u8]) -> impl DoubleEndedIterator<Item = (usize, u8)> + '_ {
    data.windows(4)
        .enumerate()
        .filter_map(|(i, w)| (w[..3] == [0, 0, 1]).then_some((i, w[3])))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Mp2DecoderSnapshot {
    history: Vec<Vec<u8>>,
}

struct Mp2Decoder {
    decoder: MpaDecoder,
    history: VecDeque<Vec<u8>>,
}

fn new_mp2_decoder() -> MpaDecoder {
    let mut params = AudioCodecParameters::new();
    params.for_codec(CODEC_ID_MP2);
    MpaDecoder::try_new(&params, &AudioDecoderOptions::default())
        .expect("symphonia-bundle-mp3 is built with its mp2 feature")
}

impl Mp2Decoder {
    fn new() -> Self {
        Self {
            decoder: new_mp2_decoder(),
            history: VecDeque::new(),
        }
    }

    fn decode(&mut self, frame: &[u8]) -> Option<Vec<(f32, f32)>> {
        let packet = Packet::new(0, Timestamp::ZERO, Duration::ZERO, frame);
        let result = match self.decoder.decode_ref(&packet.as_packet_ref()) {
            Ok(GenericAudioBufferRef::F32(buf)) => {
                if buf.num_planes() == 1 {
                    buf.plane(0)
                        .map(|p| p.iter().map(|&v| (v, v)).collect::<Vec<_>>())
                } else {
                    buf.plane_pair(0, 1)
                        .map(|(l, r)| l.iter().zip(r).map(|(&l, &r)| (l, r)).collect())
                }
            }
            _ => None,
        };
        self.history.push_back(frame.to_vec());
        while self.history.len() > 4 {
            self.history.pop_front();
        }
        result.filter(|samples| !samples.is_empty())
    }

    fn restore(snapshot: Mp2DecoderSnapshot) -> Self {
        let mut decoder = Self::new();
        for frame in snapshot.history {
            let _ = decoder.decode(&frame);
        }
        decoder
    }
}

impl serde::Serialize for Mp2Decoder {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        Mp2DecoderSnapshot {
            history: self.history.iter().cloned().collect(),
        }
        .serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Mp2Decoder {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Ok(Self::restore(Mp2DecoderSnapshot::deserialize(
            deserializer,
        )?))
    }
}

#[derive(Clone, Copy)]
struct Mp2Header {
    len: usize,
    rate: u32,
    mono: bool,
    raw: [u8; 4],
}

fn parse_mp2_header(raw: [u8; 4]) -> Option<Mp2Header> {
    let h = u32::from_be_bytes(raw);
    if h & 0xFFE0_0000 != 0xFFE0_0000 {
        return None;
    }
    let version = (h >> 19) & 3;
    let layer = (h >> 17) & 3;
    let bitrate_index = ((h >> 12) & 15) as usize;
    let rate_index = ((h >> 10) & 3) as usize;
    if version != 3 || layer != 2 || bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        return None;
    }
    const KBPS: [u32; 15] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
    ];
    const RATES: [u32; 3] = [44_100, 48_000, 32_000];
    let rate = RATES[rate_index];
    let padding = (h >> 9) & 1;
    let len = (144 * KBPS[bitrate_index] * 1000 / rate + padding) as usize;
    Some(Mp2Header {
        len,
        rate,
        mono: (h >> 6) & 3 == 3,
        raw,
    })
}

#[derive(serde::Serialize, serde::Deserialize)]
struct AudioPcmFrame {
    samples: Vec<(f32, f32)>,
    index: usize,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct NewPacket {
    remaining: u16,
    pts: u64,
    pts_valid: bool,
}

/// Complete CD32 FMV cartridge state.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Cd32Fmv {
    rom: Vec<u8>,
    ram: Vec<u8>,
    io_reg: u16,

    cl_regs: Vec<u16>,
    cl_imem: Vec<u16>,
    cl_tmem: Vec<u16>,
    cl_hmem: [u16; CL450_HMEM_WORDS],
    cl_vid: [u16; CL450_VID_REGS],
    cl_play: i8,
    cl_blank: bool,
    cl_border: u32,
    cl_interrupt_mask: u16,
    cl_pending_interrupts: u16,
    cl_threshold: u16,
    cl_buffer_empty_count: u8,
    cl_buffer_empty_acc: u64,
    cl_newpacket_mode: bool,
    cl_newpackets: VecDeque<NewPacket>,
    video_input: Vec<u8>,
    video_decoder: VideoDecoder,
    video_frames: VecDeque<FmvFrame>,
    current_frame: Option<FmvFrame>,
    frame_width: u32,
    frame_height: u32,
    frame_period: u32,
    video_present_acc: u64,
    presentation_generation: u64,
    scr: u64,
    scr_acc: u64,

    audio_regs: [u16; 32],
    audio_int_mask: [u16; 2],
    audio_int_status: [u16; 2],
    audio_fifo: Vec<u8>,
    audio_es: Vec<u8>,
    audio_frame_detect: u8,
    audio_head_detect: u8,
    audio_decoder: Mp2Decoder,
    audio_pcm: VecDeque<AudioPcmFrame>,
    audio_sample_acc: u64,
    audio_rate: u32,
    audio_resampler_rate: u32,
    audio_resampler: Resampler,
}

impl Cd32Fmv {
    pub fn new(rom: Vec<u8>) -> Result<Self> {
        if rom.len() != ROM_BYTES {
            bail!(
                "CD32 FMV ROM must be exactly {ROM_BYTES} bytes (256 KiB), got {}",
                rom.len()
            );
        }
        let mut board = Self {
            rom,
            ram: vec![0; RAM_BYTES],
            io_reg: 0,
            cl_regs: vec![0; 256],
            cl_imem: vec![0; CL450_IMEM_WORDS],
            cl_tmem: vec![0; CL450_TMEM_WORDS],
            cl_hmem: [0; CL450_HMEM_WORDS],
            cl_vid: [0; CL450_VID_REGS],
            cl_play: 0,
            cl_blank: false,
            cl_border: 0,
            cl_interrupt_mask: 0,
            cl_pending_interrupts: 0,
            cl_threshold: 4096,
            cl_buffer_empty_count: 0,
            cl_buffer_empty_acc: 0,
            cl_newpacket_mode: false,
            cl_newpackets: VecDeque::new(),
            video_input: Vec::with_capacity(CL450_MPEG_BUFFER_SIZE),
            video_decoder: VideoDecoder::new(),
            video_frames: VecDeque::new(),
            current_frame: None,
            frame_width: 0,
            frame_height: 0,
            frame_period: 1_080_000,
            video_present_acc: 0,
            presentation_generation: 0,
            scr: 0,
            scr_acc: 0,
            audio_regs: [0; 32],
            audio_int_mask: [0; 2],
            audio_int_status: [0; 2],
            audio_fifo: Vec::new(),
            audio_es: Vec::new(),
            audio_frame_detect: 0,
            audio_head_detect: 0,
            audio_decoder: Mp2Decoder::new(),
            audio_pcm: VecDeque::new(),
            audio_sample_acc: 0,
            audio_rate: MIX_SAMPLE_RATE,
            audio_resampler_rate: MIX_SAMPLE_RATE,
            audio_resampler: Resampler::new(MIX_SAMPLE_RATE, MIX_SAMPLE_RATE),
        };
        board.reset_all();
        Ok(board)
    }

    pub fn presentation(&self) -> FmvPresentation {
        FmvPresentation {
            enabled: self.io_reg & IO_CL450_VIDEO != 0,
            blank: self.cl_blank,
            border: self.cl_border,
            generation: self.presentation_generation,
            frame: self.current_frame.clone(),
        }
    }

    fn reset_all(&mut self) {
        self.ram.fill(0);
        self.io_reg = 0;
        self.reset_cl450();
        self.reset_l64111();
        self.presentation_generation = self.presentation_generation.wrapping_add(1);
    }

    fn reset_cl450(&mut self) {
        self.cl_play = 0;
        self.cl_pending_interrupts = 0;
        self.cl_interrupt_mask = 0;
        self.cl_blank = false;
        self.cl_border = 0;
        self.cl_threshold = 4096;
        self.cl_buffer_empty_count = 0;
        self.cl_buffer_empty_acc = 0;
        self.cl_newpacket_mode = false;
        self.cl_newpackets.clear();
        self.video_input.clear();
        self.video_frames.clear();
        self.current_frame = None;
        self.frame_width = 0;
        self.frame_height = 0;
        self.frame_period = 1_080_000;
        self.video_present_acc = 0;
        self.scr = 0;
        self.scr_acc = 0;
        self.cl_regs.fill(0);
        self.cl_imem.fill(0);
        self.cl_tmem.fill(0);
        self.cl_hmem.fill(0);
        self.cl_vid.fill(0);
        self.video_decoder.reset();
        self.write_dram(CL_DRAM_VER, 0x0200);
        self.write_dram(CL_DRAM_PID, 0x0002);
    }

    fn init_cl450_cpu(&mut self) {
        log::debug!("CD32 FMV: CL450 CPU enabled");
        self.cl_hmem[15] = 0;
        self.cl_regs[HOST_NEWCMD] = 0;
        self.cl_regs[CMEM_CONTROL] = 2;
        self.cl_regs[HOST_CONTROL] = 0x0081;
        self.cl_vid[0] = 0xA8C6;
        self.cl_vid[1] = 0x4967;
        self.cl_regs[HOST_SCR2] = 0x1DE0;
        self.cl_regs[HOST_SCR1] = 0;
        self.cl_regs[HOST_SCR0] = 0;
        self.scr = 0;
        self.write_dram(CL_DRAM_VER, 0x0200);
        self.write_dram(CL_DRAM_PID, 0x0002);
        self.ram[0x10..0x100].fill(0);
    }

    fn reset_l64111(&mut self) {
        self.audio_regs.fill(0);
        self.audio_int_mask = [0; 2];
        self.audio_int_status = [0; 2];
        self.audio_regs[A_CONTROL3] = 0x80;
        self.reset_audio_parser();
        self.audio_pcm.clear();
        self.audio_sample_acc = 0;
        self.audio_rate = MIX_SAMPLE_RATE;
        self.audio_resampler_rate = MIX_SAMPLE_RATE;
        self.audio_resampler = Resampler::new(MIX_SAMPLE_RATE, MIX_SAMPLE_RATE);
        self.audio_decoder = Mp2Decoder::new();
    }

    fn reset_audio_parser(&mut self) {
        self.audio_fifo.clear();
        self.audio_es.clear();
        self.audio_frame_detect = 0;
        self.audio_head_detect = 0;
    }

    fn write_dram(&mut self, word: usize, value: u16) {
        let at = word * 2;
        if at + 1 < self.ram.len() {
            self.ram[at..at + 2].copy_from_slice(&value.to_be_bytes());
        }
    }

    fn cl450_irq(&self) -> bool {
        self.cl_regs[CPU_CONTROL] & 1 != 0 && self.cl_regs[HOST_CONTROL] & 0x80 == 0
    }

    fn l64111_irq(&self) -> bool {
        self.audio_int_status[0] & self.audio_int_mask[0] != 0
            || (((self.audio_int_status[1] << 1) | (self.audio_int_status[1] >> 7))
                & self.audio_int_mask[1]
                != 0)
    }

    fn cl450_set_status(&mut self, mask: u16) {
        self.cl_pending_interrupts |= mask & self.cl_interrupt_mask;
        if self.cl_hmem[CL_HMEM_INT_STATUS] == 0 && self.cl_pending_interrupts != 0 {
            self.cl_hmem[CL_HMEM_INT_STATUS] = self.cl_pending_interrupts;
            self.cl_pending_interrupts = 0;
            self.cl_regs[HOST_CONTROL] &= !0x80;
        }
    }

    fn audio_set_status(&mut self, bank: usize, mask: u16) {
        self.audio_int_status[bank] |= mask;
    }

    fn read_be(data: &[u8], at: usize, size: usize) -> u32 {
        match size {
            1 => data.get(at).copied().unwrap_or(0xFF).into(),
            2 => u16::from_be_bytes([
                data.get(at).copied().unwrap_or(0xFF),
                data.get(at + 1).copied().unwrap_or(0xFF),
            ])
            .into(),
            4 => u32::from_be_bytes([
                data.get(at).copied().unwrap_or(0xFF),
                data.get(at + 1).copied().unwrap_or(0xFF),
                data.get(at + 2).copied().unwrap_or(0xFF),
                data.get(at + 3).copied().unwrap_or(0xFF),
            ]),
            _ => 0,
        }
    }

    fn write_be(data: &mut [u8], at: usize, size: usize, value: u32) {
        match size {
            1 => {
                if let Some(byte) = data.get_mut(at) {
                    *byte = value as u8;
                }
            }
            2 => {
                if let Some(dst) = data.get_mut(at..at + 2) {
                    dst.copy_from_slice(&(value as u16).to_be_bytes());
                }
            }
            4 => {
                if let Some(dst) = data.get_mut(at..at + 4) {
                    dst.copy_from_slice(&value.to_be_bytes());
                }
            }
            _ => {}
        }
    }

    fn io_read(&self, off: u32) -> u16 {
        if off & 0xFFFF != 0 {
            return 0;
        }
        let mut value = IO_CL450_IRQ | IO_L64111_IRQ | IO_CL450_FIFO_STATUS;
        if self.cl450_irq() {
            value &= !IO_CL450_IRQ;
        }
        if self.l64111_irq() {
            value &= !IO_L64111_IRQ;
        }
        value
    }

    fn io_write(&mut self, off: u32, value: u16) {
        if off & 0xFFFF == 0 {
            let changed = self.io_reg ^ value;
            self.io_reg = value;
            if changed & IO_CL450_VIDEO != 0 {
                self.presentation_generation = self.presentation_generation.wrapping_add(1);
            }
        }
    }

    fn l64111_read(&mut self, off: u32) -> u16 {
        let reg = ((off >> 1) & 31) as usize;
        match reg {
            A_INT1 => {
                let value = self.audio_int_status[0];
                self.audio_int_status[0] = 0;
                value
            }
            A_INT2 => {
                let value = self.audio_int_status[1] & 0x7F;
                self.audio_int_status[1] = 0;
                value
            }
            _ => self.audio_regs[reg],
        }
    }

    fn l64111_write(&mut self, off: u32, value: u16) {
        let reg = ((off >> 1) & 31) as usize;
        match reg {
            A_CONTROL1 => {
                if value & 2 != 0 {
                    self.reset_l64111();
                } else {
                    if (value ^ self.audio_regs[A_CONTROL1]) & 1 != 0 {
                        self.reset_audio_parser();
                    }
                    if value & 4 != 0 {
                        self.audio_regs[A_CB_WRITE] = 0;
                        self.audio_regs[A_CB_READ] = 0;
                        self.audio_regs[A_CB_STATUS] = 0;
                        self.audio_pcm.clear();
                        self.audio_sample_acc = 0;
                        self.audio_resampler_rate = self.audio_rate.max(1);
                        self.audio_resampler =
                            Resampler::new(self.audio_resampler_rate, MIX_SAMPLE_RATE);
                    }
                }
                self.audio_regs[A_CONTROL1] = value;
            }
            A_DATA => {
                self.audio_regs[A_DATA] = value;
                if self.audio_fifo.len() + 2 <= MAX_AUDIO_STREAM_BYTES {
                    self.audio_fifo.extend_from_slice(&value.to_be_bytes());
                    self.parse_audio_stream();
                }
            }
            A_INT1 => self.audio_int_mask[0] = value,
            A_INT2 => self.audio_int_mask[1] = value,
            _ => self.audio_regs[reg] = value,
        }
    }

    fn parse_audio_stream(&mut self) {
        if self.audio_regs[A_CONTROL1] & 1 == 0 {
            self.audio_fifo.clear();
            return;
        }
        if self.audio_regs[A_CONTROL2] & 0x08 != 0 {
            self.audio_es.append(&mut self.audio_fifo);
            self.parse_audio_elementary();
            return;
        }
        loop {
            let Some(prefix) = self.audio_fifo.windows(3).position(|w| w == [0, 0, 1]) else {
                if self.audio_fifo.len() > 2 {
                    let keep = self.audio_fifo.split_off(self.audio_fifo.len() - 2);
                    self.audio_fifo = keep;
                }
                break;
            };
            if prefix != 0 {
                self.audio_fifo.drain(..prefix);
            }
            if self.audio_fifo.len() < 4 {
                break;
            }
            let code = self.audio_fifo[3];
            if code == 0xBA {
                let Some(total) = pack_header_len(&self.audio_fifo) else {
                    break;
                };
                if self.audio_fifo.len() < total {
                    break;
                }
                self.audio_fifo.drain(..total);
                continue;
            }
            if self.audio_fifo.len() < 6 {
                break;
            }
            let packet_len =
                usize::from(u16::from_be_bytes([self.audio_fifo[4], self.audio_fifo[5]]));
            let total = 6 + packet_len;
            if self.audio_fifo.len() < total {
                break;
            }
            let packet: Vec<u8> = self.audio_fifo.drain(..total).collect();
            if (0xC0..=0xDF).contains(&code)
                && (self.audio_regs[A_CONTROL3] & 0x80 != 0
                    || usize::from(code - 0xC0) == usize::from(self.audio_regs[A_CONTROL3] & 31))
            {
                self.audio_head_detect = self.audio_head_detect.saturating_add(1);
                if self.audio_head_detect == 3 {
                    self.audio_set_status(1, SYSTEM_SYNC);
                }
                let (payload, pts) = pes_audio_payload(&packet);
                if let Some(pts) = pts {
                    self.audio_regs[A_PRESENT1] = pts as u16;
                    self.audio_regs[A_PRESENT2] = (pts >> 8) as u16;
                    self.audio_regs[A_PRESENT3] = (pts >> 16) as u16;
                    self.audio_regs[A_PRESENT4] = (pts >> 24) as u16;
                    self.audio_regs[A_PRESENT5] = (pts >> 32) as u16;
                    if self.audio_head_detect >= 3 {
                        self.audio_set_status(1, AUDIO_PTS_AVAILABLE);
                    }
                }
                self.audio_es.extend_from_slice(payload);
                self.parse_audio_elementary();
            }
        }
    }

    fn parse_audio_elementary(&mut self) {
        loop {
            if self.audio_es.len() < 4 {
                break;
            }
            let Some((at, header)) = (0..=self.audio_es.len() - 4).find_map(|at| {
                parse_mp2_header([
                    self.audio_es[at],
                    self.audio_es[at + 1],
                    self.audio_es[at + 2],
                    self.audio_es[at + 3],
                ])
                .map(|header| (at, header))
            }) else {
                self.audio_es.drain(..self.audio_es.len() - 3);
                break;
            };
            if at != 0 {
                self.audio_es.drain(..at);
            }
            if self.audio_es.len() < header.len {
                break;
            }
            let frame: Vec<u8> = self.audio_es.drain(..header.len).collect();
            self.audio_frame_detect = self.audio_frame_detect.saturating_add(1);
            self.audio_regs[A_PARAM1] =
                ((u16::from(header.raw[1]) << 4) | u16::from(header.raw[2] >> 4)) & 0xFF;
            self.audio_regs[A_PARAM2] =
                ((u16::from(header.raw[2]) << 4) | u16::from(header.raw[3] >> 4)) & 0xFF;
            self.audio_regs[A_PARAM3] = u16::from(header.raw[3]) << 4 & 0xFF;
            self.audio_set_status(
                1,
                AUDIO_FRAME_DETECT
                    | if self.audio_frame_detect == 3 {
                        AUDIO_SYNC
                    } else {
                        0
                    },
            );
            self.audio_rate = header.rate;
            if let Some(samples) = self.audio_decoder.decode(&frame) {
                if self.audio_pcm.len() < 64 {
                    self.audio_pcm
                        .push_back(AudioPcmFrame { samples, index: 0 });
                    self.audio_regs[A_CB_WRITE] = (self.audio_regs[A_CB_WRITE] + 1) & 63;
                    self.audio_regs[A_CB_STATUS] = self.audio_regs[A_CB_STATUS].saturating_add(1);
                    self.audio_set_status(1, AUDIO_NEW_FRAME);
                }
            }
            let _ = header.mono;
        }
    }

    fn cl450_read(&self, off: u32) -> u16 {
        let reg = (off as usize) & 0xFE;
        match reg {
            HOST_INTVECR | HOST_CONTROL | HOST_RADDR | HOST_NEWCMD | CPU_IADDR | CPU_TADDR
            | CPU_PC | CPU_CONTROL | VID_CONTROL => self.cl_regs[reg],
            HOST_RDATA => self.cl_hmem[usize::from(self.cl_regs[HOST_RADDR] & 15)],
            VID_REGDATA => self.cl_vid[usize::from((self.cl_regs[VID_CONTROL] >> 1) & 15)],
            CMEM_DMACTRL | CMEM_STATUS | CPU_INT | CPU_INTENB => self.cl_regs[reg],
            _ => 0,
        }
    }

    fn cl450_write(&mut self, off: u32, value: u16) {
        let reg = (off as usize) & 0xFE;
        match reg {
            CMEM_DATA => self.cl450_data_write(value),
            CMEM_CONTROL => {
                self.cl_regs[CMEM_CONTROL] = value;
                if value & 0x40 != 0 {
                    self.reset_cl450();
                }
            }
            CMEM_DMACTRL | DRAM_REFCNT | CPU_PC | HOST_SCR0 | HOST_SCR1 | HOST_SCR2 => {
                self.cl_regs[reg] = value
            }
            HOST_INTVECW => self.cl_regs[HOST_INTVECR] = value,
            HOST_CONTROL => self.cl_regs[HOST_CONTROL] = value,
            HOST_RADDR => self.cl_regs[HOST_RADDR] = value & 15,
            HOST_RDATA => {
                let at = usize::from(self.cl_regs[HOST_RADDR] & 15);
                self.cl_hmem[at] = value;
                self.cl_regs[HOST_RADDR] = (self.cl_regs[HOST_RADDR] + 1) & 15;
            }
            HOST_NEWCMD => {
                self.cl_regs[HOST_NEWCMD] = value;
                self.cl450_command();
            }
            CPU_CONTROL => {
                if self.cl_regs[CPU_CONTROL] & 1 == 0 && value & 1 != 0 {
                    self.init_cl450_cpu();
                }
                self.cl_regs[CPU_CONTROL] = value & 1;
            }
            CPU_IADDR => self.cl_regs[CPU_IADDR] = value & (CL450_IMEM_WORDS as u16 - 1),
            CPU_IMEM => {
                let at = usize::from(self.cl_regs[CPU_IADDR]) & (CL450_IMEM_WORDS - 1);
                self.cl_imem[at] = value;
                self.cl_regs[CPU_IADDR] =
                    (self.cl_regs[CPU_IADDR] + 1) & (CL450_IMEM_WORDS as u16 - 1);
            }
            CPU_TADDR => self.cl_regs[CPU_TADDR] = value & (CL450_TMEM_WORDS as u16 - 1),
            CPU_TMEM => {
                let at = usize::from(self.cl_regs[CPU_TADDR]) & (CL450_TMEM_WORDS - 1);
                self.cl_tmem[at] = value;
                self.cl_regs[CPU_TADDR] =
                    (self.cl_regs[CPU_TADDR] + 1) & (CL450_TMEM_WORDS as u16 - 1);
            }
            VID_CONTROL => self.cl_regs[VID_CONTROL] = value & ((CL450_VID_REGS as u16 - 1) << 1),
            VID_REGDATA => {
                let at = usize::from((self.cl_regs[VID_CONTROL] >> 1) & 15);
                self.cl_vid[at] = value;
            }
            _ => {}
        }
    }

    fn cl450_data_write(&mut self, value: u16) {
        if self.video_input.len() <= CL450_MPEG_BUFFER_SIZE - 2 {
            self.video_input.extend_from_slice(&value.to_be_bytes());
        }
    }

    fn cl450_command(&mut self) {
        if matches!(self.cl_hmem[0], CL_NEW_PACKET | CL_INQUIRE_BUFFER_FULLNESS) {
            log::trace!(
                "CD32 FMV: CL450 NewPacket size={} flags={:#06X}",
                self.cl_hmem[1],
                self.cl_hmem[2]
            );
        } else {
            log::debug!(
                "CD32 FMV: CL450 command {:#06X} args={:04X},{:04X},{:04X},{:04X}",
                self.cl_hmem[0],
                self.cl_hmem[1],
                self.cl_hmem[2],
                self.cl_hmem[3],
                self.cl_hmem[4]
            );
        }
        match self.cl_hmem[0] {
            CL_PLAY => self.cl_play = 1,
            CL_PAUSE => {
                if self.cl_play > 0 {
                    self.scr = 0;
                    self.scr_to_regs();
                }
                self.cl_play = -self.cl_play;
            }
            CL_NEW_PACKET => self.cl450_new_packet(),
            CL_INQUIRE_BUFFER_FULLNESS => self.cl_hmem[0x0B] = self.video_input.len() as u16,
            CL_SET_BLANK => {
                let blank = self.cl_hmem[1] & 1 != 0;
                if self.cl_blank != blank {
                    self.cl_blank = blank;
                    self.presentation_generation = self.presentation_generation.wrapping_add(1);
                }
            }
            CL_SET_BORDER => {
                let border = (u32::from(self.cl_hmem[3] & 0xFF) << 16) | u32::from(self.cl_hmem[4]);
                if self.cl_border != border {
                    self.cl_border = border;
                    self.presentation_generation = self.presentation_generation.wrapping_add(1);
                }
            }
            CL_SET_INTERRUPT_MASK => self.cl_interrupt_mask = self.cl_hmem[1],
            CL_SET_THRESHOLD => self.cl_threshold = self.cl_hmem[1],
            CL_ACCESS_SCR => {
                if self.cl_hmem[1] & 0x8000 != 0 {
                    self.scr_to_regs();
                    self.cl_hmem[1] = 0x8000 | (self.cl_regs[HOST_SCR0] & 7);
                    self.cl_hmem[2] = self.cl_regs[HOST_SCR1] & 0x7FFF;
                    self.cl_hmem[3] = self.cl_regs[HOST_SCR2] & 0x7FFF;
                } else {
                    self.cl_regs[HOST_SCR0] = self.cl_hmem[1] & 7;
                    self.cl_regs[HOST_SCR1] = self.cl_hmem[2] & 0x7FFF;
                    self.cl_regs[HOST_SCR2] = self.cl_hmem[3] & 0x7FFF;
                    self.regs_to_scr();
                }
            }
            CL_RESET => {
                self.cl_blank = true;
                self.cl_play = 0;
                self.cl_newpackets.clear();
                self.cl_newpacket_mode = false;
                self.cl_interrupt_mask = 0;
                self.video_input.clear();
                self.cl_buffer_empty_count = 0;
                self.video_decoder.reset();
                self.video_frames.clear();
                self.presentation_generation = self.presentation_generation.wrapping_add(1);
            }
            CL_FLUSH_BITSTREAM => {
                self.video_input.clear();
                self.cl_newpackets.clear();
            }
            CL_SET_COLOR_MODE | CL_SET_VIDEO_FORMAT | CL_SET_WINDOW | CL_DISPLAY_STILL
            | CL_SCAN | CL_SINGLE_STEP | CL_SLOW_MOTION => {}
            _ => log::debug!(
                "CD32 FMV: unsupported CL450 command {:#06X}",
                self.cl_hmem[0]
            ),
        }
        self.cl_regs[HOST_NEWCMD] = 0;
    }

    fn cl450_new_packet(&mut self) {
        self.cl_newpacket_mode = true;
        if self.cl_newpackets.len() == CL450_NEWPACKET_BUFFER_SIZE {
            self.cl_newpackets.pop_front();
        }
        let pts_valid = self.cl_hmem[2] & 0x8000 != 0;
        let pts = (u64::from(self.cl_regs[HOST_SCR0] & 7) << 30)
            | (u64::from(self.cl_regs[HOST_SCR1] & 0x7FFF) << 15)
            | u64::from(self.cl_regs[HOST_SCR2] & 0x7FFF);
        self.cl_newpackets.push_back(NewPacket {
            remaining: self.cl_hmem[1],
            pts,
            pts_valid,
        });
    }

    fn consume_newpacket_bytes(&mut self, mut count: usize) {
        while count != 0 {
            let Some(front) = self.cl_newpackets.front_mut() else {
                break;
            };
            let used = count.min(usize::from(front.remaining));
            front.remaining -= used as u16;
            count -= used;
            if front.remaining == 0 {
                self.cl_newpackets.pop_front();
            }
        }
    }

    fn scr_to_regs(&mut self) {
        self.cl_regs[HOST_SCR0] = (self.cl_regs[HOST_SCR0] & !7) | ((self.scr >> 30) as u16 & 7);
        self.cl_regs[HOST_SCR1] = (self.scr >> 15) as u16 & 0x7FFF;
        self.cl_regs[HOST_SCR2] = self.scr as u16 & 0x7FFF;
    }

    fn regs_to_scr(&mut self) {
        self.scr = (u64::from(self.cl_regs[HOST_SCR0] & 7) << 30)
            | (u64::from(self.cl_regs[HOST_SCR1] & 0x7FFF) << 15)
            | u64::from(self.cl_regs[HOST_SCR2] & 0x7FFF);
    }

    fn service_video_decoder(&mut self) {
        if self.cl_play <= 0 || self.video_frames.len() >= CL450_VIDEO_BUFFERS - 1 {
            return;
        }
        let mut offered = false;
        for _ in 0..256 {
            match self.video_decoder.parse() {
                VideoEvent::NeedInput => {
                    if offered || self.video_input.len() < 512 {
                        break;
                    }
                    let input = std::mem::take(&mut self.video_input);
                    if self.video_decoder.stream_bytes == 0 {
                        log::debug!(
                            "CD32 FMV: first CL450 bitstream buffer: {} bytes, prefix {:02X?}",
                            input.len(),
                            &input[..input.len().min(16)]
                        );
                    }
                    self.consume_newpacket_bytes(input.len());
                    self.video_decoder.offer(input);
                    offered = true;
                }
                VideoEvent::Sequence {
                    width,
                    height,
                    frame_period,
                } => {
                    if self.frame_width != width
                        || self.frame_height != height
                        || self.frame_period != frame_period
                    {
                        log::debug!(
                            "CD32 FMV: MPEG sequence {}x{}, frame period {}",
                            width,
                            height,
                            frame_period
                        );
                    }
                    self.frame_width = width;
                    self.frame_height = height;
                    self.frame_period = frame_period.max(1);
                    let rate = 27_000_000 / self.frame_period;
                    self.write_dram(CL_DRAM_PICTURE_RATE, rate as u16);
                    self.write_dram(CL_DRAM_H_SIZE, width as u16);
                    self.write_dram(CL_DRAM_V_SIZE, height as u16);
                    self.cl450_set_status(CL_INT_SEQ_V);
                }
                VideoEvent::Gop {
                    hours,
                    minutes,
                    seconds,
                    pictures,
                } => {
                    self.write_dram(
                        CL_DRAM_TIME_CODE_0,
                        (u16::from(hours) << 6) | u16::from(minutes),
                    );
                    self.write_dram(
                        CL_DRAM_TIME_CODE_1,
                        (u16::from(seconds) << 6) | u16::from(pictures),
                    );
                }
                VideoEvent::Frame(frame) => {
                    log::trace!(
                        "CD32 FMV: decoded MPEG frame {}x{}",
                        frame.width,
                        frame.height
                    );
                    self.video_frames.push_back(frame);
                    break;
                }
            }
        }
    }

    fn advance_video(&mut self, cck: u32) {
        if self.cl_play > 0 {
            self.scr_acc += u64::from(cck) * 90_000;
            self.scr += self.scr_acc / u64::from(PAULA_CLOCK_HZ);
            self.scr_acc %= u64::from(PAULA_CLOCK_HZ);

            self.video_present_acc += u64::from(cck) * 27_000_000;
            let period = u64::from(PAULA_CLOCK_HZ) * u64::from(self.frame_period.max(1));
            while self.video_present_acc >= period {
                self.video_present_acc -= period;
                self.cl450_set_status(CL_INT_PIC_D);
                if let Some(frame) = self.video_frames.pop_front() {
                    if self.current_frame.is_none() {
                        log::debug!(
                            "CD32 FMV: presenting first MPEG frame {}x{}",
                            frame.width,
                            frame.height
                        );
                    }
                    self.current_frame = Some(frame);
                    self.presentation_generation = self.presentation_generation.wrapping_add(1);
                }
            }
        }

        self.cl_buffer_empty_acc += u64::from(cck);
        let check_period = u64::from(PAULA_CLOCK_HZ) / 250;
        while self.cl_buffer_empty_acc >= check_period {
            self.cl_buffer_empty_acc -= check_period;
            if self.video_input.is_empty() {
                if self.cl_buffer_empty_count >= 2 {
                    self.cl450_set_status(CL_INT_UND);
                } else {
                    self.cl_buffer_empty_count += 1;
                }
            } else {
                self.cl_buffer_empty_count = 0;
            }
        }

        if self.cl_play > 0
            && self.cl_newpacket_mode
            && self.video_input.len() < usize::from(self.cl_threshold)
        {
            let pending: usize = self
                .cl_newpackets
                .iter()
                .map(|p| usize::from(p.remaining))
                .sum();
            if self.video_input.len() >= pending.saturating_sub(6) {
                self.cl450_set_status(CL_INT_RDY);
            }
        }
        self.service_video_decoder();
    }

    fn advance_audio(&mut self, cck: u32, ring: Option<&mut CdAudioRing>) {
        let Some(ring) = ring else { return };
        if self.audio_regs[A_CONTROL1] & 1 == 0 {
            return;
        }
        let native_rate = self.audio_rate.max(1);
        if self.audio_resampler_rate != native_rate {
            self.audio_resampler_rate = native_rate;
            self.audio_resampler = Resampler::new(native_rate, MIX_SAMPLE_RATE);
        }
        // The L64111 accepts 32/44.1/48 kHz Layer II streams, while the
        // analogue CD input is sampled on Copperline's fixed mixer grid.
        self.audio_sample_acc += u64::from(cck) * u64::from(MIX_SAMPLE_RATE);
        let muted =
            self.audio_regs[A_CONTROL2] & (1 << 5) != 0 || self.io_reg & IO_L64111_MUTE != 0;
        while self.audio_sample_acc >= u64::from(PAULA_CLOCK_HZ) {
            self.audio_sample_acc -= u64::from(PAULA_CLOCK_HZ);
            let sample = {
                let Self {
                    audio_resampler,
                    audio_pcm,
                    audio_regs,
                    ..
                } = self;
                audio_resampler.next(|| Self::pop_native_audio_sample(audio_pcm, audio_regs))
            };
            let sample = if muted { (0.0, 0.0) } else { sample };
            let _ = ring.push_frame(sample.0, sample.1);
        }
    }

    fn pop_native_audio_sample(
        audio_pcm: &mut VecDeque<AudioPcmFrame>,
        audio_regs: &mut [u16; 32],
    ) -> (f32, f32) {
        let Some(frame) = audio_pcm.front_mut() else {
            return (0.0, 0.0);
        };
        let sample = frame
            .samples
            .get(frame.index)
            .copied()
            .unwrap_or((0.0, 0.0));
        frame.index += 1;
        if frame.index >= frame.samples.len() {
            audio_pcm.pop_front();
            audio_regs[A_CB_READ] = (audio_regs[A_CB_READ] + 1) & 63;
            audio_regs[A_CB_STATUS] = audio_regs[A_CB_STATUS].saturating_sub(1);
        }
        sample
    }
}

impl ZorroDevice for Cd32Fmv {
    fn read(&mut self, off: u32, size: usize, _host: &mut DeviceHost) -> u32 {
        let bank = off & BANK_MASK;
        if off < IO_BASE {
            return Self::read_be(&self.rom, (off - ROM_BASE) as usize, size);
        }
        if off >= RAM_BASE {
            return Self::read_be(&self.ram, (off - RAM_BASE) as usize, size);
        }
        if size == 4 {
            return (self.read(off, 2, _host) << 16) | self.read(off + 2, 2, _host);
        }
        let word = match bank {
            IO_BASE => self.io_read(off),
            L64111_BASE => self.l64111_read(off),
            CL450_BASE => self.cl450_read(off),
            _ => 0,
        };
        if size == 1 {
            u32::from(word as u8)
        } else {
            u32::from(word)
        }
    }

    fn write(&mut self, off: u32, size: usize, value: u32, _host: &mut DeviceHost) {
        if off >= RAM_BASE {
            Self::write_be(&mut self.ram, (off - RAM_BASE) as usize, size, value);
            return;
        }
        if off < IO_BASE {
            return;
        }
        if size == 4 {
            self.write(off, 2, value >> 16, _host);
            self.write(off + 2, 2, value, _host);
            return;
        }
        let bank = off & BANK_MASK;
        if size == 1 {
            match bank {
                L64111_BASE => self.l64111_write(off, value as u16),
                CL450_BASE => self.cl450_write(off, value as u16),
                _ => {}
            }
            return;
        }
        match bank {
            IO_BASE => {
                log::debug!("CD32 FMV: I/O control <- {:#06X}", value as u16);
                self.io_write(off, value as u16);
            }
            L64111_BASE => self.l64111_write(off, value as u16),
            CL450_DATA => self.cl450_data_write(value as u16),
            CL450_BASE => self.cl450_write(off, value as u16),
            _ => {}
        }
    }

    fn peek_word(&self, off: u32) -> Option<u16> {
        if off < IO_BASE {
            return Some(Self::read_be(&self.rom, off as usize, 2) as u16);
        }
        if off >= RAM_BASE {
            return Some(Self::read_be(&self.ram, (off - RAM_BASE) as usize, 2) as u16);
        }
        None
    }

    fn tick(&mut self, cck: u32, host: &mut DeviceHost) {
        self.advance_video(cck);
        self.advance_audio(cck, host.cd_audio_opt());
    }

    fn int2_line(&self) -> bool {
        self.cl450_irq() || self.l64111_irq()
    }

    fn reset(&mut self) {
        self.reset_all();
    }

    fn kind(&self) -> &'static str {
        "cd32-fmv"
    }
}

fn pack_header_len(data: &[u8]) -> Option<usize> {
    let version = *data.get(4)?;
    if version & 0xF0 == 0x20 {
        Some(12)
    } else if version & 0xC0 == 0x40 {
        Some(14 + usize::from(*data.get(13)? & 7))
    } else {
        Some(4)
    }
}

fn pes_audio_payload(packet: &[u8]) -> (&[u8], Option<u64>) {
    if packet.len() <= 6 {
        return (&[], None);
    }
    let mut at = 6;
    let mut pts = None;
    if packet.get(at).is_some_and(|b| b & 0xC0 == 0x80) {
        let flags = packet.get(at + 1).copied().unwrap_or(0);
        let header_len = usize::from(packet.get(at + 2).copied().unwrap_or(0));
        if flags & 0x80 != 0 {
            pts = parse_pts(packet.get(at + 3..at + 8).unwrap_or(&[]));
        }
        at = (at + 3 + header_len).min(packet.len());
    } else {
        while packet.get(at) == Some(&0xFF) {
            at += 1;
        }
        if packet.get(at).is_some_and(|b| b & 0xC0 == 0x40) {
            at += 2;
        }
        match packet.get(at).map(|b| b & 0xF0) {
            Some(0x20) => {
                pts = parse_pts(packet.get(at..at + 5).unwrap_or(&[]));
                at += 5;
            }
            Some(0x30) => {
                pts = parse_pts(packet.get(at..at + 5).unwrap_or(&[]));
                at += 10;
            }
            _ if packet.get(at) == Some(&0x0F) => at += 1,
            _ => {}
        }
        at = at.min(packet.len());
    }
    (&packet[at..], pts)
}

fn parse_pts(data: &[u8]) -> Option<u64> {
    if data.len() < 5 {
        return None;
    }
    Some(
        (u64::from((data[0] >> 1) & 7) << 30)
            | (u64::from(data[1]) << 22)
            | (u64::from(data[2] >> 1) << 15)
            | (u64::from(data[3]) << 7)
            | u64::from(data[4] >> 1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom() -> Vec<u8> {
        (0..ROM_BYTES).map(|i| i as u8).collect()
    }

    #[test]
    fn board_map_exposes_rom_ram_and_active_low_irq_status() {
        let mut board = Cd32Fmv::new(rom()).unwrap();
        let mut mem = crate::memory::Memory::placeholder(0x20_0000, 0, Default::default());
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(0, 4, &mut host), 0x0001_0203);
        board.write(RAM_BASE + 2, 4, 0x1234_5678, &mut host);
        assert_eq!(board.read(RAM_BASE + 2, 4, &mut host), 0x1234_5678);
        assert_eq!(board.read(IO_BASE, 2, &mut host), 0xC800);
        board.cl_regs[CPU_CONTROL] = 1;
        board.cl_regs[HOST_CONTROL] = 0;
        assert_eq!(
            board.read(IO_BASE, 2, &mut host) & u32::from(IO_CL450_IRQ),
            0
        );
        assert!(board.int2_line());
    }

    #[test]
    fn cl450_host_memory_and_command_protocol() {
        let mut board = Cd32Fmv::new(rom()).unwrap();
        board.cl450_write(CPU_CONTROL as u32, 1);
        assert_eq!(board.cl_regs[HOST_CONTROL], 0x0081);
        board.cl450_write(HOST_RADDR as u32, 0);
        board.cl450_write(HOST_RDATA as u32, CL_SET_BORDER);
        board.cl450_write(HOST_RDATA as u32, 0);
        board.cl450_write(HOST_RDATA as u32, 0);
        board.cl450_write(HOST_RDATA as u32, 0x12);
        board.cl450_write(HOST_RDATA as u32, 0x3456);
        board.cl450_write(HOST_NEWCMD as u32, 1);
        assert_eq!(board.cl_border, 0x123456);
        assert_eq!(board.cl_regs[HOST_NEWCMD], 0);
    }

    #[test]
    fn mpeg_layer_two_header_has_vcd_frame_size() {
        // MPEG-1 Layer II, 224 kbps, 44.1 kHz, stereo.
        let header = parse_mp2_header([0xFF, 0xFD, 0xB0, 0x00]).unwrap();
        assert_eq!(header.rate, 44_100);
        assert_eq!(header.len, 731);
        assert!(!header.mono);
    }

    #[test]
    fn non_vcd_audio_rates_are_resampled_to_mixer_cadence() {
        for (rate, expected_native_frames) in [(32_000, 32_064), (48_000, 48_064)] {
            let mut board = Cd32Fmv::new(rom()).unwrap();
            board.audio_regs[A_CONTROL1] = 1;
            board.audio_rate = rate;
            board.audio_pcm.push_back(AudioPcmFrame {
                samples: vec![(0.25, -0.25); 50_000],
                index: 0,
            });
            let mut ring = CdAudioRing::default();
            board.advance_audio(PAULA_CLOCK_HZ, Some(&mut ring));
            assert_eq!(
                board.audio_pcm.front().unwrap().index,
                expected_native_frames,
                "native rate {rate}"
            );
        }
    }

    #[test]
    fn gop_time_code_survives_fragmented_input() {
        let mut decoder = VideoDecoder::new();
        let header = [0x00, 0x00, 0x01, 0xB8, 0x48, 0xD5, 0x2A, 0x80];
        decoder.offer(header[..3].to_vec());
        decoder.offer(header[3..7].to_vec());
        decoder.offer(header[7..].to_vec());
        assert_eq!(decoder.pending_gops.len(), 1);
        let gop = decoder.pending_gops.front().unwrap();
        assert_eq!(
            (gop.hours, gop.minutes, gop.seconds, gop.pictures),
            (18, 13, 41, 21)
        );
    }

    #[test]
    fn video_decoder_snapshot_restores_exact_rust_state() {
        // A valid PAL VCD sequence header followed by a GOP/end code exercises
        // both the incremental bit reader and Copperline's metadata scanner.
        let bytes = vec![
            0x00, 0x00, 0x01, 0xB3, 0x16, 0x01, 0x20, 0x83, 0x02, 0xE1, 0xA0, 0xA4, 0x00, 0x00,
            0x01, 0xB8, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x01, 0xB7,
        ];
        let mut decoder = VideoDecoder::new();
        decoder.offer(bytes);

        let encoded = bincode::serialize(&decoder).unwrap();
        let restored: VideoDecoder = bincode::deserialize(&encoded).unwrap();
        assert_eq!(
            restored.decoder.stream_position(),
            decoder.decoder.stream_position()
        );
        assert_eq!(
            restored.decoder.buffered_bytes(),
            decoder.decoder.buffered_bytes()
        );
        assert_eq!(restored.gop_scan_tail, decoder.gop_scan_tail);
        assert_eq!(restored.stream_bytes, decoder.stream_bytes);
        assert_eq!(restored.pending_gops.len(), decoder.pending_gops.len());
    }
}
