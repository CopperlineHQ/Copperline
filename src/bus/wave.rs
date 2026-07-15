//! Waveform-capture taps: the bus-side half of the VCD "logic analyser"
//! export (src/waveform.rs owns the writer and capture state machine).
//!
//! The per-quantum sampler joins the other gated instrumentation sinks in
//! `advance_one_chip_bus_quantum_limited`; register writes and CPU chip-bus
//! grants tap their existing choke points. Every tap is fenced behind the
//! plain `wave_on` bool so the hot path costs a single predictable branch
//! while no capture is armed.

use super::{BeamWriteSource, Bus, ChipBusOwner, CpuBusAccessKind, CHIP_BUS_OWNER_NAMES};
use crate::chipset::paula::{pending_ipl, INT_MASTER, PAULA_CLOCK_HZ};
use crate::waveform::{QuantumSample, Trigger, WaveCapture, WaveOptions, WaveStatus};

impl Bus {
    /// Arm a waveform capture, replacing (and finishing) any existing one.
    /// The VCD declaration section is written immediately; value changes
    /// start when the trigger fires.
    pub fn wave_arm(&mut self, opts: WaveOptions) -> std::io::Result<()> {
        if let Some(mut old) = self.wave.take() {
            old.finish();
        }
        let capture = WaveCapture::create(opts)?;
        let status = capture.status();
        log::info!(
            "waveform: armed (trigger {}, duration {}, signals {}) -> {}",
            status.trigger,
            status.duration,
            status.signals,
            capture.path().display()
        );
        self.wave_pc_trigger = matches!(capture.trigger(), Trigger::Pc(_));
        let immediate = matches!(capture.trigger(), Trigger::Now);
        self.wave_on = true;
        self.wave = Some(Box::new(capture));
        if immediate {
            self.wave_fire();
        }
        Ok(())
    }

    /// Stop and discard the capture (finishing its file), returning the
    /// final status. None when no capture exists.
    pub fn wave_stop(&mut self) -> Option<WaveStatus> {
        self.wave_on = false;
        self.wave_pc_trigger = false;
        let mut wave = self.wave.take()?;
        wave.finish();
        Some(wave.status())
    }

    /// Status of the current capture (armed, capturing, or done), if any.
    pub fn wave_status(&self) -> Option<WaveStatus> {
        self.wave.as_deref().map(WaveCapture::status)
    }

    /// Transition Armed -> Capturing at the current emulated time.
    fn wave_fire(&mut self) {
        let cck_per_frame =
            u64::from(self.agnus.current_frame_lines()) * u64::from(self.agnus.current_line_cck());
        let now = self.emulated_cck;
        self.wave_pc_trigger = false;
        if let Some(wave) = self.wave.as_deref_mut() {
            wave.fire(now, cck_per_frame, PAULA_CLOCK_HZ as f64);
        }
        log::info!(
            "waveform: trigger fired at [{},{}] frame {}",
            self.agnus.vpos,
            self.agnus.hpos,
            self.emulated_frames
        );
    }

    /// Close out the capture window: flush the file and drop back to the
    /// zero-cost path. The finished capture stays around for status queries.
    fn wave_finish(&mut self) {
        self.wave_on = false;
        self.wave_pc_trigger = false;
        if let Some(wave) = self.wave.as_deref_mut() {
            wave.finish();
            if wave.write_failed() {
                log::warn!(
                    "waveform: capture aborted by a write error; {} is incomplete",
                    wave.path().display()
                );
            } else {
                log::info!(
                    "waveform: wrote {} ({} samples)",
                    wave.path().display(),
                    wave.samples()
                );
            }
        }
    }

    /// Per-quantum sampler and Now/Time trigger check. Called from the
    /// chip-bus arbitration point with the owner of the quantum that is
    /// about to elapse; the beam counters and `emulated_cck` still hold
    /// the quantum-start position.
    pub(super) fn wave_tap_quantum(&mut self, owner: ChipBusOwner) {
        if let Some(wave) = self.wave.as_deref() {
            if wave.is_armed() {
                let fire = match wave.trigger() {
                    Trigger::Now => true,
                    Trigger::Time(secs) => self.emulated_seconds() >= secs,
                    _ => false,
                };
                if fire {
                    self.wave_fire();
                }
            }
        }
        if !self.wave.as_deref().is_some_and(WaveCapture::is_capturing) {
            return;
        }
        let pending = self.paula.intena & self.cpu_visible_intreq();
        let ipl = if self.paula.intena & INT_MASTER == 0 {
            0
        } else {
            pending_ipl(pending)
        };
        let audio_channel = if matches!(owner, ChipBusOwner::Audio) {
            Self::audio_dma_channel_at(self.agnus.hpos).map(|channel| channel as u8)
        } else {
            None
        };
        let sample = QuantumSample {
            cck: self.emulated_cck,
            vpos: self.agnus.vpos,
            hpos: self.agnus.hpos,
            frame: self.emulated_frames,
            owner_index: owner.accounting_index() as u8,
            owner_name: CHIP_BUS_OWNER_NAMES[owner.accounting_index()],
            dmacon: self.agnus.dmacon,
            data_bus: self.data_bus,
            cop_pc: self.copper.pc(),
            cop_state: self.copper.state_label(),
            blt_busy: self.blitter.busy,
            blt_slot: self.blitter.current_slot_label(),
            blt_pt: [
                self.blitter.bltapt,
                self.blitter.bltbpt,
                self.blitter.bltcpt,
                self.blitter.bltdpt,
            ],
            ipl,
            intreq: self.paula.intreq,
            intena: self.paula.intena,
            audio_channel,
        };
        let now = self.emulated_cck;
        let Some(wave) = self.wave.as_deref_mut() else {
            return;
        };
        wave.sample_quantum(&sample);
        if wave.expired(now) {
            self.wave_finish();
        }
    }

    /// Beam-trigger check, mirroring the crossing semantics of
    /// `check_ui_beam_traps`: called after the beam advanced from `old`
    /// (exclusive) to the current position (inclusive). Only flips the
    /// capture on -- it never stops the machine.
    pub(super) fn wave_note_beam(
        &mut self,
        old: (u32, u32),
        old_frame_lines: u32,
        new_frames: u32,
    ) {
        let Some(wave) = self.wave.as_deref() else {
            return;
        };
        if !wave.is_armed() {
            return;
        }
        let Trigger::Beam { vpos, hpos } = wave.trigger() else {
            return;
        };
        let target = (u32::from(vpos), u32::from(hpos.unwrap_or(0)));
        let cur = (self.agnus.vpos, self.agnus.hpos);
        let hit = match new_frames {
            0 => old < target && target <= cur,
            1 => (target > old && target.0 < old_frame_lines) || target <= cur,
            _ => target.0 < old_frame_lines || target <= cur,
        };
        if hit {
            self.wave_fire();
        }
    }

    /// Custom-register-write tap: `reg=` trigger plus the `regs` group
    /// sample. Offsets are recorded mirror-free ($000-$1FE).
    pub(super) fn wave_note_reg_write(&mut self, off: u16, value: u16, source: BeamWriteSource) {
        let off = off & 0x1FE;
        if let Some(wave) = self.wave.as_deref() {
            if wave.is_armed() && wave.trigger() == Trigger::RegWrite(off) {
                self.wave_fire();
            }
        }
        let cck = self.emulated_cck;
        let source = match source {
            BeamWriteSource::Copper => "copper",
            BeamWriteSource::Cpu | BeamWriteSource::CpuCopperIrq => "cpu",
        };
        if let Some(wave) = self.wave.as_deref_mut() {
            wave.sample_reg_write(cck, off, value, source);
        }
    }

    /// CPU chip-bus grant tap (`cpu` group): the granted slot's beam
    /// position has been reached but the quantum has not elapsed yet, so
    /// `emulated_cck` matches the quantum sampler's timestamp.
    pub(super) fn wave_note_cpu_access(
        &mut self,
        addr: Option<u32>,
        kind: CpuBusAccessKind,
        wait_cck: u32,
    ) {
        let cck = self.emulated_cck;
        let (kind_name, is_write) = match kind {
            CpuBusAccessKind::Fetch => ("fetch", false),
            CpuBusAccessKind::Read => ("read", false),
            CpuBusAccessKind::Write => ("write", true),
            CpuBusAccessKind::Custom => ("custom", false),
        };
        if let Some(wave) = self.wave.as_deref_mut() {
            wave.sample_cpu_access(cck, addr, kind_name, is_write, wait_cck);
        }
    }

    /// `pc=` trigger check, called per retired instruction while the
    /// cheap `wave_pc_trigger` gate is set.
    pub(crate) fn wave_note_pc(&mut self, pc: u32) {
        let Some(wave) = self.wave.as_deref() else {
            return;
        };
        if wave.is_armed() && wave.trigger() == Trigger::Pc(pc) {
            self.wave_fire();
        }
    }
}
