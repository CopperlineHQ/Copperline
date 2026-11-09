#![no_main]

#[path = "../harnesses/debug_info.rs"]
mod harness;

libfuzzer_sys::fuzz_target!(|data: &[u8]| harness::exercise(data));
