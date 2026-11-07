// SPDX-License-Identifier: GPL-3.0-or-later

//! The ribbon between the Flyer and the Toaster 4000.
//!
//! On the real cards the Toaster's inputs 3 and 4 pass through the Flyer,
//! which can put its playback channel A on input 3 and channel B on input
//! 4 instead (`ToasterMux`), and the Flyer's audio output is the system's
//! sound (the Toaster has none). Here the two cards are separate devices
//! (the Flyer a Zorro board, the Toaster on the video slot), so they share
//! a [`FlyerLink`] made when both are fitted:
//!
//! - the Flyer sets the routing and, every field, says what each channel
//!   shows (a clip field, black, a matte) and adds a field of its sound;
//! - the Toaster, when it composes a frame, asks for a routed input's
//!   picture and takes the sound since the last frame.
//!
//! Decoding a field (VTASC, then the composite samples to colour) is the
//! costly part, so it is kept off the emulation thread: the Flyer also
//! tells the link which fields its channels are about to show
//! ([`FlyerLink::prefetch`]), and a few worker threads decode those ahead
//! into a small cache of pictures. When the Toaster weaves a field it is
//! normally ready; one that is not is waited for, or decoded on the spot if
//! no worker has started it, so the picture never depends on how fast the
//! host is. A channel nobody looks at costs only the read-ahead.
//!
//! The Toaster composes its outputs on a thread of its own, a field or two
//! behind the machine, so what a channel showed is not read off the link
//! by that thread (it would see whatever the Flyer had shown by then): the
//! Toaster takes each channel's fields shown since its last frame
//! ([`FlyerLink::take_shown`]) and the sound since then at a fixed point of
//! emulated time, on the emulation thread, and hands them to the compose
//! thread with the field. The weave ([`FlyerLink::weave_shown`]) then
//! happens there, in order, into the channel's picture.

use super::clip::ClipFile;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

/// Picture size: the Toaster's inputs.
pub const PICTURE_W: usize = 752;
pub const PICTURE_H: usize = 480;

/// What a Flyer channel puts out for a field.
#[derive(Clone, Debug)]
pub enum FieldSource {
    /// Field `n` of a clip.
    Clip(Arc<ClipFile>, u32),
    /// A flat colour (RGBA): black, or the clip's end matte.
    Matte(u32),
}

/// Black, as the Toaster's pictures hold it.
pub const BLACK: u32 = u32::from_le_bytes([0, 0, 0, 0xFF]);

/// One playback channel's output as a picture: its two fields woven.
struct Channel {
    picture: Vec<u32>,
    /// What the channel showed since the picture was last woven: see
    /// [`push_shown`].
    pending: Vec<FieldSource>,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            picture: vec![BLACK; PICTURE_W * PICTURE_H],
            pending: Vec::new(),
        }
    }
}

/// Note that a channel showed `source`, keeping only what the next weave
/// needs: a matte wipes the picture, so anything before it is moot, and of
/// the clip fields after the last matte only the latest of each line
/// parity reaches the picture.
pub fn push_shown(shown: &mut Vec<FieldSource>, source: FieldSource) {
    match &source {
        FieldSource::Matte(_) => shown.clear(),
        FieldSource::Clip(_, n) => {
            let parity = n & 1;
            shown.retain(|s| !matches!(s, FieldSource::Clip(_, m) if m & 1 == parity));
        }
    }
    shown.push(source);
}

/// Most of the Flyer's sound kept when nobody takes it: one second.
const SOUND_CAP: usize = 44_100;

#[derive(Default)]
struct Shared {
    /// TOASTMUX: input 3 shows channel A, input 4 channel B.
    routed: [bool; 2],
    channels: [Channel; 2],
    /// Stereo output at the Flyer's rate (735 samples a field), not yet
    /// taken by the Toaster.
    sound: Vec<(f32, f32)>,
    /// Peak of the Toaster's Program sound in its last frame (0-1), the
    /// level the Flyer's record meters show.
    line_peak: (f32, f32),
}

/// The shared state; see the module documentation.
#[derive(Default)]
pub struct FlyerLink {
    shared: Mutex<Shared>,
    decoder: Decoder,
}

impl std::fmt::Debug for FlyerLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlyerLink").finish_non_exhaustive()
    }
}

impl FlyerLink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- The Flyer's side ----

    /// TOASTMUX: whether inputs 3 and 4 show the Flyer's channels A and B.
    pub fn set_routing(&self, input3: bool, input4: bool) {
        self.lock().routed = [input3, input4];
    }

    /// The routing: whether inputs 3 and 4 show channels A and B.
    pub fn routing(&self) -> [bool; 2] {
        self.lock().routed
    }

    /// Channel `ch` (0 = A, 1 = B) shows `source` this field.
    pub fn show(&self, ch: usize, source: FieldSource) {
        let mut s = self.lock();
        let Some(channel) = s.channels.get_mut(ch) else {
            return;
        };
        push_shown(&mut channel.pending, source);
    }

    /// A channel will show field `n` of `clip` soon: decode it ahead.
    pub fn prefetch(&self, clip: &Arc<ClipFile>, n: u32) {
        self.decoder.prefetch(clip, n);
    }

    /// Sound will be wanted from colour frame `f` of `clip` soon: read it
    /// ahead.
    pub fn prefetch_sound(&self, clip: &Arc<ClipFile>, f: u32) {
        self.decoder.prefetch_sound(clip, f);
    }

    /// A field of the Flyer's sound, at its own rate.
    pub fn push_sound(&self, sound: &[(f32, f32)]) {
        let mut s = self.lock();
        s.sound.extend_from_slice(sound);
        if s.sound.len() > SOUND_CAP {
            let excess = s.sound.len() - SOUND_CAP;
            s.sound.drain(..excess);
        }
    }

    /// The peak level the Toaster last put out (for the record meters).
    pub fn line_peak(&self) -> (f32, f32) {
        self.lock().line_peak
    }

    // ---- The Toaster's side ----

    /// The Flyer channel on Toaster input `n` (0-based), if it is routed
    /// there: channel A on input 3, B on input 4.
    pub fn channel_on_input(&self, n: usize) -> Option<usize> {
        let ch = n.checked_sub(2).filter(|&c| c < 2)?;
        self.lock().routed[ch].then_some(ch)
    }

    /// Channel `ch`'s current picture into `out` (752 x 480), weaving in
    /// the fields it showed since the last call. [`Self::take_shown`] and
    /// [`Self::weave_shown`] are the same in two steps.
    pub fn picture(&self, ch: usize, out: &mut [u32]) {
        let shown = self.take_shown(ch);
        self.weave_shown(ch, &shown, out);
    }

    /// What channel `ch` showed since the last call, for
    /// [`Self::weave_shown`].
    pub fn take_shown(&self, ch: usize) -> Vec<FieldSource> {
        let mut s = self.lock();
        match s.channels.get_mut(ch) {
            Some(channel) => std::mem::take(&mut channel.pending),
            None => Vec::new(),
        }
    }

    /// Weave what channel `ch` showed (from [`Self::take_shown`], in the
    /// order taken) into its picture, and copy the picture into `out`.
    pub fn weave_shown(&self, ch: usize, shown: &[FieldSource], out: &mut [u32]) {
        if ch >= 2 {
            out.fill(BLACK);
            return;
        }
        // Every field queued first, so a worker can take one while this
        // thread decodes another.
        for source in shown {
            if let FieldSource::Clip(clip, n) = source {
                self.decoder.prefetch(clip, *n);
            }
        }
        let fields: Vec<Option<(u32, Arc<Decoded>)>> = shown
            .iter()
            .map(|source| match source {
                FieldSource::Clip(clip, n) => self.decoder.field(clip, *n).map(|f| (*n, f)),
                FieldSource::Matte(_) => None,
            })
            .collect();
        let mut s = self.lock();
        let channel = &mut s.channels[ch];
        for (source, field) in shown.iter().zip(fields) {
            match (source, field) {
                (FieldSource::Matte(rgba), _) => channel.picture.fill(*rgba),
                (_, Some((n, field))) => weave(&field, n, &mut channel.picture),
                // A clip field without a picture leaves the lines as they were.
                (_, None) => {}
            }
        }
        let len = out.len().min(channel.picture.len());
        out[..len].copy_from_slice(&channel.picture[..len]);
    }

    /// The Flyer's sound since the last call, at its own rate (735
    /// samples a field).
    pub fn take_sound(&self) -> Vec<(f32, f32)> {
        std::mem::take(&mut self.lock().sound)
    }

    /// Note the peak of the sound the Toaster just put out.
    pub fn set_line_peak(&self, peak: (f32, f32)) {
        self.lock().line_peak = peak;
    }

    /// Fields the decoder has done, and how many times the Toaster had to
    /// wait for one a worker had in hand (for tests and diagnostics).
    pub fn decode_stats(&self) -> (u64, u64) {
        self.decoder.stats()
    }

    /// Whether field `n` of `clip` is decoded, or asked for.
    #[cfg(test)]
    pub(crate) fn has_field(&self, clip: &ClipFile, n: u32) -> bool {
        self.decoder
            .pool
            .lock()
            .entries
            .contains_key(&(clip.id(), n))
    }
}

/// A decoded field: its lines (`PICTURE_H / 2` at most), RGBA, one after
/// another.
type Decoded = Vec<u32>;

/// Lines of a field that reach the picture.
const FIELD_LINES: usize = PICTURE_H / 2;

/// Decode field `n` of `clip` into colour lines; `None` for a field with
/// no picture (its lines are left as they were).
fn decode(clip: &ClipFile, n: u32) -> Option<Decoded> {
    let rows = clip.decode_field(n)?;
    let f = n % 4;
    let lines = rows.len().min(FIELD_LINES);
    let mut out = vec![BLACK; lines * PICTURE_W];
    for (y, samples) in rows.iter().take(lines).enumerate() {
        let positive = flyerclip::composite::phase_positive(f, y);
        crate::toaster::composite::decode_single(
            samples,
            positive,
            &mut out[y * PICTURE_W..(y + 1) * PICTURE_W],
        );
    }
    Some(out)
}

/// Put a decoded field (`n % 4` of its colour frame) on its lines of the
/// picture: field I/III on the even lines, II/IV on the odd.
fn weave(field: &[u32], n: u32, picture: &mut [u32]) {
    let parity = (n & 1) as usize;
    for (y, row) in field.chunks_exact(PICTURE_W).enumerate() {
        let line = 2 * y + parity;
        if line >= PICTURE_H {
            break;
        }
        picture[line * PICTURE_W..(line + 1) * PICTURE_W].copy_from_slice(row);
    }
}

// ---------------------------------------------------------------------------
// The decoder threads
// ---------------------------------------------------------------------------

/// Decoded fields kept: the read-ahead of both channels, a scrub's looping
/// colour frame, and the fields just shown.
const CACHE_FIELDS: usize = 48;

/// What a worker does.
enum Job {
    Field(Arc<ClipFile>, u32),
    Sound(Arc<ClipFile>, u32),
}

/// A field's place in the cache.
enum Entry {
    /// Waiting for a worker.
    Queued,
    /// Being decoded (by a worker, or by the thread that needed it).
    Decoding,
    /// Decoded (`None`: the field has no picture), with when it was last
    /// wanted.
    Ready(Option<Arc<Decoded>>, u64),
}

#[derive(Default)]
struct Cache {
    entries: HashMap<(u64, u32), Entry>,
    queue: VecDeque<Job>,
    /// Sound frames queued or read recently, so they are not queued twice.
    sound_seen: VecDeque<(u64, u32)>,
    tick: u64,
    workers: usize,
    stop: bool,
    decoded: u64,
    waited: u64,
}

#[derive(Default)]
struct Pool {
    cache: Mutex<Cache>,
    /// Signalled when a job is queued and when a field is ready.
    changed: Condvar,
}

/// The cache of decoded fields and the threads that fill it.
#[derive(Default)]
struct Decoder {
    pool: Arc<Pool>,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.pool.lock().stop = true;
        self.pool.changed.notify_all();
    }
}

impl Pool {
    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A worker: decode queued fields until the link goes away.
    fn work(&self) {
        loop {
            let job = {
                let mut c = self.lock();
                loop {
                    if c.stop {
                        return;
                    }
                    match c.queue.pop_front() {
                        // Skip a field the Toaster decoded itself meanwhile.
                        Some(Job::Field(clip, n)) => {
                            let key = (clip.id(), n);
                            if matches!(c.entries.get(&key), Some(Entry::Queued)) {
                                c.entries.insert(key, Entry::Decoding);
                                break Job::Field(clip, n);
                            }
                        }
                        Some(job) => break job,
                        None => {
                            c = self.changed.wait(c).unwrap_or_else(|e| e.into_inner());
                        }
                    }
                }
            };
            match job {
                Job::Field(clip, n) => {
                    let field = decode(&clip, n).map(Arc::new);
                    self.finish(&clip, n, field, false);
                }
                Job::Sound(clip, f) => {
                    clip.frame_audio(f);
                }
            }
        }
    }

    /// File a decoded field and wake whoever waits for it.
    fn finish(&self, clip: &ClipFile, n: u32, field: Option<Arc<Decoded>>, wanted: bool) {
        let mut c = self.lock();
        c.tick += 1;
        c.decoded += 1;
        let tick = c.tick;
        c.entries.insert(
            (clip.id(), n),
            Entry::Ready(field, if wanted { tick } else { 0 }),
        );
        evict(&mut c);
        drop(c);
        self.changed.notify_all();
    }
}

/// Drop the least recently wanted decoded fields beyond the cache size.
/// Fields decoded ahead and not yet wanted go last: they are about to be.
fn evict(c: &mut Cache) {
    let ready = c
        .entries
        .values()
        .filter(|e| matches!(e, Entry::Ready(..)))
        .count();
    if ready <= CACHE_FIELDS {
        return;
    }
    let mut old: Vec<((u64, u32), u64)> = c
        .entries
        .iter()
        .filter_map(|(k, e)| match e {
            Entry::Ready(_, used) => Some((*k, *used)),
            _ => None,
        })
        .collect();
    // Never wanted yet (0) sorts as newest.
    old.sort_by_key(|&(_, used)| if used == 0 { u64::MAX } else { used });
    for (key, _) in old.into_iter().take(ready - CACHE_FIELDS) {
        c.entries.remove(&key);
    }
}

impl Decoder {
    /// Worker threads: one fewer than the host's cores, two to four.
    fn workers() -> usize {
        std::thread::available_parallelism()
            .map_or(2, |n| n.get().saturating_sub(1))
            .clamp(2, 4)
    }

    /// Start the workers on first use.
    fn ensure_workers(&self, c: &mut Cache) {
        #[cfg(not(target_arch = "wasm32"))]
        while c.workers < Self::workers() {
            let pool = Arc::clone(&self.pool);
            let started = std::thread::Builder::new()
                .name(format!("flyer-decode-{}", c.workers + 1))
                .spawn(move || pool.work());
            if started.is_err() {
                break;
            }
            c.workers += 1;
        }
        #[cfg(target_arch = "wasm32")]
        let _ = c;
    }

    fn prefetch(&self, clip: &Arc<ClipFile>, n: u32) {
        let mut c = self.pool.lock();
        self.ensure_workers(&mut c);
        if c.workers == 0 {
            return;
        }
        let key = (clip.id(), n);
        if c.entries.contains_key(&key) {
            return;
        }
        c.entries.insert(key, Entry::Queued);
        c.queue.push_back(Job::Field(Arc::clone(clip), n));
        drop(c);
        self.pool.changed.notify_all();
    }

    fn prefetch_sound(&self, clip: &Arc<ClipFile>, f: u32) {
        let mut c = self.pool.lock();
        self.ensure_workers(&mut c);
        if c.workers == 0 {
            return;
        }
        let key = (clip.id(), f);
        if c.sound_seen.contains(&key) {
            return;
        }
        if c.sound_seen.len() >= 32 {
            c.sound_seen.pop_front();
        }
        c.sound_seen.push_back(key);
        c.queue.push_back(Job::Sound(Arc::clone(clip), f));
        drop(c);
        self.pool.changed.notify_all();
    }

    /// Field `n` of `clip`, decoded: from the cache, waiting for a worker
    /// that has it in hand, or decoded here.
    fn field(&self, clip: &Arc<ClipFile>, n: u32) -> Option<Arc<Decoded>> {
        let key = (clip.id(), n);
        let mut c = self.pool.lock();
        loop {
            c.tick += 1;
            let tick = c.tick;
            match c.entries.get_mut(&key) {
                Some(Entry::Ready(field, used)) => {
                    *used = tick;
                    return field.clone();
                }
                Some(Entry::Decoding) => {
                    c.waited += 1;
                    c = self.pool.changed.wait(c).unwrap_or_else(|e| e.into_inner());
                }
                // Queued but not started, or never asked for: do it here.
                Some(Entry::Queued) | None => {
                    c.entries.insert(key, Entry::Decoding);
                    drop(c);
                    let field = decode(clip, n).map(Arc::new);
                    self.pool.finish(clip, n, field.clone(), true);
                    return field;
                }
            }
        }
    }

    fn stats(&self) -> (u64, u64) {
        let c = self.pool.lock();
        (c.decoded, c.waited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_3_and_4_carry_channels_a_and_b_when_routed() {
        let link = FlyerLink::new();
        assert_eq!(link.channel_on_input(2), None);
        link.set_routing(true, false);
        assert_eq!(link.channel_on_input(2), Some(0));
        assert_eq!(link.channel_on_input(3), None);
        assert_eq!(link.channel_on_input(0), None);
        link.set_routing(false, true);
        assert_eq!(link.channel_on_input(3), Some(1));
    }

    #[test]
    fn a_clip_field_is_decoded_onto_its_lines_when_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Clip");
        super::super::clip::tests::write_test_clip(&path, 2);
        let clip = Arc::new(ClipFile::open(&path).unwrap());
        let link = FlyerLink::new();
        let white = u32::from_le_bytes([255, 255, 255, 255]);
        link.show(0, FieldSource::Matte(white));
        let mut out = vec![0u32; PICTURE_W * PICTURE_H];
        link.picture(0, &mut out);
        assert!(out.iter().all(|&p| p == white));
        // Field 4 (field I) replaces the even lines only.
        link.show(0, FieldSource::Clip(Arc::clone(&clip), 4));
        link.picture(0, &mut out);
        let grey = |p: u32| p.to_le_bytes()[1];
        assert!(grey(out[100 * PICTURE_W + 300]) < 200);
        assert_eq!(out[101 * PICTURE_W + 300], white);
        // Then field 5 the odd ones; channel B is untouched.
        link.show(0, FieldSource::Clip(clip, 5));
        link.picture(0, &mut out);
        assert!(grey(out[101 * PICTURE_W + 300]) < 200);
        link.picture(1, &mut out);
        assert!(out.iter().all(|&p| p == BLACK));
    }

    #[test]
    fn fields_read_ahead_give_the_same_picture_as_fields_decoded_when_shown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Clip");
        super::super::clip::tests::write_test_clip(&path, 3);
        let clip = Arc::new(ClipFile::open(&path).unwrap());
        let ahead = FlyerLink::new();
        let late = FlyerLink::new();
        for n in 0..12 {
            ahead.prefetch(&clip, n);
        }
        let mut a = vec![0u32; PICTURE_W * PICTURE_H];
        let mut b = vec![0u32; PICTURE_W * PICTURE_H];
        for n in 0..12 {
            ahead.show(1, FieldSource::Clip(Arc::clone(&clip), n));
            late.show(1, FieldSource::Clip(Arc::clone(&clip), n));
            if n % 2 == 1 {
                ahead.picture(1, &mut a);
                late.picture(1, &mut b);
                assert_eq!(a, b, "field {n}");
            }
        }
        // Each field was decoded once, by whichever thread got to it.
        assert_eq!(ahead.decode_stats().0, 12);
        assert_eq!(late.decode_stats().0, 12);
        // A field without a picture leaves the lines as they were.
        ahead.show(1, FieldSource::Clip(Arc::clone(&clip), 99));
        ahead.picture(1, &mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn the_cache_keeps_the_newest_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Clip");
        super::super::clip::tests::write_test_clip(&path, 16);
        let clip = Arc::new(ClipFile::open(&path).unwrap());
        let link = FlyerLink::new();
        for n in 0..64 {
            assert!(link.decoder.field(&clip, n).is_some());
        }
        let c = link.decoder.pool.lock();
        assert_eq!(c.entries.len(), CACHE_FIELDS);
        assert!(c.entries.contains_key(&(clip.id(), 63)));
        assert!(!c.entries.contains_key(&(clip.id(), 0)));
    }

    #[test]
    fn what_a_channel_showed_keeps_only_what_the_weave_needs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Clip");
        super::super::clip::tests::write_test_clip(&path, 2);
        let clip = Arc::new(ClipFile::open(&path).unwrap());
        let link = FlyerLink::new();
        let white = u32::from_le_bytes([255, 255, 255, 255]);
        // Two fields of each parity: the latest of each stays, in order.
        for n in [4, 5, 6, 7] {
            link.show(0, FieldSource::Clip(Arc::clone(&clip), n));
        }
        let shown = link.take_shown(0);
        let fields: Vec<u32> = shown
            .iter()
            .filter_map(|s| match s {
                FieldSource::Clip(_, n) => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(fields, vec![6, 7]);
        // A matte wipes what came before it; a field after it is kept.
        link.show(0, FieldSource::Clip(Arc::clone(&clip), 4));
        link.show(0, FieldSource::Matte(white));
        link.show(0, FieldSource::Clip(Arc::clone(&clip), 5));
        let shown = link.take_shown(0);
        assert_eq!(shown.len(), 2);
        assert!(matches!(shown[0], FieldSource::Matte(c) if c == white));
        assert!(matches!(shown[1], FieldSource::Clip(_, 5)));
        // Woven in that order: white with field 5 on the odd lines, and
        // the same picture whether woven in one step or two.
        let mut a = vec![0u32; PICTURE_W * PICTURE_H];
        link.weave_shown(0, &shown, &mut a);
        assert_eq!(a[100 * PICTURE_W + 300], white);
        let grey = |p: u32| p.to_le_bytes()[1];
        assert!(grey(a[101 * PICTURE_W + 300]) < 200);
        let other = FlyerLink::new();
        other.show(0, FieldSource::Clip(Arc::clone(&clip), 4));
        other.show(0, FieldSource::Matte(white));
        other.show(0, FieldSource::Clip(clip, 5));
        let mut b = vec![0u32; PICTURE_W * PICTURE_H];
        other.picture(0, &mut b);
        assert_eq!(a, b);
        assert!(link.take_shown(0).is_empty());
    }

    #[test]
    fn sound_is_kept_until_taken_and_bounded() {
        let link = FlyerLink::new();
        link.push_sound(&[(0.5, -0.5); 735]);
        link.push_sound(&[(0.25, 0.0); 735]);
        let s = link.take_sound();
        assert_eq!(s.len(), 1470);
        assert_eq!(s[0], (0.5, -0.5));
        assert!(link.take_sound().is_empty());
        for _ in 0..100 {
            link.push_sound(&[(0.1, 0.1); 735]);
        }
        assert_eq!(link.take_sound().len(), SOUND_CAP);
    }
}
