// SPDX-License-Identifier: GPL-3.0-or-later

//! The bundled HostSocket board (`[hostsocket]`): `bsdsocket.library` for
//! the guest, backed by an embedded smoltcp TCP/IP stack running on the
//! host instead of a real Amiga TCP/IP stack running inside the emulated
//! CPU. A guest application opens `bsdsocket.library` and calls
//! `socket()`/`connect()`/`send()`/`recv()` exactly as it would against
//! AmiTCP or Roadshow, with no guest-side stack to boot at all.
//!
//! The board is an ordinary WASM plugin board (crates/hostsocket-plugin,
//! hosted by `src/wasmboard.rs`) whose module and guest autoboot ROM are
//! embedded in the binary rather than loaded from user-supplied files --
//! the same bundling trick as the AROS boot ROM ([`BUNDLED_AROS_ROM`]) and
//! the services handler ([`crate::filesys::FILESYS_HANDLER`]). Config
//! resolution turns `[hostsocket]` into a [`WasmBoardConfig`] carrying the
//! [`BUNDLED_HOSTSOCKET_WASM`] path sentinel, so the plugin host (and a
//! save-state reload, which reopens modules by path) resolves the embedded
//! bytes instead of touching the filesystem.
//!
//! [`BUNDLED_AROS_ROM`]: crate::config::BUNDLED_AROS_ROM

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::WasmBoardConfig;
use crate::net::NetConfig;
use crate::wasm_manifest::{WasmCaps, WasmManifest};
use crate::zorro::BoardSpec;

/// Path sentinel standing in for the embedded plugin module. Chosen to be
/// impossible as a real relative path (like [`crate::config::BUNDLED_AROS_ROM`]).
pub const BUNDLED_HOSTSOCKET_WASM: &str = "<bundled-hostsocket>";

/// Resource-path sentinel standing in for the embedded guest stub ROM,
/// carried by the manifest's `rom` file-typed config value.
pub const BUNDLED_HOSTSOCKET_ROM: &str = "<bundled-hostsocket-rom>";

/// The compiled plugin module (crates/hostsocket-plugin), rebuilt into
/// assets/ by `make` in that crate (which builds the wasm32 target and
/// installs the artifact); a plain cargo build embeds the committed
/// artifact.
#[cfg(feature = "wasm-boards")]
pub const HOSTSOCKET_WASM: &[u8] = include_bytes!("../assets/hostsocket/hostsocket_plugin.wasm");

/// The guest-side bsdsocket.library stub ROM (guest/hostsocket), served to
/// the plugin as its `rom` resource and by the plugin to the guest as its
/// `diag_vec` autoboot ROM.
#[cfg(feature = "wasm-boards")]
pub const HOSTSOCKET_ROM: &[u8] = include_bytes!("../assets/hostsocket/hostsocket_rom.bin");

/// DiagArea offset within the board window: the guest ROM is served at
/// window offset 0x08 and places its DiagArea 0x40 in (see
/// guest/hostsocket/hostsocket_board.h's ROM_OFFSET/DIAG_OFFSET, kept in
/// sync with this by hand, same as every other offset the guest ROM and
/// this board share).
pub const DIAG_OFFSET: u16 = 0x48;

/// `gethostbyname()`'s default DNS resolver: Copperline NAT's own DNS
/// forwarder address (see `crate::net::nat`), which answers via the host's
/// resolver. Only reachable under `net = "nat"`; override it only for
/// `net = "bridge"` (direct LAN access, a real resolver IP makes sense
/// there).
pub const DEFAULT_DNS_SERVER: &str = "10.0.2.3";

/// `gethostname()`'s default return value. Purely cosmetic.
pub const DEFAULT_HOSTNAME: &str = "amiga";

/// Build the resolved board entry `[hostsocket]` expands to: the autoconfig
/// identity, the embedded-module path sentinel, and a manifest identical in
/// shape to what a `[[zorro]]` metadata file would have produced.
///
/// `address`/`gateway` matter only under `net = "bridge"` (see the
/// configuration guide's `[hostsocket]` section): unlike `dns_server`/
/// `hostname`, which have a meaningful cross-backend default, the right
/// interface address/gateway is entirely backend- and LAN-specific, so
/// these are left out of the manifest's `[config]` entirely when unset --
/// the plugin's own `net = "nat"`/`"loopback"`-shaped defaults
/// (`INTERFACE_ADDR`/`NAT_GATEWAY_ADDR`) apply exactly when nothing here
/// overrides them.
pub fn board_config(
    net: NetConfig,
    dns_server: Option<&str>,
    hostname: Option<&str>,
    address: Option<&str>,
    gateway: Option<&str>,
    resolver: Option<&str>,
) -> WasmBoardConfig {
    let mut config = BTreeMap::new();
    config.insert("rom".to_string(), BUNDLED_HOSTSOCKET_ROM.to_string());
    config.insert(
        "dns_server".to_string(),
        dns_server.unwrap_or(DEFAULT_DNS_SERVER).to_string(),
    );
    config.insert(
        "hostname".to_string(),
        hostname.unwrap_or(DEFAULT_HOSTNAME).to_string(),
    );
    if let Some(address) = address {
        config.insert("address".to_string(), address.to_string());
    }
    if let Some(gateway) = gateway {
        config.insert("gateway".to_string(), gateway.to_string());
    }
    // Absent means "dns" (the plugin's own default when config_get_string
    // returns None) -- src/config.rs already validated "host" against the
    // resolved net backend before this is ever called.
    if let Some(resolver) = resolver {
        config.insert("resolver".to_string(), resolver.to_string());
    }
    let spec = BoardSpec::hostsocket();
    WasmBoardConfig {
        manifest: WasmManifest {
            name: spec.name.clone(),
            caps: WasmCaps {
                dma: true,
                // The blocking-call wake path (the guest's INTB_PORTS
                // interrupt server draining the plugin's wake queue) rides
                // the polled `int2` export, so the board genuinely asserts
                // INT2. The original out-of-tree manifest omitted the
                // declaration and worked only because the cap is advisory.
                int2: true,
                int6: false,
                net: true,
                // The plugin module always imports resolve_start/
                // resolve_poll (used only when `[hostsocket] resolver =
                // "host"` -- see board_config's own doc comment), so this
                // is granted unconditionally, the same as dma/int2 above:
                // an ungranted-but-imported host function would fail
                // instantiation outright, not just go unused.
                resolve: true,
            },
            net,
            config,
            file_keys: vec!["rom".to_string()],
        },
        spec,
        wasm_path: PathBuf::from(BUNDLED_HOSTSOCKET_WASM),
    }
}
