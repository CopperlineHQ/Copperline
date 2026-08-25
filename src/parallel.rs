// SPDX-License-Identifier: GPL-3.0-or-later

//! Amiga Centronics parallel-port peripheral boundary.
//!
//! CIA-A port B (`$BFE101`) carries the eight data pins and CIA-A's `PC` output
//! is the active-low printer strobe. An output peripheral that accepts a
//! strobed byte returns `true`; the bus turns that response into the printer's
//! active-low `/ACK` edge on CIA-A `FLAG`. An input peripheral instead drives
//! the data pins itself (see [`ParallelPort::read_data`]), which is how an
//! audio sampler digitizes the port. The Centronics status lines BUSY, POUT,
//! and SEL are CIA-B port A pins 0-2 (see [`ParallelPort::control_lines`]);
//! a printer must hold them at ready-online levels or the guest's
//! parallel.device never sends a byte. The default null peripheral is an
//! unplugged cable: it neither acknowledges nor drives any pin.

use std::io::Write;

/// Centronics status-line bits, as wired to CIA-B port A pins 0-2. All three
/// are peripheral-driven inputs with motherboard pull-ups, so an unplugged
/// connector reads them all high. The levels are the physical pin states:
/// BUSY high = not ready, POUT high = out of paper, SEL high = online.
pub const CTL_BUSY: u8 = 0x01;
pub const CTL_POUT: u8 = 0x02;
pub const CTL_SEL: u8 = 0x04;

pub trait ParallelPort: Send {
    /// Observe one CIA-A `PC` strobe with the current physical port-B pin
    /// levels. `at_cck` is the deterministic power-on colour-clock timestamp.
    /// Return true to drive one `/ACK` falling edge back to CIA-A `FLAG`.
    fn strobe(&mut self, data: u8, at_cck: u64) -> bool;

    /// Drive the eight parallel data pins as an input peripheral. Called on
    /// every CIA-A port-B read; returning `Some(byte)` replaces the value the
    /// guest reads from `$BFE101`, `None` leaves the CIA's own pin state. An
    /// output-only peripheral (printer) keeps the default and never drives the
    /// pins. `at_cck` is the deterministic power-on colour-clock timestamp,
    /// which a free-running capture device uses to advance in emulated time.
    fn read_data(&mut self, _at_cck: u64) -> Option<u8> {
        None
    }

    /// Drive the Centronics status lines BUSY, POUT, and SEL (the `CTL_*`
    /// bits, CIA-B port A pins 0-2). `Some(bits)` holds the pins at those
    /// levels; `None` leaves them undriven, so the pull-ups read them all
    /// high like an unplugged cable -- which is why the guest's
    /// parallel.device waits forever on an empty port, exactly as on a real
    /// machine. Pins the guest has switched to outputs stay CIA-driven
    /// regardless.
    fn control_lines(&mut self) -> Option<u8> {
        None
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    /// Whether speculative run-ahead frames may touch this peripheral.
    /// A printer writes an irreversible host stream and a sampler consumes
    /// live input, so only the unplugged port opts in.
    fn runahead_safe(&self) -> bool {
        false
    }
}

pub struct NullParallelPort;

impl ParallelPort for NullParallelPort {
    fn strobe(&mut self, _data: u8, _at_cck: u64) -> bool {
        false
    }

    fn runahead_safe(&self) -> bool {
        true
    }
}

pub fn null_parallel_port() -> Box<dyn ParallelPort> {
    Box::new(NullParallelPort)
}

/// Raw printer capture. Opening a capture replaces any existing host file;
/// each accepted Centronics byte is then written in order and acknowledged.
/// This deliberately does not interpret printer escape languages, preserving
/// the exact guest byte stream for a real spooler or conversion tool.
pub struct FileParallelPort {
    writer: std::io::BufWriter<std::fs::File>,
    failed: bool,
}

impl FileParallelPort {
    pub fn create(path: &std::path::Path) -> anyhow::Result<Self> {
        let file = std::fs::File::create(path)
            .map_err(|e| anyhow::anyhow!("[parallel] creating output {}: {e}", path.display()))?;
        Ok(Self {
            writer: std::io::BufWriter::new(file),
            failed: false,
        })
    }
}

impl ParallelPort for FileParallelPort {
    fn strobe(&mut self, data: u8, _at_cck: u64) -> bool {
        if self.failed {
            return false;
        }
        if let Err(err) = self.writer.write_all(&[data]) {
            log::warn!("parallel output failed: {err}");
            self.failed = true;
            return false;
        }
        true
    }

    /// A ready online printer: SEL high, BUSY and POUT low. Without these
    /// levels parallel.device never sends a byte, since its default write
    /// path polls BUSY before each transfer.
    fn control_lines(&mut self) -> Option<u8> {
        Some(CTL_SEL)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_parallel_port_models_an_unplugged_peripheral() {
        assert!(!NullParallelPort.strobe(0x55, 123));
        assert_eq!(NullParallelPort.control_lines(), None);
    }

    #[test]
    fn printer_drives_ready_online_status_lines() {
        let path = std::env::temp_dir().join(format!(
            "copperline-parallel-status-{}.raw",
            std::process::id()
        ));
        let mut port = FileParallelPort::create(&path).unwrap();
        // SEL high (online), BUSY and POUT low (ready, paper loaded).
        assert_eq!(port.control_lines(), Some(CTL_SEL));

        drop(port);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn file_parallel_port_replaces_old_capture_and_writes_raw_bytes() {
        let path = std::env::temp_dir().join(format!(
            "copperline-parallel-capture-{}.raw",
            std::process::id()
        ));
        std::fs::write(&path, b"stale capture").unwrap();

        let mut port = FileParallelPort::create(&path).unwrap();
        assert!(port.strobe(0x1b, 10));
        assert!(port.strobe(b'@', 20));
        port.flush().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), [0x1b, b'@']);

        drop(port);
        let _ = std::fs::remove_file(path);
    }
}
