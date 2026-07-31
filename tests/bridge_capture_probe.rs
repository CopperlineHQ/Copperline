// SPDX-License-Identifier: GPL-3.0-or-later

//! Hardware probes for the floppy-bridge capture path.
//!
//! All `#[ignore]`d: they need a FloppyBridge interface (a Greaseweazle in
//! practice) with an AmigaDOS disk in the drive. There is one drive, so run
//! one test at a time:
//!
//! ```sh
//! cargo test --release --test bridge_capture_probe -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `bridge_decoder_smoke` validates the sector decoder against a single
//! `compatible`-mode capture of cylinder 0. `bridge_seek_capture_soak` mimics
//! a boot's scattered access pattern -- seek away, seek back, capture -- and
//! decodes every capture, to measure how often a capture arrives with a
//! damaged sector and where the damage sits. Knobs, all environment variables:
//!
//! - `PROBE_MODE`: `normal` (default) or `compatible`
//! - `PROBE_CAPTURES`: total captures to take (default 120)
//! - `PROBE_TARGETS`: comma-separated `cyl:side` list to cycle through
//!   (default `54:0,57:0,58:1`, tracks that read marginally on the
//!   reference disk)
//! - `PROBE_AWAY`: how many cylinders to seek away between captures (default 20)
//! - `PROBE_SEEK`: `pulse` (default; one-cylinder seeks 3 ms apart, as the
//!   emulated stepper drives the bridge) or `direct` (one seek to the target)

#![cfg(feature = "floppybridge")]

use std::time::{Duration, Instant};

use copperline::floppybridge::{drivers, Bridge, BridgeConfig, BridgeMode};

const MASK: u32 = 0x5555_5555;

/// One decoded AmigaDOS sector and where it sits in the capture.
#[derive(Debug)]
struct Sector {
    /// Bit position of the first sync word of the pair.
    start_bit: usize,
    track: u8,
    sector: u8,
    header_ok: bool,
    data_ok: bool,
}

struct Scan {
    bit_len: usize,
    syncs: usize,
    sectors: Vec<Sector>,
}

impl Scan {
    fn sectors_ok(&self) -> usize {
        self.sectors
            .iter()
            .filter(|s| s.header_ok && s.data_ok)
            .count()
    }

    fn bad(&self) -> Vec<&Sector> {
        self.sectors
            .iter()
            .filter(|s| !(s.header_ok && s.data_ok))
            .collect()
    }
}

/// Read one bit of the capture, as a ring.
fn bit_at(words: &[u16], bit_len: usize, i: usize) -> bool {
    let i = i % bit_len;
    words[i / 16] & (1 << (15 - (i % 16))) != 0
}

fn long_at(words: &[u16], bit_len: usize, start: usize) -> u32 {
    let mut v = 0u32;
    for k in 0..32 {
        v = (v << 1) | u32::from(bit_at(words, bit_len, start + k));
    }
    v
}

/// Decode an odd/even MFM long pair into its data long.
fn deinterleave(odd: u32, even: u32) -> u32 {
    ((odd & MASK) << 1) | (even & MASK)
}

/// Decode every AmigaDOS sector in a captured revolution, treating the capture
/// as the ring the emulator serves. Follows the on-disk layout: two 0x4489
/// sync words, then odd/even split info (1 long), label (4 longs), header
/// checksum, data checksum, data (128 longs). Checksums are the XOR of the
/// masked MFM longs of the region they cover.
fn decode(words: &[u16], bit_len: usize) -> Scan {
    // Find every sync word position.
    let mut syncs = Vec::new();
    let mut window: u16 = 0;
    for i in 0..bit_len + 15 {
        window = (window << 1) | u16::from(bit_at(words, bit_len, i));
        if i >= 15 && window == 0x4489 {
            syncs.push((i - 15) % bit_len);
        }
    }

    // A sector begins at a 0x4489 immediately followed by another. Walk the
    // pairs; a lone sync (the first of a pair found again at the ring end) is
    // skipped by the distance check.
    let mut sectors = Vec::new();
    for &s in &syncs {
        // The word after this sync must also be a sync, and the one before
        // must not be (otherwise `s` is the second of the pair).
        let next_is_sync = syncs.contains(&((s + 16) % bit_len));
        let prev_is_sync = syncs.contains(&((s + bit_len - 16) % bit_len));
        if !next_is_sync || prev_is_sync {
            continue;
        }
        let body = s + 32; // past both sync words
        let info_odd = long_at(words, bit_len, body);
        let info_even = long_at(words, bit_len, body + 32);
        let info = deinterleave(info_odd, info_even);
        let [format, track, sector, _to_gap] = info.to_be_bytes();
        if format != 0xFF || sector >= 11 {
            continue;
        }

        // Header checksum covers info + label as masked MFM longs.
        let mut hsum = (info_odd & MASK) ^ (info_even & MASK);
        for l in 0..8 {
            hsum ^= long_at(words, bit_len, body + 64 + l * 32) & MASK;
        }
        let stored_h = deinterleave(
            long_at(words, bit_len, body + 320),
            long_at(words, bit_len, body + 352),
        );
        let header_ok = hsum == stored_h;

        // Data checksum covers the 256 MFM longs of the payload.
        let data_start = body + 448;
        let mut dsum = 0u32;
        for l in 0..256 {
            dsum ^= long_at(words, bit_len, data_start + l * 32) & MASK;
        }
        let stored_d = deinterleave(
            long_at(words, bit_len, body + 384),
            long_at(words, bit_len, body + 416),
        );
        let data_ok = dsum == stored_d;

        sectors.push(Sector {
            start_bit: s,
            track,
            sector,
            header_ok,
            data_ok,
        });
    }

    Scan {
        bit_len,
        syncs: syncs.len(),
        sectors,
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn open_bridge(mode: BridgeMode) -> Bridge {
    let driver = drivers()
        .into_iter()
        .find(|d| d.name.to_ascii_lowercase().contains("greaseweazle"))
        .expect("no greaseweazle driver in this build");
    Bridge::open(&BridgeConfig {
        driver: driver.index,
        mode,
        ..Default::default()
    })
    .expect("open the drive")
}

fn spin_up(bridge: &mut Bridge) {
    bridge.set_motor(false, true);
    let deadline = Instant::now() + Duration::from_secs(4);
    while !bridge.is_ready() {
        assert!(Instant::now() < deadline, "drive never became ready");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Move the head the way the emulated stepper does: one cylinder per seek,
/// 3 ms apart, so the driver sees the same command stream a boot produces.
fn seek_pulsed(bridge: &mut Bridge, from: u8, to: u8, side: bool) {
    let mut at = from;
    while at != to {
        at = if to > at { at + 1 } else { at - 1 };
        bridge.seek(at, side);
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn poll_track(bridge: &mut Bridge, cyl: u8, side: bool) -> Option<(Vec<u16>, usize, u128)> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(4);
    loop {
        if let Some((words, bits)) = bridge.read_track(cyl, side) {
            return Some((words, bits, started.elapsed().as_millis()));
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// One capture of cylinder 0 in `compatible` mode, decoded and printed.
/// Proves the decoder against an index-aligned capture before the soak's
/// results can mean anything.
#[test]
#[ignore = "needs a FloppyBridge device with an AmigaDOS disk inserted"]
fn bridge_decoder_smoke() {
    let mut bridge = open_bridge(BridgeMode::Compatible);
    spin_up(&mut bridge);
    let (words, bits, waited) = poll_track(&mut bridge, 0, false).expect("no capture");
    let scan = decode(&words, bits);
    println!(
        "cyl 0 lower: {} bits in {waited}ms, {} syncs, {}/{} sectors clean",
        scan.bit_len,
        scan.syncs,
        scan.sectors_ok(),
        scan.sectors.len(),
    );
    for s in &scan.sectors {
        println!(
            "  t{} s{:>2} at bit {:>6}  header={} data={}",
            s.track, s.sector, s.start_bit, s.header_ok, s.data_ok
        );
    }
    assert_eq!(scan.sectors.len(), 11, "expected 11 sectors");
    assert_eq!(scan.sectors_ok(), 11, "expected all sectors clean");
}

/// The soak: capture repeatedly across seeks and score every capture.
#[test]
#[ignore = "needs a FloppyBridge device with an AmigaDOS disk inserted"]
fn bridge_seek_capture_soak() {
    let mode = match env_or("PROBE_MODE", "normal").as_str() {
        "compatible" => BridgeMode::Compatible,
        _ => BridgeMode::Fast,
    };
    let captures: usize = env_or("PROBE_CAPTURES", "120").parse().unwrap();
    let away: u8 = env_or("PROBE_AWAY", "20").parse().unwrap();
    let pulse = env_or("PROBE_SEEK", "pulse") == "pulse";
    let targets: Vec<(u8, bool)> = env_or("PROBE_TARGETS", "54:0,57:0,58:1")
        .split(',')
        .map(|t| {
            let (c, s) = t.split_once(':').expect("cyl:side");
            (c.trim().parse().unwrap(), s.trim() == "1")
        })
        .collect();

    println!(
        "mode={mode:?} captures={captures} away={away} seek={}",
        if pulse { "pulse" } else { "direct" }
    );

    let mut bridge = open_bridge(mode);
    spin_up(&mut bridge);

    let mut damaged = 0usize;
    let mut timeouts = 0usize;
    let mut at: u8 = 0;
    for n in 0..captures {
        let (cyl, side) = targets[n % targets.len()];
        // Seek away first so every capture follows real head movement, the
        // way trackdisk's retry-with-recalibrate reaches a track.
        let park = if cyl >= away { cyl - away } else { cyl + away };
        if pulse {
            seek_pulsed(&mut bridge, at, park, side);
        } else {
            bridge.seek(park, side);
        }
        // Let the driver begin (and then abandon) a capture there, as it
        // does between a boot's seeks.
        std::thread::sleep(Duration::from_millis(60));
        if pulse {
            seek_pulsed(&mut bridge, park, cyl, side);
        } else {
            bridge.seek(cyl, side);
        }
        at = cyl;
        // Give the background thread time to land a capture that began just
        // after the seek -- the case under test -- then retire whatever was
        // current so the fresh one is served.
        std::thread::sleep(Duration::from_millis(700));
        bridge.switch_buffer(side);

        let Some((words, bits, waited)) = poll_track(&mut bridge, cyl, side) else {
            timeouts += 1;
            println!("#{n:>4} cyl {cyl} side {}: TIMEOUT", u8::from(side));
            continue;
        };
        let scan = decode(&words, bits);
        let ok = scan.sectors_ok();
        if ok < scan.sectors.len() || scan.sectors.len() < 11 {
            damaged += 1;
            let spans: Vec<String> = scan
                .bad()
                .iter()
                .map(|s| {
                    format!(
                        "t{} s{} at {} ({}h/{}d)",
                        s.track,
                        s.sector,
                        s.start_bit,
                        u8::from(s.header_ok),
                        u8::from(s.data_ok)
                    )
                })
                .collect();
            println!(
                "#{n:>4} cyl {cyl} side {}: {ok}/{} in {bits} bits, {waited}ms  BAD {spans:?}",
                u8::from(side),
                scan.sectors.len(),
            );
        } else {
            println!(
                "#{n:>4} cyl {cyl} side {}: 11/11 in {bits} bits, {waited}ms",
                u8::from(side)
            );
        }
    }

    println!("\nsummary: mode={mode:?} captures={captures} damaged={damaged} timeouts={timeouts}");
}
