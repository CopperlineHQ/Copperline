// SPDX-License-Identifier: GPL-3.0-or-later

//! The slice of Munt's C API that Copperline calls.
//!
//! Declared by hand rather than generated: it is a dozen functions that have
//! been stable across the library's 2.x line, and the C API is the one the
//! library versions deliberately (see `vendor/munt/src/c_interface`).

use std::ffi::{c_char, c_double, c_void};

/// Opaque engine instance.
pub type Context = *mut c_void;

/// `mt32emu_return_code` values Copperline checks.
pub const RC_OK: i32 = 0;
pub const RC_ADDED_CONTROL_ROM: i32 = 1;
pub const RC_ADDED_PCM_ROM: i32 = 2;

/// `MT32EMU_REPORT_HANDLER_VERSION_0`: the interface declared below.
pub const REPORT_HANDLER_VERSION_0: u32 = 0;

/// `MT32EMU_AOM_ACCURATE`: the analogue output stage modelled properly,
/// which is what the resampler then works from.
pub const AOM_ACCURATE: i32 = 2;

/// `mt32emu_report_handler_i`, a union of one pointer per interface version.
#[repr(C)]
pub struct ReportHandler {
    pub v0: *const ReportHandlerV0,
}

/// `MT32EMU_REPORT_HANDLER_I_V0`, in the order the header declares it. The
/// order is the ABI, so a member added upstream must be added here too.
///
/// Supplying this is what stops the engine printing its own diagnostics to
/// stderr: with a null `printDebug` it falls back to its built-in one.
#[repr(C)]
pub struct ReportHandlerV0 {
    pub get_version_id: Option<unsafe extern "C" fn(ReportHandler) -> u32>,
    /// The engine's own running commentary. The third argument is a C
    /// `va_list`; see `copperline_mt32_print_debug`.
    pub print_debug: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void)>,
    pub on_error_control_rom: Option<unsafe extern "C" fn(*mut c_void)>,
    pub on_error_pcm_rom: Option<unsafe extern "C" fn(*mut c_void)>,
    pub show_lcd_message: Option<unsafe extern "C" fn(*mut c_void, *const c_char)>,
    pub on_midi_message_played: Option<unsafe extern "C" fn(*mut c_void)>,
    pub on_midi_queue_overflow: Option<unsafe extern "C" fn(*mut c_void) -> u8>,
    pub on_midi_system_realtime: Option<unsafe extern "C" fn(*mut c_void, u8)>,
    pub on_device_reset: Option<unsafe extern "C" fn(*mut c_void)>,
    pub on_device_reconfig: Option<unsafe extern "C" fn(*mut c_void)>,
    pub on_new_reverb_mode: Option<unsafe extern "C" fn(*mut c_void, u8)>,
    pub on_new_reverb_time: Option<unsafe extern "C" fn(*mut c_void, u8)>,
    pub on_new_reverb_level: Option<unsafe extern "C" fn(*mut c_void, u8)>,
    pub on_poly_state_changed: Option<unsafe extern "C" fn(*mut c_void, u8)>,
    pub on_program_changed:
        Option<unsafe extern "C" fn(*mut c_void, u8, *const c_char, *const c_char)>,
}

unsafe extern "C" {
    /// The report-handler entry, implemented in C (`print_debug.cpp`)
    /// because it receives a `va_list`. Rust only ever takes its address:
    /// the third argument is declared as a pointer for want of a type, and
    /// nothing here calls it or looks at it.
    pub fn copperline_mt32_print_debug(
        instance_data: *mut c_void,
        fmt: *const c_char,
        args: *mut c_void,
    );

    pub fn mt32emu_get_library_version_string() -> *const c_char;

    pub fn mt32emu_create_context(
        report_handler: ReportHandler,
        instance_data: *mut c_void,
    ) -> Context;
    pub fn mt32emu_free_context(context: Context);

    pub fn mt32emu_add_rom_file(context: Context, filename: *const c_char) -> i32;

    pub fn mt32emu_set_stereo_output_samplerate(context: Context, samplerate: c_double);
    pub fn mt32emu_set_analog_output_mode(context: Context, mode: i32);
    pub fn mt32emu_get_actual_stereo_output_samplerate(context: Context) -> u32;

    pub fn mt32emu_open_synth(context: Context) -> i32;
    pub fn mt32emu_close_synth(context: Context);

    pub fn mt32emu_parse_stream(context: Context, stream: *const u8, length: u32);
    pub fn mt32emu_render_bit16s(context: Context, stream: *mut i16, len: u32);

    pub fn mt32emu_get_display_state(
        context: Context,
        target_buffer: *mut c_char,
        narrow_lcd: u8,
    ) -> u8;
    pub fn mt32emu_set_main_display_mode(context: Context);
    pub fn mt32emu_read_memory(context: Context, addr: u32, len: u32, data: *mut u8);
    pub fn mt32emu_play_sysex_now(context: Context, sysex: *const u8, len: u32);
}
