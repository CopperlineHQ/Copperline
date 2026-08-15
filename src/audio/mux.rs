// SPDX-License-Identifier: GPL-3.0-or-later

//! Audio source fan-out: [`AudioMux`] sits between Paula's mixer and the
//! live/capture sinks in [`crate::audio`], letting every audio producer
//! (Paula, drive sounds, CD audio, MT-32) register as a named source once,
//! so stem capture (`--audio-stems`) and future boards (e.g. a Toccata AHI
//! board) need no further host-side plumbing. See docs/internals/audio.md.

use super::{open_wav_writer, AudioRuntimeStatus, AudioSink};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

/// A capture granularity `--audio-stems-mode` can select. Independently
/// selectable and combinable -- a run can request `master` and `source`
/// together, for instance. `Channel` is a deliberate opt-in: selecting
/// `Source` alone never implies per-channel files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StemGranularity {
    /// `DIR/master.wav` -- the same mixed-master signal a listener hears.
    Master,
    /// One file per registered source (`DIR/paula.wav`, `DIR/cdda.wav`, ...).
    Source,
    /// One file per named sub-channel of a source (`DIR/paula-0.wav` ..
    /// `DIR/paula-3.wav`). Never implied by `Source` alone.
    Channel,
}

impl StemGranularity {
    pub fn as_str(self) -> &'static str {
        match self {
            StemGranularity::Master => "master",
            StemGranularity::Source => "source",
            StemGranularity::Channel => "channel",
        }
    }

    /// Parse a comma-separated `--audio-stems-mode`/`[audio] stem_granularity`
    /// value, e.g. `"master,source"`. Rejects unknown tokens and an empty
    /// list (whitespace/commas only) -- capture behavior must be explicit.
    pub fn parse_list(s: &str) -> Result<Vec<StemGranularity>> {
        let values: Vec<StemGranularity> = s
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| match part {
                "master" => Ok(StemGranularity::Master),
                "source" => Ok(StemGranularity::Source),
                "channel" => Ok(StemGranularity::Channel),
                other => Err(anyhow!(
                    "unknown audio stem granularity {other:?} (expected \"master\", \"source\", or \"channel\")"
                )),
            })
            .collect::<Result<_>>()?;
        if values.is_empty() {
            return Err(anyhow!(
                "audio stem granularity list is empty (expected at least one of \"master\", \"source\", \"channel\")"
            ));
        }
        Ok(values)
    }
}

/// One audio producer's registration with the mux: a stable id used for its
/// stem file name (`paula.wav`), plus the names of any physically distinct
/// sub-channels it exposes (Paula's four channels: `paula-0.wav` ..
/// `paula-3.wav`). Sources without sub-channels register an empty slice.
#[derive(Debug, Clone, Copy)]
pub struct SourceSpec {
    pub id: &'static str,
    pub channel_names: &'static [&'static str],
}

type Writer = hound::WavWriter<BufWriter<File>>;

struct StemWriters {
    master: Option<Writer>,
    sources: HashMap<&'static str, Writer>,
    channels: HashMap<(&'static str, &'static str), Writer>,
}

/// Fan-out point every audio producer pushes through. Owns the master sink
/// (the same [`AudioSink`] the emulator has always used for live playback or
/// `--audio-wav` capture) and, once [`AudioMux::enable_stems`] is called,
/// the per-granularity stem writers.
pub struct AudioMux {
    master: Box<dyn AudioSink>,
    stems: Option<StemWriters>,
}

impl AudioMux {
    pub fn new(master: Box<dyn AudioSink>) -> Self {
        Self {
            master,
            stems: None,
        }
    }

    /// Replace the live master sink in place (device hot-swap, output
    /// picker changes, device-loss recovery). Stem writers, if any, are
    /// unaffected -- a capture in progress keeps writing across a live
    /// output device change.
    pub fn set_master(&mut self, master: Box<dyn AudioSink>) {
        self.master = master;
    }

    /// Enable stem capture into `dir` (created if missing) for the selected
    /// granularities. Only files implied by `granularities` are opened: a
    /// `sources` entry with no `Channel` granularity selected never opens
    /// its per-channel files, and a source absent from `sources` never gets
    /// a file at all (the caller decides which sources are worth capturing,
    /// e.g. omitting `cdda` when no CD drive is configured this run).
    ///
    /// `dir` must be empty (or not yet exist). A stem directory's file set
    /// says what this run captured; silently writing into a directory that
    /// already holds stems from an earlier run (possibly a different
    /// granularity selection) would leave stale files mixed in with fresh
    /// ones, contradicting that and making directory-based comparisons
    /// unreliable.
    pub fn enable_stems(
        &mut self,
        dir: &Path,
        granularities: &[StemGranularity],
        sources: &[SourceSpec],
    ) -> Result<()> {
        std::fs::create_dir_all(dir)
            .map_err(|e| anyhow!("create audio stems directory {}: {e}", dir.display()))?;
        let mut existing = std::fs::read_dir(dir)
            .map_err(|e| anyhow!("read audio stems directory {}: {e}", dir.display()))?;
        if existing.next().is_some() {
            return Err(anyhow!(
                "audio stems directory {} is not empty; pick an empty directory so this \
                 run's stem files aren't mixed with an earlier run's",
                dir.display()
            ));
        }

        let master = granularities
            .contains(&StemGranularity::Master)
            .then(|| open_wav_writer(&dir.join("master.wav")))
            .transpose()?;

        let mut source_writers = HashMap::new();
        if granularities.contains(&StemGranularity::Source) {
            for spec in sources {
                let path = dir.join(format!("{}.wav", spec.id));
                source_writers.insert(spec.id, open_wav_writer(&path)?);
            }
        }

        let mut channel_writers = HashMap::new();
        if granularities.contains(&StemGranularity::Channel) {
            for spec in sources {
                for &channel in spec.channel_names {
                    let path = dir.join(format!("{}-{channel}.wav", spec.id));
                    channel_writers.insert((spec.id, channel), open_wav_writer(&path)?);
                }
            }
        }

        log::info!(
            "audio: stem capture writing to {} (granularities: {})",
            dir.display(),
            granularities
                .iter()
                .map(|g| g.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );

        self.stems = Some(StemWriters {
            master,
            sources: source_writers,
            channels: channel_writers,
        });
        Ok(())
    }

    /// Push the final post-master-volume/stereo-width stereo frame -- the
    /// same value every mixed-master sink (live or `--audio-wav`) has
    /// always received. Also feeds the `master` stem writer, if enabled.
    pub fn push_master(&mut self, left: f32, right: f32) {
        self.master.push(left, right);
        if let Some(stems) = &mut self.stems {
            if let Some(writer) = &mut stems.master {
                let _ = writer.write_sample(left);
                let _ = writer.write_sample(right);
            }
        }
    }

    pub fn flush(&mut self) {
        self.master.flush();
        if let Some(stems) = &mut self.stems {
            if let Some(writer) = &mut stems.master {
                let _ = writer.flush();
            }
            for writer in stems.sources.values_mut() {
                let _ = writer.flush();
            }
            for writer in stems.channels.values_mut() {
                let _ = writer.flush();
            }
        }
    }

    pub fn live_output_lead_seconds(&self) -> f64 {
        self.master.live_output_lead_seconds()
    }

    pub fn runtime_status(&self) -> AudioRuntimeStatus {
        self.master.runtime_status()
    }

    pub fn set_live_output_suspended(&mut self, suspended: bool) {
        self.master.set_live_output_suspended(suspended);
    }

    pub fn reset_live_output_after_timeline_jump(&mut self) {
        self.master.reset_live_output_after_timeline_jump();
    }

    pub fn is_null_sink(&self) -> bool {
        self.master.is_null_sink()
    }

    pub fn device_lost(&self) -> bool {
        self.master.device_lost()
    }

    /// Tap a named source's stereo contribution (e.g. "paula", "cdda",
    /// "mt32", "drivesounds") for `Source`-granularity stem capture.
    /// A no-op unless stems are enabled and this source was registered.
    pub fn push_source(&mut self, source: &'static str, left: f32, right: f32) {
        if let Some(stems) = &mut self.stems {
            if let Some(writer) = stems.sources.get_mut(source) {
                let _ = writer.write_sample(left);
                let _ = writer.write_sample(right);
            }
        }
    }

    /// Tap one named sub-channel of a source (e.g. Paula's four physical
    /// channels) for `Channel`-granularity stem capture. Written as a
    /// mono-as-stereo frame (L == R), like [`Self::push_source`]'s mono
    /// sources -- every stem shares one fixed stereo f32 WAV format.
    pub fn push_source_channel(
        &mut self,
        source: &'static str,
        channel: &'static str,
        sample: f32,
    ) {
        if let Some(stems) = &mut self.stems {
            if let Some(writer) = stems.channels.get_mut(&(source, channel)) {
                let _ = writer.write_sample(sample);
                let _ = writer.write_sample(sample);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::NullSink;

    struct CollectSink(std::rc::Rc<std::cell::RefCell<Vec<(f32, f32)>>>);
    impl AudioSink for CollectSink {
        fn push(&mut self, left: f32, right: f32) {
            self.0.borrow_mut().push((left, right));
        }
        fn flush(&mut self) {}
    }

    const PAULA: SourceSpec = SourceSpec {
        id: "paula",
        channel_names: &["0", "1", "2", "3"],
    };
    const CDDA: SourceSpec = SourceSpec {
        id: "cdda",
        channel_names: &[],
    };

    #[test]
    fn push_master_forwards_the_exact_frame_to_the_master_sink() {
        let frames = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut mux = AudioMux::new(Box::new(CollectSink(frames.clone())));
        mux.push_master(0.25, -0.5);
        mux.push_master(1.0, -1.0);
        assert_eq!(*frames.borrow(), vec![(0.25, -0.5), (1.0, -1.0)]);
    }

    #[test]
    fn set_master_swaps_the_sink_without_losing_the_mux() {
        let mut mux = AudioMux::new(Box::new(NullSink));
        assert!(mux.is_null_sink());
        let frames = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        mux.set_master(Box::new(CollectSink(frames.clone())));
        assert!(!mux.is_null_sink());
        mux.push_master(0.1, 0.2);
        assert_eq!(*frames.borrow(), vec![(0.1, 0.2)]);
    }

    #[test]
    fn granularity_lists_parse_and_reject_unknown_or_empty_input() {
        assert_eq!(
            StemGranularity::parse_list("master,source").unwrap(),
            vec![StemGranularity::Master, StemGranularity::Source]
        );
        assert_eq!(
            StemGranularity::parse_list(" channel , master ").unwrap(),
            vec![StemGranularity::Channel, StemGranularity::Master]
        );
        assert!(StemGranularity::parse_list("bogus").is_err());
        assert!(StemGranularity::parse_list("").is_err());
        assert!(StemGranularity::parse_list(" , ").is_err());
    }

    #[test]
    fn enable_stems_only_opens_files_for_selected_granularities() {
        let dir = std::env::temp_dir().join(format!(
            "copperline-audio-mux-test-{:?}",
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut mux = AudioMux::new(Box::new(NullSink));
        mux.enable_stems(&dir, &[StemGranularity::Master], &[PAULA, CDDA])
            .unwrap();
        mux.push_master(0.5, -0.5);
        mux.flush();

        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        entries.sort();
        assert_eq!(entries, vec!["master.wav"]);
        drop(mux); // close the open WavWriter before removing its file (Windows)
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn enable_stems_opens_source_and_channel_files_when_selected() {
        let dir = std::env::temp_dir().join(format!(
            "copperline-audio-mux-test-src-chan-{:?}",
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut mux = AudioMux::new(Box::new(NullSink));
        mux.enable_stems(
            &dir,
            &[StemGranularity::Source, StemGranularity::Channel],
            &[PAULA, CDDA],
        )
        .unwrap();
        mux.push_source("paula", 0.1, 0.2);
        mux.push_source("cdda", 0.3, 0.4);
        mux.push_source_channel("paula", "0", 0.05);
        mux.flush();

        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            vec![
                "cdda.wav",
                "paula-0.wav",
                "paula-1.wav",
                "paula-2.wav",
                "paula-3.wav",
                "paula.wav",
            ]
        );
        drop(mux); // close the open WavWriters before removing their files (Windows)
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
