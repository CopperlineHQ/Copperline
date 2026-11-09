#![no_main]

#[path = "../harnesses/control_protocol.rs"]
mod harness;

libfuzzer_sys::fuzz_target!(|data: &[u8]| harness::exercise(data));
