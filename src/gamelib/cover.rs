// SPDX-License-Identifier: GPL-3.0-or-later

//! Cover art, read from the cache a scan filled.
//!
//! Nothing here touches the network. A scan downloads the art for the
//! games in the library -- only for those, since a full catalogue is a few
//! thousand games and almost all of that would be artwork for games nobody
//! has -- and this reads what it left behind. Putting a download in the
//! middle of scrolling a list is how a list becomes slow, and it would
//! also mean the page behaved differently online and off.
//!
//! The reading happens on a worker thread, so nothing here blocks the
//! launcher: [`Covers::want_around`] says which digests are wanted, and
//! the pictures appear on a later frame.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex};

/// A decoded cover, ready to draw.
#[derive(Debug, Clone)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    /// Straight RGBA, as the panel draws.
    pub pixels: Vec<u8>,
}

/// What became of one request, so a game with no art is not asked about
/// again every frame.
#[derive(Debug)]
enum Held {
    /// On its way.
    Wanted,
    /// Here.
    Have(Image),
    /// Asked, and there is none -- or it could not be read. Remembered so
    /// the request is made once rather than on every redraw.
    None,
}

/// The covers this session has asked for.
///
/// One worker serves them all, newest request first. A thread per request
/// looked simpler until the arrow keys started repeating: holding one
/// through a long list asks for a hundred covers in a second, and a
/// hundred threads racing for the network is slower at delivering the one
/// you stopped on than a single worker that serves it first.
pub struct Covers {
    held: HashMap<String, Held>,
    /// Insertion order, for capping how much decoded art is kept.
    order: std::collections::VecDeque<String>,
    dir: PathBuf,
    queue: Arc<Queue>,
    rx: Receiver<(String, Option<Image>)>,
}

/// How many decoded covers are kept. Only one is ever drawn; the rest are
/// there so walking back up a list is instant. At a quarter of a megabyte
/// each this is a few tens of megabytes, which is the right order for a
/// convenience.
const KEEP: usize = 96;

/// How far either side of the selection to fetch ahead. Enough that a
/// steady scroll finds the next one already decoded, small enough that a
/// fast one does not queue work nobody will look at.
const AHEAD: usize = 3;

/// How many requests the queue holds. Exactly what one [`Covers::want_around`]
/// asks for -- the selection and `AHEAD` either side -- and no more: a list
/// scrolled past leaves a trail of requests nobody is waiting for, and
/// serving that trail is time taken from what is on screen now.
const QUEUE_DEPTH: usize = 2 * AHEAD + 1;

/// The work waiting, newest first, and whether anyone is still asking.
struct Queue {
    pending: Mutex<Pending>,
    woken: Condvar,
}

#[derive(Default)]
struct Pending {
    wanted: Vec<String>,
    /// Cleared when the [`Covers`] that owns this goes away. The worker
    /// spends its life parked on the condvar waiting for work, so without
    /// something to wake it and tell it to stop it would park there for
    /// the life of the process.
    open: bool,
}

impl Queue {
    /// Put one at the front of the queue, which is the back of the vector:
    /// the worker pops from there, so the most recent ask is served first.
    ///
    /// The queue is bounded, so a fast scroll pushes older asks off the
    /// back. Those are handed back rather than dropped: the caller marked
    /// them as on their way, and something that is never coming has to stop
    /// being marked so, or it can never be asked for again and the window
    /// loop stays awake waiting for it.
    fn push(&self, sha1: String) -> Vec<String> {
        let Ok(mut pending) = self.pending.lock() else {
            return vec![sha1];
        };
        if !pending.open {
            return vec![sha1];
        }
        pending.wanted.retain(|held| held != &sha1);
        pending.wanted.push(sha1);
        let over = pending.wanted.len().saturating_sub(QUEUE_DEPTH);
        let dropped = pending.wanted.drain(..over).collect();
        self.woken.notify_one();
        dropped
    }

    /// Wait for work and take the newest. `None` once nobody is waiting for
    /// an answer, which is the worker's cue to stop.
    fn take(&self) -> Option<String> {
        let mut pending = self.pending.lock().ok()?;
        loop {
            if !pending.open {
                return None;
            }
            if let Some(next) = pending.wanted.pop() {
                return Some(next);
            }
            pending = self.woken.wait(pending).ok()?;
        }
    }

    /// Say that nothing more will be asked for, and wake the worker to
    /// notice.
    fn close(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.open = false;
            pending.wanted.clear();
        }
        self.woken.notify_all();
    }
}

impl std::fmt::Debug for Covers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Covers")
            .field("held", &self.held.len())
            .field("dir", &self.dir)
            .finish()
    }
}

impl Clone for Covers {
    /// A fresh set sharing nothing: the queue and the channel belong to the
    /// requests in flight, and a clone has none.
    fn clone(&self) -> Self {
        Covers::new(self.dir.clone())
    }
}

impl Default for Covers {
    fn default() -> Self {
        Covers::new(PathBuf::new())
    }
}

impl Drop for Covers {
    /// Let the worker go. It is parked waiting for a request that will
    /// never come now, and a launcher opened and closed a few times should
    /// not leave a thread behind each time.
    fn drop(&mut self) {
        self.queue.close();
    }
}

impl Covers {
    pub fn new(dir: PathBuf) -> Covers {
        let (tx, rx) = std::sync::mpsc::channel();
        let queue = Arc::new(Queue {
            pending: Mutex::new(Pending {
                wanted: Vec::new(),
                open: true,
            }),
            woken: Condvar::new(),
        });
        let work = Arc::clone(&queue);
        let at = dir.clone();
        std::thread::spawn(move || {
            while let Some(sha1) = work.take() {
                let image = read_cached(&at, &sha1);
                // The page has gone: nothing is waiting for this and
                // nothing will wait for the next one either.
                if tx.send((sha1, image)).is_err() {
                    return;
                }
            }
        });
        Covers {
            held: HashMap::new(),
            order: std::collections::VecDeque::new(),
            dir,
            queue,
            rx,
        }
    }

    /// Collect anything that has arrived since the last frame, answering
    /// whether the page should be redrawn.
    pub fn poll(&mut self) -> bool {
        let mut arrived = false;
        while let Ok((sha1, image)) = self.rx.try_recv() {
            let held = match image {
                Some(image) => Held::Have(image),
                None => Held::None,
            };
            if self.held.insert(sha1.clone(), held).is_none() {
                self.order.push_back(sha1);
            }
            arrived = true;
        }
        // Only decoded pictures are worth capping; a remembered "there is
        // none" costs a hash entry.
        while self.order.len() > KEEP {
            if let Some(oldest) = self.order.pop_front() {
                self.held.remove(&oldest);
            }
        }
        arrived
    }

    /// Forget what was asked for, so a game that had no art last time is
    /// looked for again. A scan has just filled the cache; a "there is
    /// none" remembered from before it would outlast the answer.
    pub fn forget(&mut self) {
        self.held.clear();
        self.order.clear();
    }

    /// Forget one, so a picture that has just been replaced is read again
    /// rather than answered from what it replaced.
    pub fn forget_one(&mut self, key: &str) {
        self.held.remove(key);
        self.order.retain(|held| held != key);
    }

    /// Forget only the ones that came back empty.
    ///
    /// For while a scan is running: art it has just written should be
    /// picked up, but a picture already decoded should not be thrown away
    /// and fetched again.
    pub fn forget_missing(&mut self) {
        self.held.retain(|_, held| !matches!(held, Held::None));
        self.order.retain(|sha1| self.held.contains_key(sha1));
    }

    /// Whether anything is still on its way. The window loop stays awake
    /// while it is: a picture that lands with nothing watching for it sits
    /// there unseen until some other event happens to wake the loop, which
    /// is what "it appears the second time you scroll past" was.
    /// Whether this digest is still on its way.
    #[cfg(test)]
    pub fn is_wanted(&self, sha1: &str) -> bool {
        matches!(self.held.get(sha1), Some(Held::Wanted))
    }

    pub fn pending(&self) -> bool {
        self.held.values().any(|held| matches!(held, Held::Wanted))
    }

    /// The art for a digest if it is here, without asking for it. For the
    /// draw path, which has no say in what gets fetched.
    pub fn get(&self, sha1: &str) -> Option<&Image> {
        match self.held.get(sha1) {
            Some(Held::Have(image)) => Some(image),
            _ => None,
        }
    }

    /// Ask for a digest, if this is the first time it has been asked for.
    pub fn want(&mut self, sha1: &str) {
        if self.held.contains_key(sha1) {
            return;
        }
        self.held.insert(sha1.to_string(), Held::Wanted);
        for dropped in self.queue.push(sha1.to_string()) {
            // Pushed off the back of the queue: forget it was asked for, so
            // scrolling back to it asks again.
            self.held.remove(&dropped);
        }
    }

    /// Ask for what is being looked at, and for its neighbours after it.
    ///
    /// The one in the middle goes in last so it is served first. Reading
    /// ahead is what makes a steady scroll find each cover already
    /// decoded rather than waiting for it at every step.
    pub fn want_around<'a>(&mut self, around: impl Iterator<Item = Option<&'a str>>, at: usize) {
        let window: Vec<Option<&str>> = around.collect();
        let first = at.saturating_sub(AHEAD);
        let last = (at + AHEAD).min(window.len().saturating_sub(1));
        for i in (first..=last).rev() {
            if i != at {
                if let Some(Some(sha1)) = window.get(i) {
                    self.want(sha1);
                }
            }
        }
        if let Some(Some(sha1)) = window.get(at) {
            self.want(sha1);
        }
    }
}

/// Where a digest's picture is kept inside the covers directory.
///
/// The one place the file name is spelt, so the scan that writes it and
/// the launcher that reads it cannot drift apart -- which they did, and
/// the symptom was a library of downloaded art that never appeared.
pub fn cover_file(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.png"))
}

/// One cover, read from the cache.
///
/// The cache and nothing else: downloading is the scan's job, and a page
/// that fetched what it was missing would put the network in the middle of
/// scrolling a list. After a scan every matched game's art is here, so this
/// is a file read and a decode -- a millisecond or two, which is why it can
/// keep up with a held arrow key.
fn read_cached(dir: &Path, sha1: &str) -> Option<Image> {
    let png = std::fs::read(cover_file(dir, sha1)).ok()?;
    decode(&png)
}

/// Take somebody's own picture and make it look like the rest.
///
/// The service scales what it serves so the shorter side is
/// [`super::openretro::COVER_PIXELS`]; measured across a real cache that
/// is 256 wide by a median 314 tall, a ratio of about 0.82. A picture
/// chosen by hand is brought to the same bound so the cache holds one kind
/// of thing -- a phone photograph of a box would otherwise sit in there at
/// several megabytes and be scaled down on every single frame that drew
/// it.
///
/// Only ever smaller. Blowing a small picture up would spend bytes to add
/// nothing, and the page scales to its frame at draw time anyway.
///
/// Answers the re-encoded PNG, or `None` if the bytes were not a picture.
pub fn normalise(png: &[u8]) -> Option<Vec<u8>> {
    let image = decode(png)?;
    let short = image.width.min(image.height);
    let bound = super::openretro::COVER_PIXELS as usize;
    if short <= bound {
        // Already the right sort of size. Re-encoded anyway rather than
        // copied, so what lands in the cache is always plain 8-bit RGBA
        // whatever the file happened to be.
        return encode(&image);
    }
    let (w, h) = (
        (image.width * bound / short).max(1),
        (image.height * bound / short).max(1),
    );
    encode(&resample(&image, w, h))
}

/// Scale by averaging the source pixels each destination pixel covers.
///
/// A box filter, not the nearest-neighbour the draw path uses: this runs
/// once and is kept, so it is worth the pass. Picking single pixels out of
/// a photograph shrunk by a factor of eight is how a cover ends up looking
/// like it was drawn with a dying pen.
fn resample(src: &Image, w: usize, h: usize) -> Image {
    let mut pixels = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let (y0, y1) = (
            y * src.height / h,
            ((y + 1) * src.height / h).max(y * src.height / h + 1),
        );
        for x in 0..w {
            let (x0, x1) = (
                x * src.width / w,
                ((x + 1) * src.width / w).max(x * src.width / w + 1),
            );
            let mut sum = [0u32; 4];
            let mut n = 0u32;
            for sy in y0..y1.min(src.height) {
                for sx in x0..x1.min(src.width) {
                    let at = (sy * src.width + sx) * 4;
                    let Some(px) = src.pixels.get(at..at + 4) else {
                        continue;
                    };
                    for (slot, &v) in sum.iter_mut().zip(px) {
                        *slot += u32::from(v);
                    }
                    n += 1;
                }
            }
            let n = n.max(1);
            pixels.extend(sum.iter().map(|c| (c / n) as u8));
        }
    }
    Image {
        width: w,
        height: h,
        pixels,
    }
}

/// An [`Image`] back as PNG bytes.
fn encode(image: &Image) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width as u32, image.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&image.pixels).ok()?;
    }
    Some(out)
}

/// Whether these bytes are a PNG that decodes.
///
/// Both halves matter: the signature says somebody meant it to be a PNG,
/// and the decode says it is one. A file that only passes the first would
/// go into the cache and draw as an empty box for ever after.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n") && decode(bytes).is_some()
}

/// A PNG as RGBA, whatever it was encoded as.
///
/// The decoder is asked to do the awkward parts: `EXPAND` turns a palette
/// into real colours and a sub-byte greyscale into bytes, and `STRIP_16`
/// brings sixteen-bit samples down to eight. Without them a palette image
/// could not be drawn at all and a sixteen-bit one would be read as though
/// every other byte were a pixel, which is a picture of noise rather than
/// a picture. What arrives here is then one of four shapes, all 8-bit.
fn decode(png: &[u8]) -> Option<Image> {
    let mut decoder = png::Decoder::new(png);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    // The buffer belongs to the frame that was read, which may be smaller
    // than the header claimed; taking a slice of it on trust is how a
    // truncated file becomes a panic.
    let take = |n: usize| -> Option<&[u8]> { buf.get(..w.checked_mul(h)?.checked_mul(n)?) };
    let pixels = match info.color_type {
        png::ColorType::Rgba => take(4)?.to_vec(),
        png::ColorType::Rgb => take(3)?
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 0xFF])
            .collect(),
        png::ColorType::Grayscale => take(1)?.iter().flat_map(|&v| [v, v, v, 0xFF]).collect(),
        png::ColorType::GrayscaleAlpha => take(2)?
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        // EXPAND turned any palette into one of the above; nothing else
        // reaches here.
        png::ColorType::Indexed => return None,
    };
    Some(Image {
        width: w,
        height: h,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cover pushed off the bounded queue is handed back, so the caller
    /// can stop counting it as on its way.
    ///
    /// The queue holds a fixed number and scrolling fast asks for more than
    /// that, so the oldest asks are dropped. Dropped silently, they would
    /// stay marked as wanted for ever: the picture could never be asked for
    /// again -- scrolling back to it would show an empty frame -- and
    /// `pending` would stay true, holding the window loop awake with
    /// nothing to wait for.
    ///
    /// Tested against the queue rather than through `Covers`, whose worker
    /// takes entries off concurrently.
    #[test]
    fn a_full_queue_hands_back_what_it_pushed_off() {
        let queue = Queue {
            pending: Mutex::new(Pending {
                wanted: Vec::new(),
                open: true,
            }),
            woken: Condvar::new(),
        };
        // Fill it exactly: nothing has been pushed off yet.
        for i in 0..QUEUE_DEPTH {
            assert!(
                queue.push(format!("sha{i}")).is_empty(),
                "dropped something while there was still room"
            );
        }
        // One more, and the oldest comes back.
        assert_eq!(queue.push("newest".to_string()), vec!["sha0".to_string()]);
        assert_eq!(queue.push("newer".to_string()), vec!["sha1".to_string()]);

        // Asking again for something already queued moves it rather than
        // growing the queue, so nothing is pushed off for it.
        assert!(queue.push("newest".to_string()).is_empty());

        // A closed queue takes nothing, and says so by handing it straight
        // back -- otherwise it would be marked wanted and never arrive.
        queue.close();
        assert_eq!(queue.push("late".to_string()), vec!["late".to_string()]);
    }

    fn encode(w: u32, h: u32, colour: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, w, h);
            encoder.set_color(colour);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(data).unwrap();
        }
        out
    }

    #[test]
    fn a_cover_decodes_whatever_it_was_encoded_as() {
        // The service sends RGB; the widening is what makes anything else
        // draw rather than vanish.
        let rgb = encode(2, 1, png::ColorType::Rgb, &[1, 2, 3, 4, 5, 6]);
        let image = decode(&rgb).expect("rgb decodes");
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixels, [1, 2, 3, 255, 4, 5, 6, 255]);

        let rgba = encode(1, 1, png::ColorType::Rgba, &[9, 8, 7, 128]);
        assert_eq!(decode(&rgba).expect("rgba decodes").pixels, [9, 8, 7, 128]);

        let grey = encode(2, 1, png::ColorType::Grayscale, &[40, 200]);
        assert_eq!(
            decode(&grey).expect("grey decodes").pixels,
            [40, 40, 40, 255, 200, 200, 200, 255]
        );

        // A palette image, which somebody's own cover art may well be: the
        // decoder expands it rather than the picture being refused.
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, 2, 1);
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_palette(vec![1, 2, 3, 9, 8, 7]);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0, 1]).unwrap();
        }
        assert_eq!(
            decode(&out).expect("a palette image decodes").pixels,
            [1, 2, 3, 255, 9, 8, 7, 255]
        );
    }

    #[test]
    fn a_chosen_picture_is_brought_to_the_size_the_rest_are() {
        // The service bounds the shorter side to 256; a picture somebody
        // chose is bounded the same way so the cache holds one kind of
        // thing.
        let big = super::encode(&Image {
            width: 1000,
            height: 1250,
            pixels: vec![128; 1000 * 1250 * 4],
        })
        .unwrap();
        let out = decode(&normalise(&big).expect("normalises")).unwrap();
        assert_eq!(out.width.min(out.height), 256);
        assert_eq!((out.width, out.height), (256, 320), "the shape changed");
        // The colour survived the averaging.
        assert!(out.pixels.iter().all(|&v| v == 128));

        // A landscape one is bounded on its height instead.
        let wide = super::encode(&Image {
            width: 1000,
            height: 500,
            pixels: vec![7; 1000 * 500 * 4],
        })
        .unwrap();
        let out = decode(&normalise(&wide).expect("normalises")).unwrap();
        assert_eq!((out.width, out.height), (512, 256));

        // One already small enough keeps its size rather than being blown
        // up to look worse.
        let small = super::encode(&Image {
            width: 64,
            height: 80,
            pixels: vec![9; 64 * 80 * 4],
        })
        .unwrap();
        let out = decode(&normalise(&small).expect("normalises")).unwrap();
        assert_eq!((out.width, out.height), (64, 80));

        assert!(normalise(b"not a picture").is_none());
    }

    #[test]
    fn something_that_is_not_a_png_is_not_art() {
        // An error page served with a 200, or a download that stopped
        // half way, must not be kept as though it were a picture.
        assert!(decode(b"<html>not found</html>").is_none());
        assert!(decode(&[]).is_none());
        let png = encode(1, 1, png::ColorType::Rgb, &[1, 2, 3]);
        assert!(decode(&png[..png.len() / 2]).is_none());
    }

    #[test]
    fn a_dropped_set_lets_its_worker_go() {
        // The worker spends its life parked waiting for a request. Without
        // something to wake it and say there will not be another, every
        // launcher opened would leave a thread behind for the life of the
        // process.
        let queue = Arc::new(Queue {
            pending: Mutex::new(Pending {
                wanted: Vec::new(),
                open: true,
            }),
            woken: Condvar::new(),
        });
        let work = Arc::clone(&queue);
        let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&ended);
        let worker = std::thread::spawn(move || {
            while work.take().is_some() {}
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        // Parked, because nothing has been asked for.
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!ended.load(std::sync::atomic::Ordering::SeqCst));
        queue.close();
        worker.join().expect("the worker ended");
        assert!(ended.load(std::sync::atomic::Ordering::SeqCst));
        // And a closed queue takes nothing further.
        queue.push("abc".to_string());
        assert!(queue.take().is_none());
    }

    #[test]
    fn a_game_with_no_art_is_asked_about_once() {
        // Otherwise every redraw starts another request for a picture that
        // is not there.
        let mut covers = Covers::new(std::env::temp_dir().join("copperline-covers-test"));
        covers.want("abc");
        assert!(covers.get("abc").is_none());
        // Answer it as the worker would, with nothing.
        covers.held.insert("abc".to_string(), Held::None);
        covers.want("abc");
        assert!(
            matches!(covers.held.get("abc"), Some(Held::None)),
            "asking again put it back to wanted"
        );
    }
}
