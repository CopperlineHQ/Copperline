// SPDX-License-Identifier: GPL-3.0-or-later

//! Cached environment-variable access.
//!
//! Every COPPERLINE_* knob is a start-up setting, but some are consulted from very
//! hot paths (per instruction, per color clock, per device tick). A live
//! `std::env::var*` call takes a process-wide lock and scans, so reading one
//! millions of times a second pins the host CPU and -- on macOS -- starves the
//! audio thread of that same lock (cpal underruns). To make that class of bug
//! impossible, the whole environment is snapshotted once on first access and
//! every lookup reads from the snapshot with no further OS calls or locks.
//!
//! Consequence: variables are read exactly once (at first access). They are
//! start-up knobs, so that is the intended behaviour. Code that needs a runtime
//! "do this once" toggle must use its own latch, not `remove_var`.

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use std::collections::HashMap;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use std::ffi::OsStr;
use std::ffi::OsString;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use std::sync::OnceLock;

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn snapshot() -> &'static HashMap<OsString, OsString> {
    static SNAPSHOT: OnceLock<HashMap<OsString, OsString>> = OnceLock::new();
    SNAPSHOT.get_or_init(|| std::env::vars_os().collect())
}

/// Whether any `COPPERLINE_*` variable is present at all. Every knob this
/// module exposes is named with that prefix, and the vast majority are
/// diagnostic switches consulted from very hot paths (per register write, per
/// memory access, per color clock). A normal run sets none of them, so the
/// snapshot lookup below would otherwise hash a ~25-character string with the
/// default SipHasher millions of times a second only to miss. Cache the
/// "are any of our knobs set?" answer once and short-circuit the common
/// no-knobs case to a single bool read.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn any_copperline_var() -> bool {
    static ANY: OnceLock<bool> = OnceLock::new();
    *ANY.get_or_init(|| {
        snapshot().keys().any(|key| {
            key.to_str()
                .is_some_and(|name| name.starts_with("COPPERLINE"))
        })
    })
}

/// True when `name` is one of our knobs and we already know none are set, so
/// the caller can skip the snapshot hash entirely.
#[inline]
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn definitely_unset(name: &str) -> bool {
    name.starts_with("COPPERLINE") && !any_copperline_var()
}

/// Whether the variable is set (presence check), like `var_os(..).is_some()`.
///
/// A browser WASM build has no process environment. Its implementation is a
/// compile-time `false`, rather than a lookup in an empty snapshot, so release
/// optimization can erase diagnostic branches from per-cycle hot paths.
#[inline(always)]
pub fn flag(name: &str) -> bool {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        let _ = name;
        false
    }
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        if definitely_unset(name) {
            return false;
        }
        snapshot().contains_key(OsStr::new(name))
    }
}

/// The variable's raw value, like `std::env::var_os`.
///
/// Browser WASM returns `None` at compile time for the same reason as
/// [`flag`].
#[inline(always)]
pub fn var_os(name: &str) -> Option<OsString> {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        let _ = name;
        None
    }
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        if definitely_unset(name) {
            return None;
        }
        snapshot().get(OsStr::new(name)).cloned()
    }
}

/// The variable's value as UTF-8, like `std::env::var(..).ok()`.
///
/// Browser WASM returns `None` at compile time for the same reason as
/// [`flag`].
#[inline(always)]
pub fn var(name: &str) -> Option<String> {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        let _ = name;
        None
    }
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        if definitely_unset(name) {
            return None;
        }
        snapshot()
            .get(OsStr::new(name))
            .and_then(|v| v.to_str().map(str::to_owned))
    }
}

/// Parse a diagnostic integer value, accepting decimal or `0x`-prefixed hex.
pub fn parse_u32(raw: &str) -> Option<u32> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        raw.parse::<u32>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::parse_u32;

    #[test]
    fn parse_u32_accepts_decimal_and_prefixed_hex() {
        assert_eq!(parse_u32("42"), Some(42));
        assert_eq!(parse_u32("0x2a"), Some(42));
        assert_eq!(parse_u32("0X2A"), Some(42));
        assert_eq!(parse_u32(" 0x2a "), Some(42));
    }

    #[test]
    fn parse_u32_rejects_empty_and_invalid_values() {
        assert_eq!(parse_u32(""), None);
        assert_eq!(parse_u32("   "), None);
        assert_eq!(parse_u32("2a"), None);
        assert_eq!(parse_u32("0x"), None);
    }
}
