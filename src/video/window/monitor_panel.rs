// SPDX-License-Identifier: GPL-3.0-or-later

//! The card monitors' front panels: where each button is on the built-in
//! artwork, what the buttons set, and what those settings do to the
//! picture on the way to the tube.
//!
//! The picture controls work as a studio monitor's do, on the signal:
//! CONTRAST is the picture's gain, BRIGHT its black level, CHROMA and
//! PHASE the colour's saturation and hue (in YIQ, about the colour
//! burst), APERTURE the horizontal peaking (or softening, below its
//! preset). BLUE ONLY drives the blue gun alone, UNDER SCAN shrinks the
//! raster inside the glass, and H-V DELAY shows the picture shifted half a
//! line and half a field, the blanking and sync between. INPUT B and C
//! carry nothing (a dark tube); ANALOG/DIGITAL only lights. RESET puts the
//! picture controls back to their presets; VOL is the Program monitor's
//! speaker, the card's sound. POWER switches the tube on (it warms up,
//! after the degaussing coil's thump) or off (the picture collapses to a
//! line, a dot, then dark).
//!
//! Each monitor's settings are kept in `.copperline/monitors.json` in the
//! card's media folder.

use std::time::Instant;

/// A picture control with up and down buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Knob {
    Aperture,
    Bright,
    Chroma,
    Phase,
    Contrast,
    Vol,
}

impl Knob {
    pub(super) const ALL: [Knob; 6] = [
        Knob::Aperture,
        Knob::Bright,
        Knob::Chroma,
        Knob::Phase,
        Knob::Contrast,
        Knob::Vol,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Knob::Aperture => "APERTURE",
            Knob::Bright => "BRIGHT",
            Knob::Chroma => "CHROMA",
            Knob::Phase => "PHASE",
            Knob::Contrast => "CONTRAST",
            Knob::Vol => "VOLUME",
        }
    }

    /// The control's range: picture controls -10..=10 about their preset
    /// 0, volume 0..=20 (preset 16).
    pub(super) fn range(self) -> (i8, i8) {
        match self {
            Knob::Vol => (0, VOL_MAX),
            _ => (-10, 10),
        }
    }
}

/// The volume's top, and its preset (the card's sound as made).
pub(super) const VOL_MAX: i8 = 20;
pub(super) const VOL_PRESET: i8 = 16;

/// A front-panel button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Key {
    Input(u8),
    BlueOnly,
    UnderScan,
    HvDelay,
    AnalogDigital,
    Reset,
    Up(Knob),
    Down(Knob),
    Power,
}

/// A rectangle in an artwork's pixels, `(x0, y0, x1, y1)`.
pub(super) type ArtRect = (u32, u32, u32, u32);

/// Where a built-in artwork's buttons and lamps are, in its pixels.
pub(super) struct Panel {
    pub(super) keys: &'static [(Key, ArtRect)],
    /// The power lamp and the RESET lamp: centre and radius.
    pub(super) power_lamp: (f32, f32, f32),
    pub(super) reset_lamp: (f32, f32, f32),
}

pub(super) const PROGRAM_PANEL: Panel = Panel {
    keys: &[
        (Key::Input(0), (497, 939, 516, 958)),
        (Key::Input(1), (543, 939, 562, 958)),
        (Key::Input(2), (588, 939, 607, 958)),
        (Key::BlueOnly, (497, 1002, 516, 1021)),
        (Key::UnderScan, (543, 1001, 561, 1021)),
        (Key::HvDelay, (589, 1001, 608, 1021)),
        (Key::AnalogDigital, (645, 1002, 664, 1021)),
        (Key::Reset, (825, 987, 858, 1023)),
        (Key::Up(Knob::Aperture), (882, 935, 915, 970)),
        (Key::Down(Knob::Aperture), (882, 987, 915, 1023)),
        (Key::Up(Knob::Bright), (940, 935, 974, 971)),
        (Key::Down(Knob::Bright), (940, 987, 974, 1023)),
        (Key::Up(Knob::Chroma), (999, 935, 1032, 970)),
        (Key::Down(Knob::Chroma), (999, 987, 1032, 1023)),
        (Key::Up(Knob::Phase), (1057, 935, 1090, 971)),
        (Key::Down(Knob::Phase), (1057, 987, 1090, 1023)),
        (Key::Up(Knob::Contrast), (1116, 935, 1149, 970)),
        (Key::Down(Knob::Contrast), (1116, 987, 1149, 1023)),
        (Key::Up(Knob::Vol), (1174, 935, 1208, 970)),
        (Key::Down(Knob::Vol), (1174, 987, 1208, 1023)),
        (Key::Power, (1252, 982, 1284, 1013)),
    ],
    power_lamp: (1266.0, 950.0, 6.0),
    reset_lamp: (839.0, 942.0, 5.0),
};

pub(super) const PREVIEW_PANEL: Panel = Panel {
    keys: &[
        (Key::Input(0), (496, 932, 514, 951)),
        (Key::Input(1), (541, 932, 560, 952)),
        (Key::Input(2), (587, 932, 605, 951)),
        (Key::BlueOnly, (496, 994, 514, 1014)),
        (Key::UnderScan, (541, 994, 559, 1014)),
        (Key::HvDelay, (587, 994, 605, 1014)),
        (Key::AnalogDigital, (642, 994, 661, 1014)),
        (Key::Reset, (822, 979, 854, 1015)),
        (Key::Up(Knob::Aperture), (879, 928, 912, 964)),
        (Key::Down(Knob::Aperture), (879, 980, 912, 1015)),
        (Key::Up(Knob::Bright), (937, 927, 970, 964)),
        (Key::Down(Knob::Bright), (937, 980, 970, 1015)),
        (Key::Up(Knob::Chroma), (995, 927, 1028, 964)),
        (Key::Down(Knob::Chroma), (995, 980, 1028, 1015)),
        (Key::Up(Knob::Phase), (1053, 927, 1086, 964)),
        (Key::Down(Knob::Phase), (1053, 978, 1086, 1016)),
        (Key::Up(Knob::Contrast), (1112, 926, 1145, 964)),
        (Key::Down(Knob::Contrast), (1112, 978, 1145, 1016)),
        (Key::Up(Knob::Vol), (1170, 926, 1204, 964)),
        (Key::Down(Knob::Vol), (1170, 978, 1204, 1015)),
        (Key::Power, (1248, 976, 1279, 1006)),
    ],
    power_lamp: (1261.0, 943.0, 6.0),
    reset_lamp: (843.0, 953.0, 5.0),
};

/// What a monitor's front panel is set to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Settings {
    pub(super) power: bool,
    /// 0 for INPUT A (the card), 1 and 2 for B and C (nothing).
    pub(super) input: u8,
    pub(super) blue_only: bool,
    pub(super) under_scan: bool,
    pub(super) hv_delay: bool,
    pub(super) digital: bool,
    /// Indexed by [`Knob::ALL`]'s order.
    pub(super) knobs: [i8; 6],
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            power: true,
            input: 0,
            blue_only: false,
            under_scan: false,
            hv_delay: false,
            digital: false,
            knobs: [0, 0, 0, 0, 0, VOL_PRESET],
        }
    }
}

fn knob_index(k: Knob) -> usize {
    Knob::ALL.iter().position(|&x| x == k).unwrap_or(0)
}

impl Settings {
    pub(super) fn knob(&self, k: Knob) -> i8 {
        self.knobs[knob_index(k)]
    }

    /// The picture controls are all at their presets.
    pub(super) fn at_presets(&self) -> bool {
        self.knobs[..5].iter().all(|&v| v == 0)
    }

    /// Press `key`. Returns what to show on the screen afterwards (a
    /// control's new level).
    pub(super) fn press(&mut self, key: Key) -> Option<Knob> {
        match key {
            Key::Input(n) => self.input = n.min(2),
            Key::BlueOnly => self.blue_only = !self.blue_only,
            Key::UnderScan => self.under_scan = !self.under_scan,
            Key::HvDelay => self.hv_delay = !self.hv_delay,
            Key::AnalogDigital => self.digital = !self.digital,
            Key::Reset => {
                for v in &mut self.knobs[..5] {
                    *v = 0;
                }
            }
            Key::Up(k) | Key::Down(k) => {
                let (lo, hi) = k.range();
                let v = &mut self.knobs[knob_index(k)];
                *v = if matches!(key, Key::Up(_)) {
                    (*v + 1).min(hi)
                } else {
                    (*v - 1).max(lo)
                };
                return Some(k);
            }
            Key::Power => self.power = !self.power,
        }
        None
    }

    /// The speaker's gain: 1.5 dB a step about the preset, nothing at 0
    /// or with the power off.
    pub(super) fn speaker_gain(&self) -> f32 {
        let v = self.knob(Knob::Vol);
        if !self.power || v <= 0 {
            return 0.0;
        }
        10f32.powf(f32::from(v - VOL_PRESET) * 1.5 / 20.0)
    }

    /// The button is lit (latched in).
    pub(super) fn lit(&self, key: Key) -> bool {
        match key {
            Key::Input(n) => self.input == n,
            Key::BlueOnly => self.blue_only,
            Key::UnderScan => self.under_scan,
            Key::HvDelay => self.hv_delay,
            Key::AnalogDigital => self.digital,
            _ => false,
        }
    }

    fn to_json(self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "power": self.power,
            "input": self.input,
            "blue_only": self.blue_only,
            "under_scan": self.under_scan,
            "hv_delay": self.hv_delay,
            "digital": self.digital,
        });
        for k in Knob::ALL {
            v[k.label().to_ascii_lowercase()] = serde_json::json!(self.knob(k));
        }
        v
    }

    fn from_json(v: &serde_json::Value) -> Self {
        let mut s = Self::default();
        let flag = |name: &str, d: bool| v.get(name).and_then(|x| x.as_bool()).unwrap_or(d);
        s.power = flag("power", s.power);
        s.blue_only = flag("blue_only", false);
        s.under_scan = flag("under_scan", false);
        s.hv_delay = flag("hv_delay", false);
        s.digital = flag("digital", false);
        s.input = v
            .get("input")
            .and_then(|x| x.as_u64())
            .map_or(0, |n| n.min(2) as u8);
        for k in Knob::ALL {
            if let Some(n) = v
                .get(k.label().to_ascii_lowercase())
                .and_then(|x| x.as_i64())
            {
                let (lo, hi) = k.range();
                s.knobs[knob_index(k)] = n.clamp(i64::from(lo), i64::from(hi)) as i8;
            }
        }
        s
    }
}

fn settings_file(media_root: &std::path::Path) -> std::path::PathBuf {
    media_root.join(".copperline").join("monitors.json")
}

/// Output `name`'s monitor settings, as last left.
pub(super) fn load(media_root: Option<&std::path::Path>, name: &str) -> Settings {
    media_root
        .and_then(|r| std::fs::read_to_string(settings_file(r)).ok())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get(name).map(Settings::from_json))
        .unwrap_or_default()
}

/// Keep output `name`'s monitor settings for next time.
pub(super) fn save(media_root: Option<&std::path::Path>, name: &str, s: Settings) {
    let Some(root) = media_root else {
        return;
    };
    let path = settings_file(root);
    let mut all = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    all[name] = s.to_json();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, all.to_string()) {
        log::warn!("toaster monitors: {}: {e}", path.display());
    }
}

/// How long the tube takes to warm up, and to go dark.
pub(super) const WARM_UP: f32 = 2.6;
pub(super) const COOL_DOWN: f32 = 1.7;

/// Where the beam is in a power change: the raster's size about the
/// centre, its brightness, a ripple left by the degaussing field, and the
/// dot a dying tube leaves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Beam {
    pub(super) scale: (f32, f32),
    pub(super) gain: f32,
    /// Ripple, in picture pixels, and its phase.
    pub(super) ripple: (f32, f32),
    /// A dot at the centre: radius (fraction of the picture's width) and
    /// brightness.
    pub(super) dot: Option<(f32, f32)>,
}

impl Beam {
    pub(super) const STEADY: Beam = Beam {
        scale: (1.0, 1.0),
        gain: 1.0,
        ripple: (0.0, 0.0),
        dot: None,
    };

    /// `t` seconds after the power went `on` (or off). `None` once there
    /// is nothing on the tube (off and cooled).
    pub(super) fn at(on: bool, t: f32) -> Option<Beam> {
        let smooth = |x: f32| {
            let x = x.clamp(0.0, 1.0);
            x * x * (3.0 - 2.0 * x)
        };
        if on {
            if t >= WARM_UP {
                return Some(Self::STEADY);
            }
            // Dark while the heaters warm, then the picture comes up,
            // swelling to size with the high voltage, the last of the
            // degaussing field still rippling it.
            let up = smooth((t - 0.7) / (WARM_UP - 0.7));
            let ripple = 5.0 * (1.0 - up) * (-(t - 0.7).max(0.0) / 0.9).exp();
            return Some(Beam {
                scale: (0.93 + 0.07 * up, 0.93 + 0.07 * up),
                gain: up,
                ripple: (ripple, t * 37.0),
                dot: None,
            });
        }
        if t >= COOL_DOWN {
            return None;
        }
        Some(if t < 0.06 {
            // The high voltage sags: the picture swells and brightens.
            let k = t / 0.06;
            Beam {
                scale: (1.0 + 0.03 * k, 1.0 + 0.03 * k),
                gain: 1.0 + 0.3 * k,
                ripple: (0.0, 0.0),
                dot: None,
            }
        } else if t < 0.26 {
            // The field scan stops first: down to a line.
            let k = smooth((t - 0.06) / 0.2);
            Beam {
                scale: (1.03, (1.03 * (1.0 - k)).max(0.004)),
                gain: 1.3 + 1.7 * k,
                ripple: (0.0, 0.0),
                dot: None,
            }
        } else if t < 0.40 {
            // Then the line scan: down to a dot.
            let k = smooth((t - 0.26) / 0.14);
            Beam {
                scale: ((1.03 * (1.0 - k)).max(0.01), 0.004),
                gain: 3.0,
                ripple: (0.0, 0.0),
                dot: Some((0.012, k)),
            }
        } else {
            // The dot, fading as the tube's charge drains.
            let k = (t - 0.40) / (COOL_DOWN - 0.40);
            Beam {
                scale: (0.0, 0.0),
                gain: 0.0,
                ripple: (0.0, 0.0),
                dot: Some((0.012 - 0.006 * k, (1.0 - k).powi(2))),
            }
        })
    }
}

/// When the power last changed, for the painter.
#[derive(Clone, Copy, Debug)]
pub(super) struct PowerEdge {
    pub(super) on: bool,
    pub(super) at: Instant,
}

/// What the settings do to the signal: a colour matrix and offset in
/// 12-bit fixed point, the peaking, the gun, the delay.
pub(super) struct Look {
    matrix: [[i32; 3]; 3],
    offset: i32,
    /// Peaking, 8-bit fixed point: positive sharpens, negative softens.
    peak: i32,
    blue_only: bool,
    hv_delay: bool,
}

impl Look {
    pub(super) fn of(s: &Settings) -> Look {
        let contrast = 1.0 + 0.05 * f32::from(s.knob(Knob::Contrast));
        let bright = 6.0 * f32::from(s.knob(Knob::Bright));
        let chroma = 1.0 + 0.1 * f32::from(s.knob(Knob::Chroma));
        let phase = f32::from(s.knob(Knob::Phase)).to_radians() * 3.0;
        let aperture = f32::from(s.knob(Knob::Aperture));
        // RGB to YIQ, adjust, back.
        let to_yiq = [
            [0.299, 0.587, 0.114],
            [0.596, -0.274, -0.322],
            [0.211, -0.523, 0.312],
        ];
        // The exact inverse, so the presets give back the picture as it
        // came.
        let to_rgb = invert(to_yiq);
        let (sin, cos) = phase.sin_cos();
        let c = chroma * contrast;
        let adjust = [
            [contrast, 0.0, 0.0],
            [0.0, c * cos, -c * sin],
            [0.0, c * sin, c * cos],
        ];
        let mul = |a: [[f32; 3]; 3], b: [[f32; 3]; 3]| {
            let mut m = [[0.0f32; 3]; 3];
            for (i, row) in m.iter_mut().enumerate() {
                for (j, v) in row.iter_mut().enumerate() {
                    *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
                }
            }
            m
        };
        let m = mul(to_rgb, mul(adjust, to_yiq));
        let mut matrix = [[0i32; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                matrix[i][j] = (m[i][j] * 4096.0).round() as i32;
            }
        }
        let peak = if aperture >= 0.0 {
            aperture * 0.08
        } else {
            (aperture * 0.05).max(-0.5)
        };
        Look {
            matrix,
            offset: bright.round() as i32,
            peak: (peak * 256.0).round() as i32,
            blue_only: s.blue_only,
            hv_delay: s.hv_delay,
        }
    }

    /// The settings leave the picture as it is.
    pub(super) fn is_plain(&self) -> bool {
        self.offset == 0
            && self.peak == 0
            && !self.blue_only
            && !self.hv_delay
            && self.matrix == [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]]
    }
}

fn invert(m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let mut inv = [[0.0f32; 3]; 3];
    for (i, row) in inv.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            // The cofactor of m[j][i].
            let (a, b) = ((j + 1) % 3, (j + 2) % 3);
            let (c, d) = ((i + 1) % 3, (i + 2) % 3);
            *v = (m[a][c] * m[b][d] - m[a][d] * m[b][c]) / det;
        }
    }
    inv
}

/// A picture column or row's source: a pixel of the picture, blanking
/// (black level), or sync or no raster at all (no light whatever the
/// controls).
const BLANK: i32 = -1;
const DARK: i32 = -2;

/// Where each of `n` display positions comes from: the beam's `scale`
/// about the centre, then (`delay`) the raster shifted by half a line or
/// field, `blank` positions of blanking between, `sync` of them sync from
/// `sync_at`.
fn source_map(n: usize, scale: f32, delay: Option<(f32, f32, f32)>) -> Vec<i32> {
    let c = n as f32 / 2.0;
    let (blank, sync_at, sync) = delay.map_or((0, 0, 0), |(b, s0, s1)| {
        let b = (n as f32 * b).round() as usize;
        (b, (b as f32 * s0) as usize, (b as f32 * s1) as usize)
    });
    let raster = n + blank;
    (0..n)
        .map(|x| {
            if scale <= 0.0 {
                return DARK;
            }
            let u = c + (x as f32 + 0.5 - c) / scale;
            if u < 0.0 || u >= n as f32 {
                return DARK;
            }
            let mut u = u as usize;
            if delay.is_some() {
                u = (u + raster / 2) % raster;
            }
            if u < n {
                u as i32
            } else if (sync_at..sync_at + sync).contains(&(u - n)) {
                DARK
            } else {
                BLANK
            }
        })
        .collect()
}

/// The picture `src` (`vw` x `vh` RGBA) as the tube shows it with `look`
/// and `beam`, into `out`.
pub(super) fn process(
    src: &[u32],
    vw: usize,
    vh: usize,
    look: &Look,
    beam: &Beam,
    out: &mut Vec<u32>,
) {
    out.clear();
    out.resize(vw * vh, u32::from_le_bytes([0, 0, 0, 0xFF]));
    if vw == 0 || vh == 0 || src.len() < vw * vh {
        return;
    }
    // NTSC: horizontal blanking is about a fifth of the active line,
    // sync 4.7 of its 10.9 us after a 1.5 us front porch; vertical
    // blanking about 21 lines a field, the broad pulses lines 4 to 9.
    let cols = source_map(
        vw,
        beam.scale.0,
        look.hv_delay.then_some((0.207, 0.14, 0.43)),
    );
    let rows = source_map(
        vh,
        beam.scale.1,
        look.hv_delay.then_some((0.09, 0.14, 0.27)),
    );
    let gain = (beam.gain * 256.0).round() as i32;
    let m = &look.matrix;
    let light = |r: i32, g: i32, b: i32| -> u32 {
        let mut o = [0i32; 3];
        for (i, v) in o.iter_mut().enumerate() {
            let x = (m[i][0] * r + m[i][1] * g + m[i][2] * b + 2048) >> 12;
            *v = (((x + look.offset) * gain) >> 8).clamp(0, 255);
        }
        if look.blue_only {
            o[0] = 0;
            o[1] = 0;
        }
        u32::from_le_bytes([o[0] as u8, o[1] as u8, o[2] as u8, 0xFF])
    };
    let blank = light(0, 0, 0);
    let ch = |p: u32, s: u32| ((p >> s) & 0xFF) as i32;
    for (y, &sy) in rows.iter().enumerate() {
        let line = &mut out[y * vw..(y + 1) * vw];
        if sy == DARK {
            continue;
        }
        let shift = if beam.ripple.0 > 0.0 {
            (beam.ripple.0 * (y as f32 * 0.045 + beam.ripple.1).sin()).round() as i32
        } else {
            0
        };
        for (x, o) in line.iter_mut().enumerate() {
            let cx = x as i32 - shift;
            let sx = if cx < 0 || cx >= vw as i32 {
                DARK
            } else {
                cols[cx as usize]
            };
            *o = if sx == DARK {
                continue;
            } else if sy == BLANK || sx == BLANK {
                blank
            } else {
                let row = &src[sy as usize * vw..(sy as usize + 1) * vw];
                let sx = sx as usize;
                let p = row[sx];
                let (mut r, mut g, mut b) = (ch(p, 0), ch(p, 8), ch(p, 16));
                if look.peak != 0 {
                    let l = row[sx.saturating_sub(1)];
                    let rr = row[(sx + 1).min(vw - 1)];
                    let pk =
                        |c: i32, s: u32| c + ((look.peak * (2 * c - ch(l, s) - ch(rr, s))) >> 8);
                    (r, g, b) = (pk(r, 0), pk(g, 8), pk(b, 16));
                }
                light(r, g, b)
            };
        }
    }
    if let Some((radius, level)) = beam.dot {
        let r = (radius * vw as f32).max(1.0);
        let (cx, cy) = (vw as f32 / 2.0, vh as f32 / 2.0);
        let reach = r * 3.0;
        let y0 = (cy - reach).max(0.0) as usize;
        let y1 = ((cy + reach) as usize).min(vh);
        let x0 = (cx - reach).max(0.0) as usize;
        let x1 = ((cx + reach) as usize).min(vw);
        for y in y0..y1 {
            for x in x0..x1 {
                let d2 = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)) / (r * r);
                let add = (255.0 * level * (-d2).exp()) as i32;
                let p = &mut out[y * vw + x];
                let b = p.to_le_bytes();
                let mix = |c: u8| (i32::from(c) + add).min(255) as u8;
                *p = u32::from_le_bytes([mix(b[0]), mix(b[1]), mix(b[2]), 0xFF]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> u32 {
        u32::from_le_bytes([r, g, b, 0xFF])
    }

    #[test]
    fn the_presets_leave_the_picture_alone() {
        let look = Look::of(&Settings::default());
        assert!(look.is_plain());
        let src: Vec<u32> = (0..64u32)
            .map(|i| rgb(i as u8 * 4, 200 - i as u8, 17))
            .collect();
        let mut out = Vec::new();
        process(&src, 8, 8, &look, &Beam::STEADY, &mut out);
        assert_eq!(out, src);
    }

    #[test]
    fn the_controls_change_the_picture_as_a_monitor_does() {
        let mut s = Settings::default();
        let grey = vec![rgb(100, 100, 100); 16];
        let red = vec![rgb(200, 40, 40); 16];
        let mut out = Vec::new();
        let at = |s: &Settings, src: &[u32], out: &mut Vec<u32>| {
            process(src, 4, 4, &Look::of(s), &Beam::STEADY, out);
            out[5].to_le_bytes()
        };
        s.press(Key::Up(Knob::Bright));
        assert_eq!(at(&s, &grey, &mut out), [106, 106, 106, 0xFF]);
        s.press(Key::Reset);
        for _ in 0..4 {
            s.press(Key::Up(Knob::Contrast));
        }
        assert_eq!(at(&s, &grey, &mut out), [120, 120, 120, 0xFF]);
        s.press(Key::Reset);
        for _ in 0..10 {
            s.press(Key::Down(Knob::Chroma));
        }
        let p = at(&s, &red, &mut out);
        assert!(
            p[0].abs_diff(p[1]) <= 1 && p[1].abs_diff(p[2]) <= 1,
            "{p:?}"
        );
        s.press(Key::Reset);
        s.press(Key::BlueOnly);
        let p = at(&s, &red, &mut out);
        assert_eq!((p[0], p[1]), (0, 0));
        assert!(s.at_presets());
        s.press(Key::Down(Knob::Vol));
        assert!(s.speaker_gain() < 1.0 && s.speaker_gain() > 0.5);
        s.press(Key::Power);
        assert_eq!(s.speaker_gain(), 0.0);
    }

    #[test]
    fn hv_delay_puts_blanking_and_sync_across_the_middle() {
        let s = Settings {
            hv_delay: true,
            ..Settings::default()
        };
        let (w, h) = (100, 60);
        let src = vec![rgb(200, 200, 200); w * h];
        let mut out = Vec::new();
        process(&src, w, h, &Look::of(&s), &Beam::STEADY, &mut out);
        let row: Vec<u8> = (0..w).map(|x| out[5 * w + x].to_le_bytes()[0]).collect();
        let dark = row.iter().filter(|&&v| v == 0).count();
        assert!((15..=25).contains(&dark), "{dark} dark of {w}");
        assert_eq!(row[0], 200);
        assert_eq!(row[w - 1], 200);
    }

    #[test]
    fn the_tube_warms_up_and_collapses_to_a_dot() {
        assert_eq!(Beam::at(true, 0.1).map(|b| b.gain), Some(0.0));
        assert_eq!(Beam::at(true, WARM_UP), Some(Beam::STEADY));
        let line = Beam::at(false, 0.25).unwrap();
        assert!(line.scale.1 < 0.05 && line.scale.0 > 1.0);
        let dot = Beam::at(false, 0.8).unwrap();
        assert!(dot.dot.is_some() && dot.gain == 0.0);
        assert_eq!(Beam::at(false, COOL_DOWN), None);
        // The dot is drawn bright at the centre of a black tube.
        let (w, h) = (400, 300);
        let src = vec![rgb(90, 90, 90); w * h];
        let mut out = Vec::new();
        process(&src, w, h, &Look::of(&Settings::default()), &dot, &mut out);
        assert!(out[150 * w + 200].to_le_bytes()[0] > 100);
        assert_eq!(out[0].to_le_bytes()[0], 0);
    }

    #[test]
    fn the_settings_survive_a_round_trip() {
        let dir = std::env::temp_dir().join(format!("cl-monitors-{}", std::process::id()));
        let mut s = Settings::default();
        s.press(Key::HvDelay);
        s.press(Key::Down(Knob::Phase));
        s.press(Key::Input(2));
        save(Some(&dir), "Program", s);
        save(Some(&dir), "Preview", Settings::default());
        assert_eq!(load(Some(&dir), "Program"), s);
        assert_eq!(load(Some(&dir), "Preview"), Settings::default());
        assert_eq!(load(None, "Program"), Settings::default());
        let _ = std::fs::remove_dir_all(dir);
    }
}
