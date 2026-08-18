// SPDX-License-Identifier: GPL-3.0-or-later

//! The bundled ZZ9000 SDK crypto board (`[zz9k]`): a register-compatible
//! subset of the MNT ZZ9000's "SDK v2" service platform (CORE + MEMORY +
//! CRYPTO services plus DIAG_READ) whose crypto runs host-side instead of
//! on the real card's ARM core. The unmodified SDK Amiga-side stack -- its
//! transport library, the zz9k-* tools, and the accelerated AmiSSL build --
//! detects and drives this board exactly as it does real hardware, giving
//! guest TLS-era crypto host-speed offload. The register/opcode contract is
//! docs/internals/zz9k.md.
//!
//! The board is an ordinary WASM plugin board (crates/zz9k-plugin, hosted
//! by `src/wasmboard.rs`) whose module is embedded in the binary -- the
//! same bundling as the HostSocket board ([`crate::hostsocket`]). It is
//! pure compute (no DMA, no network, no host sockets), so fitting it keeps
//! a machine fully deterministic and replay-safe.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::WasmBoardConfig;
use crate::net::NetConfig;
use crate::wasm_manifest::{WasmCaps, WasmManifest};
use crate::zorro::{BoardSpec, ZorroVersion};

/// Path sentinel standing in for the embedded plugin module (like
/// [`crate::hostsocket::BUNDLED_HOSTSOCKET_WASM`]).
pub const BUNDLED_ZZ9K_WASM: &str = "<bundled-zz9k>";

/// The compiled plugin module (crates/zz9k-plugin), rebuilt into assets/ by
/// `make` in that crate; a plain cargo build embeds the committed artifact.
#[cfg(feature = "wasm-boards")]
pub const ZZ9K_WASM: &[u8] = include_bytes!("../assets/zz9k/zz9k_plugin.wasm");

/// The only Zorro II window size the SDK transport accepts HOST_WINDOW
/// shared-buffer allocations for (its "historical fixed 4 MB" profile --
/// the board deliberately does not advertise the newer aperture-layout
/// negotiation). Also the Zorro III default, where any power of two works.
pub const Z2_BOARD_SIZE: usize = 0x0040_0000;

/// Build the resolved board entry `[zz9k]` expands to: the ZZ9000's own
/// autoconfig identity (that is the point -- the SDK finds the board by
/// manufacturer/product), the embedded-module path sentinel, and a manifest
/// identical in shape to what a `[[zorro]]` metadata file would have
/// produced. `int2` selects the completion-interrupt line the guest's
/// ZZ9000.CFG key query reports (false = INT6/EXTER, the hardware
/// default). `seed` is the reserved deterministic DRBG seed (hex); no
/// current operation draws from it.
pub fn board_config(
    version: ZorroVersion,
    size_bytes: usize,
    int2: bool,
    seed: Option<&str>,
) -> WasmBoardConfig {
    let mut config = BTreeMap::new();
    config.insert("size".to_string(), size_bytes.to_string());
    config.insert("int2".to_string(), if int2 { "1" } else { "0" }.to_string());
    if let Some(seed) = seed {
        config.insert("seed".to_string(), seed.to_string());
    }
    let spec = BoardSpec::zz9k(version, size_bytes);
    WasmBoardConfig {
        manifest: WasmManifest {
            name: spec.name.clone(),
            caps: WasmCaps {
                // Pure compute: payloads travel through the board window
                // (guest-side copies, as on the real card), so no DMA, and
                // no network of any kind. The interrupt declarations are
                // advisory, but the board genuinely drives both lines (the
                // guest picks one via the ZZ9000.CFG key).
                dma: false,
                int2: true,
                int6: true,
                net: false,
                resolve: false,
                host_sockets: false,
            },
            net: NetConfig::None,
            config,
            file_keys: Vec::new(),
        },
        spec,
        wasm_path: PathBuf::from(BUNDLED_ZZ9K_WASM),
    }
}
