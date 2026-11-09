//! Shared integration-test process checks.
#![allow(dead_code)]

use std::io::{self, Read, Seek, SeekFrom};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub fn require_optimized() {
    if cfg!(debug_assertions) {
        panic!("this test needs an optimized emulator; run cargo test --profile ci (or --release)");
    }
}

struct ReapOnDrop(Child);

impl Drop for ReapOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Capture to files so a full stdout/stderr pipe cannot deadlock the child.
/// A timeout kills and reaps the child and retains its diagnostic output.
pub fn output_with_timeout(cmd: &mut Command, timeout: Duration) -> io::Result<Output> {
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    cmd.stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    let mut child = ReapOnDrop(cmd.spawn()?);
    let started = Instant::now();
    let (status, timed_out) = loop {
        if let Some(status) = child.0.try_wait()? {
            break (status, false);
        }
        if started.elapsed() >= timeout {
            child.0.kill()?;
            break (child.0.wait()?, true);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    stdout.seek(SeekFrom::Start(0))?;
    stderr.seek(SeekFrom::Start(0))?;
    let mut output = Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    stdout.read_to_end(&mut output.stdout)?;
    stderr.read_to_end(&mut output.stderr)?;
    if timed_out {
        let tail = |bytes: &[u8]| {
            String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(4096)..]).into_owned()
        };
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "{cmd:?} exceeded {timeout:?}\nstdout tail:\n{}\nstderr tail:\n{}",
                tail(&output.stdout),
                tail(&output.stderr)
            ),
        ));
    }
    Ok(output)
}
