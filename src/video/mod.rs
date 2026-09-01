// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(feature = "frontend")]
pub mod about;
pub mod beam;
pub mod bitplane;
pub mod deinterlace;
pub mod font;
#[cfg(feature = "frontend")]
pub mod launcher;
#[cfg(feature = "frontend")]
pub mod menu;
#[cfg(feature = "frontend")]
pub mod nav;
pub mod present_common;
#[cfg(feature = "frontend")]
pub mod ui;
#[cfg(feature = "frontend")]
pub mod window;

#[cfg(target_os = "macos")]
pub const HOST_SHORTCUT_MODIFIER_LABEL: &str = "Cmd";
#[cfg(not(target_os = "macos"))]
pub const HOST_SHORTCUT_MODIFIER_LABEL: &str = "Alt";

/// Native hi-res Amiga PAL overscan field = 716x285 emulated pixels.
/// DiagROM's main menu copperlist sets BPLCON0 to hi-res (bit 15)
/// with 3 bitplanes, so we size the framebuffer for hi-res by
/// default. Lo-res content is rendered with each pixel doubled
/// horizontally inside `render`.
///
/// The width and horizontal origin match vAmiga's 716-wide regression
/// cutout: 716 hi-res pixels = 179 colour clocks, anchored 8 colour
/// clocks (16 lo-res pixels) further left than the standard display so
/// the deep-left overscan a real Denise can fetch/display is captured
/// rather than clipped. See the origin anchors in `bitplane.rs`
/// (`DIW_HSTART_FB0` / `COPPER_WAIT_HPOS_FB0`).
pub const FB_WIDTH: usize = 716;
pub const FB_HEIGHT: usize = 285;
pub const FB_PIXELS: usize = FB_WIDTH * FB_HEIGHT;

/// Tallest scan the capture/present pipeline supports: a full ECS/AGA
/// programmable 31 kHz frame (VTOTAL+1 = 626 half-length lines).
/// Standard PAL/NTSC frames keep using FB_HEIGHT rows; programmable
/// geometry is clamped here, matching a multisync monitor that simply
/// cannot scan an arbitrarily tall frame.
pub const MAX_VISIBLE_LINES: usize = 626;
pub const MAX_FB_PIXELS: usize = FB_WIDTH * MAX_VISIBLE_LINES;

/// Largest render canvas: the tallest scan at the 35 ns (double-width)
/// pixel pitch a programmable super-hi-res frame paints
/// (`bitplane::canvas_scale_for`). Buffers passed to the render paths are
/// sized for this so any frame's canvas fits.
pub const MAX_CANVAS_PIXELS: usize = 2 * MAX_FB_PIXELS;

/// Per-frame display geometry, latched at the frame wrap (like the
/// interlace long-field flag). Standard PAL/NTSC frames report exactly
/// the fixed-canvas values (FB_HEIGHT rows, 227-cck lines) so the
/// classic path stays byte-identical; ECS/AGA VARBEAMEN frames derive
/// their window from HTOTAL/VTOTAL and the programmable vertical
/// blank. The presentation scales whatever scan this describes onto
/// the fixed 4:3 output, like a multisync monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FrameGeometry {
    /// BEAMCON0.VARBEAMEN geometry was active at the frame start.
    pub programmable: bool,
    /// Beam line mapped to framebuffer row 0.
    pub visible_start_vpos: u32,
    /// Rows captured/rendered/presented this frame (<= MAX_VISIBLE_LINES).
    pub visible_lines: usize,
    /// Line length in colour clocks (HTOTAL+1 under VARBEAMEN, else 227).
    pub line_cck: u32,
    /// Beam lines in the frame this geometry describes. This is transient
    /// render metadata: older save states did not carry it, and the loader
    /// re-derives it from Agnus after restore.
    #[serde(skip)]
    pub frame_lines: u32,
    /// BPLCON0.LACE at the frame start (field weaving).
    pub lace: bool,
}

impl FrameGeometry {
    pub fn standard(visible_start_vpos: u32, frame_lines: u32, lace: bool) -> Self {
        Self {
            programmable: false,
            visible_start_vpos,
            visible_lines: FB_HEIGHT,
            line_cck: 227,
            frame_lines,
            lace,
        }
    }
}

/// 4:3 presentation height for screenshots/window scaling: the internal
/// 716x285 overscan field buffer is presented with the non-square pixel
/// aspect of a standard Amiga display on a 4:3 CRT.
pub const PRESENT_HEIGHT_TV: usize = FB_WIDTH * 3 / 4;

/// Square-pixel presentation height: one host row per woven scanline
/// (570 for a standard field), so a lo-res display is an exact 2x2 of
/// its bitmap (320x256 PAL occupies precisely 640x512 output pixels).
pub const PRESENT_HEIGHT_SQUARE: usize = deinterlace::OUT_HEIGHT;

/// The active presentation pixel aspect (`[display] pixel_aspect`,
/// runtime-toggled by the menu's Pixel Aspect item). Process-global like
/// the envcfg snapshot: presentation layout helpers (status bar rects,
/// hit tests, menu geometry) are free functions called from deep in the
/// window/UI code, and all reads and writes happen on the main thread --
/// the atomic only satisfies `static` safety.
static SQUARE_PIXEL_ASPECT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn set_pixel_aspect(aspect: crate::config::PixelAspect) {
    SQUARE_PIXEL_ASPECT.store(
        aspect == crate::config::PixelAspect::Square,
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// How the presentation canvas is scaled into the window
/// (`[display] scaling`, runtime-toggled by the menu's Scaling item).
/// Main thread only, like [`SQUARE_PIXEL_ASPECT`]; the atomic only
/// satisfies `static` safety. The pixel aspect decides the picture's
/// shape and this decides how it reaches the window -- but integer
/// scaling draws from the unresampled canvas, so it has a say in the
/// canvas height too ([`square_canvas`]).
static INTEGER_SCALING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_display_scaling(scaling: crate::config::DisplayScaling) {
    INTEGER_SCALING.store(
        scaling == crate::config::DisplayScaling::Integer,
        std::sync::atomic::Ordering::Relaxed,
    );
}

pub fn display_scaling() -> crate::config::DisplayScaling {
    if INTEGER_SCALING.load(std::sync::atomic::Ordering::Relaxed) {
        crate::config::DisplayScaling::Integer
    } else {
        crate::config::DisplayScaling::Smooth
    }
}

/// Whether a monitor bezel is drawn around the picture (`[display] bezel`,
/// runtime-toggled by the menu and its shortcut). A mirror of the window's
/// own style field for the canvas rule below: a bezel's fixed 4:3 opening
/// keeps the tv canvas under integer scaling, so the canvas height has to
/// know. Main thread only, like [`SQUARE_PIXEL_ASPECT`]; the atomic only
/// satisfies `static` safety.
static BEZEL_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_bezel_shown(shown: bool) {
    BEZEL_SHOWN.store(shown, std::sync::atomic::Ordering::Relaxed);
}

pub fn bezel_shown() -> bool {
    BEZEL_SHOWN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether the window presentation crops to the display window the
/// hardware programs (`[display] autocrop`, runtime-toggled by the
/// menu's Autocrop item). Main thread only, like
/// [`SQUARE_PIXEL_ASPECT`]; the atomic only satisfies `static` safety.
static AUTOCROP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_autocrop(autocrop: bool) {
    AUTOCROP.store(autocrop, std::sync::atomic::Ordering::Relaxed);
}

pub fn autocrop() -> bool {
    AUTOCROP.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether the status bar is hidden, so the emulated display scales to fill the
/// whole window. Toggled live from the window/menu (main thread only, like
/// [`SQUARE_PIXEL_ASPECT`]); the atomic only satisfies `static` safety.
static STATUS_BAR_HIDDEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_status_bar_hidden(hidden: bool) {
    STATUS_BAR_HIDDEN.store(hidden, std::sync::atomic::Ordering::Relaxed);
}

pub fn status_bar_hidden() -> bool {
    STATUS_BAR_HIDDEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether this process is a publisher-kit player: a dedicated, launcher-free
/// build of one game. Seeded once by the player's `main` before the window is
/// built and never changed; the full build never sets it. It selects the
/// trimmed player menu tree, disables the debug and capture shortcuts, and
/// turns menu changes into settings that persist per game.
static PLAYER_PROFILE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the player menu offers the quick save/load slots; the game's
/// manifest decides. Meaningless outside the player profile.
static PLAYER_SAVE_STATES: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn set_player_profile(save_states: bool) {
    PLAYER_PROFILE.store(true, std::sync::atomic::Ordering::Relaxed);
    PLAYER_SAVE_STATES.store(save_states, std::sync::atomic::Ordering::Relaxed);
}

pub fn player_profile() -> bool {
    PLAYER_PROFILE.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn player_save_states() -> bool {
    PLAYER_SAVE_STATES.load(std::sync::atomic::Ordering::Relaxed)
}

/// The window title and icon in force: Copperline's own for the emulator, the
/// game's for a player build. Seeded once before the window is built, like
/// [`PLAYER_PROFILE`]; `None` means the built-in branding.
static BRANDING: std::sync::OnceLock<(String, Option<Vec<u8>>)> = std::sync::OnceLock::new();

/// Adopt a window title and, optionally, PNG icon bytes to replace the
/// built-in branding. Call before any window exists; a later call is
/// ignored, like every other seed-once global here.
pub fn set_branding(title: String, icon_png: Option<Vec<u8>>) {
    let _ = BRANDING.set((title, icon_png));
}

pub fn branding_title() -> Option<&'static str> {
    BRANDING.get().map(|(title, _)| title.as_str())
}

pub fn branding_icon_png() -> Option<&'static [u8]> {
    BRANDING.get().and_then(|(_, icon)| icon.as_deref())
}

/// Whether the text caret is in the lit half of its blink.
///
/// Set by the window each pass while something is being typed into, and
/// read at the moment the caret is drawn. A flag rather than a clock in the
/// drawing code, so a redraw is reproducible: a preview or a test renders
/// the same pixels whenever it runs, and the caret is lit unless something
/// deliberately puts it out.
static CARET_LIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_caret_lit(lit: bool) {
    CARET_LIT.store(lit, std::sync::atomic::Ordering::Relaxed);
}

pub fn caret_lit() -> bool {
    CARET_LIT.load(std::sync::atomic::Ordering::Relaxed)
}

// Per-thread answers for the two strips that take height from the canvas,
// in test builds only.
//
// Both flags have to be process-global in a running window: every draw
// helper sizes and clips itself from the canvas height they decide, so a
// parameter would have to be threaded through all of them. That is fine
// for one window on one thread, but `cargo test` runs its tests in
// parallel threads inside one process, where a test that put a strip up
// would move the canvas under another test's buffer -- and the helpers,
// which clamp against the height as it is *now*, would write past the end
// of a buffer allocated for the shorter one. Each test thread therefore
// carries its own answer, and the shared flag stays at its default for any
// thread that never set one.
#[cfg(test)]
thread_local! {
    static KEYBOARD_PANEL_SHOWN_LOCAL: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
    static MT32_PANEL_SHOWN_LOCAL: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Whether the on-screen Amiga keyboard is shown under the display
/// (status-bar button / menu). Main thread only, like [`SQUARE_PIXEL_ASPECT`];
/// the atomic only satisfies `static` safety.
static KEYBOARD_PANEL_SHOWN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn set_keyboard_panel_shown(shown: bool) {
    #[cfg(test)]
    KEYBOARD_PANEL_SHOWN_LOCAL.with(|flag| flag.set(Some(shown)));
    #[cfg(not(test))]
    KEYBOARD_PANEL_SHOWN.store(shown, std::sync::atomic::Ordering::Relaxed);
}

pub fn keyboard_panel_shown() -> bool {
    #[cfg(test)]
    if let Some(shown) = KEYBOARD_PANEL_SHOWN_LOCAL.with(std::cell::Cell::get) {
        return shown;
    }
    KEYBOARD_PANEL_SHOWN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether the MT-32's front panel is shown under the display
/// (`[serial] mt32_panel`, toggled live from the menu). Main thread only,
/// like [`SQUARE_PIXEL_ASPECT`]; the atomic only satisfies `static` safety.
static MT32_PANEL_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_csynth_panel_shown(shown: bool) {
    #[cfg(test)]
    GM_PANEL_SHOWN_LOCAL.with(|flag| flag.set(Some(shown)));
    #[cfg(not(test))]
    GM_PANEL_SHOWN.store(shown, std::sync::atomic::Ordering::Relaxed);
}

pub fn csynth_panel_shown() -> bool {
    #[cfg(test)]
    if let Some(shown) = GM_PANEL_SHOWN_LOCAL.with(std::cell::Cell::get) {
        return shown;
    }
    GM_PANEL_SHOWN.load(std::sync::atomic::Ordering::Relaxed)
}

static GM_PANEL_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
thread_local! {
    static GM_PANEL_SHOWN_LOCAL: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

pub fn set_mt32_panel_shown(shown: bool) {
    #[cfg(test)]
    MT32_PANEL_SHOWN_LOCAL.with(|flag| flag.set(Some(shown)));
    #[cfg(not(test))]
    MT32_PANEL_SHOWN.store(shown, std::sync::atomic::Ordering::Relaxed);
}

/// How the MT-32's display is lit (`[serial] mt32_lcd`). Held as an index
/// into [`Mt32Lcd::MENU_ORDER`]; main thread only, like the flags above.
static MT32_LCD: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_mt32_lcd(style: crate::config::Mt32Lcd) {
    let index = crate::config::Mt32Lcd::MENU_ORDER
        .iter()
        .position(|s| *s == style)
        .unwrap_or(0);
    MT32_LCD.store(index as u8, std::sync::atomic::Ordering::Relaxed);
}

pub fn mt32_lcd() -> crate::config::Mt32Lcd {
    let index = usize::from(MT32_LCD.load(std::sync::atomic::Ordering::Relaxed));
    crate::config::Mt32Lcd::MENU_ORDER
        .get(index)
        .copied()
        .unwrap_or_default()
}

pub fn mt32_panel_shown() -> bool {
    #[cfg(test)]
    if let Some(shown) = MT32_PANEL_SHOWN_LOCAL.with(std::cell::Cell::get) {
        return shown;
    }
    MT32_PANEL_SHOWN.load(std::sync::atomic::Ordering::Relaxed)
}

/// How large the pop-up menu is drawn (`[display] menu_scale`, changed live
/// from the menu itself). Held as an index into [`MenuScale::MENU_ORDER`];
/// main thread only, like [`SQUARE_PIXEL_ASPECT`].
static MENU_SCALE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_menu_scale(scale: crate::config::MenuScale) {
    let index = crate::config::MenuScale::MENU_ORDER
        .iter()
        .position(|s| *s == scale)
        .unwrap_or(0);
    MENU_SCALE.store(index as u8, std::sync::atomic::Ordering::Relaxed);
}

pub fn menu_scale() -> crate::config::MenuScale {
    let index = usize::from(MENU_SCALE.load(std::sync::atomic::Ordering::Relaxed));
    crate::config::MenuScale::MENU_ORDER
        .get(index)
        .copied()
        .unwrap_or_default()
}

pub fn pixel_aspect() -> crate::config::PixelAspect {
    if SQUARE_PIXEL_ASPECT.load(std::sync::atomic::Ordering::Relaxed) {
        crate::config::PixelAspect::Square
    } else {
        crate::config::PixelAspect::Tv
    }
}

/// Height of the window's presentation canvas: the display region of the
/// backing texture, which the window, the tool windows and every overlay
/// size themselves from. The pure [`present_height_for`] variant is what
/// unit tests target, so they never have to mutate the process-global
/// modes.
pub fn present_height() -> usize {
    present_height_for(pixel_aspect(), display_scaling(), bezel_shown())
}

/// Whether the window canvas keeps one row per woven scanline
/// (`PRESENT_HEIGHT_SQUARE`) rather than the 4:3 resample
/// (`PRESENT_HEIGHT_TV`). The square-pixel aspect asks for that outright.
/// Integer scaling asks for it too, whatever the aspect: a whole-number
/// draw is only pixel-exact from an unresampled canvas, and under the tv
/// aspect the scaler pass then draws the square canvas with a separate
/// whole-number factor per axis to put the 4:3 shape back
/// (`window/present.rs`, `per_axis_fit`). A monitor bezel is the exception:
/// its fixed opening frames the whole glass and shows the aperture
/// resampled onto it, so it keeps the tv canvas for the tv aspect.
pub fn square_canvas() -> bool {
    square_canvas_for(pixel_aspect(), display_scaling(), bezel_shown())
}

pub fn square_canvas_for(
    aspect: crate::config::PixelAspect,
    scaling: crate::config::DisplayScaling,
    bezel_shown: bool,
) -> bool {
    aspect == crate::config::PixelAspect::Square
        || (scaling == crate::config::DisplayScaling::Integer && !bezel_shown)
}

pub fn present_height_for(
    aspect: crate::config::PixelAspect,
    scaling: crate::config::DisplayScaling,
    bezel_shown: bool,
) -> usize {
    if square_canvas_for(aspect, scaling, bezel_shown) {
        PRESENT_HEIGHT_SQUARE
    } else {
        PRESENT_HEIGHT_TV
    }
}

/// Height of the capture canvas: the pixel aspect's own presentation of
/// the field, which screenshots, frame dumps and video recordings save.
/// Captures are presentation-independent -- the scaling mode and the
/// bezel decide how the window draws, never what a saved picture is -- so
/// this follows the aspect alone, where [`present_height`] follows the
/// window's canvas rule.
pub fn capture_height() -> usize {
    capture_height_for(pixel_aspect())
}

pub fn capture_height_for(aspect: crate::config::PixelAspect) -> usize {
    match aspect {
        crate::config::PixelAspect::Tv => PRESENT_HEIGHT_TV,
        crate::config::PixelAspect::Square => PRESENT_HEIGHT_SQUARE,
    }
}

/// Blend two RGBA pixels channel-wise: frac=0 returns a, frac=256
/// returns b. Used by horizontal resampling for programmable scan
/// geometry.
#[inline]
pub fn blend_rgba(a: u32, b: u32, frac: u32) -> u32 {
    let inv = 256 - frac;
    let rb = ((a & 0x00FF_00FF) * inv + (b & 0x00FF_00FF) * frac) >> 8;
    let ag = (((a >> 8) & 0x00FF_00FF) * inv + ((b >> 8) & 0x00FF_00FF) * frac) >> 8;
    (rb & 0x00FF_00FF) | ((ag & 0x00FF_00FF) << 8)
}

#[cfg(test)]
mod canvas_rule_tests {
    use super::*;
    use crate::config::{DisplayScaling, PixelAspect};

    /// The canvas is square for the square aspect, and for integer
    /// scaling under either aspect unless a bezel is drawn; captures
    /// follow the aspect alone.
    #[test]
    fn integer_scaling_takes_the_square_canvas_unless_a_bezel_is_drawn() {
        let tv = PixelAspect::Tv;
        let square = PixelAspect::Square;
        let (smooth, integer) = (DisplayScaling::Smooth, DisplayScaling::Integer);
        assert_eq!(present_height_for(tv, smooth, false), PRESENT_HEIGHT_TV);
        assert_eq!(
            present_height_for(tv, integer, false),
            PRESENT_HEIGHT_SQUARE
        );
        assert_eq!(present_height_for(tv, integer, true), PRESENT_HEIGHT_TV);
        assert_eq!(present_height_for(tv, smooth, true), PRESENT_HEIGHT_TV);
        for scaling in [smooth, integer] {
            for bezel in [false, true] {
                assert_eq!(
                    present_height_for(square, scaling, bezel),
                    PRESENT_HEIGHT_SQUARE
                );
            }
        }
        assert_eq!(capture_height_for(tv), PRESENT_HEIGHT_TV);
        assert_eq!(capture_height_for(square), PRESENT_HEIGHT_SQUARE);
    }
}
