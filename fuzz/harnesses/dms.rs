//! Fuzz the DMS (DiskMasher) archive decoder. `decode_dms_adf` runs five
//! compression modes over attacker-supplied bytes before anything else in a
//! session touches them.

pub fn exercise(data: &[u8]) {
    // Errors are fine; panics, hangs, and over-allocation are not.
    let _ = copperline::dms::decode_dms_adf(data);
}
