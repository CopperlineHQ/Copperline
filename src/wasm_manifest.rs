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
