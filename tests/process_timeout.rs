//! Exercise timeout cleanup and output capture with a portable child fixture.
mod common;

use std::io::Write;
use std::process::Command;
use std::time::{Duration, Instant};

fn fixture(mode: &str) -> Command {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "captures_output_larger_than_a_pipe_buffer",
        "--nocapture",
    ])
    .env("TEST_PROCESS_TIMEOUT_MODE", mode);
    cmd
}

#[test]
fn captures_output_larger_than_a_pipe_buffer() {
    if child_fixture() {
        return;
    }
    let output = common::output_with_timeout(&mut fixture("output"), Duration::from_secs(10))
        .expect("fixture finishes without blocking on its output");
    assert!(output.status.success());
    assert!(output
        .stdout
        .windows(131_072)
        .any(|s| s.iter().all(|&b| b == b'x')));
    assert!(output
        .stderr
        .windows(131_072)
        .any(|s| s.iter().all(|&b| b == b'y')));
}

#[test]
fn kills_a_stalled_child_and_reports_its_output() {
    let started = Instant::now();
    let error = common::output_with_timeout(&mut fixture("stall"), Duration::from_secs(1))
        .expect_err("stalled child must time out");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("fixture ready"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

fn child_fixture() -> bool {
    let Ok(mode) = std::env::var("TEST_PROCESS_TIMEOUT_MODE") else {
        return false;
    };
    match mode.as_str() {
        "output" => {
            std::io::stdout().write_all(&vec![b'x'; 131_072]).unwrap();
            std::io::stderr().write_all(&vec![b'y'; 131_072]).unwrap();
        }
        "stall" => {
            eprintln!("fixture ready");
            std::thread::sleep(Duration::from_secs(30));
        }
        mode => panic!("unknown fixture mode: {mode}"),
    }
    true
}
