//! Fuzz the save-state loader. `.clstate` files are untrusted input (a
//! downloaded state, a shared speedrun snapshot), and the loader parses a
//! full bincode machine image before anything else validates it.

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::cell::RefCell;

thread_local! {
    // One deterministic AROS-bootable machine per fuzzing thread; every
    // iteration restores into it from scratch. The emulator is !Send (the
    // audio sink holds host handles), so it lives in thread-local storage.
    static EMULATOR: RefCell<Option<copperline::emulator::Emulator>> =
        const { RefCell::new(None) };
}

fuzz_target!(|data: &[u8]| {
    EMULATOR.with(|cell| {
        let mut slot = cell.borrow_mut();
        let emulator = slot.get_or_insert_with(|| {
            let cfg = copperline::config::Config::default();
            copperline::emulator::build_machine(
                &cfg,
                Box::new(copperline::audio::NullSink),
                false,
                true,
            )
            .expect("the factory configuration boots without external assets")
        });
        // Errors are fine; panics, hangs, and over-allocation are not.
        let _ = emulator.load_state_bytes(data);
    });
});
