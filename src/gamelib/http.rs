// SPDX-License-Identifier: GPL-3.0-or-later

//! The one place an HTTP client is built.
//!
//! Not a wrapper for its own sake: the TLS backend differs by platform, and
//! ureq will not choose it. `TlsProvider` defaults to Rustls and its own
//! documentation says of the alternative that "the setting is never picked
//! up automatically", so a build carrying only native-tls still asks for
//! Rustls and panics mid-request:
//!
//! ```text
//! uri scheme is https, provider is Rustls but feature is not enabled: rustls
//! ```
//!
//! That is a runtime failure on one platform, in whichever request happens
//! to run first, and nothing about writing `Agent::config_builder()` warns
//! you. So there is one constructor, every caller goes through it, and a
//! test below fails if a second one appears.

use std::time::Duration;

/// An agent for `timeout`, with Copperline's user agent and the platform's
/// TLS.
///
/// The timeout is global rather than per-connect: a transfer that stalls
/// half way through has to end too, or it holds a worker for ever.
pub fn agent(timeout: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(concat!("Copperline/", env!("CARGO_PKG_VERSION")));
    // Windows builds link schannel instead of rustls, because rustls's
    // crypto provider needs a C toolchain that an ARM64 Windows machine
    // does not have by default (see the two ureq entries in Cargo.toml).
    // Naming it is not optional -- ureq defaults to Rustls whatever is
    // compiled in.
    #[cfg(windows)]
    let config = config.tls_config(
        ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .build(),
    );
    config.build().into()
}

#[cfg(test)]
mod tests {
    /// Every HTTP client in the game library comes from [`super::agent`].
    ///
    /// Reading the sources rather than the behaviour, because the behaviour
    /// this protects only misbehaves on Windows: a second agent built by
    /// hand works perfectly on the machine of whoever writes it and panics
    /// on somebody else's. That is exactly how the first one got in.
    #[test]
    fn nothing_else_builds_an_agent() {
        for (name, source) in [
            ("openretro.rs", include_str!("openretro.rs")),
            ("support.rs", include_str!("support.rs")),
            ("cover.rs", include_str!("cover.rs")),
            ("scan.rs", include_str!("scan.rs")),
        ] {
            assert!(
                !source.contains("config_builder"),
                "{name} builds its own ureq agent; call gamelib::http::agent \
                 instead, or Windows will panic on the first request"
            );
        }
    }
}
