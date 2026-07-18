// SPDX-License-Identifier: GPL-3.0-or-later

//! Amiga Centronics parallel-port peripheral boundary.
//!
//! CIA-A port B (`$BFE101`) carries the eight data pins and CIA-A's `PC` output
//! is the active-low printer strobe. An output peripheral that accepts a
//! strobed byte returns `true`; the bus turns that response into the printer's
//! active-low `/ACK` edge on CIA-A `FLAG`. An input peripheral instead drives
//! the data pins itself (see [`ParallelPort::read_data`]), which is how an
//! audio sampler digitizes the port. The default null peripheral is an
//! unplugged cable: it neither acknowledges nor drives the pins.

use std::io::Write;

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

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct NullParallelPort;

impl ParallelPort for NullParallelPort {
    fn strobe(&mut self, _data: u8, _at_cck: u64) -> bool {
        false
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
