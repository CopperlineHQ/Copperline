// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared HTTP construction: user agent, transfer timeout and the TLS provider
//! available in this platform's build. All game-library callers use `agent`.

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
    use super::*;
    use ureq::tls::TlsProvider;

    #[test]
    fn agent_selects_the_platforms_compiled_tls_provider() {
        let client = agent(Duration::from_secs(5));
        let expected = if cfg!(windows) {
            TlsProvider::NativeTls
        } else {
            TlsProvider::Rustls
        };
        assert_eq!(client.config().tls_config().provider(), expected);
    }
}
