// SPDX-License-Identifier: LGPL-2.1-or-later

//! The LCD and the MIDI MESSAGE lamp.
//!
//! Four modes, exactly as the firmware runs them: the main (Master Volume)
//! screen with the part activity marks, the program change notice, the
//! custom message a program writes over SysEx, and the error banner. The
//! elder and later firmwares differ observably -- whole-buffer custom
//! messages against positional ones, which modes outrank which, whether an
//! error banner times out -- and both behaviours are kept, chosen by the
//! same quirk flags the memory model reads from the ROM.
//!
//! Time is the rendered-frame count, as on the engine: the program change
//! notice and the elder error banner stand for 41943 frames (ten overflows
//! of the unit's 500 kHz 16-bit timer), and the lamp and the rhythm mark
//! blink for 80 milliseconds of frames.

use crate::layout::Layout;

/// The width of the LCD in characters.
pub const LCD_TEXT_SIZE: usize = 20;

/// The full-block character standing in for an active part's number.
pub const ACTIVE_PART_INDICATOR: u8 = 1;

/// How long the lamp and the rhythm mark stay lit past their trigger.
const BLINK_FRAMES: u32 = 80 * crate::SAMPLE_RATE / 1000;

/// How long a program change notice (and, on the elder units, an error
/// banner) stands before the display returns to the main screen.
const DISPLAY_RESET_FRAMES: u32 = 41943;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Main,
    Startup,
    ProgramChange,
    CustomMessage,
    ErrorMessage,
}

/// The display: what the glass and the lamp show, driven by events and the
/// rendered-frame clock.
#[derive(Debug)]
pub struct Display {
    /// Whether this firmware behaves as the elder units do.
    old_compatible: bool,
    /// The eldest units let a custom message replace even an error banner.
    custom_priority_quirk: bool,
    error: [u8; LCD_TEXT_SIZE],
    buffer: [u8; LCD_TEXT_SIZE],
    custom: [u8; LCD_TEXT_SIZE],
    mode: Mode,
    last_led: bool,
    lcd_dirty: bool,
    lcd_update_signalled: bool,
    last_rhythm_state: bool,
    voice_part_states: [bool; 8],
    prog_part: u8,
    prog_group: [u8; 8],
    prog_timbre: [u8; 10],
    display_reset_at: u32,
    display_reset_scheduled: bool,
    led_reset_at: u32,
    midi_played: bool,
    rhythm_reset_at: u32,
    rhythm_played: bool,
}

impl Display {
    /// A display coming up: the startup banner, with the return to the
    /// main screen already scheduled.
    pub fn power_on(control: &[u8], layout: &Layout) -> Display {
        let line = |at: u16| {
            let at = usize::from(at);
            control[at..at + LCD_TEXT_SIZE].try_into().unwrap()
        };
        Display {
            old_compatible: layout.quirks.old_mt32_display_features,
            custom_priority_quirk: layout.quirks.display_custom_message_priority,
            error: line(layout.sysex_error_message),
            // The banner stands in the buffer from the start; a reset
            // builds a whole new display, so it is never needed again.
            buffer: line(layout.startup_message),
            custom: [b' '; LCD_TEXT_SIZE],
            mode: Mode::Startup,
            last_led: false,
            lcd_dirty: false,
            lcd_update_signalled: false,
            last_rhythm_state: false,
            voice_part_states: [false; 8],
            prog_part: 0,
            prog_group: [b' '; 8],
            prog_timbre: [0; 10],
            display_reset_at: DISPLAY_RESET_FRAMES,
            display_reset_scheduled: true,
            led_reset_at: 0,
            midi_played: false,
            rhythm_reset_at: 0,
            rhythm_played: false,
        }
    }

    /// The per-render check: expires timers, settles the lamp, and marks
    /// the LCD for rebuilding. `now` is the rendered-frame count.
    pub fn refresh(&mut self, now: u32) {
        let mut led = self.midi_played;
        if self.midi_played && timer_expired(self.led_reset_at, now) {
            self.midi_played = false;
        }
        // The lamp answers for the voice parts too, not only the wire.
        led = led || self.voice_part_states.iter().any(|&s| s);
        self.last_led = led;
        if self.display_reset_scheduled && timer_expired(self.display_reset_at, now) {
            self.set_main_display_mode();
        }
        if self.last_rhythm_state != self.rhythm_played && self.mode == Mode::Main {
            self.lcd_dirty = true;
        }
        self.last_rhythm_state = self.rhythm_played;
        if self.rhythm_played && timer_expired(self.rhythm_reset_at, now) {
            self.rhythm_played = false;
        }
        if self.lcd_dirty && !self.lcd_update_signalled {
            self.lcd_update_signalled = true;
        }
    }

    /// What the glass and the lamp show. The text is rebuilt only when a
    /// [`refresh`](Self::refresh) has signalled a change, as on the
    /// engine; `master_volume` is read at that moment.
    pub fn state(&mut self, master_volume: u8) -> ([u8; LCD_TEXT_SIZE], bool) {
        if self.lcd_update_signalled {
            self.lcd_dirty = false;
            self.lcd_update_signalled = false;
            match self.mode {
                Mode::CustomMessage => {
                    if self.old_compatible {
                        self.buffer = self.custom;
                    } else {
                        copy_null_terminated(&mut self.buffer, &self.custom);
                    }
                }
                Mode::ErrorMessage => self.buffer = self.error,
                Mode::ProgramChange => {
                    self.buffer[0] = b'1' + self.prog_part;
                    self.buffer[1] = b'|';
                    self.buffer[2..10].copy_from_slice(&self.prog_group);
                    copy_null_terminated(&mut self.buffer[10..], &self.prog_timbre);
                }
                Mode::Main => {
                    for part in 0..5 {
                        self.buffer[part * 2] = if self.voice_part_states[part] {
                            ACTIVE_PART_INDICATOR
                        } else {
                            b'1' + part as u8
                        };
                        self.buffer[part * 2 + 1] = b' ';
                    }
                    self.buffer[10] = if self.last_rhythm_state {
                        ACTIVE_PART_INDICATOR
                    } else {
                        b'R'
                    };
                    self.buffer[11] = b' ';
                    self.buffer[12..].copy_from_slice(b"|vol:  0");
                    let mut at = LCD_TEXT_SIZE;
                    let mut volume = u32::from(master_volume);
                    while volume > 0 {
                        at -= 1;
                        self.buffer[at] = b'0' + (volume % 10) as u8;
                        volume /= 10;
                    }
                }
                Mode::Startup => {}
            }
        }
        (self.buffer, self.last_led)
    }

    /// Back to the main screen, as the panel's own buttons or the Display
    /// Reset function put it.
    pub fn set_main_display_mode(&mut self) {
        self.display_reset_scheduled = false;
        self.mode = Mode::Main;
        self.lcd_dirty = true;
    }

    /// Anything arrived over the wire: the lamp blinks.
    pub fn midi_message_played(&mut self, now: u32) {
        self.midi_played = true;
        self.led_reset_at = now.wrapping_add(BLINK_FRAMES);
    }

    /// A rhythm note landed: its mark blinks, the lamp with it, and on the
    /// elder units it puts a custom message away.
    pub fn rhythm_note_played(&mut self, now: u32) {
        self.rhythm_played = true;
        self.rhythm_reset_at = now.wrapping_add(BLINK_FRAMES);
        self.midi_message_played(now);
        if self.old_compatible && self.mode == Mode::CustomMessage {
            self.set_main_display_mode();
        }
    }

    /// A voice part began or stopped sounding.
    pub fn voice_part_state_changed(&mut self, part: usize, active: bool) {
        if self.mode == Mode::Main {
            self.lcd_dirty = true;
        }
        self.voice_part_states[part] = active;
        if self.old_compatible && self.mode == Mode::CustomMessage {
            self.set_main_display_mode();
        }
    }

    /// The master volume moved; the main screen redraws its figure.
    pub fn master_volume_changed(&mut self) {
        if self.mode == Mode::Main {
            self.lcd_dirty = true;
        }
    }

    /// A part changed program: the notice, with the sound group and timbre
    /// name as the part knows them. On the later units a custom message or
    /// error banner outranks it.
    pub fn program_changed(&mut self, now: u32, part: u8, group: [u8; 8], timbre: [u8; 10]) {
        if !self.old_compatible
            && (self.mode == Mode::CustomMessage || self.mode == Mode::ErrorMessage)
        {
            return;
        }
        self.mode = Mode::ProgramChange;
        self.lcd_dirty = true;
        self.schedule_display_reset(now);
        self.prog_part = part;
        self.prog_group = group;
        self.prog_timbre = timbre;
    }

    /// A message failed its checksum: the banner, timed out on the elder
    /// units and standing until reset on the later.
    pub fn checksum_error(&mut self, now: u32) {
        if self.mode != Mode::ErrorMessage {
            self.mode = Mode::ErrorMessage;
            self.lcd_dirty = true;
        }
        if self.old_compatible {
            self.schedule_display_reset(now);
        } else {
            self.display_reset_scheduled = false;
        }
    }

    /// A write landed in the display window. Whether it is to be shown --
    /// which is also whether a host callback would hear of it.
    pub fn custom_message(&mut self, message: &[u8], start: u32) -> bool {
        if self.old_compatible {
            // The whole buffer at once: control characters to spaces, the
            // remainder filled with them.
            for (i, slot) in self.custom.iter_mut().enumerate() {
                let c = message.get(i).copied().unwrap_or(b' ');
                *slot = if (32..127).contains(&c) { c } else { b' ' };
            }
            if !self.custom_priority_quirk
                && (self.mode == Mode::ProgramChange || self.mode == Mode::ErrorMessage)
            {
                return false;
            }
            // The display reset timer keeps running, as on the units.
        } else {
            if start > 0x80 {
                return false;
            }
            if start == 0x80 {
                // The Display Reset function, which a program change
                // notice outranks.
                if self.mode != Mode::ProgramChange {
                    self.set_main_display_mode();
                }
                return false;
            }
            self.display_reset_scheduled = false;
            let start = start as usize;
            if start < LCD_TEXT_SIZE {
                let len = message.len().min(LCD_TEXT_SIZE - start);
                self.custom[start..start + len].copy_from_slice(&message[..len]);
            }
        }
        self.mode = Mode::CustomMessage;
        self.lcd_dirty = true;
        true
    }

    /// A short write of address bytes alone: control rather than text.
    pub fn display_control(&mut self, message: &[u8]) {
        if self.old_compatible {
            if message.len() == 1 {
                let shown = self.custom;
                self.custom_message(&shown, 0);
            } else {
                self.custom_message(&[], 0);
            }
        } else if message.len() == 2 {
            self.custom_message(&[], u32::from(message[1]) << 7);
        } else if message.len() == 1 {
            self.custom[0] = 0;
            self.custom_message(&[], 0x80);
        }
    }

    fn schedule_display_reset(&mut self, now: u32) {
        self.display_reset_at = now.wrapping_add(DISPLAY_RESET_FRAMES);
        self.display_reset_scheduled = true;
    }
}

/// Whether a wrapping frame-count deadline has passed.
fn timer_expired(deadline: u32, now: u32) -> bool {
    (deadline.wrapping_sub(now) as i32) < 0
}

/// Copy until the source's terminator; what lies beyond it stays.
fn copy_null_terminated(destination: &mut [u8], source: &[u8]) {
    for (slot, &c) in destination.iter_mut().zip(source) {
        if c == 0 {
            break;
        }
        *slot = c;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LAYOUTS;

    fn a_display(short_name: &str) -> Display {
        let layout = LAYOUTS.iter().find(|l| l.short_name == short_name).unwrap();
        let mut control = vec![b'X'; 64 * 1024];
        let put = |control: &mut Vec<u8>, at: u16, text: &[u8]| {
            control[usize::from(at)..usize::from(at) + text.len()].copy_from_slice(text);
        };
        put(
            &mut control,
            layout.startup_message,
            b"* the power-on line *",
        );
        put(
            &mut control,
            layout.sysex_error_message,
            b"the checksum banner ",
        );
        Display::power_on(&control, layout)
    }

    /// The startup banner stands until the scheduled reset, then the main
    /// screen shows the parts and the volume.
    #[test]
    fn startup_gives_way_to_the_main_screen() {
        let mut d = a_display("ctrl_mt32_2_07");
        d.refresh(100);
        assert_eq!(&d.state(100).0, b"* the power-on line ");
        d.refresh(DISPLAY_RESET_FRAMES + 1);
        assert_eq!(&d.state(100).0, b"1 2 3 4 5 R |vol:100");
        d.voice_part_state_changed(0, true);
        d.refresh(DISPLAY_RESET_FRAMES + 2);
        let (line, led) = d.state(51);
        assert_eq!(line[0], ACTIVE_PART_INDICATOR, "part 1 marks itself");
        assert_eq!(&line[12..], b"|vol: 51");
        assert!(led, "a sounding part holds the lamp");
    }

    /// The lamp blinks for its 80 milliseconds and goes out.
    #[test]
    fn the_lamp_blinks_and_goes_out() {
        let mut d = a_display("ctrl_mt32_2_07");
        d.midi_message_played(1000);
        d.refresh(1000);
        assert!(d.state(100).1);
        // The engine's check reads the lamp before expiring it, so the
        // first look past the deadline still shows lit and the next one
        // dark -- one render chunk of lag, as on the reference.
        d.refresh(1000 + BLINK_FRAMES + 1);
        assert!(d.state(100).1, "the expiring look still shows lit");
        d.refresh(1000 + BLINK_FRAMES + 2);
        assert!(!d.state(100).1);
    }

    /// The later units write characters in place, reset through 0x80, and
    /// keep an error banner until told otherwise.
    #[test]
    fn the_later_display_is_positional_and_holds_its_errors() {
        let mut d = a_display("ctrl_mt32_2_07");
        d.refresh(DISPLAY_RESET_FRAMES + 1);
        d.state(100);
        assert!(d.custom_message(b"HELLO", 0));
        d.refresh(DISPLAY_RESET_FRAMES + 2);
        assert_eq!(&d.state(100).0[..5], b"HELLO");
        assert!(d.custom_message(b"!", 5));
        d.refresh(DISPLAY_RESET_FRAMES + 3);
        assert_eq!(&d.state(100).0[..6], b"HELLO!");
        // The error banner arrives and outlasts any timer.
        d.checksum_error(DISPLAY_RESET_FRAMES + 3);
        d.refresh(2 * DISPLAY_RESET_FRAMES + 100);
        assert_eq!(&d.state(100).0, b"the checksum banner ");
        // Display Reset puts it away.
        assert!(!d.custom_message(&[], 0x80));
        d.refresh(2 * DISPLAY_RESET_FRAMES + 101);
        assert_eq!(&d.state(100).0, b"1 2 3 4 5 R |vol:100");
    }

    /// The elder units take the whole buffer at once, space-fill it, and
    /// time their error banner out like a program change.
    #[test]
    fn the_elder_display_replaces_whole_and_times_its_errors_out() {
        let mut d = a_display("ctrl_mt32_1_07");
        d.refresh(DISPLAY_RESET_FRAMES + 1);
        d.state(100);
        assert!(d.custom_message(b"OLD\x01WORLD", 0));
        d.refresh(DISPLAY_RESET_FRAMES + 2);
        assert_eq!(&d.state(100).0, b"OLD WORLD           ");
        let t = DISPLAY_RESET_FRAMES + 2;
        d.checksum_error(t);
        d.refresh(t + 1);
        assert_eq!(&d.state(100).0, b"the checksum banner ");
        d.refresh(t + DISPLAY_RESET_FRAMES + 1);
        assert_eq!(
            &d.state(100).0,
            b"1 2 3 4 5 R |vol:100",
            "the elder banner times out"
        );
    }
}
