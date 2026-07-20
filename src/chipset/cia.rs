// SPDX-License-Identifier: GPL-3.0-or-later

//! Minimal 8520 CIA model.
//!
//! Two CIAs sit on the bus:
//!   CIA-A at $BFE001, $BFE101, ..., $BFEF01 (odd byte, /LDS)
//!   CIA-B at $BFD000, $BFD100, ..., $BFDF00 (even byte, /UDS)
//!
//! Each CIA exposes 16 registers, decoded from address bits A8..A11.
//!
//! Implemented: I/O ports and PC pulse, Timer A/B, binary TOD/alarm, FLAG/CNT
//! pins, PB6/PB7 timer outputs, and serial shift input/output.

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Which {
    A,
    B,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Cia {
    which: Which,
    regs: [u8; 16],

    // Timer A
    pub ta_count: u16,
    pub ta_latch: u16,
    pub ta_running: bool,
    pub ta_oneshot: bool,
    ta_counts_cnt: bool,

    // Timer B
    pub tb_count: u16,
    pub tb_latch: u16,
    pub tb_running: bool,
    pub tb_oneshot: bool,
    tb_input_mode: TimerBInputMode,

    /// ICR data register: pending interrupt sources.
    ///   bit 0 TA, 1 TB, 2 ALRM, 3 SP, 4 FLG, 7 IR
    /// Reading ICR returns this and then clears all bits (including IR).
    icr_data: u8,
    /// The external /IRQ pin. Timer underflows drive it in the SAME
    /// E-cycle as the underflow; TOD/FLAG/serial latches and ICR mask
    /// writes lag one E-cycle (the 6526-family interrupt delay, vAmiga
    /// CIASetInt1 vs CIASetInt0). `irq_pin_delay_eticks` counts down
    /// the remaining lag of a pending delayed assert.
    #[serde(default)]
    irq_pin: bool,
    #[serde(default)]
    irq_pin_delay_eticks: u8,
    /// An ICR read is in flight in the current E-cycle: a timer
    /// underflow racing it asserts the pin one E-cycle late instead of
    /// in the same cycle (vAmiga CIAReadIcr0 gating triggerTimerIrq).
    #[serde(default)]
    icr_read_race: bool,
    /// ICR mask: which sources are allowed to raise IR.
    icr_mask: u8,
    /// Serial Data Register. The Amiga keyboard wires its data line
    /// to CIA-A's SP pin, so this is where the keyboard byte lands.
    sdr: u8,
    sdr_out: Option<u8>,
    sdr_shift_count: u8,
    cnt_pin_high: bool,
    ta_pb_output_high: bool,
    tb_pb_output_high: bool,
    ta_pb_pulse_low: bool,
    tb_pb_pulse_low: bool,
    pc_pulse_pending: bool,
    flag_pin_high: bool,

    // ---- Time-of-day (TOD) -------------------------------------
    // 8520 has a 24-bit binary TOD counter clocked from its TOD pin.
    // On the Amiga, CIA-A's TOD is wired to VSYNC (50/60 Hz) and
    // CIA-B's TOD is wired to HSYNC (~15.6 kHz). Reads of TODHI
    // latch the whole counter for atomic read-out; reads of TODLO
    // release the latch. Writes of TODHI stop the counter; writes
    // of TODLO restart it. If CRB bit 7 is set, writes target the
    // write-only alarm register instead of the counter; reads still
    // return TOD time. When the counter equals the alarm, ICR.ALRM
    // is asserted.
    tod_count: u32,
    tod_latch: u32,
    tod_alarm: u32,
    tod_latched: bool,
    tod_stopped: bool,
    tod_write_alarm: bool,
    /// Alarm comparator edge detector: the ALRM interrupt fires only on the
    /// TRANSITION into counter==alarm, whether that transition comes from a
    /// counter increment or from software rewriting either register (real
    /// 8520 behaviour; vAmiga models the same flag as TOD::matching). True
    /// at power-on: counter and alarm both reset to zero already matching.
    tod_matching: bool,
    tod_frame_anchor: Option<TodFrameAnchor>,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct TodFrameAnchor {
    count_at_write: u32,
    line_phase: u32,
    frames: u32,
}

pub const REG_PRA: usize = 0x0;
pub const REG_PRB: usize = 0x1;
pub const REG_DDRA: usize = 0x2;
pub const REG_DDRB: usize = 0x3;
pub const REG_TALO: usize = 0x4;
pub const REG_TAHI: usize = 0x5;
pub const REG_TBLO: usize = 0x6;
pub const REG_TBHI: usize = 0x7;
pub const REG_TODLO: usize = 0x8;
pub const REG_TODMID: usize = 0x9;
pub const REG_TODHI: usize = 0xA;
pub const REG_SDR: usize = 0xC;
pub const REG_ICR: usize = 0xD;
pub const REG_CRA: usize = 0xE;
pub const REG_CRB: usize = 0xF;

const ICR_TA: u8 = 1 << 0;
const ICR_TB: u8 = 1 << 1;
const ICR_ALRM: u8 = 1 << 2;
const ICR_SP: u8 = 1 << 3;
const ICR_FLG: u8 = 1 << 4;
const ICR_IR: u8 = 1 << 7;
const CR_PBON: u8 = 1 << 1;
const CR_OUTMODE: u8 = 1 << 2;
const CR_LOAD: u8 = 1 << 4;
const CRA_SPMODE: u8 = 1 << 6;
const CRA_TODIN: u8 = 1 << 7;

impl Cia {
    pub fn new(which: Which) -> Self {
        let mut regs = [0u8; 16];
        // CIA-A PRA bits are all open-drain pulled high (=released)
        // by default. The bits we care about:
        //   0: /OVL line input until DDRA bit 0 is configured
        //   6: /FIR0 (left mouse button, port 1)
        //   7: /FIR1 (left mouse button, port 2)
        // We deliberately set 6 and 7 high so DiagROM's "stuck button"
        // sampler sees buttons in the released state on both samples
        // (it only flags as stuck if the second sample is "pressed").
        if which == Which::A {
            regs[REG_PRA] = 0xC1;
        }
        Self {
            which,
            regs,
            ta_count: 0xFFFF,
            ta_latch: 0xFFFF,
            ta_running: false,
            ta_oneshot: false,
            ta_counts_cnt: false,
            tb_count: 0xFFFF,
            tb_latch: 0xFFFF,
            tb_running: false,
            tb_oneshot: false,
            tb_input_mode: TimerBInputMode::Phi2,
            icr_data: 0,
            irq_pin: false,
            irq_pin_delay_eticks: 0,
            icr_read_race: false,
            icr_mask: 0,
            sdr: 0,
            sdr_out: None,
            sdr_shift_count: 0,
            cnt_pin_high: true,
            ta_pb_output_high: true,
            tb_pb_output_high: true,
            ta_pb_pulse_low: false,
            tb_pb_pulse_low: false,
            pc_pulse_pending: false,
            flag_pin_high: true,
            tod_count: 0,
            tod_latch: 0,
            // The TOD alarm resets to $000000 (WinUAE CIA_reset memsets it,
            // vAmiga leaves the member zeroed). The boot ROM relies on this:
            // it writes ONLY the alarm HI byte (= 0), expecting
            // the low bytes to already be zero. A nonzero reset value such
            // as $FFFFFF (used by some Verilog cores) leaves the alarm at
            // $00FFFF, which CIA-B TOD (counting HSYNC) reaches ~4.2s after
            // the last TOD write - that latched a stray ICR.ALRM during
            // demo loaders (9 Fingers) and crashed through the dead
            // timer.device vector when the demo re-enabled INTEN|EXTER.
            // No spurious match at power-on either way: the alarm fires on
            // the transition INTO equality, and the counter leaves 0 on its
            // first tick.
            tod_alarm: 0x0000_0000,
            tod_latched: false,
            tod_stopped: false,
            tod_write_alarm: false,
            tod_matching: true,
            tod_frame_anchor: None,
        }
    }

    /// Number of PHI2 ticks until the next running-timer underflow.
    /// Returns None if no timer is currently running. The emulator
    /// caps its instruction slice to this value (converted to
    /// instructions) so that CIA state updates land close to the
    /// real underflow time even when the CPU is tight-polling ICR
    /// or the timer count, instead of being batched at slice
    /// boundaries. Ignores the IRQ mask because polling-based
    /// timing loops (as used by DiagROM's CIA test) don't enable
    /// the CIA's IR line - they just read ICR directly.
    pub fn debug_icr_data(&self) -> u8 {
        self.icr_data
    }

    pub fn next_underflow_ticks(&self) -> Option<u32> {
        let mut min: Option<u32> = None;
        if self.ta_running && !self.ta_counts_cnt {
            min = Some(self.ta_count as u32 + 1);
        }
        if self.tb_running && self.tb_input_mode == TimerBInputMode::Phi2 {
            let n = self.tb_count as u32 + 1;
            min = Some(match min {
                Some(m) => m.min(n),
                None => n,
            });
        }
        min
    }

    pub fn next_tod_alarm_ticks(&self) -> Option<u32> {
        if self.tod_stopped {
            return None;
        }
        let delta = self.tod_alarm.wrapping_sub(self.tod_count) & 0x00FF_FFFF;
        Some(if delta == 0 { 0x0100_0000 } else { delta })
    }

    /// Advance the 24-bit TOD counter by one tick. CIA-A is ticked
    /// once per VSYNC, CIA-B once per HSYNC. An alarm match latches
    /// ICR.ALRM; the /IRQ pin follows one E-cycle later from the
    /// `tick` drain, so this returns false (kept for signature parity
    /// with the pin-edge sources the bus forwards to INTREQ).
    pub fn tick_tod(&mut self) -> bool {
        if self.tod_stopped {
            return false;
        }
        // The 8520 TOD counter increments nibble-serially: the carry ripples
        // lo.lo -> lo.hi -> mid.lo -> mid.hi -> hi.lo -> hi.hi. After the
        // mid.lo stage the intermediate value is briefly visible to the
        // alarm comparator (the A500 "TOD bug"; vAmiga models the same
        // mid-ripple checkIrq), so a carry that reaches mid.lo can fire the
        // alarm on a value the settled counter never shows.
        fn inc_nibble(byte: &mut u32, shift: u32) -> bool {
            let nibble = (*byte >> shift) & 0xF;
            if nibble < 0xF {
                *byte += 1 << shift;
                false
            } else {
                *byte &= !(0xF << shift);
                true
            }
        }
        let mut count = self.tod_count;
        let mut fired = false;
        let rippled_to_mid =
            inc_nibble(&mut count, 0) && inc_nibble(&mut count, 4) && inc_nibble(&mut count, 8);
        if rippled_to_mid {
            self.tod_count = count;
            fired |= self.tod_check_alarm();
            if inc_nibble(&mut count, 12) && inc_nibble(&mut count, 16) {
                inc_nibble(&mut count, 20);
            }
        }
        self.tod_count = count;
        fired |= self.tod_check_alarm();
        fired
    }

    /// Alarm comparator: latch ICR.ALRM on the transition into equality
    /// (see `tod_matching`). The alarm is a delayed source: the /IRQ pin
    /// follows one E-cycle later, surfacing from the `tick` drain, so
    /// this always returns false (no immediate pin edge).
    fn tod_check_alarm(&mut self) -> bool {
        let equal = self.tod_count == self.tod_alarm;
        let fired = !self.tod_matching && equal;
        self.tod_matching = equal;
        if !fired {
            return false;
        }
        self.latch_interrupts(ICR_ALRM)
    }

    /// Anchor the TOD counter to the current raster line. The emulator
    /// still advances CIA-B TOD per HSYNC between frames, but snapping
    /// at frame boundaries removes host-slice jitter from VBlank-based
    /// line-count tests.
    pub fn anchor_tod_to_frame(&mut self, line_phase: u32) {
        self.tod_frame_anchor = Some(TodFrameAnchor {
            count_at_write: self.tod_count,
            line_phase,
            frames: 0,
        });
    }

    /// Snap an anchored TOD counter to the exact frame-boundary value.
    /// An alarm match latches ICR.ALRM; the pin edge follows an E-cycle
    /// later from the `tick` drain (see `tick_tod`).
    pub fn sync_tod_to_frame(&mut self, lines_per_frame: u32) -> bool {
        if self.tod_stopped {
            return false;
        }
        let Some(mut anchor) = self.tod_frame_anchor else {
            return false;
        };
        anchor.frames = anchor.frames.saturating_add(1);
        self.tod_frame_anchor = Some(anchor);

        let phase = anchor.line_phase.min(lines_per_frame.saturating_sub(1));
        let first_frame_lines = lines_per_frame.saturating_sub(phase);
        let elapsed = first_frame_lines.saturating_add(
            anchor
                .frames
                .saturating_sub(1)
                .saturating_mul(lines_per_frame),
        );
        self.tod_count = anchor.count_at_write.wrapping_add(elapsed) & 0x00FF_FFFF;

        self.tod_check_alarm()
    }

    pub fn tod_writes_alarm(&self) -> bool {
        self.tod_write_alarm
    }

    /// Assert the external FLAG input. On Amiga CIA-B this is wired to
    /// the floppy index pulse, and software observes it via ICR bit 4.
    pub fn assert_flag(&mut self) -> bool {
        self.set_flag_pin(false)
    }

    pub fn release_flag(&mut self) {
        self.flag_pin_high = true;
    }

    fn set_flag_pin(&mut self, high: bool) -> bool {
        let falling_edge = self.flag_pin_high && !high;
        self.flag_pin_high = high;
        if falling_edge {
            return self.latch_interrupts(ICR_FLG);
        }
        false
    }

    pub fn read(&mut self, reg: usize) -> u8 {
        let reg = reg & 0xF;
        match reg {
            REG_TALO => (self.ta_count & 0xFF) as u8,
            REG_PRB => self.read_prb(),
            REG_PRA => self.read_port(REG_PRA, REG_DDRA),
            REG_TAHI => (self.ta_count >> 8) as u8,
            REG_TBLO => (self.tb_count & 0xFF) as u8,
            REG_TBHI => (self.tb_count >> 8) as u8,
            REG_TODLO => {
                // Reading TODLO releases the latch (subsequent reads
                // return live values again).
                let v = if !self.tod_write_alarm && self.tod_latched {
                    self.tod_latch
                } else {
                    self.tod_count
                };
                self.tod_latched = false;
                (v & 0xFF) as u8
            }
            REG_TODMID => {
                let v = if !self.tod_write_alarm && self.tod_latched {
                    self.tod_latch
                } else {
                    self.tod_count
                };
                ((v >> 8) & 0xFF) as u8
            }
            REG_TODHI => {
                // Reading TODHI latches the entire counter so the
                // following MID and LO reads return a consistent
                // snapshot. Re-reading TODHI before TODLO must not
                // refresh an existing snapshot.
                if !self.tod_write_alarm && !self.tod_latched {
                    self.tod_latch = self.tod_count;
                    self.tod_latched = true;
                }
                let v = if !self.tod_write_alarm && self.tod_latched {
                    self.tod_latch
                } else {
                    self.tod_count
                };
                ((v >> 16) & 0xFF) as u8
            }
            REG_SDR => self.sdr,
            REG_ICR => {
                // Reading ICR returns the latched events and the IR
                // bit, then clears every bit (and lowers the line,
                // cancelling a pin assert still in its E-cycle lag).
                let v = self.icr_data;
                self.icr_data = 0;
                self.irq_pin = false;
                self.irq_pin_delay_eticks = 0;
                // A timer underflow in this same E-cycle loses the race
                // and asserts the pin one E-cycle late.
                self.icr_read_race = true;
                v
            }
            _ => self.regs[reg],
        }
    }

    pub fn peek_register(&self, reg: usize) -> u8 {
        self.regs[reg & 0xF]
    }

    pub fn take_pc_pulse(&mut self) -> bool {
        std::mem::take(&mut self.pc_pulse_pending)
    }

    /// Port-A data direction register: set bits are CIA-driven outputs. The
    /// bus consults this to decide which port-A pins an external peripheral
    /// (the parallel port's Centronics status lines on CIA-B PA0-2) may
    /// drive.
    pub fn port_a_ddr(&self) -> u8 {
        self.regs[REG_DDRA]
    }

    /// Port-A pin levels as contributed by the CIA itself: outputs at their
    /// programmed level, inputs released (open-drain, pulled high). External
    /// peripherals are not overlaid here, so a pin an attached device drives
    /// (the parallel port's Centronics status inputs on CIA-B PA0-2, see
    /// `Bus::cia_b_read`) can sit at a different board-level value. The
    /// RS-232 control outputs on CIA-B (/DTR on PA7, /RTS on PA6) have no
    /// external driver, so for them this is the wire level, which a
    /// host-side serial bridge observes the way an attached modem would.
    pub fn port_a_pins(&self) -> u8 {
        self.read_port(REG_PRA, REG_DDRA)
    }

    /// Physical port-B pin levels without the `PC` strobe side effect of a
    /// guest PRB read. Motherboard wiring (floppy outputs and the Centronics
    /// data bus) observes pins continuously and must not create a second
    /// printer strobe merely by sampling them.
    pub fn port_b_pins(&self) -> u8 {
        let mut v = self.read_port(REG_PRB, REG_DDRB);
        if self.regs[REG_CRA] & CR_PBON != 0 {
            if self.ta_pb_output_high {
                v |= 1 << 6;
            } else {
                v &= !(1 << 6);
            }
        }
        if self.regs[REG_CRB] & CR_PBON != 0 {
            if self.tb_pb_output_high {
                v |= 1 << 7;
            } else {
                v &= !(1 << 7);
            }
        }
        v
    }

    pub fn write(&mut self, reg: usize, val: u8) -> CiaSideEffect {
        let reg = reg & 0xF;
        let prev_no_overlay = self.cia_a_no_overlay_line();
        let prev = self.regs[reg];
        self.regs[reg] = val;
        let mut started_timer = false;
        let keyboard_handshake_start = self.which == Which::A
            && reg == REG_CRA
            && prev & CRA_SPMODE == 0
            && val & CRA_SPMODE != 0;
        let keyboard_handshake_end = self.which == Which::A
            && reg == REG_CRA
            && prev & CRA_SPMODE != 0
            && val & CRA_SPMODE == 0;
        if reg == REG_CRA && (prev ^ val) & CRA_SPMODE != 0 {
            // Changing the serial-port direction resets the shift
            // counter (8520 behaviour). This is what makes the keyboard
            // protocol self-aligning: every KDAT handshake toggles
            // SPMODE, so the next byte always starts on a fresh count
            // even after lone sync bits shifted into the register.
            self.sdr_shift_count = 0;
            self.sdr_out = None;
        }

        match reg {
            REG_TALO => self.ta_latch = (self.ta_latch & 0xFF00) | val as u16,
            REG_TAHI => {
                self.ta_latch = (self.ta_latch & 0x00FF) | ((val as u16) << 8);
                if !self.ta_running {
                    // Per the 8520 datasheet: writing TAHI when the
                    // timer isn't running loads the count from the
                    // latch. In one-shot mode (CRA bit 3 = 1), it
                    // ALSO auto-starts the timer for one underflow.
                    // DiagROM's OLD CIA test relies on this: its
                    // preamble sets CRA = $08 (oneshot, run=0) and
                    // then re-arms each iteration by writing only
                    // TALO/TAHI, expecting the timer to fire once
                    // per re-arm. Reflect started_timer so the slice
                    // gets preempted; otherwise the CPU's tight
                    // poll-on-ICR loop would burn through a full
                    // default slice before our CIA tick advances the
                    // timer.
                    self.ta_count = self.ta_latch;
                    if self.ta_oneshot {
                        self.ta_running = true;
                        started_timer = true;
                        // The auto-started one-shot reads back as running:
                        // the 8520 sets the CRA START bit, and clears it on
                        // underflow (see the tick() underflow path). Without
                        // this, code that polls CRA bit 0 to time a one-shot
                        // delay (e.g. the Bitmap Brothers trackloader's motor
                        // spin-up wait) sees START=0 immediately and skips the
                        // delay entirely.
                        self.regs[REG_CRA] |= 0x01;
                    }
                }
            }
            REG_TBLO => self.tb_latch = (self.tb_latch & 0xFF00) | val as u16,
            REG_TBHI => {
                self.tb_latch = (self.tb_latch & 0x00FF) | ((val as u16) << 8);
                if !self.tb_running {
                    self.tb_count = self.tb_latch;
                    if self.tb_oneshot {
                        self.tb_running = true;
                        started_timer = true;
                        // Mirror the CRB START bit for the auto-started
                        // one-shot, same as timer A above.
                        self.regs[REG_CRB] |= 0x01;
                    }
                }
            }
            REG_PRB => {
                self.pc_pulse_pending = true;
            }
            REG_SDR => {
                self.sdr = val;
                if self.regs[REG_CRA] & CRA_SPMODE != 0 {
                    self.sdr_out = Some(val);
                    self.sdr_shift_count = 0;
                }
            }
            REG_ICR => {
                // bit 7 = SET/CLR. Low 5 bits select which mask bits.
                // The mask gates the CIA IR output level, so enabling
                // a source that is already latched asserts IR
                // immediately instead of waiting for a fresh edge.
                let bits = val & 0x1F;
                if val & 0x80 != 0 {
                    self.icr_mask |= bits;
                } else {
                    self.icr_mask &= !bits;
                }
                self.update_irq_line();
            }
            REG_TODLO => {
                if self.tod_write_alarm {
                    self.tod_alarm = (self.tod_alarm & 0xFFFF00) | val as u32;
                } else {
                    self.tod_count = (self.tod_count & 0xFFFF00) | val as u32;
                    // Writing TODLO restarts the counter after a
                    // TODHI write stopped it.
                    self.tod_stopped = false;
                }
                // A write that lands counter==alarm fires the comparator
                // just like an increment would (edge-detected).
                let _ = self.tod_check_alarm();
            }
            REG_TODMID => {
                if self.tod_write_alarm {
                    self.tod_alarm = (self.tod_alarm & 0xFF00FF) | ((val as u32) << 8);
                } else {
                    self.tod_count = (self.tod_count & 0xFF00FF) | ((val as u32) << 8);
                }
                let _ = self.tod_check_alarm();
            }
            REG_TODHI => {
                if self.tod_write_alarm {
                    self.tod_alarm = (self.tod_alarm & 0x00FFFF) | ((val as u32) << 16);
                } else {
                    self.tod_count = (self.tod_count & 0x00FFFF) | ((val as u32) << 16);
                    // Writing TODHI stops the counter until TODLO
                    // is written.
                    self.tod_stopped = true;
                }
                let _ = self.tod_check_alarm();
            }
            REG_CRA => {
                let prev_run = self.ta_running;
                self.ta_running = val & 0x01 != 0;
                self.ta_oneshot = val & 0x08 != 0;
                self.ta_counts_cnt = val & 0x20 != 0;
                self.regs[REG_CRA] = val & !CR_LOAD;
                // CRA bit 7 is the 8520 TODIN select latch. Copperline's
                // TOD pin source is configured by PAL/NTSC beam timing,
                // but the control bit is still readable by software.
                self.regs[REG_CRA] |= val & CRA_TODIN;
                // bit 4 = FORCE LOAD: copy latch -> count immediately.
                if val & CR_LOAD != 0 {
                    self.ta_count = self.ta_latch;
                }
                if !prev_run && self.ta_running {
                    started_timer = true;
                }
            }
            REG_CRB => {
                let prev_run = self.tb_running;
                self.tb_running = val & 0x01 != 0;
                self.tb_oneshot = val & 0x08 != 0;
                self.tb_input_mode = TimerBInputMode::from_crb(val);
                self.regs[REG_CRB] = val & !CR_LOAD;
                if val & CR_LOAD != 0 {
                    self.tb_count = self.tb_latch;
                }
                // Bit 7: 0 = TOD writes update the counter,
                //        1 = TOD writes update the alarm register.
                self.tod_write_alarm = val & 0x80 != 0;
                if !prev_run && self.tb_running {
                    started_timer = true;
                }
            }
            _ => {}
        }

        if self.which == Which::B && matches!(reg, REG_PRA | REG_DDRA) {
            // A500 board tie: CIA-B PA0 is wired to this CIA's own SP pin
            // and PA1 to its own CNT pin (the parallel port's SEL/POUT
            // lines, pulled up when not driven). Driving PA1 as an output
            // therefore clocks the CNT-mode timers, and PA0 supplies the
            // SP level the serial input shifter samples on that edge.
            let (old_pra, old_ddra) = if reg == REG_PRA {
                (prev, self.regs[REG_DDRA])
            } else {
                (self.regs[REG_PRA], prev)
            };
            let pin = |pra: u8, ddra: u8, bit: u8| -> bool { ddra & bit == 0 || pra & bit != 0 };
            let old_pa1 = pin(old_pra, old_ddra, 0x02);
            let new_pa1 = pin(self.regs[REG_PRA], self.regs[REG_DDRA], 0x02);
            if !old_pa1 && new_pa1 {
                let new_pa0 = pin(self.regs[REG_PRA], self.regs[REG_DDRA], 0x01);
                // Latches any timer/serial interrupt; the bus re-samples
                // irq_line_asserted after every CIA write.
                let _ = self.cnt_rising_edge(new_pa0);
            } else if old_pa1 && !new_pa1 {
                self.cnt_falling_edge();
            }
        }

        let mut effect = CiaSideEffect::default();
        if self.which == Which::A && matches!(reg, REG_PRA | REG_DDRA) {
            let now_no_overlay = self.cia_a_no_overlay_line();
            if !prev_no_overlay && now_no_overlay {
                effect.disable_overlay = true;
            }
        }
        effect.timer_started = started_timer;
        effect.keyboard_handshake = if keyboard_handshake_start {
            Some(true)
        } else if keyboard_handshake_end {
            Some(false)
        } else {
            None
        };
        effect
    }

    /// Advance both timers by `ticks` (CIA PHI2 cycles = CPU/10).
    /// Returns true if the external /IRQ pin asserted during this span,
    /// either from a masked-in timer underflow (same E-cycle) or from a
    /// delayed source (TOD/FLAG/serial/mask write) whose one-E-cycle
    /// lag drains here.
    pub fn tick(&mut self, ticks: u32) -> bool {
        // Zero E-clock ticks advance nothing: timer A needs ticks > 0, the
        // SDR shifter and timer B only move on timer-A underflows (or CNT,
        // which nothing drives between calls), and latch_interrupts(0) is a
        // no-op. The bus calls this per chip-bus quantum (1-4 cck), so 0
        // ticks is the common case (1 E-clock per 5 cck).
        if ticks == 0 {
            return false;
        }
        // PBON pulse mode holds PB6/PB7 low for one E-clock. A pulse armed by
        // the previous timer/CNT underflow returns high at this E-clock; a new
        // underflow later in this call can arm the next pulse. Reads only
        // observe the pin and never shorten the pulse.
        if self.ta_pb_pulse_low {
            self.ta_pb_pulse_low = false;
            self.ta_pb_output_high = true;
        }
        if self.tb_pb_pulse_low {
            self.tb_pb_pulse_low = false;
            self.tb_pb_output_high = true;
        }
        // Delayed asserts armed earlier (by FLAG, SDR, TOD, or mask
        // writes) surface here on the E-clock grid.
        let mut pin_edge = self.drain_irq_pin_delay(ticks);
        let mut timer_mask: u8 = 0;
        let mut ta_underflows = 0;
        if self.ta_running && !self.ta_counts_cnt && ticks > 0 {
            ta_underflows = advance(&mut self.ta_count, self.ta_latch, ticks);
            timer_mask |= u8::from(ta_underflows != 0) * ICR_TA;
            if timer_mask & ICR_TA != 0 && self.ta_oneshot {
                self.ta_running = false;
                self.regs[REG_CRA] &= !0x01;
            }
            if timer_mask & ICR_TA != 0 {
                self.update_timer_a_pb_output();
            }
        }
        let sp_mask = self.tick_sdr_output(ta_underflows);
        let tb_ticks = match self.tb_input_mode {
            TimerBInputMode::Phi2 => ticks,
            TimerBInputMode::Cnt => 0,
            TimerBInputMode::TimerA => ta_underflows,
            TimerBInputMode::TimerAWhileCntHigh => {
                if self.cnt_pin_high {
                    ta_underflows
                } else {
                    0
                }
            }
        };
        if self.tb_running && tb_ticks > 0 {
            let tb_underflows = advance(&mut self.tb_count, self.tb_latch, tb_ticks);
            timer_mask |= u8::from(tb_underflows != 0) * ICR_TB;
            if timer_mask & ICR_TB != 0 && self.tb_oneshot {
                self.tb_running = false;
                self.regs[REG_CRB] &= !0x01;
            }
            if timer_mask & ICR_TB != 0 {
                self.update_timer_b_pb_output();
            }
        }
        // Timer underflows drive the pin in the same E-cycle; serial
        // completion is a delayed source.
        pin_edge |= self.latch_timer_interrupts(timer_mask);
        let _ = self.latch_interrupts(sp_mask);
        // A delayed arm from this call's final E-cycle asserts the pin
        // at the NEXT E-cycle, so it does not fold into this call's edge.
        pin_edge |= self.drain_irq_pin_delay(ticks.saturating_sub(1));
        // Any ICR-read race window ends with the E-cycle covered here.
        self.icr_read_race = false;
        pin_edge
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_cnt_pin(&mut self, high: bool) {
        self.cnt_pin_high = high;
    }

    /// KCLK rising edge from the keyboard MCU, with the KDAT level
    /// currently on the SP pin: counts CNT-mode timers and, in serial
    /// input mode (SPMODE=0), shifts the bit into SDR. Returns true if
    /// the CIA's IRQ line just asserted.
    pub fn cnt_rising_edge(&mut self, sp_level: bool) -> bool {
        self.cnt_pin_high = true;
        let timers = self.pulse_cnt();
        let shifted = self.shift_sdr_input_bit(sp_level);
        timers || shifted
    }

    /// KCLK falling edge: only the pin level changes (timer-B's
    /// "timer A while CNT high" gate samples it).
    pub fn cnt_falling_edge(&mut self) {
        self.cnt_pin_high = false;
    }

    // Models a CNT pin edge (CRA/CRB INMODE counting CNT). Driven by the
    // keyboard MCU's KCLK line (CIA-A) and the PA1-to-CNT parallel-port
    // board tie (CIA-B) through cnt_rising_edge.
    fn pulse_cnt(&mut self) -> bool {
        let mut timer_mask = 0;
        let mut sp_mask = 0;
        if self.ta_running && self.ta_counts_cnt {
            let ta_underflows = advance(&mut self.ta_count, self.ta_latch, 1);
            timer_mask |= u8::from(ta_underflows != 0) * ICR_TA;
            if ta_underflows != 0 && self.ta_oneshot {
                self.ta_running = false;
                self.regs[REG_CRA] &= !0x01;
            }
            if ta_underflows != 0 {
                self.update_timer_a_pb_output();
            }
            sp_mask |= self.tick_sdr_output(ta_underflows);
            if self.tb_running
                && matches!(
                    self.tb_input_mode,
                    TimerBInputMode::TimerA | TimerBInputMode::TimerAWhileCntHigh
                )
                && (self.tb_input_mode != TimerBInputMode::TimerAWhileCntHigh || self.cnt_pin_high)
                && ta_underflows != 0
            {
                let tb_underflows = advance(&mut self.tb_count, self.tb_latch, ta_underflows);
                timer_mask |= u8::from(tb_underflows != 0) * ICR_TB;
                if tb_underflows != 0 {
                    self.update_timer_b_pb_output();
                }
            }
        }
        if self.tb_running && self.tb_input_mode == TimerBInputMode::Cnt {
            let tb_underflows = advance(&mut self.tb_count, self.tb_latch, 1);
            timer_mask |= u8::from(tb_underflows != 0) * ICR_TB;
            if tb_underflows != 0 && self.tb_oneshot {
                self.tb_running = false;
                self.regs[REG_CRB] &= !0x01;
            }
            if tb_underflows != 0 {
                self.update_timer_b_pb_output();
            }
        }
        let edge = self.latch_timer_interrupts(timer_mask);
        let _ = self.latch_interrupts(sp_mask);
        edge
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn shift_sdr_input_bit(&mut self, bit: bool) -> bool {
        if self.regs[REG_CRA] & CRA_SPMODE != 0 {
            return false;
        }
        self.sdr = (self.sdr << 1) | u8::from(bit);
        self.sdr_shift_count = self.sdr_shift_count.saturating_add(1);
        if self.sdr_shift_count < 8 {
            return false;
        }
        self.sdr_shift_count = 0;
        self.latch_interrupts(ICR_SP)
    }

    fn tick_sdr_output(&mut self, ta_underflows: u32) -> u8 {
        if self.regs[REG_CRA] & CRA_SPMODE == 0 || self.sdr_out.is_none() || ta_underflows == 0 {
            return 0;
        }
        let pulses = ta_underflows.min(8 - self.sdr_shift_count as u32) as u8;
        self.sdr_shift_count += pulses;
        if self.sdr_shift_count < 8 {
            return 0;
        }
        self.sdr_out = None;
        self.sdr_shift_count = 0;
        ICR_SP
    }

    /// Latch delayed interrupt sources (TOD alarm, FLAG, serial): the
    /// external pin follows one E-cycle later (vAmiga CIASetInt0), so
    /// no immediate edge is ever returned; it surfaces from the `tick`
    /// drain. The constant false keeps the signature parallel with
    /// `latch_timer_interrupts` for callers that forward the edge.
    fn latch_interrupts(&mut self, fired_mask: u8) -> bool {
        if fired_mask == 0 {
            return false;
        }
        self.icr_data |= fired_mask;
        let enabled = fired_mask & self.icr_mask;
        if enabled != 0 {
            let was_set = self.icr_data & ICR_IR != 0;
            self.icr_data |= ICR_IR;
            if !was_set {
                self.arm_irq_pin();
            }
        }
        false
    }

    /// Latch timer-underflow sources: the underflow drives the pin in
    /// the SAME E-cycle (vAmiga CIASetInt1) and the edge is returned to
    /// the caller now -- unless the underflow races an ICR read in this
    /// E-cycle, which pushes the assert one E-cycle out like a delayed
    /// source (vAmiga CIAReadIcr0 gating triggerTimerIrq).
    fn latch_timer_interrupts(&mut self, fired_mask: u8) -> bool {
        if fired_mask == 0 {
            return false;
        }
        self.icr_data |= fired_mask;
        if fired_mask & self.icr_mask == 0 {
            return false;
        }
        self.icr_data |= ICR_IR;
        if self.icr_read_race {
            self.arm_irq_pin();
            return false;
        }
        if self.irq_pin {
            return false;
        }
        self.irq_pin = true;
        self.irq_pin_delay_eticks = 0;
        true
    }

    /// The internal interrupt condition just asserted: the external pin
    /// follows one E-clock cycle later (drained in `drain_irq_pin_delay`).
    fn arm_irq_pin(&mut self) {
        if !self.irq_pin && self.irq_pin_delay_eticks == 0 {
            self.irq_pin_delay_eticks = 1;
        }
    }

    /// Advance the pin-lag countdown by `eticks`. Returns true when the
    /// pin asserts during this span (the bus latches INTREQ on that edge).
    fn drain_irq_pin_delay(&mut self, eticks: u32) -> bool {
        if self.irq_pin_delay_eticks == 0 || eticks == 0 {
            return false;
        }
        if eticks as u64 >= u64::from(self.irq_pin_delay_eticks) {
            self.irq_pin_delay_eticks = 0;
            // The condition may have been acknowledged inside the lag
            // window; the pin only rises if it still holds.
            if self.icr_data & ICR_IR != 0 {
                self.irq_pin = true;
                return true;
            }
        } else {
            self.irq_pin_delay_eticks -= eticks as u8;
        }
        false
    }

    pub fn irq_line_asserted(&self) -> bool {
        self.irq_pin
    }

    /// Test-harness stand-in for the E-clock passing: settle a pending
    /// pin assert whose lag the caller's own timekeeping already covers
    /// (the bus drains through `tick`; timers are not advanced here).
    #[cfg(test)]
    pub fn settle_irq_pin(&mut self) -> bool {
        self.drain_irq_pin_delay(u32::from(u8::MAX))
    }

    fn update_irq_line(&mut self) {
        if self.icr_data & self.icr_mask & 0x1F != 0 {
            if self.icr_data & ICR_IR == 0 {
                self.icr_data |= ICR_IR;
                self.arm_irq_pin();
            }
        } else {
            self.icr_data &= !ICR_IR;
            self.irq_pin = false;
            self.irq_pin_delay_eticks = 0;
        }
    }

    fn read_port(&self, port_reg: usize, ddr_reg: usize) -> u8 {
        let ddr = self.regs[ddr_reg];
        (self.regs[port_reg] & ddr) | !ddr
    }

    fn cia_a_no_overlay_line(&self) -> bool {
        self.which == Which::A && (self.read_port(REG_PRA, REG_DDRA) & 0x01) == 0
    }

    fn read_prb(&mut self) -> u8 {
        self.pc_pulse_pending = true;
        self.port_b_pins()
    }

    fn update_timer_a_pb_output(&mut self) {
        self.update_pb_output(REG_CRA, true);
    }

    fn update_timer_b_pb_output(&mut self) {
        self.update_pb_output(REG_CRB, false);
    }

    fn update_pb_output(&mut self, control_reg: usize, timer_a: bool) {
        let control = self.regs[control_reg];
        if control & CR_PBON == 0 {
            return;
        }
        if control & CR_OUTMODE != 0 {
            if timer_a {
                self.ta_pb_output_high = !self.ta_pb_output_high;
            } else {
                self.tb_pb_output_high = !self.tb_pb_output_high;
            }
        } else if timer_a {
            self.ta_pb_output_high = false;
            self.ta_pb_pulse_low = true;
        } else {
            self.tb_pb_output_high = false;
            self.tb_pb_pulse_low = true;
        }
    }
}

#[inline]
fn advance(count: &mut u16, latch: u16, ticks: u32) -> u32 {
    // Decrement `*count` by `ticks`, wrapping through the latch on
    // underflow. Returns 1 if at least one underflow happened.
    let mut underflows = 0u32;
    let mut t = ticks;
    loop {
        let c = *count as u32;
        if t <= c {
            *count = (c - t) as u16;
            return underflows;
        }
        t -= c + 1;
        *count = latch;
        underflows = underflows.saturating_add(1);
        if latch == 0 {
            return underflows;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
enum TimerBInputMode {
    #[default]
    Phi2,
    Cnt,
    TimerA,
    TimerAWhileCntHigh,
}

impl TimerBInputMode {
    fn from_crb(val: u8) -> Self {
        match (val >> 5) & 0x03 {
            0 => Self::Phi2,
            1 => Self::Cnt,
            2 => Self::TimerA,
            _ => Self::TimerAWhileCntHigh,
        }
    }
}

/// Side effects from a single `Cia::write`. These are independent flags,
/// not an exclusive choice: one CRA byte write can both start a timer
/// (bit 0) and toggle SPMODE (bit 6) in the same write, and both must be
/// reported or the keyboard handshake edge is silently dropped whenever
/// it coincides with a timer start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CiaSideEffect {
    /// CIA-A /OVL was driven low: stop overlaying the ROM at $0 and
    /// switch to chip RAM there.
    pub disable_overlay: bool,
    /// CRA/CRB bit 0 transitioned 0 -> 1: a timer just started. The
    /// emulator preempts the current instruction slice so the next
    /// slice's dynamic cap (computed from `next_underflow_ticks`)
    /// takes effect for the new run, instead of leaving the slice
    /// to run all the way to its larger default size while the CPU
    /// tight-polls ICR/timer counts.
    pub timer_started: bool,
    /// CIA-A CRA.SPMODE transitioned: `Some(true)` on 0 -> 1 (serial
    /// output mode drives the SP/KDAT line low, which the keyboard MCU
    /// sees as the start of the post-byte handshake pulse), `Some(false)`
    /// on 1 -> 0 (SP released; the MCU measures the pulse between start
    /// and end and accepts any deliberate handshake).
    pub keyboard_handshake: Option<bool>,
}

/// Map a raw 24-bit Amiga bus address into a CIA register index using
/// address bits A8..A11.
pub fn reg_from_addr(addr: u64) -> usize {
    ((addr >> 8) & 0xF) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchored_tod_syncs_to_pal_frame_boundaries() {
        let mut cia = Cia::new(Which::B);
        cia.write(REG_TODHI, 0);
        cia.write(REG_TODMID, 0);
        cia.write(REG_TODLO, 0);
        cia.anchor_tod_to_frame(0);

        for _ in 0..16 {
            cia.sync_tod_to_frame(crate::chipset::agnus::PAL_LINES);
        }

        assert_eq!(cia.read(REG_TODHI), 0x00);
        assert_eq!(cia.read(REG_TODMID), 0x13);
        assert_eq!(cia.read(REG_TODLO), 0x90);
    }

    #[test]
    fn cia_a_cra_spmode_transitions_report_keyboard_handshake_edges() {
        let mut cia = Cia::new(Which::A);

        assert_eq!(
            cia.write(REG_CRA, CRA_SPMODE).keyboard_handshake,
            Some(true)
        );
        // Rewriting the same mode is not an edge.
        assert_eq!(cia.write(REG_CRA, CRA_SPMODE).keyboard_handshake, None);
        assert_eq!(cia.write(REG_CRA, 0).keyboard_handshake, Some(false));
        assert_eq!(cia.write(REG_CRA, 0).keyboard_handshake, None);
    }

    #[test]
    fn cia_a_cra_write_reports_timer_start_and_keyboard_edge_together() {
        // A single CRA byte can flip both START (bit 0) and SPMODE (bit 6)
        // at once. Both side effects must be reported, or the keyboard MCU
        // edge is silently dropped whenever it coincides with a timer
        // start (this previously collapsed to TimerStarted only).
        let mut cia = Cia::new(Which::A);
        let effect = cia.write(REG_CRA, CRA_SPMODE | 0x01);
        assert!(effect.timer_started);
        assert_eq!(effect.keyboard_handshake, Some(true));
    }

    #[test]
    fn cnt_rising_edges_shift_sp_bits_into_sdr_in_input_mode() {
        let mut cia = Cia::new(Which::A);
        cia.write(REG_ICR, 0x80 | ICR_SP); // unmask SP

        // 0xA5 MSB-first.
        for (i, bit) in [true, false, true, false, false, true, false, true]
            .into_iter()
            .enumerate()
        {
            let _ = cia.cnt_rising_edge(bit);
            cia.cnt_falling_edge();
            // The pin lags the internal latch by one E-cycle.
            let irq = cia.settle_irq_pin();
            assert_eq!(irq, i == 7, "IRQ only on the 8th bit (bit {i})");
        }
        assert_eq!(cia.read(REG_SDR), 0xA5);
        assert_ne!(cia.read(REG_ICR) & ICR_SP, 0);
    }

    #[test]
    fn cnt_rising_edges_do_not_shift_sdr_in_output_mode() {
        let mut cia = Cia::new(Which::A);
        cia.write(REG_ICR, 0x80 | ICR_SP); // unmask SP
        cia.write(REG_CRA, CRA_SPMODE);
        for _ in 0..5 {
            cia.cnt_rising_edge(true);
            cia.cnt_falling_edge();
        }
        // Input shifter untouched by the output-mode edges: back in
        // input mode, a full byte still needs all 8 edges, with the SP
        // interrupt exactly on the 8th.
        cia.write(REG_CRA, 0);
        for i in 0..8 {
            let _ = cia.cnt_rising_edge(i % 2 == 0);
            cia.cnt_falling_edge();
            let fired = cia.settle_irq_pin();
            assert_eq!(fired, i == 7, "SP must latch exactly on edge 8 (edge {i})");
        }
        assert_eq!(cia.read(REG_SDR), 0xAA);
    }

    #[test]
    fn cia_b_pra_output_toggles_clock_its_own_cnt_pin() {
        // A500 board tie: CIA-B PA1 (driven as an output) is wired back to
        // the same CIA's CNT pin, so software can clock a CNT-mode timer by
        // toggling PRA - the vAmigaTS CIA/cnt tests' mechanism.
        let mut cia = Cia::new(Which::B);
        cia.write(REG_ICR, 0x80 | ICR_TA); // unmask timer A
        cia.write(REG_DDRA, 0xFF); // PA outputs; PRA=0 drives PA1 low
        cia.write(REG_TALO, 3);
        cia.write(REG_TAHI, 0);
        cia.write(REG_CRA, 0x21); // START | INMODE = CNT edges

        for _ in 0..2 {
            cia.write(REG_PRA, 0xFF); // rising edge on PA1 -> CNT
            cia.write(REG_PRA, 0x00); // falling edge
        }
        assert_eq!(cia.ta_count, 1, "two PA1 rises must count twice");
        assert!(!cia.irq_line_asserted());

        cia.write(REG_PRA, 0xFF);
        assert_eq!(cia.ta_count, 0);
        cia.write(REG_PRA, 0x00);
        cia.write(REG_PRA, 0xFF); // underflow edge asserts the pin same-cycle
        assert!(
            cia.irq_line_asserted(),
            "underflow must assert the IRQ line"
        );

        // PA1 as an INPUT floats high: further PRA writes are no edges.
        cia.write(REG_ICR, ICR_TA); // mask + ack
        let _ = cia.read(REG_ICR);
        cia.write(REG_DDRA, 0x00);
        let count = cia.ta_count;
        cia.write(REG_PRA, 0xFF);
        cia.write(REG_PRA, 0x00);
        assert_eq!(
            cia.ta_count, count,
            "input-mode PRA writes must not clock CNT"
        );
    }

    #[test]
    fn port_a_pins_report_driven_levels_and_released_inputs() {
        // CIA-B PA7 is /DTR: driven low = asserted, and an undriven pin
        // (input) reads high through the pull-up, so a freshly reset CIA
        // reports the line deasserted. This is what a host-side serial
        // bridge observes to tell whether a terminal has opened the port.
        let mut cia = Cia::new(Which::B);
        assert_eq!(cia.port_a_pins() & 0x80, 0x80, "reset: /DTR released");

        cia.write(REG_DDRA, 0xC0); // /DTR + /RTS as outputs
        cia.write(REG_PRA, 0x00); // drive both low = asserted
        assert_eq!(cia.port_a_pins() & 0xC0, 0x00, "driven low = asserted");

        cia.write(REG_PRA, 0x80); // raise /DTR, keep /RTS low
        assert_eq!(cia.port_a_pins() & 0xC0, 0x80);

        cia.write(REG_DDRA, 0x00); // back to inputs: pins float high
        assert_eq!(cia.port_a_pins() & 0xC0, 0xC0, "inputs read released");
    }

    #[test]
    fn cnt_edges_count_timer_a_in_cnt_mode() {
        let mut cia = Cia::new(Which::A);
        cia.write(REG_ICR, 0x80 | ICR_TA); // unmask timer A
        cia.write(REG_TALO, 3);
        cia.write(REG_TAHI, 0);
        // CRA: START | LOAD | INMODE=CNT (bit 5).
        cia.write(REG_CRA, 0x01 | 0x10 | 0x20);
        let mut edges = 0;
        let fired = loop {
            edges += 1;
            // A timer underflow asserts the pin on the same edge.
            let fired = cia.cnt_rising_edge(true);
            cia.cnt_falling_edge();
            if fired || edges > 16 {
                break fired;
            }
        };
        assert!(fired, "timer A never fired from CNT edges");
        // Latch 3 underflows on the edge that wraps past 0.
        assert_eq!(edges, 4);
        assert_ne!(cia.read(REG_ICR) & ICR_TA, 0);
    }

    #[test]
    fn control_force_load_bit_is_a_write_only_strobe() {
        let mut cia = Cia::new(Which::A);
        cia.ta_latch = 0x1234;
        cia.tb_latch = 0x5678;

        cia.write(REG_CRA, CRA_TODIN | CR_LOAD | 0x01);
        cia.write(REG_CRB, 0x80 | CR_LOAD | 0x01);

        assert_eq!(cia.ta_count, 0x1234);
        assert_eq!(cia.tb_count, 0x5678);
        assert_eq!(cia.read(REG_CRA) & CR_LOAD, 0);
        assert_eq!(cia.read(REG_CRB) & CR_LOAD, 0);
        assert_ne!(cia.read(REG_CRA) & CRA_TODIN, 0);
    }

    #[test]
    fn todin_bit_is_readback_latch_not_tod_clock_source() {
        let mut cia = Cia::new(Which::A);

        cia.write(REG_CRA, CRA_TODIN);
        assert_ne!(cia.read(REG_CRA) & CRA_TODIN, 0);
        cia.tick_tod();
        assert_eq!(cia.tod_count, 1);

        cia.write(REG_CRA, 0);
        assert_eq!(cia.read(REG_CRA) & CRA_TODIN, 0);
        cia.tick_tod();
        assert_eq!(cia.tod_count, 2);
    }

    #[test]
    fn tod_alarm_deadline_counts_ticks_until_match() {
        let mut cia = Cia::new(Which::B);
        cia.tod_count = 5;
        cia.tod_alarm = 8;

        assert_eq!(cia.next_tod_alarm_ticks(), Some(3));

        cia.tod_alarm = 5;
        assert_eq!(cia.next_tod_alarm_ticks(), Some(0x0100_0000));

        cia.tod_stopped = true;
        assert_eq!(cia.next_tod_alarm_ticks(), None);
    }

    #[test]
    fn tod_alarm_resets_to_zero_and_does_not_fire_at_power_on() {
        // The alarm resets to $000000 (WinUAE/vAmiga agree); boot code relies
        // on it by writing only the alarm HI byte. There is
        // still no spurious match at power-on: the alarm fires on the
        // transition INTO equality, and the counter leaves $000000 on its
        // first tick.
        let mut cia = Cia::new(Which::A);
        cia.write(REG_ICR, 0x80 | ICR_ALRM);

        for _ in 0..16 {
            assert!(!cia.tick_tod());
        }
        assert_eq!(cia.read(REG_ICR) & (ICR_ALRM | ICR_IR), 0);
    }

    #[test]
    fn tod_alarm_write_that_matches_the_counter_fires_the_comparator() {
        // The 8520 alarm comparator is evaluated on counter/alarm WRITES as
        // well as on counter ticks, edge-detected: arming the alarm at the
        // live count raises ALRM immediately, re-writing the same value does
        // not re-fire, and the power-on state (counter and alarm both zero)
        // counts as already matching so the boot ROM's alarm-HI-only write
        // stays silent. (vAmiga TOD::checkIrq; vAmigaTS CIA/TOD tod1/tod3.)
        let mut cia = Cia::new(Which::A);
        cia.write(REG_ICR, 0x80 | ICR_ALRM);

        // Power-on: counter == alarm == 0. Writing an alarm byte that keeps
        // them equal must not fire (9 Fingers boot relies on this).
        cia.write(REG_CRB, 0x80);
        cia.write(REG_TODHI, 0x00);
        assert_eq!(cia.read(REG_ICR) & (ICR_ALRM | ICR_IR), 0);

        // Move the counter off the alarm value.
        cia.write(REG_CRB, 0x00);
        cia.write(REG_TODHI, 0x00);
        cia.write(REG_TODMID, 0x00);
        cia.write(REG_TODLO, 0x10);
        assert_eq!(cia.read(REG_ICR) & (ICR_ALRM | ICR_IR), 0);

        // Arming the alarm at the live count fires on the write.
        cia.write(REG_CRB, 0x80);
        cia.write(REG_TODHI, 0x00);
        cia.write(REG_TODMID, 0x00);
        cia.write(REG_TODLO, 0x10);
        assert_eq!(cia.tod_alarm, 0x000010);
        assert_eq!(cia.read(REG_ICR) & (ICR_ALRM | ICR_IR), ICR_ALRM | ICR_IR);

        // Still matching: re-writing the same alarm byte does not re-fire.
        cia.write(REG_TODLO, 0x10);
        assert_eq!(cia.read(REG_ICR) & (ICR_ALRM | ICR_IR), 0);

        // The tick away from the alarm un-matches without firing.
        assert!(!cia.tick_tod());
        assert_eq!(cia.tod_count, 0x000011);
        assert_eq!(cia.read(REG_ICR) & ICR_ALRM, 0);
    }

    #[test]
    fn tod_alarm_fires_on_the_tick_that_reaches_it() {
        let mut cia = Cia::new(Which::A);
        cia.write(REG_ICR, 0x80 | ICR_ALRM);
        // Alarm 0x21, counter 0x20, both through the register interface so
        // the comparator's matching edge state tracks them.
        cia.write(REG_CRB, 0x80);
        cia.write(REG_TODHI, 0x00);
        cia.write(REG_TODMID, 0x00);
        cia.write(REG_TODLO, 0x21);
        cia.write(REG_CRB, 0x00);
        cia.write(REG_TODHI, 0x00);
        cia.write(REG_TODMID, 0x00);
        cia.write(REG_TODLO, 0x20);
        let _ = cia.read(REG_ICR);

        assert!(!cia.tick_tod(), "the pin lags the alarm latch");
        assert_eq!(cia.tod_count, 0x000021);
        assert!(!cia.irq_line_asserted());
        assert!(
            cia.settle_irq_pin(),
            "the alarm reaches the pin an E-cycle later"
        );
        assert_eq!(cia.read(REG_ICR) & (ICR_ALRM | ICR_IR), ICR_ALRM | ICR_IR);
    }

    #[test]
    fn todhi_read_keeps_existing_latch_until_todlo_releases_it() {
        let mut cia = Cia::new(Which::A);
        cia.tod_count = 0x010203;

        assert_eq!(cia.read(REG_TODHI), 0x01);
        cia.tod_count = 0x040506;
        assert_eq!(cia.read(REG_TODHI), 0x01);
        assert_eq!(cia.read(REG_TODMID), 0x02);
        assert_eq!(cia.read(REG_TODLO), 0x03);
        assert_eq!(cia.read(REG_TODHI), 0x04);
    }

    #[test]
    fn tod_alarm_mode_writes_alarm_but_reads_live_counter() {
        let mut cia = Cia::new(Which::A);
        cia.tod_count = 0x445566;
        cia.tod_alarm = 0x112233;
        cia.write(REG_CRB, 0x80);

        cia.write(REG_TODHI, 0xAA);
        cia.write(REG_TODMID, 0xBB);
        cia.write(REG_TODLO, 0xCC);
        assert_eq!(cia.tod_alarm, 0xAABBCC);
        assert_eq!(cia.tod_count, 0x445566);

        assert_eq!(cia.read(REG_TODHI), 0x44);
        assert!(!cia.tod_latched);
        cia.tod_count = 0x778899;
        assert_eq!(cia.read(REG_TODMID), 0x88);
        assert_eq!(cia.read(REG_TODLO), 0x99);
    }

    #[test]
    fn timer_underflow_asserts_pin_in_same_e_cycle() {
        // vAmiga CIASetInt1: a timer underflow pulls the pin down in the
        // E-cycle of the underflow itself, with no one-cycle lag.
        let mut cia = Cia::new(Which::A);
        cia.write(REG_ICR, 0x80 | ICR_TA);
        cia.write(REG_TALO, 3);
        cia.write(REG_TAHI, 0);
        cia.write(REG_CRA, 0x01);

        assert!(!cia.tick(3));
        assert!(!cia.irq_line_asserted());
        assert!(cia.tick(1), "underflow must assert the pin same-cycle");
        assert!(cia.irq_line_asserted());
    }

    #[test]
    fn icr_read_race_delays_timer_pin_by_one_e_cycle() {
        // vAmiga CIAReadIcr0: a timer underflow in the same E-cycle as
        // an ICR read asserts the pin one E-cycle late.
        let mut cia = Cia::new(Which::A);
        cia.write(REG_ICR, 0x80 | ICR_TA);
        cia.write(REG_TALO, 3);
        cia.write(REG_TAHI, 0);
        cia.write(REG_CRA, 0x01);

        assert!(!cia.tick(3));
        let _ = cia.read(REG_ICR); // the read the underflow races
        assert!(!cia.tick(1), "racing underflow must not assert same-cycle");
        assert!(!cia.irq_line_asserted());
        assert!(cia.tick(1), "the assert lands one E-cycle later");
        assert!(cia.irq_line_asserted());
    }

    #[test]
    fn probe_pin_drain_after_sdr_arm() {
        let mut cia = Cia::new(Which::A);
        cia.write(REG_ICR, 0x88); // enable SP
                                  // SPMODE=0 input; shift 8 bits in via CNT.
        for _ in 0..8 {
            let _ = cia.cnt_rising_edge(true);
            cia.cnt_falling_edge();
        }
        assert!(!cia.irq_line_asserted(), "pin must lag the ICR condition");
        assert!(cia.tick(1), "one E-cycle later the pin asserts");
        assert!(cia.irq_line_asserted());
    }

    #[test]
    fn flag_input_latches_icr_and_respects_mask() {
        let mut cia = Cia::new(Which::B);

        assert!(!cia.assert_flag());
        assert_eq!(cia.read(REG_ICR), ICR_FLG);
        assert!(!cia.assert_flag());
        assert_eq!(cia.read(REG_ICR), 0);

        cia.write(REG_ICR, 0x80 | ICR_FLG);
        cia.release_flag();
        let _ = cia.assert_flag();
        assert!(
            cia.settle_irq_pin(),
            "FLAG edge reaches the pin an E-cycle later"
        );
        assert_eq!(cia.read(REG_ICR), ICR_IR | ICR_FLG);
    }

    #[test]
    fn icr_mask_set_asserts_already_latched_timer_source() {
        let mut cia = Cia::new(Which::B);
        cia.write(REG_TALO, 0);
        cia.write(REG_TAHI, 0);
        cia.write(REG_CRA, 0x01);

        assert!(!cia.tick(1));
        assert_eq!(cia.icr_data & ICR_TA, ICR_TA);
        assert!(!cia.irq_line_asserted());

        cia.write(REG_ICR, 0x80 | ICR_TA);

        assert!(!cia.irq_line_asserted(), "the pin lags the mask write");
        assert!(cia.settle_irq_pin());
        assert!(cia.irq_line_asserted());
        assert_eq!(cia.read(REG_ICR), ICR_IR | ICR_TA);
    }

    #[test]
    fn timer_b_can_count_timer_a_underflows() {
        let mut cia = Cia::new(Which::A);
        cia.write(REG_TALO, 0);
        cia.write(REG_TAHI, 0);
        cia.write(REG_TBLO, 0);
        cia.write(REG_TBHI, 0);
        cia.write(REG_CRB, 0x40 | 0x01);
        cia.write(REG_CRA, 0x01);

        cia.tick(1);

        assert_eq!(cia.read(REG_ICR) & (ICR_TA | ICR_TB), ICR_TA | ICR_TB);
    }

    #[test]
    fn timer_b_gated_timer_a_mode_respects_cnt_pin() {
        let mut cia = Cia::new(Which::A);
        cia.write(REG_TALO, 0);
        cia.write(REG_TAHI, 0);
        cia.write(REG_TBLO, 0);
        cia.write(REG_TBHI, 0);
        cia.write(REG_CRB, 0x60 | 0x01);
        cia.write(REG_CRA, 0x01);
        cia.set_cnt_pin(false);

        cia.tick(1);
        assert_eq!(cia.read(REG_ICR) & ICR_TB, 0);

        cia.set_cnt_pin(true);
        cia.tick(1);
        assert_eq!(cia.read(REG_ICR) & ICR_TB, ICR_TB);
    }

    #[test]
    fn timer_a_toggle_output_overrides_pb6_pin() {
        let mut cia = Cia::new(Which::A);
        cia.write(REG_DDRB, 0x00);
        cia.write(REG_TALO, 0);
        cia.write(REG_TAHI, 0);
        cia.write(REG_CRA, CR_PBON | CR_OUTMODE | 0x01);

        assert_ne!(cia.read(REG_PRB) & 0x40, 0);
        cia.tick(1);
        assert_eq!(cia.read(REG_PRB) & 0x40, 0);
        cia.tick(1);
        assert_ne!(cia.read(REG_PRB) & 0x40, 0);
    }

    #[test]
    fn timer_b_pulse_output_overrides_pb7_pin_once() {
        let mut cia = Cia::new(Which::A);
        cia.write(REG_DDRB, 0x00);
        cia.write(REG_TBLO, 0);
        cia.write(REG_TBHI, 0);
        cia.write(REG_CRB, CR_PBON | 0x08 | 0x01); // one-shot pulse

        cia.tick(1);
        assert_eq!(cia.read(REG_PRB) & 0x80, 0);
        assert_eq!(
            cia.read(REG_PRB) & 0x80,
            0,
            "reading PRB must not shorten the E-clock pulse"
        );
        cia.tick(1);
        assert_ne!(cia.read(REG_PRB) & 0x80, 0);
    }

    #[test]
    fn port_b_access_latches_pc_pulse() {
        let mut cia = Cia::new(Which::A);

        assert!(!cia.take_pc_pulse());
        cia.write(REG_PRB, 0x55);
        assert!(cia.take_pc_pulse());
        assert!(!cia.take_pc_pulse());

        let _ = cia.read(REG_PRB);
        assert!(cia.take_pc_pulse());

        let _ = cia.port_b_pins();
        assert!(
            !cia.take_pc_pulse(),
            "continuous pin sampling has no PC edge"
        );
    }

    #[test]
    fn cia_a_driving_ovl_low_releases_reset_overlay() {
        let mut cia = Cia::new(Which::A);

        assert_eq!(cia.write(REG_DDRA, 0x03), CiaSideEffect::default());
        assert!(cia.write(REG_PRA, 0x02).disable_overlay);
        assert_eq!(cia.read(REG_PRA) & 0x01, 0);
    }

    #[test]
    fn sdr_output_sets_sp_after_eight_timer_a_underflows() {
        let mut cia = Cia::new(Which::A);
        cia.write(REG_TALO, 0);
        cia.write(REG_TAHI, 0);
        cia.write(REG_CRA, CRA_SPMODE);
        cia.write(REG_SDR, 0xA5);
        cia.write(REG_CRA, CRA_SPMODE | 0x01);

        for _ in 0..7 {
            cia.tick(1);
        }
        assert_eq!(cia.read(REG_ICR) & ICR_SP, 0);
        cia.tick(1);
        assert_eq!(cia.read(REG_ICR) & ICR_SP, ICR_SP);
    }

    #[test]
    fn sdr_input_shifts_bits_and_sets_sp() {
        let mut cia = Cia::new(Which::A);
        for bit in [true, false, true, false, false, true, false, true] {
            cia.shift_sdr_input_bit(bit);
        }

        assert_eq!(cia.read(REG_SDR), 0b1010_0101);
        assert_eq!(cia.read(REG_ICR) & ICR_SP, ICR_SP);
    }
}
