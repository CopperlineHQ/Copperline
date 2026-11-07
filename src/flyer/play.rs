// SPDX-License-Identifier: GPL-3.0-or-later

//! Playback: the Flyer's sequencer, its one-clip player and its DSP mixer.
//!
//! The firmware (`seq.c`, `acts.c`, `Audio.c`) turns a request into
//! *events* on *tracks*: a video event shows fields of a clip on channel A
//! or B from a start time on the field clock; an audio event feeds a clip's
//! sound into a pair of the DSP's eight mono channels; DSP events set a
//! channel's volume and pan, ramping over some fields. A sequence is
//! downloaded event by event with start times relative to its start
//! (NEWSEQ, ADDSEQCLIP, AUDEKEY, ENDSEQ) and started at a base time
//! (PLAYSEQ); a single clip (FLYPLAY) makes the same events for itself and
//! its command completes when they are done. The Amiga watches a sequence
//! through `SharedCtrl` (`Done` when every track has finished).
//!
//! Here events are kept in plain lists and evaluated once a field, when the
//! field clock steps: each video channel shows the field its event says
//! (or black after a clip that asked for its matte, or holds the last
//! field), the audio events' samples go through the DSP model, and the
//! mixed field of sound goes to the Toaster through the [`FlyerLink`].
//! What the firmware does to keep its SCSI drives fed (A/B heads, drive
//! arbitration, prerolls) is not needed: clips are read straight from the
//! host's files. Each field the player also tells the link which fields
//! the channels will show next, so they are decoded before they are due.
//!
//! Jog and shuttle (`acts.c` `SearchArgs`, `DoSearch`): SRCHARGS opens a
//! session on a clip (holding the video channel, as the firmware's channel
//! semaphore does, so a FLYPLAY for it waits), and each DOSEARCH puts one
//! colour frame on the channel from the next colour-frame boundary, where
//! it loops until the next, with its sound: the frame's four fields
//! through the session's DSP pair, again and again until the next frame
//! (or once, with `APF_AUTOMUTE`). A DOSEARCH waits for the previous
//! frame to take before it queues its own, which is what paces the
//! switcher's shuttle loop to a frame per colour frame.
//!
//! The DSP: each channel has a volume (`$FFFF` = full) and a pan (the API's
//! signed word, `-$8000` left to `$7FFF` right, which the firmware turns
//! into 0..`$FFFF` by flipping the top bit), and a ramp: a change reaches
//! its target linearly over `rate` fields. A channel adds `s·v·(1-p)` to
//! the left and `s·v·p` to the right output. The sample rate is 735 per
//! field, 44,055.94 Hz, locked to the video; the Toaster stretches each
//! frame's sound to the mixer's 44.1 kHz.

use super::clip::{ClipFile, SAMPLES_PER_FIELD};
use super::link::{FieldSource, FlyerLink, BLACK};
use super::mailbox::{self, err, rd16, rd32, rd8, wr32, wr8, Slot, SHARED_CTRL};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `struct ClipAction` offsets used by playback.
pub mod ca {
    pub const VOLUME: usize = 0;
    pub const CHANNEL: usize = 9;
    pub const FLAGS: usize = 10;
    pub const PERMISS: usize = 11;
    pub const VID_START: usize = 12;
    pub const AUD_START: usize = 16;
    pub const VID_COUNT: usize = 20;
    pub const AUD_COUNT: usize = 24;
    pub const GO_CLOCK: usize = 28;
    pub const VOL_ATTACK: usize = 36;
    pub const VOL_SUST1: usize = 38;
    pub const VOL_SUST2: usize = 40;
    pub const VOL_DECAY: usize = 42;
    pub const PAN1: usize = 44;
    pub const PAN2: usize = 46;
    pub const USER_ID: usize = 80;
    pub const LAST_FIELD_DONE: usize = 126;
}

/// ClipAction flags (`CAF_`).
pub mod caf {
    pub const VIDEO: u8 = 0x01;
    pub const AUDIOL: u8 = 0x02;
    pub const AUDIOR: u8 = 0x04;
    pub const USEMATTE: u8 = 0x08;
    pub const AUDENV: u8 = 0x80;
}

/// Permission flags (`CAPF_`).
pub const CAPF_ERRIFBUSY: u8 = 0x04;
const CAPF_AUTOMUTE: u8 = 0x08;

/// More `ERR_` codes (`Errors.h`).
pub const ERR_CHANINUSE: u8 = 0x0A;
const ERR_WRONGDATATYPE: u8 = 0x60;

/// DSP channels, and video channels.
pub const DSP_CHANNELS: usize = 8;
pub const VIDEO_CHANNELS: usize = 2;

/// Fields from GETCLOCK to the start of a clip played "now" (GoClock 0),
/// and of a sequence started with no base time (`seq.c`).
const PLAY_LEAD: u32 = 20;
const SEQUENCE_LEAD: u32 = 60;

/// Fields decoded ahead of the field clock on each channel.
const READ_AHEAD: u32 = 8;

/// Who an event belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Owner {
    /// The downloaded sequence.
    Sequence,
    /// The FLYPLAY in this command slot.
    Play(usize),
    /// The jog/shuttle session.
    Search,
}

/// How a command that may have to wait went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Finished with this error code.
    Done(u8),
    /// Not yet: try again next field (the firmware's task is waiting).
    Wait,
}

/// A colour frame a DOSEARCH put up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SearchFrame {
    /// The frame's first field.
    first: u32,
    /// The field it takes at (a multiple of four).
    start: u32,
    /// Its sound plays (through the session's pair).
    audio: bool,
    /// The sound plays once, not looped (`APF_AUTOMUTE`).
    automute: bool,
}

/// A jog/shuttle session (`JogMach`).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Search {
    /// SRCHARGS opens less closes (`ji_nestcount`).
    nest: u32,
    clip: PathBuf,
    fields: u32,
    has_video: bool,
    audio_chans: u8,
    /// The video channel, and whether the session holds it (opened with
    /// video).
    channel: u8,
    holds_channel: bool,
    /// The frames go to the video channel (else to an audio engine only).
    on_video: bool,
    /// The DSP pair the session was given.
    audio: Option<AudioUse>,
    /// Frames put up, oldest first: the one showing and any still to take.
    frames: Vec<SearchFrame>,
    /// The last DOSEARCH's level changes have all happened by this field
    /// (`WaitJogDSPsettled`).
    settled: u32,
    /// The DOSEARCH whose frame is queued but whose level changes wait for
    /// the previous ones: its slot, and the frame.
    half_done: Option<(usize, SearchFrame)>,
}

impl Search {
    /// The frame showing at `clock`.
    fn frame_at(&self, clock: u32) -> Option<SearchFrame> {
        self.frames.iter().rev().find(|f| f.start <= clock).copied()
    }
}

/// Audio an event feeds to the DSP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioUse {
    /// First DSP channel (0-7).
    pub dsp: u8,
    /// Mono channels used (1 or 2).
    pub count: u8,
    /// One channel, the clip's right (`EF_RIGHTONLY`).
    pub right_only: bool,
}

/// One thing on the timeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// First field, on the field clock (relative to the sequence's start
    /// until it plays).
    pub start: u32,
    pub fields: u32,
    /// The clip's host file; `None` for black.
    pub clip: Option<PathBuf>,
    /// The clip field shown or heard at `start`.
    pub first: u32,
    /// Video channel, for a video event.
    pub video: Option<u8>,
    pub audio: Option<AudioUse>,
    /// Show black when done (`CAF_USEMATTE`, or a black event).
    pub matte_after: bool,
    pub user_id: u32,
    pub owner: Owner,
}

impl Event {
    fn end(&self) -> u32 {
        self.start.saturating_add(self.fields)
    }

    fn covers(&self, clock: u32) -> bool {
        (self.start..self.end()).contains(&clock)
    }
}

/// A volume/pan change for one DSP channel (`struct DSPevent`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DspEvent {
    pub time: u32,
    /// DSP channel, 1-8 (0: none).
    pub chan: u8,
    pub volume: u16,
    /// The API's signed pan word.
    pub pan: u16,
    /// Ramp length in fields.
    pub rate: u16,
    pub owner: Owner,
}

/// One DSP channel's level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Dsp {
    /// Volume and pan now, 0-1 (pan 0 = left).
    pub volume: f32,
    pub pan: f32,
    target: (f32, f32),
    /// Fields of ramp left.
    ramp: u32,
}

impl Dsp {
    fn set(&mut self, e: &DspEvent) {
        let target = (
            f32::from(e.volume) / 65535.0,
            f32::from(e.pan ^ 0x8000) / 65535.0,
        );
        self.target = target;
        self.ramp = u32::from(e.rate);
        if self.ramp == 0 {
            (self.volume, self.pan) = target;
        }
    }

    /// Step one field: the levels at its start and its end.
    fn step(&mut self) -> ((f32, f32), (f32, f32)) {
        let from = (self.volume, self.pan);
        if self.ramp > 0 {
            let n = self.ramp as f32;
            self.volume += (self.target.0 - self.volume) / n;
            self.pan += (self.target.1 - self.pan) / n;
            self.ramp -= 1;
        }
        (from, (self.volume, self.pan))
    }
}

/// A FLYPLAY in progress.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Play {
    slot: usize,
    /// The ClipAction's SRAM offset (results go back there).
    action: usize,
    /// The clip field reported as done at the start.
    first: u32,
    start: u32,
    paused: bool,
    audio: Option<AudioUse>,
}

/// What a channel shows, to skip telling the Toaster the same again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Shown {
    #[default]
    Nothing,
    Black,
    Clip(PathBuf, u32),
}

/// The sequencer, the one-clip player and the DSP.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Player {
    /// The downloaded sequence, start times relative to its start.
    sequence: Vec<Event>,
    sequence_dsp: Vec<DspEvent>,
    /// The audio channel and flags of the last ADDSEQCLIP, for AUDEKEY.
    last_seq_audio: (u8, u8),
    /// Events playing or waiting to, on the field clock.
    live: Vec<Event>,
    /// DSP events waiting for their time, in time order.
    dsp_queue: Vec<DspEvent>,
    pub dsp: [Dsp; DSP_CHANNELS],
    plays: Vec<Play>,
    /// The sequence was started and has events left (`TracksBusy`).
    sequence_playing: bool,
    /// Something played last field.
    was_busy: bool,
    /// The last field played.
    clock: u32,
    /// The first audio channel (1-8) of the clip playing, for CHGAUDIO.
    last_play_audio: u8,
    /// A channel shows black once its clip is done.
    matte_after: [bool; VIDEO_CHANNELS],
    shown: [Shown; VIDEO_CHANNELS],
    /// The jog/shuttle session, while one is open.
    #[serde(default)]
    search: Option<Search>,
    /// Open clips, by host path.
    #[serde(skip)]
    clips: HashMap<PathBuf, Arc<ClipFile>>,
}

/// What a FLYPLAY or ADDSEQCLIP asks for, read from its ClipAction.
#[derive(Clone, Debug)]
pub struct Request {
    pub action: usize,
    pub channel: u8,
    pub flags: u8,
    pub permiss: u8,
    pub vid_start: u32,
    pub aud_start: u32,
    pub vid_count: u32,
    pub aud_count: u32,
    pub go_clock: u32,
    pub attack: u16,
    pub sustain: [u16; 2],
    pub decay: u16,
    pub pan: [u16; 2],
    pub user_id: u32,
}

impl Request {
    pub fn read(sram: &[u8], action: usize) -> Self {
        Self {
            action,
            channel: rd8(sram, action + ca::CHANNEL),
            flags: rd8(sram, action + ca::FLAGS),
            permiss: rd8(sram, action + ca::PERMISS),
            vid_start: rd32(sram, action + ca::VID_START),
            aud_start: rd32(sram, action + ca::AUD_START),
            vid_count: rd32(sram, action + ca::VID_COUNT),
            aud_count: rd32(sram, action + ca::AUD_COUNT),
            go_clock: rd32(sram, action + ca::GO_CLOCK),
            attack: rd16(sram, action + ca::VOL_ATTACK),
            sustain: [
                rd16(sram, action + ca::VOL_SUST1),
                rd16(sram, action + ca::VOL_SUST2),
            ],
            decay: rd16(sram, action + ca::VOL_DECAY),
            pan: [rd16(sram, action + ca::PAN1), rd16(sram, action + ca::PAN2)],
            user_id: rd32(sram, action + ca::USER_ID),
        }
    }

    fn video(&self) -> bool {
        self.flags & caf::VIDEO != 0
    }

    fn audio_count(&self) -> u8 {
        u8::from(self.flags & caf::AUDIOL != 0) + u8::from(self.flags & caf::AUDIOR != 0)
    }

    fn right_only(&self) -> bool {
        self.flags & (caf::AUDIOL | caf::AUDIOR) == caf::AUDIOR
    }
}

/// A clip found on a drive: its host file and what it holds.
#[derive(Clone, Debug)]
pub struct ClipInfo {
    pub path: PathBuf,
    pub fields: u32,
    pub video: bool,
    pub audio_chans: u8,
}

impl Player {
    /// Open (or reuse) the clip at `path`.
    pub fn clip(&mut self, path: &Path) -> Option<Arc<ClipFile>> {
        if let Some(c) = self.clips.get(path) {
            return Some(Arc::clone(c));
        }
        match ClipFile::open(path) {
            Ok(c) => {
                let c = Arc::new(c);
                self.clips.insert(path.to_path_buf(), Arc::clone(&c));
                Some(c)
            }
            Err(e) => {
                log::warn!("flyer: {e}");
                None
            }
        }
    }

    /// Describe a clip for a play request.
    pub fn clip_info(&mut self, path: &Path) -> Option<ClipInfo> {
        let c = self.clip(path)?;
        Some(ClipInfo {
            path: path.to_path_buf(),
            fields: c.field_count(),
            video: c.has_video(),
            audio_chans: c.audio_channels(),
        })
    }

    /// Forget open clips (the drive's files may have changed).
    pub fn forget_clips(&mut self) {
        self.clips.clear();
    }

    /// Whether anything is playing or waiting to.
    pub fn busy(&self) -> bool {
        !self.live.is_empty()
    }

    pub fn sequence_len(&self) -> usize {
        self.sequence.len()
    }

    // ---------------------------------------------------------------
    // The sequencer
    // ---------------------------------------------------------------

    /// NEWSEQ: forget the downloaded sequence.
    pub fn new_sequence(&mut self) {
        self.sequence.clear();
        self.sequence_dsp.clear();
        self.last_seq_audio = (0, 0);
    }

    /// ADDSEQCLIP (`AddSequenceClip`): the events for one clip, start times
    /// relative to the sequence. `clip` is `None` for a request with no
    /// clip name (black on its channel for a colour frame).
    pub fn add_sequence_clip(&mut self, mut r: Request, clip: Option<ClipInfo>) -> u8 {
        if usize::from(r.channel) >= VIDEO_CHANNELS && r.video() {
            return err::BADPARAM;
        }
        let Some(clip) = clip else {
            if !r.video() {
                return err::BADPARAM;
            }
            self.sequence.push(Event {
                start: r.go_clock,
                fields: 4,
                clip: None,
                first: 0,
                video: Some(r.channel),
                audio: None,
                matte_after: true,
                user_id: r.user_id,
                owner: Owner::Sequence,
            });
            return err::OKAY;
        };
        let mut count = r.audio_count();
        if r.video() && !clip.video {
            return ERR_WRONGDATATYPE;
        }
        if count > 0 && clip.audio_chans == 0 {
            return ERR_WRONGDATATYPE;
        }
        if count == 2 && clip.audio_chans == 1 {
            r.flags &= !caf::AUDIOR;
            count = 1;
        }
        // Mono from a stereo clip: with video, the firmware plays both
        // channels and lets the volumes silence one; audio-only right
        // mono becomes stereo with the left silent.
        if count == 1 && clip.audio_chans == 2 {
            if r.video() {
                count = 2;
                r.flags |= caf::AUDIOL | caf::AUDIOR;
            } else if r.right_only() {
                count = 2;
                r.flags |= caf::AUDIOL | caf::AUDIOR;
                r.sustain[0] = 0;
            }
        }
        let right_only = count == 1 && r.right_only();
        if r.video() && r.vid_start.saturating_add(r.vid_count) > clip.fields {
            return err::BADPARAM;
        }
        if count > 0 && r.aud_start.saturating_add(r.aud_count) > clip.fields {
            return err::BADPARAM;
        }
        let vid_go = r.go_clock;
        let aud_go = if r.video() {
            // Audio leading or trailing the video moves with its in point.
            r.go_clock
                .wrapping_add(r.aud_start)
                .wrapping_sub(r.vid_start)
        } else {
            r.go_clock
        };
        let mut audchan = 0u8;
        if count > 0 {
            // The audio pair is held for the audio and, with natural
            // audio, the video too.
            let (mut lo, mut hi) = (aud_go, aud_go.saturating_add(r.aud_count));
            if r.video() {
                lo = lo.min(vid_go);
                hi = hi.max(vid_go.saturating_add(r.vid_count));
            }
            if let Some(pair) = self.free_sequence_pair(lo, hi) {
                audchan = pair + 1;
                self.sequence.push(Event {
                    start: aud_go,
                    fields: r.aud_count,
                    clip: Some(clip.path.clone()),
                    first: r.aud_start,
                    video: None,
                    audio: Some(AudioUse {
                        dsp: pair,
                        count,
                        right_only,
                    }),
                    matte_after: false,
                    user_id: r.user_id,
                    owner: Owner::Sequence,
                });
                let events = dsp_events(&r, aud_go, r.aud_count, audchan, Owner::Sequence);
                for e in events {
                    add_dsp_event(&mut self.sequence_dsp, e);
                }
            }
        }
        if r.video() {
            self.sequence.push(Event {
                start: vid_go,
                fields: r.vid_count,
                clip: Some(clip.path),
                first: r.vid_start,
                video: Some(r.channel),
                audio: None,
                matte_after: false,
                user_id: r.user_id,
                owner: Owner::Sequence,
            });
        }
        self.last_seq_audio = (audchan, r.flags);
        err::OKAY
    }

    /// The first DSP pair (0, 2, 4, 6) no sequence audio uses between
    /// `lo` and `hi` (`FindAudioTrack`).
    fn free_sequence_pair(&self, lo: u32, hi: u32) -> Option<u8> {
        (0..DSP_CHANNELS as u8).step_by(2).find(|&pair| {
            !self
                .sequence
                .iter()
                .any(|e| e.audio.is_some_and(|a| a.dsp == pair) && e.start < hi && lo < e.end())
        })
    }

    /// AUDEKEY (`AddAudEKey`): a key of the last clip's audio envelope.
    pub fn add_envelope_key(&mut self, time: u32, fields: u32, volume: [u16; 2], pan: [u16; 2]) {
        let (mut chan, flags) = self.last_seq_audio;
        let rate = fields.min(u32::from(u16::MAX)) as u16;
        for (side, flag) in [caf::AUDIOL, caf::AUDIOR].into_iter().enumerate() {
            if flags & flag != 0 {
                let e = DspEvent {
                    time,
                    chan,
                    volume: volume[side],
                    pan: pan[side],
                    rate,
                    owner: Owner::Sequence,
                };
                add_dsp_event(&mut self.sequence_dsp, e);
                chan = chan.wrapping_add(1);
            }
        }
    }

    /// PLAYSEQ (`PlaySequence`): start the sequence at `base` (or a second
    /// from now), both channels black until their first clip.
    pub fn play_sequence(
        &mut self,
        base: u32,
        clock: u32,
        sram: &mut [u8],
        link: Option<&FlyerLink>,
    ) -> u8 {
        let base = if base == 0 {
            clock.wrapping_add(SEQUENCE_LEAD)
        } else {
            base
        } & !3;
        sram[SHARED_CTRL] = err::OKAY;
        sram[SHARED_CTRL + 1] = 0;
        wr32(sram, SHARED_CTRL + 2, 0);
        wr32(sram, SHARED_CTRL + 6, 0);
        self.stop_owner(Owner::Sequence, link);
        for e in &self.sequence {
            let mut e = e.clone();
            e.start = e.start.wrapping_add(base);
            self.live.push(e);
        }
        for e in &self.sequence_dsp {
            let mut e = *e;
            e.time = e.time.wrapping_add(base);
            insert_dsp(&mut self.dsp_queue, e);
        }
        for ch in 0..VIDEO_CHANNELS {
            self.show(ch, Shown::Black, link);
            self.matte_after[ch] = false;
        }
        self.sequence_playing = true;
        log::info!(
            "flyer: sequence of {} events plays from field {base} (clock {clock})",
            self.sequence.len()
        );
        if crate::envcfg::flag("COPPERLINE_DIAG_FLYER") {
            for e in &self.sequence {
                log::info!("flyer:   {}", describe(e));
            }
            for e in &self.sequence_dsp {
                log::info!(
                    "flyer:   +{} DSP {} volume {:04X} pan {:04X} over {}",
                    e.time,
                    e.chan,
                    e.volume,
                    e.pan,
                    e.rate
                );
            }
        }
        err::OKAY
    }

    // ---------------------------------------------------------------
    // One clip
    // ---------------------------------------------------------------

    /// FLYPLAY (`Play1Clip`): check the request and set up its events.
    /// `Ok` means it is playing and the slot waits for it to finish.
    pub fn play_clip(
        &mut self,
        slot: Slot,
        mut r: Request,
        clip: ClipInfo,
        clock: u32,
        sram: &mut [u8],
        link: Option<&FlyerLink>,
    ) -> Result<(), u8> {
        if r.video() && usize::from(r.channel) >= VIDEO_CHANNELS {
            return Err(err::BADPARAM);
        }
        if r.video() && !clip.video {
            r.flags &= !caf::VIDEO;
        }
        let mut count = r.audio_count();
        if count > 0 && clip.audio_chans == 0 {
            r.flags &= !(caf::AUDIOL | caf::AUDIOR);
            count = 0;
        } else if count == 2 && clip.audio_chans == 1 {
            r.flags &= !caf::AUDIOR;
            count = 1;
        }
        if !r.video() && count == 0 {
            return Err(err::BADPARAM);
        }
        if r.video()
            && (r.vid_start > clip.fields || r.vid_start.saturating_add(r.vid_count) > clip.fields)
        {
            return Err(err::BADPARAM);
        }
        if count > 0
            && (r.aud_start > clip.fields || r.aud_start.saturating_add(r.aud_count) > clip.fields)
        {
            return Err(err::BADPARAM);
        }
        if r.vid_count == 0 {
            r.vid_count = clip.fields - r.vid_start;
        }
        if r.aud_count == 0 {
            r.aud_count = clip.fields - r.aud_start;
        }
        // The channel's current user: stolen, or an error if asked.
        if r.video() {
            let busy: Vec<Owner> = self
                .live
                .iter()
                .filter(|e| e.video == Some(r.channel))
                .map(|e| e.owner)
                .collect();
            if !busy.is_empty() && r.permiss & CAPF_ERRIFBUSY != 0 {
                return Err(ERR_CHANINUSE);
            }
            for owner in busy {
                self.abort_owner(owner, Some(&mut *sram), link);
            }
        }
        let audio = if count > 0 {
            match self.free_pair() {
                Some(pair) => Some(AudioUse {
                    dsp: pair,
                    count,
                    right_only: r.right_only() && count == 1,
                }),
                None if r.video() => None,
                None => return Err(err::CMDFAILED),
            }
        } else {
            None
        };
        let base = if r.go_clock == 0 {
            clock.wrapping_add(PLAY_LEAD)
        } else {
            r.go_clock
        } & !3;
        let (mut vid_go, mut aud_go) = (0u32, 0u32);
        if r.video() && audio.is_some() {
            // The part that starts earlier starts at the base time.
            if r.vid_start > r.aud_start {
                vid_go = r.vid_start - r.aud_start;
            } else {
                aud_go = r.aud_start - r.vid_start;
            }
        }
        let owner = Owner::Play(slot.0);
        if r.video() {
            self.live.push(Event {
                start: base.wrapping_add(vid_go),
                fields: r.vid_count,
                clip: Some(clip.path.clone()),
                first: r.vid_start,
                video: Some(r.channel),
                audio: None,
                matte_after: r.flags & caf::USEMATTE != 0,
                user_id: r.user_id,
                owner,
            });
        }
        if let Some(a) = audio {
            let start = base.wrapping_add(aud_go);
            self.live.push(Event {
                start,
                fields: r.aud_count,
                clip: Some(clip.path.clone()),
                first: r.aud_start,
                video: None,
                audio: Some(a),
                matte_after: false,
                user_id: r.user_id,
                owner,
            });
            for e in dsp_events(&r, start, r.aud_count, a.dsp + 1, owner) {
                insert_dsp(&mut self.dsp_queue, e);
            }
            self.last_play_audio = a.dsp + 1 + u8::from(a.right_only);
        }
        let (first, start) = if r.video() {
            (r.vid_start, base.wrapping_add(vid_go))
        } else {
            (r.aud_start, base.wrapping_add(aud_go))
        };
        self.plays.push(Play {
            slot: slot.0,
            action: r.action,
            first,
            start,
            paused: false,
            audio,
        });
        Ok(())
    }

    /// The first DSP pair no live event or jog session uses
    /// (`AllocAudioChan`).
    fn free_pair(&self) -> Option<u8> {
        let search = self.search.as_ref().and_then(|s| s.audio).map(|a| a.dsp);
        (0..DSP_CHANNELS as u8).step_by(2).find(|&pair| {
            search != Some(pair)
                && !self
                    .live
                    .iter()
                    .any(|e| e.audio.is_some_and(|a| a.dsp == pair))
        })
    }

    /// The Amiga changed a play's `cont` byte: 0 aborts it, `$FF` pauses
    /// and 1 resumes. Returns whether the slot held a play.
    pub fn cont_written(
        &mut self,
        slot: Slot,
        cont: u8,
        sram: &mut [u8],
        link: Option<&FlyerLink>,
    ) -> bool {
        let Some(i) = self.plays.iter().position(|p| p.slot == slot.0) else {
            return false;
        };
        match cont {
            0 => self.abort_owner(Owner::Play(slot.0), Some(sram), link),
            0xFF => self.plays[i].paused = true,
            _ => self.plays[i].paused = false,
        }
        true
    }

    /// CHGAUDIO (`ChangeAudio`): new levels for the audio of the clip
    /// playing, now, ramping over its attack time.
    pub fn change_audio(&mut self, r: &Request) {
        if self.last_play_audio == 0 {
            return;
        }
        let chan = self.last_play_audio;
        let count = r.audio_count();
        let set = |dsp: &mut Dsp, volume: u16, pan: u16| {
            dsp.set(&DspEvent {
                time: 0,
                chan: 0,
                volume,
                pan,
                rate: r.attack,
                owner: Owner::Sequence,
            })
        };
        let first = usize::from(chan - 1) % DSP_CHANNELS;
        let second = (first + 1) % DSP_CHANNELS;
        if count == 0 {
            set(&mut self.dsp[first], 0, 0);
            set(&mut self.dsp[second], 0, 0);
            return;
        }
        if r.flags & caf::AUDIOL != 0 {
            set(&mut self.dsp[first], r.sustain[0], r.pan[0]);
        } else {
            set(&mut self.dsp[first], r.sustain[1], r.pan[1]);
        }
        let stereo = self
            .plays
            .iter()
            .find_map(|p| p.audio)
            .is_some_and(|a| a.count == 2);
        if count == 2 || stereo {
            set(&mut self.dsp[second], r.sustain[1], r.pan[1]);
        }
    }

    /// RESETFLYER (`ActsReset`): stop the sequence and every clip, silence
    /// the DSP, and put the channels to black.
    pub fn reset(&mut self, sram: &mut [u8], link: Option<&FlyerLink>) {
        let owners: Vec<Owner> = self
            .plays
            .iter()
            .map(|p| Owner::Play(p.slot))
            .chain(std::iter::once(Owner::Sequence))
            .collect();
        for owner in owners {
            self.abort_owner(owner, Some(sram), link);
        }
        self.live.clear();
        self.dsp_queue.clear();
        self.dsp = Default::default();
        self.sequence_playing = false;
        self.last_play_audio = 0;
        self.search = None;
    }

    /// Drop an owner's events without finishing its command.
    fn stop_owner(&mut self, owner: Owner, link: Option<&FlyerLink>) {
        let mut channels = [false; VIDEO_CHANNELS];
        let mut dsp = [false; DSP_CHANNELS];
        self.live.retain(|e| {
            if e.owner != owner {
                return true;
            }
            if let Some(ch) = e.video {
                channels[usize::from(ch) % VIDEO_CHANNELS] = true;
            }
            if let Some(a) = e.audio {
                for c in 0..a.count.max(2) {
                    dsp[usize::from(a.dsp + c) % DSP_CHANNELS] = true;
                }
            }
            false
        });
        self.dsp_queue.retain(|e| e.owner != owner);
        for (c, mute) in dsp.iter().enumerate() {
            if *mute {
                self.dsp[c] = Dsp::default();
            }
        }
        for (ch, stopped) in channels.iter().enumerate() {
            if *stopped {
                self.show(ch, Shown::Black, link);
                self.matte_after[ch] = true;
            }
        }
        if owner == Owner::Sequence {
            self.sequence_playing = false;
        }
    }

    /// Abort an owner: its events stop, its channels go black and its
    /// audio silent; a play's command completes (with `sram`).
    fn abort_owner(&mut self, owner: Owner, sram: Option<&mut [u8]>, link: Option<&FlyerLink>) {
        self.stop_owner(owner, link);
        if let Owner::Play(slot) = owner {
            if let Some(i) = self.plays.iter().position(|p| p.slot == slot) {
                let play = self.plays.remove(i);
                if let Some(sram) = sram {
                    // The fields it showed before the abort.
                    let done = self.clock.wrapping_add(1).saturating_sub(play.start);
                    finish_play(sram, &play, done);
                }
                if play.audio.is_some() {
                    self.last_play_audio = 0;
                }
            }
        }
    }

    /// Show `what` on channel `ch`, telling the Toaster if it changed.
    fn show(&mut self, ch: usize, what: Shown, link: Option<&FlyerLink>) {
        if self.shown[ch] == what {
            return;
        }
        if let Some(link) = link {
            match &what {
                Shown::Nothing => {}
                Shown::Black => link.show(ch, FieldSource::Matte(BLACK)),
                Shown::Clip(path, n) => {
                    if let Some(c) = self.clip(path) {
                        link.show(ch, FieldSource::Clip(c, *n));
                    }
                }
            }
        }
        self.shown[ch] = what;
    }

    // ---------------------------------------------------------------
    // Jog and shuttle
    // ---------------------------------------------------------------

    /// Whether a jog session holds video channel `ch` (a FLYPLAY for it
    /// waits, as on the firmware's channel semaphore).
    pub fn channel_held(&self, ch: u8) -> bool {
        self.search
            .as_ref()
            .is_some_and(|s| s.holds_channel && s.channel == ch)
    }

    /// Whether a jog session is open.
    pub fn search_open(&self) -> bool {
        self.search.is_some()
    }

    /// SRCHARGS 1 (`SearchArgs`, setting up): open a session on `clip`, or
    /// count another opening of the one already open. With video it waits
    /// for a clip playing on the channel to finish.
    pub fn open_search(&mut self, r: &Request, clip: Option<ClipInfo>) -> Step {
        if let Some(s) = self.search.as_mut() {
            // "Jog Already Open!": counted, nothing else.
            s.nest += 1;
            return Step::Done(err::OKAY);
        }
        let Some(clip) = clip else {
            return Step::Done(mailbox::err::OBJNOTFOUND);
        };
        if r.video() && usize::from(r.channel) >= VIDEO_CHANNELS {
            return Step::Done(err::BADPARAM);
        }
        if r.video()
            && self
                .live
                .iter()
                .any(|e| e.video == Some(r.channel) && matches!(e.owner, Owner::Play(_)))
        {
            return Step::Wait;
        }
        let mut search = Search {
            nest: 1,
            clip: clip.path,
            fields: clip.fields,
            has_video: clip.video,
            audio_chans: clip.audio_chans,
            channel: r.channel,
            holds_channel: r.video(),
            on_video: r.video(),
            audio: None,
            frames: Vec::new(),
            settled: 0,
            half_done: None,
        };
        search.audio = self.search_audio(r, &search);
        log::info!(
            "flyer: jog session on {}{}",
            search.clip.display(),
            if search.on_video {
                [", channel A", ", channel B"][usize::from(r.channel) % 2]
            } else {
                ""
            }
        );
        self.search = Some(search);
        Step::Done(err::OKAY)
    }

    /// The DSP pair for a session's sound, if it asks for sound and one is
    /// free.
    fn search_audio(&self, r: &Request, s: &Search) -> Option<AudioUse> {
        let count = r.audio_count();
        if count == 0 || s.audio_chans == 0 {
            return None;
        }
        self.free_pair().map(|dsp| AudioUse {
            dsp,
            count: s.audio_chans.min(2),
            right_only: false,
        })
    }

    /// Silence the session's pair and let it go (`FreeAudioChan`).
    fn free_search_audio(&mut self) {
        let Some(a) = self.search.as_mut().and_then(|s| s.audio.take()) else {
            return;
        };
        for c in 0..2 {
            self.dsp[usize::from(a.dsp + c) % DSP_CHANNELS] = Dsp::default();
        }
        self.dsp_queue.retain(|e| e.owner != Owner::Search);
    }

    /// SRCHARGS 2 (`FindFieldAudio`): new sound settings for the session,
    /// once its last level changes have happened.
    pub fn change_search(&mut self, r: &Request, clock: u32) -> Step {
        let Some(s) = self.search.as_ref() else {
            return Step::Done(err::OKAY);
        };
        if clock < s.settled {
            return Step::Wait;
        }
        self.free_search_audio();
        let s = self.search.as_ref().expect("checked");
        let audio = self.search_audio(r, s);
        let s = self.search.as_mut().expect("checked");
        s.audio = audio;
        s.on_video = r.video();
        Step::Done(err::OKAY)
    }

    /// SRCHARGS 0 (`EndFindField`): close the session once its last frame
    /// has taken: its sound goes quiet, the channel shows black with
    /// `CAF_USEMATTE` (else it keeps the frame), and is free again.
    pub fn close_search(&mut self, r: &Request, clock: u32, link: Option<&FlyerLink>) -> Step {
        let Some(s) = self.search.as_mut() else {
            return Step::Done(err::OKAY);
        };
        if s.nest > 1 {
            s.nest -= 1;
            return Step::Done(err::OKAY);
        }
        if s.frames.last().is_some_and(|f| f.start > clock) {
            return Step::Wait;
        }
        self.free_search_audio();
        let s = self.search.take().expect("checked");
        let ch = usize::from(s.channel) % VIDEO_CHANNELS;
        if s.on_video && r.video() && r.flags & caf::USEMATTE != 0 {
            self.show(ch, Shown::Black, link);
            self.matte_after[ch] = true;
        }
        log::info!("flyer: jog session closed");
        Step::Done(err::OKAY)
    }

    /// DOSEARCH (`DoSearch`): put up the colour frame holding the field the
    /// ClipAction at `action` names, with its sound. It waits for the
    /// previous frame to take, then queues its own for the next colour
    /// frame, and completes once the previous frame's level changes have
    /// happened, with `LastFieldDone` set to the field.
    pub fn do_search(
        &mut self,
        slot: Slot,
        sram: &mut [u8],
        action: usize,
        clock: u32,
        link: Option<&FlyerLink>,
    ) -> Step {
        let Some(s) = self.search.as_ref() else {
            return Step::Done(err::BADPARAM);
        };
        // The second half of a DOSEARCH whose frame is already queued.
        if let Some((held, frame)) = s.half_done {
            if held == slot.0 {
                if clock < s.settled {
                    return Step::Wait;
                }
                let r = Request::read(sram, action);
                self.search.as_mut().expect("checked").half_done = None;
                return self.finish_search(sram, &r, frame);
            }
        }
        let mut r = Request::read(sram, action);
        let n = if r.video() { r.vid_start } else { r.aud_start };
        if n >= s.fields {
            return Step::Done(err::BADPARAM);
        }
        // "Wait for new video to take."
        if s.frames.last().is_some_and(|f| f.start > clock) {
            return Step::Wait;
        }
        if r.video() && !s.has_video {
            r.flags &= !caf::VIDEO;
        }
        let count = r.audio_count();
        if count > 0 && s.audio_chans == 0 {
            r.flags &= !(caf::AUDIOL | caf::AUDIOR);
        } else if count == 2 && s.audio_chans < 2 {
            r.flags &= !caf::AUDIOR;
        }
        wr8(sram, action + ca::FLAGS, r.flags);
        let wants_sound = r.audio_count() > 0;
        if !r.video() && !wants_sound {
            return Step::Done(err::OKAY);
        }
        // The next colour frame, or this one if it is just starting and
        // nothing of the session's is showing yet.
        let start = if clock & 3 == 0 && s.frames.is_empty() {
            clock
        } else {
            clock.wrapping_add(4) & !3
        };
        let frame = SearchFrame {
            first: n & !3,
            start,
            audio: wants_sound && s.audio.is_some(),
            automute: r.permiss & CAPF_AUTOMUTE != 0,
        };
        let settled = s.settled;
        let path = s.clip.clone();
        if let (Some(link), Some(clip)) = (link, self.clip(&path)) {
            for k in 0..4 {
                link.prefetch(&clip, frame.first + k);
            }
            link.prefetch_sound(&clip, frame.first / 4);
        }
        log::debug!(
            "flyer: search field {n}: colour frame from field {} at clock {start} (now {clock})",
            frame.first
        );
        let s = self.search.as_mut().expect("checked");
        let showing = s.frame_at(clock);
        s.frames.clear();
        s.frames.extend(showing);
        s.frames.push(frame);
        if wants_sound && clock < settled {
            s.half_done = Some((slot.0, frame));
            return Step::Wait;
        }
        self.finish_search(sram, &r, frame)
    }

    /// The level changes of a DOSEARCH's frame, and its completion.
    fn finish_search(&mut self, sram: &mut [u8], r: &Request, frame: SearchFrame) -> Step {
        let s = self.search.as_mut().expect("a session");
        if let (true, Some(a)) = (r.audio_count() > 0, s.audio) {
            let chans = if s.audio_chans == 2 { 2 } else { 1 };
            for c in 0..chans {
                let chan = a.dsp + 1 + c;
                insert_dsp(
                    &mut self.dsp_queue,
                    DspEvent {
                        time: frame.start,
                        chan,
                        volume: r.sustain[usize::from(c)],
                        pan: r.pan[usize::from(c)],
                        rate: r.attack,
                        owner: Owner::Search,
                    },
                );
                if frame.automute {
                    insert_dsp(
                        &mut self.dsp_queue,
                        DspEvent {
                            time: frame.start.wrapping_add(4),
                            chan,
                            volume: 0,
                            pan: 0,
                            rate: 0,
                            owner: Owner::Search,
                        },
                    );
                }
            }
            s.settled = frame.start.wrapping_add(if frame.automute { 4 } else { 0 });
        }
        let done = if r.video() { r.vid_start } else { r.aud_start };
        wr32(sram, r.action + ca::LAST_FIELD_DONE, done);
        Step::Done(err::OKAY)
    }

    /// The field a jog session shows on channel `ch` at `clock`: its frame
    /// looping.
    fn search_field(&self, ch: usize, clock: u32) -> Option<(PathBuf, u32)> {
        let s = self.search.as_ref()?;
        if !s.on_video || usize::from(s.channel) != ch {
            return None;
        }
        let f = s.frame_at(clock)?;
        Some((s.clip.clone(), f.first + (clock - f.start) % 4))
    }

    /// The field of sound a jog session plays at `clock`, if any.
    fn search_sound(&self, clock: u32) -> Option<(PathBuf, u32, AudioUse)> {
        let s = self.search.as_ref()?;
        let a = s.audio?;
        let f = s.frame_at(clock)?;
        let age = clock - f.start;
        if !f.audio || (f.automute && age >= 4) {
            return None;
        }
        Some((s.clip.clone(), f.first + age % 4, a))
    }

    // ---------------------------------------------------------------
    // Every field
    // ---------------------------------------------------------------

    /// What channel `ch` shows at `clock`: a jog frame, the latest video
    /// event covering it, or `None` (black or held, per the matte).
    fn video_at(&self, ch: usize, clock: u32) -> Option<Shown> {
        if let Some((path, n)) = self.search_field(ch, clock) {
            return Some(Shown::Clip(path, n));
        }
        let e = self
            .live
            .iter()
            .filter(|e| e.video == Some(ch as u8) && e.covers(clock))
            .max_by_key(|e| e.start)?;
        Some(match &e.clip {
            Some(path) => Shown::Clip(path.clone(), e.first + (clock - e.start)),
            None => Shown::Black,
        })
    }

    /// Tell the link what the channels will show in the next fields, and
    /// which sound will be wanted, so it is ready before it is due.
    fn read_ahead(&mut self, clock: u32, link: &FlyerLink) {
        for d in 1..=READ_AHEAD {
            let t = clock.wrapping_add(d);
            for ch in 0..VIDEO_CHANNELS {
                if let Some(Shown::Clip(path, n)) = self.video_at(ch, t) {
                    if let Some(clip) = self.clip(&path) {
                        link.prefetch(&clip, n);
                    }
                }
            }
        }
        let t = clock.wrapping_add(4);
        let sounds: Vec<(PathBuf, u32)> = self
            .live
            .iter()
            .filter(|e| e.audio.is_some() && e.covers(t))
            .filter_map(|e| Some((e.clip.clone()?, e.first + (t - e.start))))
            .collect();
        for (path, n) in sounds {
            if let Some(clip) = self.clip(&path) {
                link.prefetch_sound(&clip, n / 4);
            }
        }
    }

    /// Field `clock` begins: show its video, mix its sound, and finish
    /// what is done.
    pub fn field(&mut self, clock: u32, sram: &mut [u8], link: Option<&FlyerLink>) {
        self.clock = clock;
        // A paused clip holds its field: its events slip a field.
        for p in self.plays.iter().filter(|p| p.paused) {
            let owner = Owner::Play(p.slot);
            for e in self.live.iter_mut().filter(|e| e.owner == owner) {
                e.start = e.start.wrapping_add(1);
            }
            for e in self.dsp_queue.iter_mut().filter(|e| e.owner == owner) {
                e.time = e.time.wrapping_add(1);
            }
        }
        for p in self.plays.iter_mut().filter(|p| p.paused) {
            p.start = p.start.wrapping_add(1);
        }
        // DSP changes due.
        let due = self
            .dsp_queue
            .iter()
            .take_while(|e| e.time <= clock)
            .count();
        for e in self.dsp_queue.drain(..due).collect::<Vec<_>>() {
            if (1..=DSP_CHANNELS as u8).contains(&e.chan) {
                self.dsp[usize::from(e.chan - 1)].set(&e);
            }
        }
        // Video.
        for ch in 0..VIDEO_CHANNELS {
            let what = match self.video_at(ch, clock) {
                Some(what) => what,
                None if self.matte_after[ch] => Shown::Black,
                None => continue,
            };
            self.show(ch, what, link);
        }
        // Sound.
        let sound = self.mix(clock);
        if let Some(link) = link {
            link.push_sound(&sound);
            self.read_ahead(clock, link);
        }
        // What ended with this field.
        let next = clock.wrapping_add(1);
        let mut ended = Vec::new();
        self.live.retain(|e| {
            if e.end() <= next {
                ended.push(e.clone());
                false
            } else {
                true
            }
        });
        for e in &ended {
            if let Some(ch) = e.video {
                self.matte_after[usize::from(ch) % VIDEO_CHANNELS] = e.matte_after;
            }
        }
        let plays: Vec<Play> = self.plays.clone();
        for p in plays {
            let owner = Owner::Play(p.slot);
            if !self.live.iter().any(|e| e.owner == owner) {
                let done = next.saturating_sub(p.start);
                finish_play(sram, &p, done);
                self.plays.retain(|q| q.slot != p.slot);
                if p.audio.is_some() {
                    self.last_play_audio = 0;
                }
                // Its last level changes (the decay's end) still happen.
                self.dsp_queue
                    .retain(|e| e.owner != owner || e.time <= next);
            }
        }
        if self.sequence_playing && !self.live.iter().any(|e| e.owner == Owner::Sequence) {
            self.sequence_playing = false;
            self.dsp_queue
                .retain(|e| e.owner != Owner::Sequence || e.time <= next);
            log::info!("flyer: sequence done at field {clock}");
        }
        // SequenceStatus: `Done` when the last track goes idle.
        let busy = self.sequence_playing || !self.plays.is_empty();
        if self.was_busy && !busy {
            wr8(sram, SHARED_CTRL + 1, 1);
        }
        self.was_busy = busy;
    }

    /// Mix this field's sound: every audio event's samples through its
    /// DSP channels.
    fn mix(&mut self, clock: u32) -> Vec<(f32, f32)> {
        let mut input: [Option<Vec<f32>>; DSP_CHANNELS] = Default::default();
        let mut events: Vec<Event> = self
            .live
            .iter()
            .filter(|e| e.audio.is_some() && e.covers(clock))
            .cloned()
            .collect();
        // A jog frame's sound, as an event of one field.
        if let Some((path, n, a)) = self.search_sound(clock) {
            events.push(Event {
                start: clock,
                fields: 1,
                clip: Some(path),
                first: n,
                video: None,
                audio: Some(a),
                matte_after: false,
                user_id: 0,
                owner: Owner::Search,
            });
        }
        for e in events {
            let (Some(path), Some(a)) = (&e.clip, e.audio) else {
                continue;
            };
            let Some(clip) = self.clip(path) else {
                continue;
            };
            let n = e.first + (clock - e.start);
            for c in 0..a.count {
                let source = if a.right_only { 1 } else { c };
                let samples = clip.audio_field(n, source);
                let slot = input[usize::from(a.dsp + c) % DSP_CHANNELS]
                    .get_or_insert_with(|| vec![0.0; SAMPLES_PER_FIELD]);
                for (o, &s) in slot.iter_mut().zip(samples.iter()) {
                    *o += f32::from(s) / 32768.0;
                }
            }
        }
        let mut out = vec![(0.0f32, 0.0f32); SAMPLES_PER_FIELD];
        for (c, dsp) in self.dsp.iter_mut().enumerate() {
            let ((v0, p0), (v1, p1)) = dsp.step();
            let Some(samples) = &input[c] else {
                continue;
            };
            if v0 == 0.0 && v1 == 0.0 {
                continue;
            }
            let n = SAMPLES_PER_FIELD as f32;
            for (i, (o, &s)) in out.iter_mut().zip(samples.iter()).enumerate() {
                let t = (i + 1) as f32 / n;
                let v = v0 + (v1 - v0) * t;
                let p = p0 + (p1 - p0) * t;
                o.0 += s * v * (1.0 - p);
                o.1 += s * v * p;
            }
        }
        out
    }
}

/// An event for the log.
fn describe(e: &Event) -> String {
    let what = match (&e.clip, e.video, e.audio) {
        (None, Some(ch), _) => format!("black on {}", ["A", "B"][usize::from(ch) % 2]),
        (Some(_), Some(ch), _) => format!("video on {}", ["A", "B"][usize::from(ch) % 2]),
        (_, None, Some(a)) => format!(
            "audio on DSP {}{}{}",
            a.dsp + 1,
            if a.count == 2 { "+" } else { "" },
            if a.right_only { " (right)" } else { "" }
        ),
        _ => "nothing".into(),
    };
    let clip = e
        .clip
        .as_deref()
        .and_then(Path::file_name)
        .map_or(String::new(), |n| format!(" {}", n.to_string_lossy()));
    format!(
        "+{} for {}: {what}{clip} from field {} (user {})",
        e.start, e.fields, e.first, e.user_id
    )
}

/// A play's command completes: `LastFieldDone` in its ClipAction, then
/// done in its slot.
fn finish_play(sram: &mut [u8], play: &Play, fields_done: u32) {
    wr32(
        sram,
        play.action + ca::LAST_FIELD_DONE,
        play.first.wrapping_add(fields_done),
    );
    let slot = Slot(play.slot);
    slot.set_error(sram, err::OKAY);
    slot.mark_done(sram);
    log::info!(
        "flyer: clip in slot {} done after {fields_done} fields",
        play.slot
    );
}

/// `MakeDSPevents`: bring the clip's channels up at `go` over the attack
/// and down over the decay at its end; with an audio envelope the keys do
/// that, and only a final mute is added.
fn dsp_events(r: &Request, go: u32, fields: u32, chan: u8, owner: Owner) -> Vec<DspEvent> {
    let mut out = Vec::new();
    let sides: Vec<usize> = [(caf::AUDIOL, 0), (caf::AUDIOR, 1)]
        .into_iter()
        .filter(|(f, _)| r.flags & f != 0)
        .map(|(_, side)| side)
        .collect();
    let decay = u32::from(r.decay).min(fields);
    for (k, &side) in sides.iter().enumerate() {
        let chan = chan.wrapping_add(k as u8);
        if r.flags & caf::AUDENV != 0 {
            out.push(DspEvent {
                time: go.wrapping_add(fields),
                chan,
                volume: 0,
                pan: 0,
                rate: 0,
                owner,
            });
        } else {
            out.push(DspEvent {
                time: go,
                chan,
                volume: r.sustain[side],
                pan: r.pan[side],
                rate: r.attack,
                owner,
            });
            out.push(DspEvent {
                time: go.wrapping_add(fields).wrapping_sub(decay),
                chan,
                volume: 0,
                pan: 0,
                rate: decay as u16,
                owner,
            });
        }
    }
    out
}

/// Insert into a time-ordered list, after events of the same time.
fn insert_dsp(list: &mut Vec<DspEvent>, e: DspEvent) {
    let at = list.partition_point(|x| x.time <= e.time);
    list.insert(at, e);
}

/// `AddDSPevent`: insert in time order, with the firmware's tidying. An
/// immediate change at the same time on the same channel as one already
/// there replaces a mute with a level, or is dropped if it is a mute
/// after a level; a ramp starting where an immediate change is starts a
/// field later so that change takes first.
fn add_dsp_event(list: &mut Vec<DspEvent>, mut e: DspEvent) {
    let mut i = 0;
    while i < list.len() {
        let this = list[i];
        if e.time < this.time {
            break;
        }
        let same = e.time == this.time && e.chan == this.chan;
        if same && e.rate == 0 {
            if e.volume != 0 && this.volume == 0 {
                list.remove(i);
                continue;
            } else if e.volume == 0 && this.volume != 0 {
                return;
            }
        }
        if same && this.rate == 0 && e.rate > 0 {
            e.time += 1;
            e.rate -= 1;
        }
        i += 1;
    }
    list.insert(i, e);
}

/// The FLYPLAY argument: the ClipAction's SRAM offset.
pub fn action_of(sram: &[u8], slot: Slot) -> usize {
    slot.arg32(sram, 4) as usize % mailbox::SRAM_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flyer::mailbox::{STAT_DONE, STAT_INPROG};

    const ACTION: usize = 0x2000;

    fn request(flags: u8, channel: u8) -> Request {
        Request {
            action: ACTION,
            channel,
            flags,
            permiss: 0,
            vid_start: 0,
            aud_start: 0,
            vid_count: 8,
            aud_count: 8,
            go_clock: 0,
            attack: 0,
            sustain: [0xFFFF, 0xFFFF],
            decay: 0,
            pan: [0x8000, 0x7FFF],
            user_id: 7,
        }
    }

    fn info(path: &Path) -> ClipInfo {
        ClipInfo {
            path: path.to_path_buf(),
            fields: 12,
            video: true,
            audio_chans: 2,
        }
    }

    struct Rig {
        player: Player,
        sram: Vec<u8>,
        link: Arc<FlyerLink>,
        path: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Clip");
        super::super::clip::tests::write_test_clip(&path, 3);
        Rig {
            player: Player::default(),
            sram: vec![0u8; mailbox::SRAM_SIZE],
            link: FlyerLink::new(),
            path,
            _dir: dir,
        }
    }

    impl Rig {
        fn field(&mut self, clock: u32) -> Vec<(f32, f32)> {
            self.player.field(clock, &mut self.sram, Some(&self.link));
            self.link.take_sound()
        }
    }

    #[test]
    fn a_clip_plays_its_fields_and_sound_then_completes_its_command() {
        let mut r = rig();
        let slot = Slot(2);
        slot.set_opcode_word(&mut r.sram, 0x17 | STAT_INPROG);
        let req = request(caf::VIDEO | caf::AUDIOL | caf::AUDIOR | caf::USEMATTE, 1);
        let info = info(&r.path);
        r.player
            .play_clip(slot, req, info, 101, &mut r.sram, Some(&r.link))
            .unwrap();
        // Starts at (101 + 20) & ~3 = 120.
        let quiet = r.field(119);
        assert!(quiet.iter().all(|&(l, rr)| l == 0.0 && rr == 0.0));
        assert_eq!(r.player.shown[1], Shown::Nothing);
        for k in 0..8u32 {
            let sound = r.field(120 + k);
            assert_eq!(
                r.player.shown[1],
                Shown::Clip(r.path.clone(), k),
                "field {k}"
            );
            // Left carries 100·n hard left, right −100·n hard right.
            let want = 100.0 * k as f32 / 32768.0;
            assert!((sound[300].0 - want).abs() < 1e-3, "{k}: {:?}", sound[300]);
            assert!((sound[300].1 + want).abs() < 1e-3);
            let status = slot.status(&r.sram);
            assert_eq!(status == STAT_DONE, k == 7, "field {k}");
        }
        assert_eq!(rd32(&r.sram, ACTION + ca::LAST_FIELD_DONE), 8);
        // The matte comes up after the clip.
        r.field(128);
        assert_eq!(r.player.shown[1], Shown::Black);
        assert_eq!(r.player.dsp_queue.len(), 0);
    }

    #[test]
    fn an_aborted_clip_completes_at_once_and_goes_black() {
        let mut r = rig();
        let slot = Slot(1);
        let info = info(&r.path);
        let mut req = request(caf::VIDEO | caf::AUDIOL | caf::AUDIOR, 0);
        req.go_clock = 40;
        r.player
            .play_clip(slot, req, info, 10, &mut r.sram, Some(&r.link))
            .unwrap();
        r.field(40);
        r.field(41);
        assert!(r.player.cont_written(slot, 0, &mut r.sram, Some(&r.link)));
        assert_eq!(slot.status(&r.sram), STAT_DONE);
        // Two fields were shown.
        assert_eq!(rd32(&r.sram, ACTION + ca::LAST_FIELD_DONE), 2);
        assert_eq!(r.player.shown[0], Shown::Black);
        assert!(!r.player.busy());
        assert_eq!(r.player.dsp[0], Dsp::default());
    }

    #[test]
    fn a_paused_clip_holds_its_field() {
        let mut r = rig();
        let info = info(&r.path);
        let mut req = request(caf::VIDEO, 0);
        req.go_clock = 40;
        r.player
            .play_clip(Slot(0), req, info, 10, &mut r.sram, Some(&r.link))
            .unwrap();
        r.field(40);
        r.field(41);
        r.player
            .cont_written(Slot(0), 0xFF, &mut r.sram, Some(&r.link));
        r.field(42);
        r.field(43);
        assert_eq!(r.player.shown[0], Shown::Clip(r.path.clone(), 1));
        r.player
            .cont_written(Slot(0), 1, &mut r.sram, Some(&r.link));
        r.field(44);
        assert_eq!(r.player.shown[0], Shown::Clip(r.path.clone(), 2));
    }

    #[test]
    fn a_sequence_plays_from_its_base_time_and_reports_done() {
        let mut r = rig();
        let info = info(&r.path);
        r.player.new_sequence();
        let mut a = request(caf::VIDEO | caf::AUDIOL | caf::AUDIOR, 0);
        a.vid_count = 8;
        a.aud_count = 8;
        a.attack = 4;
        assert_eq!(r.player.add_sequence_clip(a, Some(info.clone())), err::OKAY);
        let mut b = request(caf::VIDEO | caf::AUDIOL | caf::AUDIOR, 1);
        b.go_clock = 4;
        b.vid_start = 4;
        b.aud_start = 4;
        b.vid_count = 8;
        b.aud_count = 8;
        assert_eq!(r.player.add_sequence_clip(b, Some(info)), err::OKAY);
        // The overlapping second clip gets the next audio pair.
        let pairs: Vec<u8> = r
            .player
            .sequence
            .iter()
            .filter_map(|e| e.audio.map(|a| a.dsp))
            .collect();
        assert_eq!(pairs, [0, 2]);
        // An envelope key on the last clip's channels (3 and 4).
        r.player.add_envelope_key(6, 2, [0x8000, 0x8000], [0, 0]);
        assert!(r
            .player
            .sequence_dsp
            .iter()
            .any(|e| e.chan == 3 && e.time == 6));
        assert_eq!(
            r.player.play_sequence(203, 190, &mut r.sram, Some(&r.link)),
            err::OKAY
        );
        assert_eq!(r.player.shown, [Shown::Black, Shown::Black]);
        r.field(199);
        assert_eq!(r.player.shown[0], Shown::Black);
        // Base 200: A shows field 0, B from 204 field 4.
        r.field(200);
        assert_eq!(r.player.shown[0], Shown::Clip(r.path.clone(), 0));
        // The attack ramps clip A's volume up over four fields.
        let first = r.player.dsp[0].volume;
        assert!(first > 0.2 && first < 0.3, "{first}");
        r.field(205);
        assert_eq!(r.player.shown[1], Shown::Clip(r.path.clone(), 5));
        assert_eq!(r.player.dsp[2].volume, 1.0);
        assert_eq!(r.sram[SHARED_CTRL + 1], 0);
        for clock in 206..212 {
            r.field(clock);
        }
        // Clip A held its last field; B ended at 211.
        assert_eq!(r.player.shown[0], Shown::Clip(r.path.clone(), 7));
        assert_eq!(r.sram[SHARED_CTRL + 1], 1);
        assert!(!r.player.busy());
    }

    #[test]
    fn a_black_event_puts_the_channel_to_black() {
        let mut r = rig();
        r.player.new_sequence();
        let mut black = request(caf::VIDEO, 0);
        black.go_clock = 8;
        assert_eq!(r.player.add_sequence_clip(black, None), err::OKAY);
        assert_eq!(
            r.player.add_sequence_clip(request(caf::AUDIOL, 0), None),
            err::BADPARAM
        );
        r.player.play_sequence(100, 0, &mut r.sram, Some(&r.link));
        r.player.shown[0] = Shown::Nothing;
        r.field(108);
        assert_eq!(r.player.shown[0], Shown::Black);
    }

    #[test]
    fn the_dsp_list_tidies_like_the_firmware() {
        let ev = |time, chan, volume, rate| DspEvent {
            time,
            chan,
            volume,
            pan: 0,
            rate,
            owner: Owner::Sequence,
        };
        let mut list = Vec::new();
        add_dsp_event(&mut list, ev(10, 1, 0, 0));
        // A level at the same time replaces the mute.
        add_dsp_event(&mut list, ev(10, 1, 100, 0));
        assert_eq!(list, [ev(10, 1, 100, 0)]);
        // A mute after a level at the same time is dropped.
        add_dsp_event(&mut list, ev(10, 1, 0, 0));
        assert_eq!(list.len(), 1);
        // A ramp starting with an immediate change waits a field.
        add_dsp_event(&mut list, ev(10, 1, 50, 4));
        assert_eq!(list[1], ev(11, 1, 50, 3));
        add_dsp_event(&mut list, ev(5, 2, 1, 0));
        assert_eq!(list[0].time, 5);
    }

    #[test]
    fn a_ramp_reaches_its_target_over_its_fields_and_pan_flips_the_top_bit() {
        let mut d = Dsp::default();
        d.set(&DspEvent {
            time: 0,
            chan: 1,
            volume: 0xFFFF,
            pan: 0,
            rate: 4,
            owner: Owner::Sequence,
        });
        assert_eq!(d.volume, 0.0);
        let mut ends = Vec::new();
        for _ in 0..5 {
            ends.push(d.step().1 .0);
        }
        assert_eq!(ends, [0.25, 0.5, 0.75, 1.0, 1.0]);
        // API pan 0 is the centre.
        assert!((d.pan - 0.5).abs() < 1e-3);
    }

    #[test]
    fn the_fields_about_to_play_are_read_ahead() {
        let mut r = rig();
        let info = info(&r.path);
        let mut req = request(caf::VIDEO, 1);
        req.go_clock = 40;
        r.player
            .play_clip(Slot(0), req, info, 10, &mut r.sram, Some(&r.link))
            .unwrap();
        r.field(33);
        let clip = r.player.clip(&r.path).unwrap();
        // Fields 34 to 41 on the clock: the clip's 0 and 1 from 40.
        assert!(r.link.has_field(&clip, 0) && r.link.has_field(&clip, 1));
        assert!(!r.link.has_field(&clip, 2));
        r.field(34);
        assert!(r.link.has_field(&clip, 2));
    }

    #[test]
    fn a_busy_channel_is_refused_when_asked_to_be() {
        let mut r = rig();
        let info = info(&r.path);
        r.player
            .play_clip(
                Slot(0),
                request(caf::VIDEO, 0),
                info.clone(),
                0,
                &mut r.sram,
                None,
            )
            .unwrap();
        let mut second = request(caf::VIDEO, 0);
        second.permiss = CAPF_ERRIFBUSY;
        assert_eq!(
            r.player
                .play_clip(Slot(1), second, info.clone(), 0, &mut r.sram, None),
            Err(ERR_CHANINUSE)
        );
        // Otherwise the newcomer takes the channel.
        r.player
            .play_clip(Slot(1), request(caf::VIDEO, 0), info, 0, &mut r.sram, None)
            .unwrap();
        assert_eq!(r.player.plays.len(), 1);
        assert_eq!(r.player.plays[0].slot, 1);
    }
}
