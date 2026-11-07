// SPDX-License-Identifier: GPL-3.0-or-later

//! Monitor windows for a video-slot card's own outputs: the Video Toaster's
//! Preview and Program, each on a picture monitor of its own.
//!
//! Each output gets a host window showing its monitor's artwork
//! (`assets/toaster/monitor-program.png` and `monitor-preview.png`, or
//! `monitor-<output>.png` or `monitor.png` in the Toaster files folder)
//! with the output's latest frame on the tube: scaled into the
//! glass, which is found in the artwork by flooding outward from its centre
//! to the dark gap around the tube, and lit by the glass itself so its
//! shading and reflection stay on top of the picture.
//!
//! Composition is on the CPU at the window's own size (the artwork is
//! resampled once per resize), presented through a `pixels` surface of
//! the same size. Each tube pixel's filter taps and glass lighting are
//! worked out once per window and picture size, in fixed point, so a new
//! frame is one pass of integer arithmetic straight into the surface's
//! buffer, split across threads. Each window has a painter thread of its
//! own that waits for the output's next frame, composes it and presents
//! it, so none of this is on the emulator's thread: the machine runs at
//! its own pace whatever the windows' size, and a painter that falls
//! behind skips to the latest frame. The event loop keeps the windows'
//! input and tells the painter about resizes and repaint requests.
//!
//! The front panels work ([`super::monitor_panel`]): the picture
//! controls, BLUE ONLY, UNDER SCAN, H-V DELAY, the inputs, VOL (the
//! Program monitor's speaker, the card's sound) and POWER, with the tube
//! warming up or collapsing to a dot and the sounds of it
//! ([`super::monitor_sounds`]). In the Program window, `R` or the REC
//! button starts and stops a recording of the output, and SETUP opens its
//! settings.

use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use log::{info, warn};
use pixels::{Pixels, PixelsBuilder, SurfaceTexture};
use winit::{
    dpi::{LogicalPosition, LogicalSize, PhysicalPosition},
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowAttributes, WindowId},
};

use super::monitor_panel::{self, Beam, Key, Knob, Look, Panel, PowerEdge, Settings};
use super::monitor_sounds;
use crate::videoslot::{CardOutput, RecordFormat, RecordSettings};

/// A built-in monitor: its artwork, and where its front panel's buttons
/// and lamps are. An artwork supplied in place of it has no panel.
struct BuiltIn {
    png: &'static [u8],
    panel: &'static Panel,
}

/// The Program monitor, and the Preview monitor (also for any other
/// output).
const PROGRAM_ART: BuiltIn = BuiltIn {
    png: include_bytes!("../../../assets/toaster/monitor-program.png"),
    panel: &monitor_panel::PROGRAM_PANEL,
};
const PREVIEW_ART: BuiltIn = BuiltIn {
    png: include_bytes!("../../../assets/toaster/monitor-preview.png"),
    panel: &monitor_panel::PREVIEW_PANEL,
};

/// How far the picture runs past the glass on each side, as a fraction of
/// the glass: a monitor overscans.
const OVERSCAN: f32 = 0.03;
/// UNDER SCAN: the whole raster inside the glass, a margin all round.
const UNDERSCAN: f32 = -0.06;

/// The monitor artwork and the tube found in it.
pub(super) struct MonitorArt {
    w: usize,
    h: usize,
    rgba: Vec<u32>,
    /// 255 on the glass, 0 elsewhere.
    mask: Vec<u8>,
    /// The glass's own brightness, for shading the picture.
    glass: Vec<u8>,
    /// The glass's bounding box, `(x0, y0, x1, y1)`, exclusive.
    screen: (usize, usize, usize, usize),
    panel: Option<&'static Panel>,
}

fn rgba_u32(r: u8, g: u8, b: u8) -> u32 {
    u32::from_le_bytes([r, g, b, 0xFF])
}

impl MonitorArt {
    /// The artwork for output `name`: `monitor-<name>.png` or
    /// `monitor.png` in `media_root` if there is one, else the built-in
    /// one.
    pub(super) fn load(media_root: Option<&std::path::Path>, name: &str) -> Option<Arc<Self>> {
        let own = format!("monitor-{}.png", name.to_ascii_lowercase());
        for file in [own.as_str(), "monitor.png"] {
            let Some(path) = media_root.map(|r| r.join(file)) else {
                break;
            };
            if path.is_file() {
                match std::fs::read(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| Self::decode(&bytes))
                {
                    Ok(art) => {
                        info!("toaster monitors: artwork {}", path.display());
                        return Some(Arc::new(art));
                    }
                    Err(e) => warn!("toaster monitors: {}: {e}", path.display()),
                }
            }
        }
        let built_in = if name == "Program" {
            &PROGRAM_ART
        } else {
            &PREVIEW_ART
        };
        match Self::decode(built_in.png).map(|mut art| {
            art.panel = Some(built_in.panel);
            art
        }) {
            Ok(art) => Some(Arc::new(art)),
            Err(e) => {
                warn!("toaster monitors: built-in artwork: {e}");
                None
            }
        }
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let img = image::load_from_memory(bytes)
            .map_err(|e| e.to_string())?
            .to_rgba8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let rgba: Vec<u32> = img
            .as_raw()
            .chunks_exact(4)
            .map(|p| rgba_u32(p[0], p[1], p[2]))
            .collect();
        let (mask, screen) = find_glass(&rgba, w, h).ok_or("no tube face found in the artwork")?;
        let glass = rgba
            .iter()
            .zip(&mask)
            .map(|(&p, &m)| {
                if m == 0 {
                    0
                } else {
                    let [r, g, b, _] = p.to_le_bytes();
                    r.max(g).max(b)
                }
            })
            .collect();
        Ok(Self {
            w,
            h,
            rgba,
            mask,
            glass,
            screen,
            panel: None,
        })
    }

    pub(super) fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }
}

/// A rectangle `(x0, y0, x1, y1)`, exclusive.
type Rect = (usize, usize, usize, usize);

/// The tube face: everything reachable from the artwork's centre without
/// crossing a near-black pixel (the shadow gap between glass and bezel).
fn find_glass(rgba: &[u32], w: usize, h: usize) -> Option<(Vec<u8>, Rect)> {
    let bright = |i: usize| {
        let [r, g, b, _] = rgba[i].to_le_bytes();
        r.max(g).max(b) > 10
    };
    // The tube sits in the upper part of a monitor front; start from the
    // middle of the upper 80 %, stepping down to the first lit pixel.
    let cx = w / 2;
    let start = (h * 2 / 5..h * 3 / 5)
        .map(|y| y * w + cx)
        .find(|&i| bright(i))?;
    let mut mask = vec![0u8; w * h];
    let mut stack = vec![start];
    mask[start] = 255;
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x + 1);
        y1 = y1.max(y + 1);
        let mut visit = |j: usize| {
            if mask[j] == 0 && bright(j) {
                mask[j] = 255;
                stack.push(j);
            }
        };
        if x > 0 {
            visit(i - 1);
        }
        if x + 1 < w {
            visit(i + 1);
        }
        if y > 0 {
            visit(i - w);
        }
        if y + 1 < h {
            visit(i + w);
        }
    }
    // A flood that leaked into the whole front found no tube.
    if (x1 - x0) * (y1 - y0) > w * h * 9 / 10 || x1 - x0 < w / 8 {
        return None;
    }
    Some((mask, (x0, y0, x1, y1)))
}

/// Box-filter `src` (`sw` x `sh` RGBA) down, or bilinear-filter it up, to
/// `dw` x `dh`.
fn resample(src: &[u32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u32> {
    let mut out = vec![0u32; dw * dh];
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return out;
    }
    let fx = sw as f32 / dw as f32;
    let fy = sh as f32 / dh as f32;
    for dy in 0..dh {
        let sy0 = dy as f32 * fy;
        let sy1 = (sy0 + fy.max(1.0)).min(sh as f32);
        for dx in 0..dw {
            let sx0 = dx as f32 * fx;
            let sx1 = (sx0 + fx.max(1.0)).min(sw as f32);
            let mut acc = [0f32; 3];
            let mut weight = 0f32;
            let mut y = sy0.floor() as usize;
            while (y as f32) < sy1 && y < sh {
                let wy = ((y + 1) as f32).min(sy1) - (y as f32).max(sy0);
                let mut x = sx0.floor() as usize;
                while (x as f32) < sx1 && x < sw {
                    let wx = ((x + 1) as f32).min(sx1) - (x as f32).max(sx0);
                    let wgt = wx * wy;
                    let [r, g, b, _] = src[y * sw + x].to_le_bytes();
                    acc[0] += f32::from(r) * wgt;
                    acc[1] += f32::from(g) * wgt;
                    acc[2] += f32::from(b) * wgt;
                    weight += wgt;
                    x += 1;
                }
                y += 1;
            }
            let c = |v: f32| (v / weight.max(1e-6)).round().clamp(0.0, 255.0) as u8;
            out[dy * dw + dx] = rgba_u32(c(acc[0]), c(acc[1]), c(acc[2]));
        }
    }
    out
}

fn resample_u8(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let as_rgba: Vec<u32> = src.iter().map(|&v| rgba_u32(v, v, v)).collect();
    resample(&as_rgba, sw, sh, dw, dh)
        .into_iter()
        .map(|p| p.to_le_bytes()[0])
        .collect()
}

/// One tube pixel's recipe: the 2 x 2 of picture pixels it filters and how
/// the glass lights it, in fixed point.
#[derive(Clone, Copy, Debug)]
struct Tap {
    /// Window pixel.
    i: u32,
    /// Picture pixel at the top left of the 2 x 2.
    base: u32,
    /// The right and lower neighbours are one on (0 at the picture's edge).
    dx: u8,
    dy: u8,
    /// Filter position within the 2 x 2, in 256ths.
    tx: u8,
    ty: u8,
    /// The glass's shading (256 = 1) and sheen.
    shade: u16,
    sheen: u8,
    /// Coverage: 255 inside the glass, less on its antialiased edge.
    a: u8,
}

/// The taps of every tube pixel for one picture size, in window order, and
/// where each window row's run starts.
struct Table {
    vw: usize,
    vh: usize,
    taps: Vec<Tap>,
    /// `rows[r]`: the first tap on window row `screen.1 + r`.
    rows: Vec<usize>,
}

/// Tube pixels below which a frame is composed on one thread.
const PARALLEL_TAPS: usize = 60_000;

/// The artwork laid out for one window size.
pub(super) struct Layer {
    w: usize,
    h: usize,
    bg: Vec<u32>,
    /// `bg` as the surface's bytes.
    bg_bytes: Vec<u8>,
    /// The taps for the last picture size composed.
    table: std::sync::Mutex<Option<Arc<Table>>>,
    mask: Vec<u8>,
    glass: Vec<u8>,
    screen: (usize, usize, usize, usize),
    /// Where the picture lands: the glass grown by the overscan (or
    /// shrunk by the underscan), at 4:3 across the glass's width, `(x, y,
    /// w, h)` in window pixels.
    picture: (f32, f32, f32, f32),
    under_scan: bool,
}

/// How the artwork fits a `w` x `h` window, centred: `(scale, aw, ah, ox,
/// oy)`, its size and top-left corner in window pixels.
fn fit(art: &MonitorArt, w: usize, h: usize) -> (f32, usize, usize, usize, usize) {
    let w = w.max(1);
    let h = h.max(1);
    let scale = (w as f32 / art.w as f32).min(h as f32 / art.h as f32);
    let aw = ((art.w as f32 * scale).round() as usize).clamp(1, w);
    let ah = ((art.h as f32 * scale).round() as usize).clamp(1, h);
    (scale, aw, ah, (w - aw) / 2, (h - ah) / 2)
}

/// A rectangle in the artwork's pixels, in a `w` x `h` window's.
fn art_px_rect(art: &MonitorArt, w: usize, h: usize, r: (u32, u32, u32, u32)) -> Rect4 {
    let (scale, _, _, ox, oy) = fit(art, w, h);
    (
        ox as f32 + r.0 as f32 * scale,
        oy as f32 + r.1 as f32 * scale,
        ox as f32 + r.2 as f32 * scale,
        oy as f32 + r.3 as f32 * scale,
    )
}

/// A lamp (centre and radius in the artwork's pixels) in window pixels.
fn art_px_lamp(art: &MonitorArt, w: usize, h: usize, l: (f32, f32, f32)) -> (f32, f32, f32) {
    let (scale, _, _, ox, oy) = fit(art, w, h);
    (
        ox as f32 + l.0 * scale,
        oy as f32 + l.1 * scale,
        l.2 * scale,
    )
}

/// The front-panel button under `(x, y)` in a `w` x `h` window.
fn key_at(art: &MonitorArt, w: usize, h: usize, x: f64, y: f64) -> Option<Key> {
    art.panel?
        .keys
        .iter()
        .find(|(_, r)| inside(art_px_rect(art, w, h, *r), x, y))
        .map(|(k, _)| *k)
}

impl Layer {
    pub(super) fn new(art: &MonitorArt, w: usize, h: usize, aspect: f32, under_scan: bool) -> Self {
        let w = w.max(1);
        let h = h.max(1);
        // Fit the artwork, centred, black around it.
        let (scale, aw, ah, ox, oy) = fit(art, w, h);
        let art_bg = resample(&art.rgba, art.w, art.h, aw, ah);
        let art_mask = resample_u8(&art.mask, art.w, art.h, aw, ah);
        let art_glass = resample_u8(&art.glass, art.w, art.h, aw, ah);
        let mut bg = vec![rgba_u32(0, 0, 0); w * h];
        let mut mask = vec![0u8; w * h];
        let mut glass = vec![0u8; w * h];
        for y in 0..ah {
            let src = y * aw;
            let dst = (y + oy) * w + ox;
            bg[dst..dst + aw].copy_from_slice(&art_bg[src..src + aw]);
            mask[dst..dst + aw].copy_from_slice(&art_mask[src..src + aw]);
            glass[dst..dst + aw].copy_from_slice(&art_glass[src..src + aw]);
        }
        let map = |x: f32, y: f32| (ox as f32 + x * scale, oy as f32 + y * scale);
        let (sx0, sy0) = map(art.screen.0 as f32, art.screen.1 as f32);
        let (sx1, sy1) = map(art.screen.2 as f32, art.screen.3 as f32);
        let screen = (
            (sx0.floor() as usize).min(w),
            (sy0.floor() as usize).min(h),
            (sx1.ceil() as usize).min(w),
            (sy1.ceil() as usize).min(h),
        );
        let scan = if under_scan { UNDERSCAN } else { OVERSCAN };
        let gw = (sx1 - sx0) * (1.0 + 2.0 * scan);
        let gh = gw / aspect.max(0.1);
        let cx = (sx0 + sx1) / 2.0;
        let cy = (sy0 + sy1) / 2.0;
        let picture = (cx - gw / 2.0, cy - gh / 2.0, gw, gh);
        let bg_bytes = bg.iter().flat_map(|p| p.to_le_bytes()).collect();
        Self {
            w,
            h,
            bg,
            bg_bytes,
            table: std::sync::Mutex::new(None),
            mask,
            glass,
            screen,
            picture,
            under_scan,
        }
    }

    /// The taps for a `vw` x `vh` picture (made on first use).
    fn table(&self, vw: usize, vh: usize) -> Arc<Table> {
        let mut slot = self.table.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = slot.as_ref().filter(|t| (t.vw, t.vh) == (vw, vh)) {
            return Arc::clone(t);
        }
        let (px, py, pw, ph) = self.picture;
        let (x0, y0, x1, y1) = self.screen;
        let sx = vw as f32 / pw;
        let sy = vh as f32 / ph;
        let mut taps = Vec::new();
        let mut rows = Vec::with_capacity(y1.saturating_sub(y0) + 1);
        for y in y0..y1 {
            rows.push(taps.len());
            let raw_y = (y as f32 + 0.5 - py) * sy;
            let off_y = raw_y < 0.0 || raw_y >= vh as f32;
            let vy = (raw_y - 0.5).clamp(0.0, (vh - 1) as f32);
            let ya = vy.floor() as usize;
            let ty = vy - ya as f32;
            for x in x0..x1 {
                let i = y * self.w + x;
                let m = self.mask[i];
                if m == 0 {
                    continue;
                }
                let raw_x = (x as f32 + 0.5 - px) * sx;
                // Underscanned, the glass past the raster is unlit.
                let off = off_y || raw_x < 0.0 || raw_x >= vw as f32;
                let vx = (raw_x - 0.5).clamp(0.0, (vw - 1) as f32);
                let xa = vx.floor() as usize;
                let tx = vx - xa as f32;
                let g = f32::from(self.glass[i]);
                // The tube's own shading darkens toward its edges; whatever
                // is brighter than the bare glass (the reflection) lies on
                // top.
                let shade = if off {
                    0.0
                } else {
                    0.72 + 0.28 * (g / 45.0).min(1.0)
                };
                let sheen = (g - 50.0).max(0.0) * 0.9;
                let fixed = |t: f32| (t * 256.0).round().min(255.0) as u8;
                taps.push(Tap {
                    i: i as u32,
                    base: (ya * vw + xa) as u32,
                    dx: u8::from(xa + 1 < vw),
                    dy: u8::from(ya + 1 < vh),
                    tx: fixed(tx),
                    ty: fixed(ty),
                    shade: (shade * 256.0).round() as u16,
                    sheen: sheen.round().min(255.0) as u8,
                    a: m,
                });
            }
        }
        rows.push(taps.len());
        let table = Arc::new(Table { vw, vh, taps, rows });
        *slot = Some(Arc::clone(&table));
        table
    }

    /// The monitor with `video` (`vw` x `vh` RGBA) on the tube, into `out`
    /// (`w * h`), its power lamp lit. No video leaves the tube dark.
    pub(super) fn compose(
        &self,
        art: &MonitorArt,
        video: Option<(&[u32], usize, usize)>,
        out: &mut [u32],
    ) {
        let mut bytes = vec![0u8; self.w * self.h * 4];
        self.compose_rgba8(video, &mut bytes, true);
        if let Some(panel) = art.panel {
            let lamp = art_px_lamp(art, self.w, self.h, panel.power_lamp);
            draw_lamp(&mut bytes, self.w, self.h, lamp, POWER_LAMP);
        }
        for (o, b) in out.iter_mut().zip(bytes.chunks_exact(4)) {
            *o = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
    }

    /// [`Self::compose`] into a surface's RGBA bytes (`w * h * 4`), lamps
    /// unlit. With `background` false the artwork around the tube is
    /// taken to be there already (the frame before it was composed by this
    /// layer), and only the tube is drawn.
    pub(super) fn compose_rgba8(
        &self,
        video: Option<(&[u32], usize, usize)>,
        frame: &mut [u8],
        background: bool,
    ) {
        let len = self.w * self.h * 4;
        if frame.len() < len {
            return;
        }
        let frame = &mut frame[..len];
        if background || video.is_none() {
            frame.copy_from_slice(&self.bg_bytes);
        }
        if let Some((video, vw, vh)) =
            video.filter(|(v, vw, vh)| *vw > 0 && *vh > 0 && v.len() >= vw * vh)
        {
            let table = self.table(vw, vh);
            let y0 = self.screen.1;
            let rows = table.rows.len().saturating_sub(1);
            let threads = if table.taps.len() < PARALLEL_TAPS {
                1
            } else {
                std::thread::available_parallelism().map_or(1, |n| n.get().clamp(1, 4))
            };
            // Each thread a band of rows: its own run of taps and of bytes.
            let per = rows.div_ceil(threads).max(1);
            let row_bytes = self.w * 4;
            let mut rest = &mut frame[y0 * row_bytes..];
            let mut bands = Vec::new();
            for start in (0..rows).step_by(per) {
                let end = (start + per).min(rows);
                let (band, tail) = rest.split_at_mut((end - start) * row_bytes);
                rest = tail;
                let taps = &table.taps[table.rows[start]..table.rows[end]];
                bands.push((band, taps, (y0 + start) * self.w));
            }
            let run = |(band, taps, first): (&mut [u8], &[Tap], usize)| {
                for t in taps {
                    let at = (t.i as usize - first) * 4;
                    let bg = self.bg[t.i as usize];
                    band[at..at + 4].copy_from_slice(&tap_pixel(t, video, vw, bg));
                }
            };
            if bands.len() <= 1 {
                bands.into_iter().for_each(run);
            } else {
                std::thread::scope(|scope| {
                    let mut bands = bands.into_iter();
                    let first = bands.next();
                    for band in bands {
                        scope.spawn(move || run(band));
                    }
                    if let Some(band) = first {
                        run(band);
                    }
                });
            }
        }
    }
}

/// The power lamp's colour.
const POWER_LAMP: [u8; 3] = [0x50, 0xFF, 0x60];

/// Light a lamp (`(x, y, r)` in window pixels) with its glow.
fn draw_lamp(frame: &mut [u8], w: usize, h: usize, lamp: (f32, f32, f32), rgb: [u8; 3]) {
    let (lx, ly, r) = lamp;
    let r = r.max(1.5);
    let glow = r * 2.2;
    let ys = ((ly - glow).floor().max(0.0) as usize)..((ly + glow).ceil() as usize).min(h);
    for y in ys {
        let xs = ((lx - glow).floor().max(0.0) as usize)..((lx + glow).ceil() as usize).min(w);
        for x in xs {
            let d = ((x as f32 + 0.5 - lx).powi(2) + (y as f32 + 0.5 - ly).powi(2)).sqrt();
            let a = if d <= r {
                1.0
            } else {
                ((glow - d) / (glow - r)).clamp(0.0, 1.0) * 0.45
            };
            if a <= 0.0 {
                continue;
            }
            let i = (y * w + x) * 4;
            if i + 3 >= frame.len() {
                continue;
            }
            for c in 0..3 {
                frame[i + c] =
                    (f32::from(frame[i + c]) * (1.0 - a) + f32::from(rgb[c]) * a).round() as u8;
            }
            frame[i + 3] = 0xFF;
        }
    }
}

/// One tube pixel: the picture filtered at its tap, lit by the glass, over
/// the artwork by its coverage.
#[inline]
fn tap_pixel(t: &Tap, video: &[u32], vw: usize, bg: u32) -> [u8; 4] {
    let b = t.base as usize;
    let dx = usize::from(t.dx);
    let below = b + usize::from(t.dy) * vw;
    let (p00, p01, p10, p11) = (video[b], video[b + dx], video[below], video[below + dx]);
    let (tx, ty) = (u32::from(t.tx), u32::from(t.ty));
    let w00 = (256 - tx) * (256 - ty);
    let w01 = tx * (256 - ty);
    let w10 = (256 - tx) * ty;
    let w11 = tx * ty;
    let a = u32::from(t.a);
    let mut out = [0, 0, 0, 0xFF];
    for (c, o) in out.iter_mut().take(3).enumerate() {
        let s = 8 * c as u32;
        let ch = |p: u32| (p >> s) & 0xFF;
        let v = (ch(p00) * w00 + ch(p01) * w01 + ch(p10) * w10 + ch(p11) * w11 + 32768) >> 16;
        let v = ((v * u32::from(t.shade) + 128) >> 8) + u32::from(t.sheen);
        let v = if a == 255 {
            v
        } else {
            (v * a + ch(bg) * (255 - a) + 127) / 255
        };
        *o = v.min(255) as u8;
    }
    out
}

/// Compose `output`'s latest frame on the artwork at the artwork's own
/// size (for headless captures).
pub(super) fn compose_native(art: &MonitorArt, output: &CardOutput) -> (Vec<u32>, usize, usize) {
    let (w, h) = art.size();
    let layer = Layer::new(art, w, h, output.aspect(), false);
    let mut video = Vec::new();
    let frame = output.frame_since(u64::MAX, &mut video);
    let mut out = vec![0u32; w * h];
    layer.compose(
        art,
        frame.map(|(vw, vh, _)| (video.as_slice(), vw, vh)),
        &mut out,
    );
    (out, w, h)
}

struct MonitorWindow {
    output: Arc<CardOutput>,
    art: Arc<MonitorArt>,
    window: Arc<Window>,
    /// The thread that paints the window.
    painter: Painter,
    /// The window's size, for the controls' hit tests.
    size: (u32, u32),
    /// What the front panel is set to.
    settings: Settings,
    /// The up or down button held, and when it next repeats.
    held: Option<(Key, Instant)>,
    /// Which of the power sounds' two takes this monitor makes.
    variant: usize,
    /// The recording setup panel is open.
    setup: bool,
    /// What the title shows: recording, saving an MP4.
    shown: (bool, bool),
    cursor: (f64, f64),
}

/// What the event loop tells a window's painter.
enum Paint {
    Resized(u32, u32),
    /// Paint the window again (it was exposed, or the lamp changed).
    Redraw,
    /// Show or hide the recording setup panel.
    Setup(bool),
    /// The front panel's settings changed; show this control's level on
    /// the screen.
    Settings(Settings, Option<Knob>),
    /// The power went on or off.
    Power(PowerEdge),
    /// A front-panel button is held down (or none).
    Pressed(Option<Key>),
    Stop,
}

/// The handle on a window's painter thread.
struct Painter {
    tx: mpsc::Sender<Paint>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Painter {
    fn send(&self, msg: Paint) {
        let _ = self.tx.send(msg);
    }
}

impl Drop for Painter {
    fn drop(&mut self) {
        self.send(Paint::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// How long a painter waits for a frame before looking at its inbox.
const PAINT_WAIT: Duration = Duration::from_millis(5);

/// The painter's state: the surface and the layer for its size, the last
/// frame taken, and what the surface's buffer holds.
struct Canvas {
    output: Arc<CardOutput>,
    art: Arc<MonitorArt>,
    pixels: Pixels<'static>,
    layer: Option<Layer>,
    size: (u32, u32),
    seen: u64,
    video: Vec<u32>,
    video_size: (usize, usize),
    /// The surface's buffer holds the current layer's artwork with these
    /// controls drawn on it.
    painted: Option<Controls>,
    /// The recording setup panel is open.
    setup: bool,
    /// The front panel.
    settings: Settings,
    look: Look,
    /// The last power change, while the tube is warming or dying.
    edge: Option<PowerEdge>,
    pressed: Option<Key>,
    /// The control whose level is on the screen, and since when.
    osd: Option<(Knob, Instant)>,
    /// The picture as the front panel makes it.
    shown: Vec<u32>,
    last_paint: Instant,
}

/// What the window's controls show: recording, an MP4 being saved, the
/// setup panel and its settings, the front panel.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Controls {
    recording: bool,
    saving: bool,
    setup: bool,
    settings: RecordSettings,
    panel: Settings,
    pressed: Option<Key>,
    osd: Option<(Knob, i8)>,
}

/// How long a control's level stays on the screen.
const OSD_TIME: Duration = Duration::from_millis(2500);
/// How often the painter repaints while the tube warms or dies.
const ANIMATION_FRAME: Duration = Duration::from_millis(16);
/// A held up or down button repeats after this, then at this rate.
const REPEAT_DELAY: Duration = Duration::from_millis(450);
const REPEAT_RATE: Duration = Duration::from_millis(110);

/// A control on the Program monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Record,
    Setup,
    Format,
    Quality,
    Folder,
    Close,
}

/// The controls' places, as fractions of the artwork: the record and
/// setup buttons on the bezel's top strip, right of the badge; the setup
/// panel over the tube, a title and then one row per [`Control`] from
/// `Format` on.
const REC_BUTTON: (f32, f32, f32, f32) = (0.715, 0.017, 0.795, 0.058);
const SETUP_BUTTON: (f32, f32, f32, f32) = (0.805, 0.017, 0.900, 0.058);
const PANEL: (f32, f32, f32, f32) = (0.24, 0.22, 0.76, 0.66);
const PANEL_ROWS: usize = 5;

/// A rectangle given as fractions of the artwork, in window pixels.
fn art_rect(art: &MonitorArt, w: usize, h: usize, f: (f32, f32, f32, f32)) -> Rect4 {
    let (scale, _, _, ox, oy) = fit(art, w, h);
    let (aw, ah) = (art.w as f32 * scale, art.h as f32 * scale);
    (
        ox as f32 + f.0 * aw,
        oy as f32 + f.1 * ah,
        ox as f32 + f.2 * aw,
        oy as f32 + f.3 * ah,
    )
}

type Rect4 = (f32, f32, f32, f32);

/// The panel's row `k` (0 the title).
fn panel_row(art: &MonitorArt, w: usize, h: usize, k: usize) -> Rect4 {
    let (x0, y0, x1, y1) = art_rect(art, w, h, PANEL);
    let rh = (y1 - y0) / PANEL_ROWS as f32;
    (x0, y0 + rh * k as f32, x1, y0 + rh * (k + 1) as f32)
}

fn inside(r: Rect4, x: f64, y: f64) -> bool {
    let (x, y) = (x as f32, y as f32);
    x >= r.0 && x < r.2 && y >= r.1 && y < r.3
}

/// The control under `(x, y)` in a `w` x `h` window.
fn control_at(
    art: &MonitorArt,
    w: usize,
    h: usize,
    setup: bool,
    x: f64,
    y: f64,
) -> Option<Control> {
    if setup {
        for (k, c) in [
            Control::Format,
            Control::Quality,
            Control::Folder,
            Control::Close,
        ]
        .into_iter()
        .enumerate()
        {
            if inside(panel_row(art, w, h, k + 1), x, y) {
                return Some(c);
            }
        }
    }
    if inside(art_rect(art, w, h, REC_BUTTON), x, y) {
        return Some(Control::Record);
    }
    if inside(art_rect(art, w, h, SETUP_BUTTON), x, y) {
        return Some(Control::Setup);
    }
    None
}

/// Fill a rectangle of an RGBA8 frame.
fn fill(frame: &mut [u8], w: usize, h: usize, r: Rect4, rgba: [u8; 4]) {
    let x0 = (r.0.max(0.0) as usize).min(w);
    let x1 = (r.2.max(0.0) as usize).min(w);
    let y0 = (r.1.max(0.0) as usize).min(h);
    let y1 = (r.3.max(0.0) as usize).min(h);
    let a = u32::from(rgba[3]);
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * w + x) * 4;
            for c in 0..3 {
                let v = (u32::from(rgba[c]) * a + u32::from(frame[i + c]) * (255 - a) + 127) / 255;
                frame[i + c] = v as u8;
            }
            frame[i + 3] = 0xFF;
        }
    }
}

/// A raised button in the monitor's dark plastic, with a label.
fn plastic_button(
    frame: &mut [u8],
    w: usize,
    h: usize,
    r: Rect4,
    label: &str,
    lit: Option<[u8; 3]>,
) {
    let edge = ((r.3 - r.1) / 12.0).max(1.0);
    fill(frame, w, h, r, [0x1C, 0x1C, 0x1E, 0xFF]);
    fill(
        frame,
        w,
        h,
        (r.0, r.1, r.2, r.1 + edge),
        [0x6A, 0x6A, 0x6E, 0xFF],
    );
    fill(
        frame,
        w,
        h,
        (r.0, r.1, r.0 + edge, r.3),
        [0x6A, 0x6A, 0x6E, 0xFF],
    );
    fill(
        frame,
        w,
        h,
        (r.0, r.3 - edge, r.2, r.3),
        [0x08, 0x08, 0x08, 0xFF],
    );
    fill(
        frame,
        w,
        h,
        (r.2 - edge, r.1, r.2, r.3),
        [0x08, 0x08, 0x08, 0xFF],
    );
    let px = (((r.3 - r.1) * 0.42) / 8.0).floor().max(1.0) as usize;
    let mut tx = r.0 + (r.2 - r.0 - crate::video::font::text_width(label, px) as f32) / 2.0;
    if let Some(rgb) = lit {
        // A lamp before the label.
        let d = (px * 6) as f32;
        tx += d * 0.6;
        let cy = (r.1 + r.3) / 2.0;
        let lx = tx - d * 1.3;
        fill(
            frame,
            w,
            h,
            (lx, cy - d / 2.0, lx + d, cy + d / 2.0),
            [rgb[0], rgb[1], rgb[2], 0xFF],
        );
    }
    let ty = (r.1 + r.3) / 2.0 - (crate::video::font::text_height(px) as f32) / 2.0;
    crate::video::font::draw_text(
        frame,
        w,
        h,
        tx.max(0.0) as usize,
        ty.max(0.0) as usize,
        label,
        u32::from_le_bytes([0xD8, 0xD8, 0xD8, 0xFF]),
        px,
    );
}

/// Draw the front panel's lamps and lit buttons, and darken a button held
/// down, over the artwork.
fn draw_panel(frame: &mut [u8], art: &MonitorArt, w: usize, h: usize, c: &Controls) {
    let Some(panel) = art.panel else {
        return;
    };
    let s = &c.panel;
    if s.power {
        let lamp = art_px_lamp(art, w, h, panel.power_lamp);
        draw_lamp(frame, w, h, lamp, POWER_LAMP);
        if s.at_presets() {
            let lamp = art_px_lamp(art, w, h, panel.reset_lamp);
            draw_lamp(frame, w, h, lamp, [0x60, 0xFF, 0x70]);
        }
    }
    for &(key, r) in panel.keys {
        let rect = art_px_rect(art, w, h, r);
        if s.power && s.lit(key) {
            // The button's lamp shows through its cap.
            let inset = (rect.2 - rect.0) * 0.12;
            fill(
                frame,
                w,
                h,
                (
                    rect.0 + inset,
                    rect.1 + inset,
                    rect.2 - inset,
                    rect.3 - inset,
                ),
                [0xFF, 0xB0, 0x40, 0x70],
            );
        }
        if c.pressed == Some(key) {
            fill(frame, w, h, rect, [0, 0, 0, 0x60]);
        }
    }
}

/// Show a control's level on the screen, as the monitor's own display
/// does: its name and a bar, low on the tube.
fn draw_osd(frame: &mut [u8], w: usize, h: usize, screen: Rect, osd: (Knob, i8)) {
    let (knob, level) = osd;
    let (x0, y0, x1, y1) = screen;
    let (tw, th) = (x1.saturating_sub(x0) as f32, y1.saturating_sub(y0) as f32);
    if tw < 40.0 || th < 40.0 {
        return;
    }
    let px = ((th / 260.0).floor() as usize).max(1);
    let green = [0x50, 0xFF, 0x60, 0xFF];
    let (lo, hi) = knob.range();
    let label = if knob == Knob::Vol {
        format!("{}  {level}", knob.label())
    } else {
        format!("{}  {level:+}", knob.label())
    };
    let text_w = crate::video::font::text_width(&label, px) as f32;
    let text_h = crate::video::font::text_height(px) as f32;
    let cx = x0 as f32 + tw / 2.0;
    let ty = y0 as f32 + th * 0.70;
    for (dx, dy, rgba) in [(px, px, [0, 0, 0, 0xFF]), (0, 0, green)] {
        crate::video::font::draw_text(
            frame,
            w,
            h,
            (cx - text_w / 2.0) as usize + dx,
            ty as usize + dy,
            &label,
            u32::from_le_bytes(rgba),
            px,
        );
    }
    // The bar: a cell a step, lit up to the level.
    let steps = (hi - lo) as usize;
    let bar_w = tw * 0.46;
    let cell = bar_w / steps as f32;
    let by = ty + text_h * 1.6;
    let bh = text_h * 0.9;
    let bx = cx - bar_w / 2.0;
    for k in 0..steps {
        let lit = (lo as i32 + k as i32) < i32::from(level);
        let x = bx + k as f32 * cell;
        let gap = (cell * 0.22).max(1.0);
        fill(
            frame,
            w,
            h,
            (
                x + px as f32,
                by + px as f32,
                x + cell - gap + px as f32,
                by + bh + px as f32,
            ),
            [0, 0, 0, 0xFF],
        );
        let rgba = if lit { green } else { [0x18, 0x50, 0x1C, 0xFF] };
        fill(frame, w, h, (x, by, x + cell - gap, by + bh), rgba);
    }
}

/// Draw the Program monitor's controls (and the setup panel when open).
fn draw_controls(frame: &mut [u8], art: &MonitorArt, w: usize, h: usize, c: Controls) {
    let (rec_label, lamp) = if c.recording {
        ("STOP", [0xFF, 0x30, 0x20])
    } else if c.saving {
        ("SAVING", [0xF0, 0xC0, 0x30])
    } else {
        ("REC", [0x70, 0x18, 0x14])
    };
    plastic_button(
        frame,
        w,
        h,
        art_rect(art, w, h, REC_BUTTON),
        rec_label,
        Some(lamp),
    );
    plastic_button(
        frame,
        w,
        h,
        art_rect(art, w, h, SETUP_BUTTON),
        "SETUP",
        None,
    );
    if !c.setup {
        return;
    }
    let panel = art_rect(art, w, h, PANEL);
    fill(frame, w, h, panel, [0x10, 0x10, 0x14, 0xE8]);
    let rows = [
        "RECORDING SETUP".to_string(),
        match c.settings.format {
            RecordFormat::Mp4 => "FORMAT   MP4 (H.264 + AAC)".to_string(),
            RecordFormat::Avi => "FORMAT   AVI (LOSSLESS)".to_string(),
        },
        if c.settings.high_quality {
            "QUALITY  HIGH".to_string()
        } else {
            "QUALITY  STANDARD".to_string()
        },
        "OPEN THE OUTPUT FOLDER".to_string(),
        "CLOSE".to_string(),
    ];
    for (k, label) in rows.iter().enumerate() {
        let r = panel_row(art, w, h, k);
        let pad = (r.3 - r.1) * 0.12;
        let r = (r.0 + pad, r.1 + pad, r.2 - pad, r.3 - pad);
        if k == 0 {
            let px = (((r.3 - r.1) * 0.45) / 8.0).floor().max(1.0) as usize;
            let tx = r.0 + (r.2 - r.0 - crate::video::font::text_width(label, px) as f32) / 2.0;
            let ty = (r.1 + r.3) / 2.0 - crate::video::font::text_height(px) as f32 / 2.0;
            crate::video::font::draw_text(
                frame,
                w,
                h,
                tx.max(0.0) as usize,
                ty.max(0.0) as usize,
                label,
                u32::from_le_bytes([0xF0, 0xC8, 0x50, 0xFF]),
                px,
            );
        } else {
            plastic_button(frame, w, h, r, label, None);
        }
    }
}

impl Canvas {
    /// Paint until told to stop: every frame the output publishes (or the
    /// latest, when painting is slower than the card), and whatever the
    /// event loop asks for.
    fn run(mut self, rx: mpsc::Receiver<Paint>) {
        loop {
            let mut redraw = false;
            loop {
                match rx.try_recv() {
                    Ok(Paint::Resized(w, h)) => {
                        self.resize(w, h);
                        redraw = true;
                    }
                    Ok(Paint::Redraw) => redraw = true,
                    Ok(Paint::Setup(open)) => {
                        self.setup = open;
                        redraw = true;
                    }
                    Ok(Paint::Settings(settings, osd)) => {
                        self.settings = settings;
                        self.look = Look::of(&settings);
                        if let Some(k) = osd {
                            self.osd = Some((k, Instant::now()));
                        }
                        redraw = true;
                    }
                    Ok(Paint::Power(edge)) => {
                        self.settings.power = edge.on;
                        self.edge = Some(edge);
                        self.osd = None;
                        redraw = true;
                    }
                    Ok(Paint::Pressed(key)) => {
                        self.pressed = key;
                        redraw = true;
                    }
                    Ok(Paint::Stop) | Err(mpsc::TryRecvError::Disconnected) => return,
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            if let Some((vw, vh, serial)) =
                self.output
                    .wait_frame_since(self.seen, PAINT_WAIT, &mut self.video)
            {
                self.seen = serial;
                self.video_size = (vw, vh);
                redraw = true;
            }
            // While the tube warms or dies, a frame every so often
            // whether or not the card sends one.
            if self.animating() && self.last_paint.elapsed() >= ANIMATION_FRAME {
                redraw = true;
            }
            if self
                .osd
                .is_some_and(|(_, since)| since.elapsed() >= OSD_TIME)
            {
                self.osd = None;
            }
            let controls = self.controls();
            if redraw || self.painted != Some(controls) {
                self.paint(controls);
            }
        }
    }

    /// The tube is warming up or going dark.
    fn animating(&self) -> bool {
        self.edge.is_some_and(|e| {
            let t = e.at.saturating_duration_since(Instant::now()).as_secs_f32();
            let since = e.at.elapsed().as_secs_f32();
            t > 0.0
                || since
                    < if e.on {
                        monitor_panel::WARM_UP
                    } else {
                        monitor_panel::COOL_DOWN
                    }
        })
    }

    /// Where the beam is: `None` with the tube dark.
    fn beam(&self) -> Option<Beam> {
        match self.edge {
            Some(e) => Beam::at(e.on, e.at.elapsed().as_secs_f32()),
            None if self.settings.power => Some(Beam::STEADY),
            None => None,
        }
    }

    /// What the controls show now; a Preview monitor has no recording
    /// controls.
    fn controls(&self) -> Controls {
        let program = self.output.name() == "Program";
        Controls {
            recording: self.output.is_recording(),
            saving: program && self.output.saving(),
            setup: program && self.setup,
            settings: self.output.record_settings(),
            panel: self.settings,
            pressed: self.pressed,
            osd: self
                .osd
                .filter(|_| self.settings.power)
                .map(|(k, _)| (k, self.settings.knob(k))),
        }
    }

    fn paint(&mut self, controls: Controls) {
        self.last_paint = Instant::now();
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        if self.layer.as_ref().is_none_or(|l| {
            (l.w, l.h) != (w as usize, h as usize) || l.under_scan != self.settings.under_scan
        }) {
            self.layer = Some(Layer::new(
                &self.art,
                w as usize,
                h as usize,
                self.output.aspect(),
                self.settings.under_scan,
            ));
            self.painted = None;
        }
        let Some(layer) = self.layer.as_ref() else {
            return;
        };
        let (vw, vh) = self.video_size;
        // INPUT B and C carry nothing; a tube that is off shows nothing.
        let beam = self
            .beam()
            .filter(|_| self.settings.input == 0 && !self.video.is_empty());
        let video = match beam {
            // The tube dark as it is when lit with nothing on it, so going
            // dark does not change the glass.
            None if vw * vh > 0 => {
                self.shown.clear();
                self.shown
                    .resize(vw * vh, u32::from_le_bytes([0, 0, 0, 0xFF]));
                Some((self.shown.as_slice(), vw, vh))
            }
            None => None,
            Some(b) if b == Beam::STEADY && self.look.is_plain() => {
                Some((self.video.as_slice(), vw, vh))
            }
            Some(b) => {
                monitor_panel::process(&self.video, vw, vh, &self.look, &b, &mut self.shown);
                Some((self.shown.as_slice(), vw, vh))
            }
        };
        // The artwork around the tube stays in the surface's buffer from
        // frame to frame; it is painted again only when it changes.
        let background = self.painted != Some(controls);
        layer.compose_rgba8(video, self.pixels.frame_mut(), background);
        if background || video.is_none() {
            draw_panel(
                self.pixels.frame_mut(),
                &self.art,
                w as usize,
                h as usize,
                &controls,
            );
        }
        if let Some(osd) = controls.osd {
            draw_osd(
                self.pixels.frame_mut(),
                w as usize,
                h as usize,
                layer.screen,
                osd,
            );
        }
        if self.output.name() == "Program" {
            draw_controls(
                self.pixels.frame_mut(),
                &self.art,
                w as usize,
                h as usize,
                controls,
            );
        }
        self.painted = Some(controls);
        if let Err(e) = self.pixels.render() {
            warn!("toaster monitor {}: {e}", self.output.name());
        }
    }

    fn resize(&mut self, w: u32, h: u32) {
        let (w, h) = (w.max(1), h.max(1));
        self.size = (w, h);
        if let Err(e) = self.pixels.resize_surface(w, h) {
            warn!("toaster monitor resize: {e}");
        }
        if let Err(e) = self.pixels.resize_buffer(w, h) {
            warn!("toaster monitor resize: {e}");
        }
        self.layer = None;
        self.painted = None;
    }
}

impl MonitorWindow {
    fn title(&self) -> String {
        let name = self.output.name();
        if self.output.is_recording() {
            format!("Toaster {name} \u{25CF} REC (R to stop)")
        } else if self.output.saving() {
            format!("Toaster {name} (saving the MP4...)")
        } else if self.is_program() {
            format!("Toaster {name} (R to record)")
        } else {
            format!("Toaster {name}")
        }
    }

    fn is_program(&self) -> bool {
        self.output.name() == "Program"
    }

    fn resized(&mut self, w: u32, h: u32) {
        let (w, h) = (w.max(1), h.max(1));
        self.size = (w, h);
        self.painter.send(Paint::Resized(w, h));
    }

    /// Press a front-panel button.
    fn press(&mut self, key: Key) {
        let name = self.output.name().to_string();
        let osd = self.settings.press(key);
        if key == Key::Power {
            monitor_sounds::power(self.settings.power, self.variant, 0.0);
            self.painter.send(Paint::Power(PowerEdge {
                on: self.settings.power,
                at: Instant::now(),
            }));
        } else {
            monitor_sounds::button_click();
            self.painter.send(Paint::Settings(self.settings, osd));
        }
        monitor_panel::save(self.output.media_root().as_deref(), &name, self.settings);
    }
}

/// The monitor windows of the fitted card.
pub(super) struct CardMonitors {
    /// Each output's artwork, loaded on first use: most sessions never fit
    /// a card.
    art: std::cell::RefCell<std::collections::HashMap<String, Option<Arc<MonitorArt>>>>,
    windows: Vec<MonitorWindow>,
    /// The Flyer clip converter has been started (or there is none).
    converter_started: bool,
    /// The card's sound level last given to the machine.
    speaker: Option<f32>,
}

impl CardMonitors {
    pub(super) fn new() -> Self {
        Self {
            art: std::cell::RefCell::new(std::collections::HashMap::new()),
            windows: Vec::new(),
            converter_started: false,
            speaker: None,
        }
    }

    fn art(&self, output: &CardOutput) -> Option<Arc<MonitorArt>> {
        self.art
            .borrow_mut()
            .entry(output.name().to_string())
            .or_insert_with(|| MonitorArt::load(output.media_root().as_deref(), output.name()))
            .clone()
    }

    /// Make the window set match `outputs`: a window per output, opened
    /// beside `beside` (the main window's outer rectangle).
    pub(super) fn sync(
        &mut self,
        event_loop: &ActiveEventLoop,
        outputs: Vec<Arc<CardOutput>>,
        beside: Option<(PhysicalPosition<i32>, winit::dpi::PhysicalSize<u32>)>,
    ) {
        let same = outputs.len() == self.windows.len()
            && outputs
                .iter()
                .zip(&self.windows)
                .all(|(o, w)| Arc::ptr_eq(o, &w.output));
        if same {
            return;
        }
        self.windows.clear();
        // A new machine's mix starts at its own level.
        self.speaker = None;
        if outputs.is_empty() {
            return;
        }
        for (n, output) in outputs.into_iter().enumerate() {
            let Some(art) = self.art(&output) else {
                continue;
            };
            let (aw, ah) = art.size();
            // Half the artwork's size, stacked to the right of the main
            // window where there is room.
            let size = LogicalSize::new(aw as f64 / 2.0, ah as f64 / 2.0);
            let mut attrs = WindowAttributes::default()
                .with_title(format!("Toaster {}", output.name()))
                .with_inner_size(size)
                .with_min_inner_size(LogicalSize::new(aw as f64 / 8.0, ah as f64 / 8.0));
            if let Some((pos, outer)) = beside {
                let x = pos.x + outer.width as i32 + 8;
                let y = pos.y + (n as i32) * (ah as i32 / 2 + 40);
                attrs = attrs.with_position(PhysicalPosition::new(x, y));
            } else {
                attrs = attrs.with_position(LogicalPosition::new(
                    40.0 + n as f64 * (aw as f64 / 2.0 + 16.0),
                    40.0,
                ));
            }
            let window = match event_loop.create_window(attrs) {
                Ok(w) => Arc::new(w),
                Err(e) => {
                    warn!("toaster monitor {}: {e}", output.name());
                    continue;
                }
            };
            let inner = window.inner_size();
            let (w, h) = (inner.width.max(1), inner.height.max(1));
            let surface = SurfaceTexture::new(w, h, Arc::clone(&window));
            let mut builder = PixelsBuilder::new(w, h, surface)
                .present_mode(super::present::window_present_mode(false));
            if cfg!(target_os = "linux") {
                builder = builder.wgpu_backend(
                    pixels::wgpu::Backends::from_env().unwrap_or(pixels::wgpu::Backends::VULKAN),
                );
            }
            let pixels = match builder.build() {
                Ok(p) => p,
                Err(e) => {
                    warn!("toaster monitor {}: {e}", output.name());
                    continue;
                }
            };
            info!("toaster monitor window: {}", output.name());
            let settings = monitor_panel::load(output.media_root().as_deref(), output.name());
            // Switched on with the machine: the Preview a moment after the
            // Program, as from a power strip.
            let delay = 0.3 * n as f32;
            let edge = settings.power.then(|| {
                monitor_sounds::power(true, n, delay);
                PowerEdge {
                    on: true,
                    at: Instant::now() + Duration::from_secs_f32(delay),
                }
            });
            let canvas = Canvas {
                output: Arc::clone(&output),
                art: Arc::clone(&art),
                pixels,
                layer: None,
                size: (w, h),
                seen: 0,
                video: Vec::new(),
                video_size: (0, 0),
                painted: None,
                setup: false,
                settings,
                look: Look::of(&settings),
                edge,
                pressed: None,
                osd: None,
                shown: Vec::new(),
                last_paint: Instant::now(),
            };
            let (tx, rx) = mpsc::channel();
            let thread = std::thread::Builder::new()
                .name(format!(
                    "toaster-monitor-{}",
                    output.name().to_ascii_lowercase()
                ))
                .spawn(move || canvas.run(rx));
            let thread = match thread {
                Ok(t) => Some(t),
                Err(e) => {
                    warn!("toaster monitor {}: no painter thread: {e}", output.name());
                    None
                }
            };
            let mw = MonitorWindow {
                output,
                art: Arc::clone(&art),
                window,
                painter: Painter { tx, thread },
                size: (w, h),
                settings,
                held: None,
                variant: n,
                setup: false,
                shown: (false, false),
                cursor: (0.0, 0.0),
            };
            mw.window.set_title(&mw.title());
            mw.painter.send(Paint::Redraw);
            self.windows.push(mw);
        }
    }

    pub(super) fn owns(&self, id: WindowId) -> bool {
        self.windows.iter().any(|w| w.window.id() == id)
    }

    /// Keep each window's title with its recording (the painters follow
    /// the frames and the lamp by themselves).
    pub(super) fn poll(&mut self) {
        for w in &mut self.windows {
            let shown = (w.output.is_recording(), w.output.saving());
            if shown != w.shown {
                w.shown = shown;
                w.window.set_title(&w.title());
            }
            // A held up or down button repeats.
            if let Some((key, next)) = w.held {
                if Instant::now() >= next {
                    w.held = Some((key, next + REPEAT_RATE));
                    w.press(key);
                }
            }
        }
    }

    /// The card's sound level: the Program monitor's volume, nothing with
    /// it switched off, as made with no Program monitor. `Some` when it
    /// changed since last asked.
    pub(super) fn speaker_change(&mut self) -> Option<f32> {
        let gain = self
            .windows
            .iter()
            .find(|w| w.is_program())
            .map_or(1.0, |w| w.settings.speaker_gain());
        (self.speaker != Some(gain)).then(|| {
            self.speaker = Some(gain);
            gain
        })
    }

    /// Handle an event for one of the monitor windows. `Some(message)` is
    /// something to tell the user (a recording started or stopped).
    pub(super) fn window_event(&mut self, id: WindowId, event: &WindowEvent) -> Option<String> {
        let w = self.windows.iter_mut().find(|w| w.window.id() == id)?;
        let art = Arc::clone(&w.art);
        match event {
            WindowEvent::RedrawRequested => w.painter.send(Paint::Redraw),
            WindowEvent::Resized(size) => w.resized(size.width, size.height),
            WindowEvent::CloseRequested => {
                // Closing a monitor turns it away rather than unplugging
                // it: the menu brings it back.
                w.window.set_visible(false);
            }
            WindowEvent::CursorMoved { position, .. } => w.cursor = (position.x, position.y),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                w.held = None;
                w.painter.send(Paint::Pressed(None));
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let (ww, wh) = (w.size.0 as usize, w.size.1 as usize);
                let control = if w.is_program() {
                    control_at(&art, ww, wh, w.setup, w.cursor.0, w.cursor.1)
                } else {
                    None
                };
                match control {
                    Some(Control::Record) => return Some(toggle_recording(w)),
                    Some(c @ (Control::Setup | Control::Close)) => {
                        w.setup = c == Control::Setup && !w.setup;
                        w.painter.send(Paint::Setup(w.setup));
                    }
                    Some(Control::Format) => {
                        let mut s = w.output.record_settings();
                        s.format = match s.format {
                            RecordFormat::Mp4 => RecordFormat::Avi,
                            RecordFormat::Avi => RecordFormat::Mp4,
                        };
                        w.output.set_record_settings(s);
                        w.painter.send(Paint::Redraw);
                    }
                    Some(Control::Quality) => {
                        let mut s = w.output.record_settings();
                        s.high_quality = !s.high_quality;
                        w.output.set_record_settings(s);
                        w.painter.send(Paint::Redraw);
                    }
                    Some(Control::Folder) => {
                        if let Some(dir) = w.output.record_dir() {
                            let _ = std::fs::create_dir_all(dir);
                            open_folder(dir);
                        }
                    }
                    None => {
                        if let Some(key) = key_at(&art, ww, wh, w.cursor.0, w.cursor.1) {
                            w.painter.send(Paint::Pressed(Some(key)));
                            if matches!(key, Key::Up(_) | Key::Down(_)) {
                                w.held = Some((key, Instant::now() + REPEAT_DELAY));
                            }
                            w.press(key);
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && !event.repeat
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyR)
                    && w.is_program() =>
            {
                return Some(toggle_recording(w));
            }
            _ => {}
        }
        None
    }

    /// Bring every monitor window back into view.
    pub(super) fn show_all(&self) {
        for w in &self.windows {
            w.window.set_visible(true);
            w.window.focus_window();
        }
    }

    /// Start or stop recording the Program output.
    pub(super) fn toggle_program_recording(&mut self) -> Option<String> {
        let w = self.windows.iter_mut().find(|w| w.is_program())?;
        Some(toggle_recording(w))
    }

    pub(super) fn program_recording(&self) -> bool {
        self.windows
            .iter()
            .any(|w| w.is_program() && w.output.is_recording())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// Headless: save every output, on its monitor, beside `path`
    /// (`shot.png` gives `shot-program.png`, `shot-preview.png`).
    pub(super) fn save_shots(&self, outputs: &[Arc<CardOutput>], path: &std::path::Path) {
        if outputs.is_empty() {
            return;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("shot");
        for output in outputs {
            let Some(art) = self.art(output) else {
                continue;
            };
            let (pixels, w, h) = compose_native(&art, output);
            let name = format!("{stem}-{}.png", output.name().to_ascii_lowercase());
            let target = path.with_file_name(name);
            match crate::screenshot::save(&target, &pixels, w as u32, h as u32) {
                Ok(()) => info!("monitor screenshot saved: {}", target.display()),
                Err(e) => warn!("monitor screenshot {}: {e:#}", target.display()),
            }
        }
    }
}

/// Show a folder in the desktop's file manager.
fn open_folder(dir: &std::path::Path) {
    #[cfg(windows)]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let cmd = "xdg-open";
    if let Err(e) = std::process::Command::new(cmd).arg(dir).spawn() {
        warn!("opening {}: {e}", dir.display());
    }
}

fn toggle_recording(w: &mut MonitorWindow) -> String {
    let name = w.output.name();
    let message = if w.output.is_recording() {
        match w.output.stop_recording() {
            Ok(Some(path)) if w.output.saving() => {
                info!("toaster {name} recording: making {}", path.display());
                format!("{name} recording: saving {}", path.display())
            }
            Ok(Some(path)) => {
                info!("toaster {name} recording saved: {}", path.display());
                format!("{name} recording saved: {}", path.display())
            }
            Ok(None) => format!("{name} was not recording"),
            Err(e) => {
                warn!("toaster {name} recording: {e:#}");
                format!("{name} recording failed: {e}")
            }
        }
    } else {
        match w.output.start_recording_auto() {
            Ok(path) => {
                info!("toaster {name} recording to {}", path.display());
                format!("Recording {name} to {}", path.display())
            }
            Err(e) => {
                warn!("toaster {name} recording: {e:#}");
                format!("{name} recording failed: {e}")
            }
        }
    };
    w.shown = (w.output.is_recording(), w.output.saving());
    w.window.set_title(&w.title());
    w.painter.send(Paint::Redraw);
    message
}

impl super::App {
    /// Open, close and repaint the card's monitor windows. Headless runs
    /// open none (their screenshots carry the monitors instead), nor does
    /// `[video_slot] monitors = false`.
    pub(super) fn service_card_monitors(&mut self, event_loop: &ActiveEventLoop) {
        let wanted = !self.headless_capture_active()
            && self.machine_config.video_slot.monitors.unwrap_or(true);
        let outputs = if wanted {
            self.emu.bus().video_slot_outputs()
        } else {
            Vec::new()
        };
        let beside = self.render.as_ref().and_then(|r| {
            r.window
                .outer_position()
                .ok()
                .map(|p| (p, r.window.outer_size()))
        });
        self.card_monitors.sync(event_loop, outputs, beside);
        self.card_monitors.poll();
        if let Some(gain) = self.card_monitors.speaker_change() {
            self.emu.bus_mut().set_video_slot_volume(gain);
        }
        self.start_flyer_converter();
    }

    /// With a Flyer fitted, start the clip converter window beside the
    /// machine once (`[video_slot] converter`, default on): the
    /// `flyer-convert` program next to this one, given the card's folder.
    /// It runs on its own and outlives the machine; a second copy exits.
    fn start_flyer_converter(&mut self) {
        if self.card_monitors.converter_started {
            return;
        }
        let slot = &self.machine_config.video_slot;
        if self.headless_capture_active()
            || !slot.flyer.unwrap_or(false)
            || !slot.converter.unwrap_or(true)
        {
            return;
        }
        let outputs = self.emu.bus().video_slot_outputs();
        let Some(root) = outputs.iter().find_map(|o| o.media_root()) else {
            return;
        };
        self.card_monitors.converter_started = true;
        let name = if cfg!(windows) {
            "flyer-convert.exe"
        } else {
            "flyer-convert"
        };
        let Some(exe) = std::env::current_exe()
            .ok()
            .map(|p| p.with_file_name(name))
            .filter(|p| p.is_file())
        else {
            info!("flyer converter: no {name} beside the emulator");
            return;
        };
        match std::process::Command::new(&exe)
            .arg("--media")
            .arg(&root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => info!("flyer converter: started {}", exe.display()),
            Err(e) => warn!("flyer converter: {}: {e}", exe.display()),
        }
    }

    /// Start or stop recording the card's Program output (menu).
    pub(super) fn toggle_card_recording(&mut self) {
        if let Some(message) = self.card_monitors.toggle_program_recording() {
            self.show_osd(message);
        }
    }

    pub(super) fn show_card_monitors(&mut self) {
        self.card_monitors.show_all();
    }

    /// Headless screenshots also save the card's outputs on their
    /// monitors, beside the main capture.
    pub(super) fn save_card_monitor_shots(&self, path: &std::path::Path) {
        let outputs = self.emu.bus().video_slot_outputs();
        self.card_monitors.save_shots(&outputs, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_artworks_have_a_tube() {
        for built_in in [&PROGRAM_ART, &PREVIEW_ART] {
            let art = MonitorArt::decode(built_in.png).expect("artwork");
            let (x0, y0, x1, y1) = art.screen;
            // The glass measured off the artworks: about 925 x 680 at
            // (265, 120).
            assert!(
                (255..=275).contains(&x0) && (110..=130).contains(&y0),
                "{:?}",
                art.screen
            );
            assert!(
                (1180..=1200).contains(&x1) && (790..=810).contains(&y1),
                "{:?}",
                art.screen
            );
            // The front panel's buttons lie on the artwork, below the
            // glass, and a click on each finds it, in a window of the
            // artwork's size and of half that.
            let mut art = art;
            art.panel = Some(built_in.panel);
            for &(key, (bx0, by0, bx1, by1)) in built_in.panel.keys {
                assert!(bx1 as usize <= art.w && by1 as usize <= art.h && by0 as usize > y1);
                assert!(bx0 < bx1 && by0 < by1);
                for div in [1, 2] {
                    let (w, h) = (art.w / div, art.h / div);
                    let x = f64::from(bx0 + bx1) / 2.0 / div as f64;
                    let y = f64::from(by0 + by1) / 2.0 / div as f64;
                    assert_eq!(key_at(&art, w, h, x, y), Some(key));
                }
            }
            assert_eq!(key_at(&art, art.w, art.h, 700.0, 500.0), None);
        }
    }

    #[test]
    fn the_fixed_point_tube_follows_the_float_model() {
        let art = MonitorArt::decode(PROGRAM_ART.png).expect("artwork");
        let layer = Layer::new(&art, 1100, 817, 4.0 / 3.0, false);
        // A picture with detail in every channel.
        let (vw, vh) = (752, 480);
        let video: Vec<u32> = (0..vw * vh)
            .map(|k| {
                let (x, y) = (k % vw, k / vw);
                rgba_u32((x * 3) as u8, (y * 5) as u8, ((x + y) * 7) as u8)
            })
            .collect();
        let mut out = vec![0u32; 1100 * 817];
        layer.compose(&art, Some((&video, vw, vh)), &mut out);
        // The float model this replaced, at a spread of tube pixels.
        let (px, py, pw, ph) = layer.picture;
        let (sx, sy) = (vw as f32 / pw, vh as f32 / ph);
        let (x0, y0, x1, y1) = layer.screen;
        let mut checked = 0;
        for y in (y0..y1).step_by(37) {
            for x in (x0..x1).step_by(41) {
                let i = y * 1100 + x;
                let m = layer.mask[i];
                if m == 0 {
                    continue;
                }
                let vy = ((y as f32 + 0.5 - py) * sy - 0.5).clamp(0.0, (vh - 1) as f32);
                let vx = ((x as f32 + 0.5 - px) * sx - 0.5).clamp(0.0, (vw - 1) as f32);
                let (ya, xa) = (vy.floor() as usize, vx.floor() as usize);
                let (yb, xb) = ((ya + 1).min(vh - 1), (xa + 1).min(vw - 1));
                let (ty, tx) = (vy - ya as f32, vx - xa as f32);
                let g = f32::from(layer.glass[i]);
                let shade = 0.72 + 0.28 * (g / 45.0).min(1.0);
                let sheen = (g - 50.0).max(0.0) * 0.9;
                let a = f32::from(m) / 255.0;
                let got = out[i].to_le_bytes();
                let bg = layer.bg[i].to_le_bytes();
                for c in 0..3 {
                    let p = |k: usize| f32::from(video[k].to_le_bytes()[c]);
                    let top = p(ya * vw + xa) * (1.0 - tx) + p(ya * vw + xb) * tx;
                    let bottom = p(yb * vw + xa) * (1.0 - tx) + p(yb * vw + xb) * tx;
                    let v = (top * (1.0 - ty) + bottom * ty) * shade + sheen;
                    let v = (v * a + f32::from(bg[c]) * (1.0 - a)).clamp(0.0, 255.0);
                    assert!(
                        (f32::from(got[c]) - v).abs() <= 3.0,
                        "({x}, {y}) channel {c}: {} vs {v}",
                        got[c]
                    );
                }
                checked += 1;
            }
        }
        assert!(checked > 200, "{checked}");
    }

    #[test]
    fn a_frame_lands_on_the_glass_only() {
        let art = MonitorArt::decode(PROGRAM_ART.png).expect("artwork");
        let layer = Layer::new(&art, 728, 540, 4.0 / 3.0, false);
        let red = vec![rgba_u32(255, 0, 0); 16 * 12];
        let mut out = vec![0u32; 728 * 540];
        layer.compose(&art, Some((&red, 16, 12)), &mut out);
        let at = |x: usize, y: usize| out[y * 728 + x].to_le_bytes();
        // The middle of the tube is red, the bezel below it is not.
        let mid = at(364, 230);
        assert!(mid[0] > 150 && mid[1] < 60, "{mid:?}");
        let bezel = at(364, 500);
        assert!(bezel[0] < 120, "{bezel:?}");
    }
}
