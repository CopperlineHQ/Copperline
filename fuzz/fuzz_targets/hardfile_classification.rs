#![no_main]

#[path = "../harnesses/hardfile_classification.rs"]
mod harness;

libfuzzer_sys::fuzz_target!(|data: &[u8]| harness::exercise(data));
