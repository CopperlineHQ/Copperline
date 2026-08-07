// SPDX-License-Identifier: GPL-3.0-or-later

//! Plugin-board manifest data shared with builds that exclude the wasmtime
//! host. `WasmCaps`/`WasmManifest` are pure configuration: `config.rs` and
//! `zorro.rs` consume them even when the `wasm-boards` feature (and with it
//! `src/wasmboard.rs`, the runtime that executes plugins) is compiled out.

use crate::net::NetConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Capabilities a plugin declares in its manifest; ungranted host imports are
/// not linked, so a module that needs more than it declared fails to
/// instantiate (loudly) rather than silently misbehaving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WasmCaps {
    /// Bus-master DMA into Amiga memory (`dma_read`/`dma_write` imports).
    pub dma: bool,
    /// Asserts the INT2 (PORTS) line (advisory; the `int2` export is polled).
    pub int2: bool,
    /// Asserts the INT6 (EXTER) line (advisory; the `int6` export is polled).
    pub int6: bool,
    /// Host networking (`net_send`/`net_recv` imports). A net board is
    /// non-deterministic; see [`crate::net`].
    pub net: bool,
    /// Host-resolver DNS lookups (`resolve_start`/`resolve_poll` imports):
    /// asks Copperline's own process to resolve a hostname via the host
    /// OS resolver on a background thread, rather than the plugin having
    /// to speak DNS wire format itself over its own `net` traffic. Like
    /// `net`, using it makes a board non-deterministic.
    pub resolve: bool,
    /// Host-socket passthrough (`sock_*` imports): lets the plugin open,
    /// connect, and read/write real host OS sockets directly, instead of
    /// implementing TCP/IP itself over `net`. This is a materially bigger
    /// grant than `net` -- a plugin holding it can reach anything the host
    /// process can reach, on the host's own network identity. Granted
    /// unconditionally to the bundled HostSocket board regardless of its
    /// own transport (`net = "loopback"`/`"nat"`/`"bridge"`/`"host"` all
    /// get it, not only `"host"` mode): the plugin module always imports
    /// the `sock_*` functions, used or not, so an ungranted capability
    /// would fail wasmtime instantiation outright rather than just go
    /// unused (`crate::hostsocket::board_config`'s own comment on this) --
    /// same reasoning `dma`/`int2`/`resolve` above are unconditional for.
    /// Not exclusive to the bundled board either: a third-party `[[zorro]]
    /// type = "wasm"` manifest can opt in explicitly via its own
    /// `host_sockets = true` key (`RawBoardMeta::host_sockets`'s own doc
    /// comment in `src/zorro.rs`). This field is therefore not, on its
    /// own, evidence that host-socket *transport* (`net = "host"`) is
    /// active -- see `Board::host_backend` in the plugin crate for that
    /// distinction. Like `net` and `resolve`, using it makes a board
    /// non-deterministic.
    pub host_sockets: bool,
}

/// A plugin's non-autoconfig metadata: its display name, capabilities, and (for
/// a NIC board) which host network backend to bring up. The autoconfig identity
/// lives in the board's [`crate::zorro::BoardSpec`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WasmManifest {
    pub name: String,
    pub caps: WasmCaps,
    pub net: NetConfig,
    /// Effective plugin settings (manifest defaults merged with the user's
    /// per-board overrides), exposed to the module via the `config_get` import.
    pub config: BTreeMap<String, String>,
    /// Config keys whose values are host file paths; the host loads each file
    /// and exposes it to the module via `resource_read` under the same key.
    pub file_keys: Vec<String>,
}
