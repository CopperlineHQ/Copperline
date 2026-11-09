#![no_main]

#[path = "../harnesses/dms.rs"]
mod harness;

libfuzzer_sys::fuzz_target!(|data: &[u8]| harness::exercise(data));
