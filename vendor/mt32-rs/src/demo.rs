// SPDX-License-Identifier: LGPL-2.1-or-later

//! The demonstration songs the second-generation control ROMs carry, and
//! playing them.
//!
//! A real MT-32 plays these itself: its microcontroller runs the code in
//! the control ROM, and that code reads the songs out of the same ROM and
//! works the synthesis chip with them. This crate re-implements what that
//! code *computes* rather than running it, so the part the missing
//! firmware would do is done here: read the songs out of the image, and
//! play them as if a sequencer were sending them down the cable --
//! [Player::advance] emits the bytes due, and the MIDI front door
//! ([crate::midi]) takes them from there.
//!
//! Carried over from the player Copperline grew beside the reference
//! engine; the layout facts are read the way Munt's own demo player reads
//! them (`mt32emu_qt/src/DemoPlayer.cpp`).
//!
//! ## What is in the image
//!
//! Only the second-generation ROMs carry songs, in the upper half the
//! engine itself never reads, one after another:
//!
//! ```text
//!   +0x000  fourteen characters, the title, padded with spaces
//!   +0x010  how long a tick is, counted on the unit's own timer
//!   +0x012  the settings it opens with: reverb, rhythm kit, timbres
//!   +0x120  the performance
//! ```
//!
//! The performance is MIDI with running status, every message followed by
//! one byte saying how many ticks to wait before the next. `FF` ends it.

use crate::rom;
use crate::sysex::dt1;

/// Where the songs and the settings they want sit in the image.
const SONG_REFS: usize = 0x86E0;
const SONG_COUNT: usize = 5;
const TIMBRE_ADDRESSES: usize = 0x8700;
const TIMBRE_COUNT: usize = 22;
const PATCH_TABLE_LOW: usize = 0x8800;
const PATCH_TABLE_HIGH: usize = 0x8900;

/// Inside one song.
const TITLE: usize = 14;
const TICK: usize = 0x10;
const REVERB: usize = 0x12;
/// Reverb, then how many partials each of the nine parts is promised.
const REVERB_LEN: usize = 3 + 9;
const RHYTHM: usize = 0x20;
const RHYTHM_LEN: usize = 256;
const STREAM: usize = 0x120;

/// The waits are counted by a timer running at this, divided by the
/// figure each song carries -- 6250 on every one of them, eighty a
/// second.
const TIMER_HZ: f32 = 500_000.0;

/// Where the settings go, as the unit addresses its own memory.
const ADDR_RHYTHM: u32 = 0x03_0110;
const ADDR_PATCH_LOW: u32 = 0x05_0000;
const ADDR_PATCH_HIGH: u32 = 0x05_0200;
const ADDR_TIMBRE: u32 = 0x08_0000;
const ADDR_REVERB: u32 = 0x10_0001;
const ADDR_RESET: u32 = 0x7F_0000;

/// A timbre, and the patches that name them.
const TIMBRE_COMMON: usize = 14;
const TIMBRE_MUTE: usize = 12;
const TIMBRE_PARTIAL: usize = 58;
const PATCH_SIZE: usize = 8;
const PATCH_BLOCK: usize = 32 * PATCH_SIZE;

/// Ends the performance, where a wait would go.
const END: u8 = 0xFF;
/// Ends it where a message would: the sequencer stopping.
const STOP: u8 = 0xFC;
/// A wait of this is the unit's own timing byte, not a message.
const SYNC: u8 = 0xF8;

/// One message and the wait before it, in ticks.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Event {
    wait: u8,
    message: Vec<u8>,
}

/// A demonstration song, read out of a control ROM.
#[derive(Debug, Clone)]
pub struct Song {
    /// What the unit shows while it plays, as the ROM spells it.
    pub title: String,
    /// The settings it wants before a note is played: its reverb and
    /// partial reserve, its rhythm kit, the timbres it was written with
    /// and the patches that name them. Without these the songs that lean
    /// on their own timbres play in whatever the unit was left in.
    setup: Vec<u8>,
    events: Vec<Event>,
    seconds_per_tick: f32,
}

impl Song {
    /// How long it runs, in seconds. Only the check against a real image
    /// asks: the unit itself just plays until the song runs out.
    #[cfg(test)]
    fn seconds(&self) -> f32 {
        let ticks: u32 = self.events.iter().map(|e| u32::from(e.wait)).sum();
        ticks as f32 * self.seconds_per_tick
    }
}

/// Every song a control ROM carries, in the order the ROM lists them.
/// Empty for the ROMs that have none: only the second generation came
/// with a demonstration to play.
pub fn songs(image: &[u8]) -> Vec<Song> {
    if rom::identify(image).and_then(|info| info.family) != Some(rom::Family::Mt32Gen2) {
        return Vec::new();
    }
    (0..SONG_COUNT)
        .map(|n| {
            // The ROM keeps a table of where they are, which is also what
            // puts them in the order the unit numbers them.
            let at = 2 * usize::from(u16le(image, SONG_REFS + 2 * n)?) + 0x8000;
            read_song(image, image.get(at..)?)
        })
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}

fn u16le(image: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*image.get(at)?, *image.get(at + 1)?]))
}

fn read_song(image: &[u8], song: &[u8]) -> Option<Song> {
    let title = String::from_utf8_lossy(song.get(..TITLE)?)
        .trim()
        .to_string();
    let seconds_per_tick = f32::from(u16le(song, TICK)?) / TIMER_HZ;

    // Everything the unit is told before the first note.
    let mut setup = dt1(ADDR_RESET, &[]);
    setup.extend(dt1(ADDR_REVERB, song.get(REVERB..REVERB + REVERB_LEN)?));
    setup.extend(dt1(ADDR_RHYTHM, song.get(RHYTHM..RHYTHM + RHYTHM_LEN)?));
    for n in 0..TIMBRE_COUNT {
        setup.extend(dt1(ADDR_TIMBRE + 0x200 * n as u32, &timbre(image, n)?));
    }
    for (address, table) in [
        (ADDR_PATCH_LOW, PATCH_TABLE_LOW),
        (ADDR_PATCH_HIGH, PATCH_TABLE_HIGH),
    ] {
        let mut patches = image.get(table..table + PATCH_BLOCK)?.to_vec();
        // Some patches name timbre group 5, which the unit has not got;
        // they mean the ones just loaded into its memory.
        for patch in patches.chunks_mut(PATCH_SIZE) {
            if patch[0] == 5 {
                patch[0] = 2;
            }
        }
        setup.extend(dt1(address, &patches));
    }

    Some(Song {
        title,
        setup,
        events: read_stream(song.get(STREAM..)?),
        seconds_per_tick,
    })
}

/// One timbre out of the ROM: its common parameters, then four partials,
/// skipping over the ones it has muted.
fn timbre(image: &[u8], n: usize) -> Option<Vec<u8>> {
    let at = usize::from(u16le(image, TIMBRE_ADDRESSES + 2 * n)?);
    let mute = *image.get(at + TIMBRE_MUTE)?;
    let mut out = image.get(at..at + TIMBRE_COMMON)?.to_vec();
    let mut from = at + TIMBRE_COMMON;
    for partial in 0..4 {
        if partial != 0 && mute & (1 << partial) != 0 {
            from += TIMBRE_PARTIAL;
        }
        out.extend_from_slice(image.get(from..from + TIMBRE_PARTIAL)?);
    }
    Some(out)
}

/// The performance: a wait, then a message, over and over.
fn read_stream(at: &[u8]) -> Vec<Event> {
    let mut events = Vec::new();
    let mut i = 0;
    let mut running = 0u8;
    while let Some(&wait) = at.get(i) {
        i += 1;
        if wait == END {
            break;
        }
        // The unit's own timing byte stands in for a message, and still
        // takes its time: dropping it would run the song short.
        if wait == SYNC {
            events.push(Event {
                wait,
                message: Vec::new(),
            });
            continue;
        }
        let Some(&byte) = at.get(i) else { break };
        i += 1;
        if byte == STOP {
            break;
        }
        if byte & 0xF0 == 0xF0 {
            events.push(Event {
                wait,
                message: Vec::new(),
            });
            continue;
        }
        let mut message = Vec::with_capacity(3);
        if byte & 0x80 != 0 {
            running = byte;
            message.push(byte);
            let Some(&d) = at.get(i) else { break };
            i += 1;
            message.push(d);
        } else if running != 0 {
            message.push(running);
            message.push(byte);
        } else {
            continue;
        }
        // Everything but a program change carries a second byte.
        if running & 0xF0 != 0xC0 {
            let Some(&d) = at.get(i) else { break };
            i += 1;
            message.push(d);
        }
        events.push(Event { wait, message });
    }
    events
}

/// Plays a song, a rendered chunk at a time.
///
/// Driven by frames rather than by any host clock, so a demo runs in the
/// engine's own time: the same frame carries the same message on every
/// run. The bytes go out through whatever feeds the engine -- typically
/// a [crate::midi::Parser] over the engine itself.
#[derive(Debug)]
pub struct Player {
    song: Song,
    next: usize,
    frames_per_tick: f32,
    in_hand: f32,
    /// The settings, still to go out ahead of the first note.
    setup_sent: bool,
}

impl Player {
    /// Start `song` at the top, timed in the module's own frames.
    pub fn new(song: Song) -> Player {
        Player::at_rate(song, crate::SAMPLE_RATE)
    }

    /// Start `song` at the top, counting frames at `sample_rate`: for a
    /// host that paces the song against a resampled stream rather than
    /// the module's own.
    pub fn at_rate(song: Song, sample_rate: u32) -> Player {
        let frames_per_tick = sample_rate as f32 * song.seconds_per_tick;
        Player {
            song,
            next: 0,
            frames_per_tick,
            in_hand: 0.0,
            setup_sent: false,
        }
    }

    /// Whether the song has run out.
    pub fn finished(&self) -> bool {
        self.next >= self.song.events.len()
    }

    /// Append the bytes due over the next `frames` to `due`, in the
    /// order they are due. Several can fall together -- a chord is a run
    /// of messages with no wait between them -- which is also how they
    /// would arrive down a cable at this speed.
    pub fn advance(&mut self, frames: usize, due: &mut Vec<u8>) {
        if !self.setup_sent {
            self.setup_sent = true;
            due.extend_from_slice(&self.song.setup);
        }
        self.in_hand += frames as f32;
        while let Some(event) = self.song.events.get(self.next) {
            let wait = f32::from(event.wait) * self.frames_per_tick;
            if wait > self.in_hand {
                break;
            }
            self.in_hand -= wait;
            due.extend_from_slice(&event.message);
            self.next += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The songs come out of every second-generation image in the
    /// directory: named, in the order the ROM lists them, timed by what
    /// each carries, and with the settings they want ahead of the first
    /// note. Roland's ROMs cannot be committed, so this runs only where
    /// they are to hand.
    #[test]
    #[ignore = "needs a ROM directory in MT32_RS_ROMS_ALL"]
    fn the_songs_come_out_of_the_real_images() {
        let Some(dir) = std::env::var_os("MT32_RS_ROMS_ALL").map(std::path::PathBuf::from) else {
            panic!("MT32_RS_ROMS_ALL is not set");
        };
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir)
            .expect("the directory opens")
            .flatten()
        {
            let image = std::fs::read(entry.path()).unwrap_or_default();
            let songs = songs(&image);
            if songs.is_empty() {
                continue;
            }
            assert_eq!(songs.len(), SONG_COUNT);
            // The order is the ROM's own, not the order they sit in it.
            let names: Vec<&str> = songs.iter().map(|s| s.title.as_str()).collect();
            assert_eq!(
                names,
                [
                    "Boiler Buster",
                    "Sinfonia 1",
                    "Short Demo",
                    "Adjarre",
                    "Good Morning"
                ]
            );
            for song in &songs {
                let secs = song.seconds();
                assert!(song.events.len() > 100, "it has a performance in it");
                assert!(
                    (20.0..600.0).contains(&secs),
                    "it runs for a plausible time: {secs}"
                );
                // Every note that starts is stopped: a misread stream
                // would lose its place and the two would not balance.
                let (mut on, mut off) = (0u32, 0u32);
                for e in song.events.iter().filter(|e| !e.message.is_empty()) {
                    match (e.message[0] & 0xF0, e.message.get(2)) {
                        (0x90, Some(&0)) | (0x80, _) => off += 1,
                        (0x90, _) => on += 1,
                        _ => {}
                    }
                }
                assert!(
                    on.abs_diff(off) <= 1,
                    "{on} notes started, {off} stopped -- the stream was misread"
                );
                // The setup is whole messages, every one of them closed.
                assert_eq!(song.setup[0], 0xF0);
                assert_eq!(*song.setup.last().unwrap(), 0xF7);
                assert_eq!(
                    song.setup.iter().filter(|&&b| b == 0xF0).count(),
                    song.setup.iter().filter(|&&b| b == 0xF7).count(),
                );
            }
            checked += songs.len();
            println!(
                "  {:<24} {} songs, titled and balanced",
                entry.file_name().to_string_lossy(),
                songs.len()
            );
        }
        assert!(checked > 0, "no second-generation ROM to check");
    }

    #[test]
    fn an_image_with_no_songs_in_it_has_none() {
        assert!(songs(&[0u8; 0x400]).is_empty(), "far too small");
        assert!(songs(&[]).is_empty(), "no image at all");
    }

    /// A wait comes before its message, and the two ways a song can end
    /// are both taken.
    #[test]
    fn the_performance_reads_wait_then_message() {
        // Wait 0, note on; wait 24, running status; wait 12, program
        // change (one byte); then the end.
        let stream = [0, 0x91, 0x3C, 0x64, 24, 0x3E, 0x64, 12, 0xC1, 0x05, END];
        let events = read_stream(&stream);
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0],
            Event {
                wait: 0,
                message: vec![0x91, 0x3C, 0x64]
            }
        );
        assert_eq!(
            events[1],
            Event {
                wait: 24,
                message: vec![0x91, 0x3E, 0x64]
            },
            "carrying on with the last status"
        );
        assert_eq!(
            events[2],
            Event {
                wait: 12,
                message: vec![0xC1, 0x05]
            },
            "a program change carries one byte"
        );
        // A Stop ends it just as surely.
        assert_eq!(read_stream(&[0, 0x91, 0x3C, 0x64, 5, STOP]).len(), 1);
        // The timing byte carries no message but still takes its time.
        let timed = read_stream(&[SYNC, SYNC, 0, 0x91, 0x3C, 0x64, END]);
        assert_eq!(timed.len(), 3);
        assert!(timed[0].message.is_empty(), "nothing to play, only to wait");
        assert_eq!(timed[2].message, vec![0x91, 0x3C, 0x64]);
    }

    /// The player pays out bytes against frames: nothing early, chords
    /// together, the setup ahead of everything.
    #[test]
    fn the_player_pays_out_on_time() {
        let song = Song {
            title: "Test".into(),
            setup: dt1(0x10_0016, &[80]),
            events: vec![
                Event {
                    wait: 0,
                    message: vec![0x91, 0x3C, 0x64],
                },
                Event {
                    wait: 0,
                    message: vec![0x91, 0x40, 0x64],
                },
                Event {
                    wait: 80,
                    message: vec![0x91, 0x3C, 0x00],
                },
            ],
            seconds_per_tick: 6250.0 / TIMER_HZ,
        };
        let mut player = Player::new(song);
        let mut due = Vec::new();
        player.advance(0, &mut due);
        assert_eq!(due[0], 0xF0, "the setup leads");
        assert_eq!(
            &due[due.len() - 6..],
            &[0x91, 0x3C, 0x64, 0x91, 0x40, 0x64],
            "the chord lands together at once"
        );
        assert!(!player.finished());
        // Eighty ticks at eighty a second is one second of frames.
        due.clear();
        player.advance(crate::SAMPLE_RATE as usize / 2, &mut due);
        assert!(due.is_empty(), "half a second early, nothing due");
        player.advance(crate::SAMPLE_RATE as usize / 2, &mut due);
        assert_eq!(due, [0x91, 0x3C, 0x00], "and on the second, the note off");
        assert!(player.finished());
    }
}
