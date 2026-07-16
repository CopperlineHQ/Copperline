// SPDX-License-Identifier: GPL-3.0-or-later

//! Amiga Centronics parallel-port peripheral boundary.
//!
//! CIA-B port B carries the eight data pins and its `PC` output is the
//! active-low printer strobe. A peripheral that accepts a strobed byte returns
//! `true`; the bus turns that response into the printer's active-low `/ACK`
//! edge on CIA-A `FLAG`. The default null peripheral is an unplugged cable and
//! never acknowledges.

use std::io::Write;

pub trait ParallelPort: Send {
    /// Observe one CIA-B `PC` strobe with the current physical port-B pin
    /// levels. `at_cck` is the deterministic power-on colour-clock timestamp.
    /// Return true to drive one `/ACK` falling edge back to CIA-A `FLAG`.
    fn strobe(&mut self, data: u8, at_cck: u64) -> bool;

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

/// Raw printer capture. Each accepted Centronics byte is appended to the host
/// file and acknowledged. This deliberately does not interpret printer escape
/// languages; it preserves the exact guest byte stream for a real spooler or
/// conversion tool.
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
    fn file_parallel_port_captures_and_acknowledges_raw_bytes() {
        let path = std::env::temp_dir().join(format!(
            "copperline-parallel-capture-{}.raw",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let mut port = FileParallelPort::create(&path).unwrap();
        assert!(port.strobe(0x1b, 10));
        assert!(port.strobe(b'@', 20));
        port.flush().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), [0x1b, b'@']);

        let _ = std::fs::remove_file(path);
    }
}
