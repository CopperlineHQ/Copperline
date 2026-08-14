// SPDX-License-Identifier: GPL-3.0-or-later

//! WASM-hosted functional Zorro board: an external plugin compiled to
//! `wasm32` that implements a board's behaviour, run under wasmtime.
//!
//! A plugin module exports its behaviour and imports a small set of host
//! services (see the ABI below); its entire mutable state lives in WebAssembly
//! linear memory, which is a flat byte array that snapshots and restores
//! exactly like the Amiga RAM the emulator already serializes. That is why WASM
//! fits Copperline's determinism / save-state contract.
//!
//! ## Module ABI
//!
//! Exports the plugin provides (all optional except `memory`):
//! - `memory`                       the linear memory (required)
//! - `init()`                       called once after instantiation
//! - `read(off: i32, size: i32) -> i32`   register read in the board window
//! - `write(off: i32, size: i32, value: i32)`  register write
//! - `tick(cck: i32)`               advance by `cck` colour clocks
//! - `int2() -> i32`                INT2 (PORTS) line state, 0/1
//! - `int6() -> i32`                INT6 (EXTER) line state, 0/1
//!
//! Imports the host provides in module `env` (capability-gated):
//! - `log(ptr: i32, len: i32)`                      always available
//! - `config_get`/`resource_len`/`resource_read`    always available
//! - `dma_read(addr: i32, ptr: i32, len: i32)`      requires the `dma` capability
//! - `dma_write(addr: i32, ptr: i32, len: i32)`     requires the `dma` capability
//! - `net_send(ptr: i32, len: i32)`                 requires the `net` capability
//! - `net_recv(ptr: i32, cap: i32) -> i32`          requires the `net` capability
//! - `resolve_start(name_ptr: i32, name_len: i32) -> i32`        requires `resolve`
//! - `resolve_poll(id: i32, out_ptr: i32) -> i32`                requires `resolve`
//! - `sock_open(domain: i32, type_: i32) -> i32`                 requires `host_sockets`
//! - `sock_connect(h: i32, ip: i32, port: i32) -> i32`           requires `host_sockets`
//! - `sock_send(h: i32, ptr: i32, len: i32) -> i32`              requires `host_sockets`
//! - `sock_recv(h: i32, ptr: i32, cap: i32) -> i32`              requires `host_sockets`
//! - `sock_poll(h: i32) -> i32`                                  requires `host_sockets`
//! - `sock_close(h: i32)`                                        requires `host_sockets`
//! - `sock_bind(h: i32, ip: i32, port: i32) -> i32`               requires `host_sockets`
//! - `sock_listen(h: i32, backlog: i32) -> i32`                   requires `host_sockets`
//! - `sock_accept(h: i32) -> i32`                                 requires `host_sockets`
//! - `sock_local_addr(h: i32, out_ptr: i32) -> i32`               requires `host_sockets`
//! - `sock_peer_addr(h: i32, out_ptr: i32) -> i32`                requires `host_sockets`
//! - `sock_sendto(h: i32, ptr: i32, len: i32, ip: i32, port: i32) -> i32`  requires `host_sockets`
//! - `sock_recvfrom(h: i32, ptr: i32, cap: i32, out_addr_ptr: i32) -> i32` requires `host_sockets`
//! - `sock_setopt(h: i32, level: i32, optname: i32, value: i32) -> i32`   requires `host_sockets`
//! - `sock_getopt(h: i32, level: i32, optname: i32, out_ptr: i32) -> i32` requires `host_sockets`
//! - `sock_dup(h: i32) -> i32`                                    requires `host_sockets`
//! - `sock_shutdown(h: i32, how: i32) -> i32`                     requires `host_sockets`
//! - `sock_peek(h: i32, ptr: i32, cap: i32) -> i32`                requires `host_sockets`
//! - `sock_nread(h: i32) -> i32`                                   requires `host_sockets`
//!
//! `dma_read` copies `len` bytes from Amiga address `addr` into the plugin's
//! linear memory at `ptr`; `dma_write` copies the other way. Both use the
//! shared 24-bit chip/slow/Zorro decode in [`crate::zorro_device`].
//! `net_send`/`net_recv` move whole Ethernet frames between the plugin's
//! linear memory and the manifest's configured [`NetConfig`] backend.
//! `resolve_start`/`resolve_poll` ask the host to resolve a hostname via
//! its own OS resolver on a background thread (`getaddrinfo` blocks, and
//! this store runs synchronously on the main emulation thread) --
//! `resolve_start` returns a request id, `resolve_poll` is a non-blocking
//! poll of it (-2 pending, -1 failed, or 0 with the resolved IPv4 address
//! written into the plugin's own linear memory at `out_ptr`). Like `net`,
//! using either makes a board non-deterministic.
//!
//! `sock_*` gives the plugin direct, non-blocking passthrough to a real
//! host OS socket -- the Amiberry-style alternative to implementing TCP/IP
//! over `net` (see `HOSTSOCKET-HOST-BACKEND-PLAN.md`). `sock_open` takes
//! BSD `domain`/`type` values (`AF_INET` = 2; `SOCK_STREAM` = 1 or
//! `SOCK_DGRAM` = 2, nothing else) and returns a plugin-scoped handle, or a
//! negative BSD-style errno. `sock_connect`'s `ip` is a big-endian IPv4 address (the
//! same byte order every other address in this ABI uses); like a real
//! non-blocking BSD `connect()`, it returns `0` on an immediate connect (rare
//! for TCP) or `-EINPROGRESS`, and the plugin polls completion with
//! `sock_poll`. `sock_send`/`sock_recv` return a byte count, `0` for
//! `sock_recv` at EOF, or a negative errno (`-EAGAIN` when the call would
//! block -- there is no separate blocking mode; the guest-side blocking
//! doorbell loop is what turns this into a blocking call, same as it
//! already does for the smoltcp backend). `sock_poll` returns a bitmask:
//! bit 0 readable, bit 1 writable, bit 2 error-pending (check with
//! `sock_getopt(..., SO_ERROR, ...)`, which reads and clears the host socket's
//! pending error). All host errno values are normalized to the guest's
//! BSD-style numbering at this boundary (matching
//! `crates/hostsocket-plugin`'s own `EAGAIN = 35` etc. -- the two are
//! hand-kept in sync, same as the guest-ROM/plugin window offsets already
//! are), so the plugin never sees a platform-specific errno. `sock_bind`/
//! `sock_listen` are plain non-blocking BSD `bind()`/`listen()`.
//! `sock_accept` is a non-blocking `accept()`: a fresh, already-nonblocking
//! handle for the new connection, or `-EAGAIN` when nothing is waiting.
//! `sock_local_addr`/`sock_peer_addr` write a socket's bound local (resp.
//! remote) address (4-byte IPv4 + 2-byte port, both big-endian) into the
//! plugin's own linear memory -- the former mainly so a `bind()` to port
//! `0` can learn the real ephemeral port the OS assigned, the latter so
//! `accept()`'s own address out-param can report a real value instead of
//! a placeholder. `sock_sendto`/`sock_recvfrom` are UDP's real
//! `sendto()`/`recvfrom()` -- an explicit per-call destination/sender
//! address instead of `sock_connect`'s single recorded peer, using the
//! same `sock_local_addr`/`sock_peer_addr` 6-byte address layout for the
//! sender `sock_recvfrom` reports. `sock_setopt`/`sock_getopt` apply a
//! small, real subset of setsockopt()/getsockopt() directly to the host
//! socket (SO_REUSEADDR/SO_KEEPALIVE/SO_RCVBUF/SO_SNDBUF/TCP_NODELAY, plus
//! a real SO_ERROR "get pending error and clear" on the getopt side) --
//! see their own doc comments below for exactly which options and why the
//! rest stay plugin-side roundtrip storage. `sock_dup` is a real `dup(2)`
//! of the host socket -- a fresh, independently closeable handle sharing
//! the same underlying connection, backing Dup2Socket/ReleaseCopyOfSocket
//! on a host-backed fd with no manual refcounting on either side of this
//! ABI. Using this capability makes a board non-deterministic, and
//! grants it far more than `net` does: a plugin holding it can reach
//! anything the host process can reach, on the host's own network
//! identity.
//!
//! ## Determinism
//!
//! The wasmtime engine is configured for determinism (NaN canonicalization, no
//! SIMD/threads). Persistent mutable state must live in linear memory: snapshots
//! capture linear memory and its page count, not WebAssembly globals (the
//! shadow-stack pointer is unwound to a constant between calls, so it needs no
//! capture). Save-state replay of a plugin is only guaranteed within one
//! wasmtime build; the version is pinned in `Cargo.toml`.

use crate::memory::Memory;
use crate::net::{make_backend, NetBackend, NetConfig};
use crate::zorro_device::{DeviceHost, ZorroDevice};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use socket2::{Domain, SockAddr, Socket, Type};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::mem::MaybeUninit;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use wasmtime::{
    Caller, Config, Engine, Extern, Linker, Memory as WasmMemory, Module, Store, TypedFunc,
};

// The manifest structs live in `crate::wasm_manifest` so config/zorro can
// consume them without the wasmtime host; re-exported here so existing
// `wasmboard::WasmManifest` paths keep working.
pub use crate::wasm_manifest::{WasmCaps, WasmManifest};

/// Store data for host imports. The Amiga-memory pointer is stored as a
/// `usize` (0 = none) so the store stays `Send`; it is set to the live
/// `&mut Memory` only for the duration of a plugin call (see [`WasmRuntime::enter`]).
struct HostCtx {
    /// Address of the live `Memory`, valid only during a plugin call.
    mem: usize,
    name: String,
    /// Host network backend for a NIC plugin (the `net` capability). A host
    /// resource, not serialized: brought up fresh from the manifest's
    /// [`NetConfig`] on instantiation and reset.
    net: Option<Box<dyn NetBackend>>,
    /// Effective plugin settings, read by the `config_get` import.
    config: BTreeMap<String, String>,
    /// Loaded file resources, read by the `resource_*` imports.
    resources: HashMap<String, Vec<u8>>,
    /// In-flight host-resolver DNS lookups (the `resolve` capability),
    /// keyed by the id `resolve_start` handed back to the plugin. Each
    /// lookup runs on its own short-lived background thread (`getaddrinfo`
    /// blocks; this store's main emulation thread cannot) and reports back
    /// over this channel -- `resolve_poll` is a plain non-blocking
    /// `try_recv()`. A host resource, not serialized: a lookup in flight
    /// when a save state is taken does not survive the restore (the
    /// module's own `host_resolve_jobs` record of it does, in its linear
    /// memory, but polling a stale id here just finds nothing -- see
    /// `resolve_poll`'s own comment).
    resolve_jobs: HashMap<i32, mpsc::Receiver<Option<Ipv4Addr>>>,
    /// The next id `resolve_start` will hand out.
    next_resolve_id: i32,
    /// Open host sockets (the `host_sockets` capability), keyed by the
    /// handle `sock_open` handed back to the plugin. A host resource, not
    /// serialized (like `net`/`resolve_jobs` above): a save-state restore
    /// starts with none open, so every handle the plugin still remembers in
    /// its own linear memory is stale after a restore and every
    /// `sock_*` call against it fails clean (`-EBADF`) rather than
    /// resurrecting a connection.
    sockets: HashMap<i32, Socket>,
    /// The next handle `sock_open` will hand out.
    next_socket_id: i32,
    pending_dma: Vec<(u32, Vec<u8>)>,
    pending_dma_bytes: usize,
}

impl HostCtx {
    fn cleanup_resources(&mut self) {
        self.net = None;
        self.sockets.clear();
        self.resolve_jobs.clear();
        self.clear_pending_dma();
    }

    fn clear_pending_dma(&mut self) {
        self.pending_dma.clear();
        self.pending_dma_bytes = 0;
    }
}

/// The typed entry points a plugin may export.
struct Exports {
    read: Option<TypedFunc<(i32, i32), i32>>,
    write: Option<TypedFunc<(i32, i32, i32), ()>>,
    tick: Option<TypedFunc<i32, ()>>,
    int2: Option<TypedFunc<(), i32>>,
    int6: Option<TypedFunc<(), i32>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InstantiationMode {
    Active,
    FaultedRestore,
}

/// The live wasmtime state for one plugin board. Holds the engine and compiled
/// module so a power-on reset can re-instantiate a fresh module (clearing the
/// plugin's RAM) without recompiling.
struct WasmRuntime {
    engine: Engine,
    module: Module,
    manifest: WasmManifest,
    /// File resources loaded once from the manifest's file-typed config values.
    resources: HashMap<String, Vec<u8>>,
    store: Store<HostCtx>,
    memory: WasmMemory,
    exports: Exports,
    faulted: bool,
}

impl WasmRuntime {
    fn new(
        engine: Engine,
        module: Module,
        manifest: WasmManifest,
        mode: InstantiationMode,
    ) -> Result<Self> {
        let resources = load_resources(&manifest)?;
        let (store, memory, exports) =
            Self::instantiate(&engine, &module, &manifest, &resources, mode)?;
        Ok(Self {
            engine,
            module,
            manifest,
            resources,
            store,
            memory,
            exports,
            faulted: mode == InstantiationMode::FaultedRestore,
        })
    }

    /// Build a fresh store/instance from an engine + compiled module.
    fn instantiate(
        engine: &Engine,
        module: &Module,
        manifest: &WasmManifest,
        resources: &HashMap<String, Vec<u8>>,
        mode: InstantiationMode,
    ) -> Result<(Store<HostCtx>, WasmMemory, Exports)> {
        let net = match mode {
            InstantiationMode::Active => make_backend(&manifest.net, None)
                .with_context(|| format!("opening network backend for {}", manifest.name))?,
            InstantiationMode::FaultedRestore => None,
        };
        let mut store = Store::new(
            engine,
            HostCtx {
                mem: 0,
                name: manifest.name.clone(),
                net,
                config: manifest.config.clone(),
                resources: resources.clone(),
                resolve_jobs: HashMap::new(),
                next_resolve_id: 0,
                sockets: HashMap::new(),
                next_socket_id: 0,
                pending_dma: Vec::new(),
                pending_dma_bytes: 0,
            },
        );
        let mut linker = Linker::new(engine);
        register_host_fns(&mut linker, manifest.caps)?;
        let instance = linker
            .instantiate(&mut store, module)
            .context("instantiating WASM plugin")?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| anyhow!("WASM plugin exports no `memory`"))?;
        let exports = Exports {
            read: instance.get_typed_func(&mut store, "read").ok(),
            write: instance.get_typed_func(&mut store, "write").ok(),
            tick: instance.get_typed_func(&mut store, "tick").ok(),
            int2: instance.get_typed_func(&mut store, "int2").ok(),
            int6: instance.get_typed_func(&mut store, "int6").ok(),
        };
        if mode == InstantiationMode::Active {
            if let Ok(init) = instance.get_typed_func::<(), ()>(&mut store, "init") {
                refuel(&mut store);
                let init_res = init.call(&mut store, ());
                store.data_mut().clear_pending_dma();
                init_res.context("WASM plugin init() trapped")?;
            }
        }
        Ok((store, memory, exports))
    }

    /// Re-instantiate from the kept engine + module (cold reset: clears RAM).
    fn reset(&mut self) -> Result<()> {
        let (store, memory, exports) = Self::instantiate(
            &self.engine,
            &self.module,
            &self.manifest,
            &self.resources,
            InstantiationMode::Active,
        )?;
        self.store = store;
        self.memory = memory;
        self.exports = exports;
        self.faulted = false;
        Ok(())
    }

    fn trigger_fault(&mut self, reason: &str) {
        if self.faulted {
            return;
        }
        log::warn!(
            "wasm[{}]: board faulted and entering offline state: {}",
            self.manifest.name,
            reason
        );
        self.faulted = true;
        self.store.data_mut().cleanup_resources();
    }

    fn commit_dma(&mut self, host: &mut DeviceHost) {
        let pending = std::mem::take(&mut self.store.data_mut().pending_dma);
        self.store.data_mut().pending_dma_bytes = 0;
        for (addr, buf) in pending {
            host.dma_write(addr, &buf);
        }
    }

    /// Point the store at the live Amiga memory for the duration of a call.
    fn enter(&mut self, mem: &mut Memory) {
        self.store.data_mut().mem = mem as *mut Memory as usize;
    }

    /// Clear the Amiga-memory pointer once a call returns.
    fn leave(&mut self) {
        self.store.data_mut().mem = 0;
    }

    /// Snapshot linear memory and its current page count.
    fn snapshot(&mut self) -> (u64, Vec<u8>) {
        let pages = self.memory.size(&self.store);
        let bytes = self.memory.data(&self.store).to_vec();
        (pages, bytes)
    }

    /// Restore a snapshot: grow linear memory to the saved page count, then
    /// write the saved bytes back.
    fn restore(&mut self, pages: u64, bytes: &[u8]) -> Result<()> {
        let cur = self.memory.size(&self.store);
        if pages > cur {
            self.memory
                .grow(&mut self.store, pages - cur)
                .context("growing WASM plugin memory on restore")?;
        }
        self.memory
            .write(&mut self.store, 0, bytes)
            .context("restoring WASM plugin memory")?;
        Ok(())
    }

    /// The host-socket handle counter (see `HostCtx::next_socket_id`'s own
    /// doc comment) -- read at snapshot time and restored afterward so it
    /// stays monotonic across a save/load round trip.
    fn next_socket_id(&self) -> i32 {
        self.store.data().next_socket_id
    }

    fn set_next_socket_id(&mut self, id: i32) {
        self.store.data_mut().next_socket_id = id;
    }
}

/// Load the file resources a manifest's file-typed config values name. Like the
/// module itself and HDF/CD images, these are reopened by path (here and again
/// on a save-state load), not carried in the snapshot.
fn load_resources(manifest: &WasmManifest) -> Result<HashMap<String, Vec<u8>>> {
    let mut map = HashMap::new();
    for key in &manifest.file_keys {
        match manifest.config.get(key) {
            Some(path) if path == crate::hostsocket::BUNDLED_HOSTSOCKET_ROM => {
                map.insert(key.clone(), crate::hostsocket::HOSTSOCKET_ROM.to_vec());
            }
            Some(path) if !path.is_empty() => {
                let bytes = std::fs::read(path)
                    .with_context(|| format!("loading WASM plugin resource {key:?} from {path}"))?;
                map.insert(key.clone(), bytes);
            }
            _ => {} // an unset file option is simply absent
        }
    }
    Ok(map)
}

/// Fuel budget refilled before every entry into a plugin. Copperline runs
/// synchronously on the main emulation thread (see CLAUDE.md), so a plugin
/// with a runaway loop in `init`/`read`/`write`/`tick`/`int2`/`int6` would
/// otherwise hang the whole process forever with no recovery path. This
/// bounds a single call to a large but finite instruction budget instead;
/// exhausting it surfaces as an ordinary trap through the existing
/// log-and-fall-back-to-default handling at each call site.
const PLUGIN_FUEL_BUDGET: u64 = 50_000_000;

/// Refill a store's fuel to [`PLUGIN_FUEL_BUDGET`] ahead of a plugin call.
/// Only fails if fuel consumption isn't configured on the engine, which
/// `make_engine` always enables.
fn refuel(store: &mut Store<HostCtx>) {
    store
        .set_fuel(PLUGIN_FUEL_BUDGET)
        .expect("engine always configures consume_fuel(true)");
}

/// Build the deterministic wasmtime engine. NaN canonicalization removes
/// host-CPU NaN bit-pattern leakage; SIMD/relaxed-SIMD/threads are disabled
/// (relaxed-SIMD is nondeterministic by spec, threads add shared-memory
/// nondeterminism). Fuel consumption caps how long any single call into a
/// plugin can run (see [`PLUGIN_FUEL_BUDGET`]).
fn make_engine() -> Result<Engine> {
    let mut cfg = Config::new();
    cfg.cranelift_nan_canonicalization(true);
    cfg.wasm_simd(false);
    cfg.wasm_relaxed_simd(false);
    cfg.consume_fuel(true);
    // The `threads` wasmtime feature is not built in (see Cargo.toml), so shared
    // memory / atomics are unavailable -- no separate knob needed.
    Engine::new(&cfg).context("creating WASM engine")
}

/// Register the host imports the plugin may call, gated by capability.
fn register_host_fns(linker: &mut Linker<HostCtx>, caps: WasmCaps) -> Result<()> {
    // log(ptr, len): always available.
    linker.func_wrap(
        "env",
        "log",
        |mut caller: Caller<'_, HostCtx>, ptr: i32, len: i32| -> Result<()> {
            let buf = read_wasm_bytes(&mut caller, ptr, len)?;
            let name = caller.data().name.clone();
            log::info!("wasm[{name}]: {}", String::from_utf8_lossy(&buf));
            Ok(())
        },
    )?;

    // config_get(key_ptr, key_len, out_ptr, out_cap) -> i32: copy the setting's
    // value into linear memory (truncated to out_cap) and return its full
    // length, or -1 if the key is absent. Always available.
    linker.func_wrap(
        "env",
        "config_get",
        |mut caller: Caller<'_, HostCtx>,
         key_ptr: i32,
         key_len: i32,
         out_ptr: i32,
         out_cap: i32|
         -> Result<i32> {
            let key = read_wasm_bytes(&mut caller, key_ptr, key_len)?;
            let key = String::from_utf8_lossy(&key).into_owned();
            let Some(value) = caller.data().config.get(&key).cloned() else {
                return Ok(-1);
            };
            let bytes = value.as_bytes();
            let n = bytes.len().min(out_cap.max(0) as usize);
            write_wasm_bytes(&mut caller, out_ptr, &bytes[..n])?;
            Ok(bytes.len() as i32)
        },
    )?;

    // resource_len(name_ptr, name_len) -> i32: byte length of a file resource,
    // or -1 if absent.
    linker.func_wrap(
        "env",
        "resource_len",
        |mut caller: Caller<'_, HostCtx>, name_ptr: i32, name_len: i32| -> Result<i32> {
            let name = read_wasm_bytes(&mut caller, name_ptr, name_len)?;
            let name = String::from_utf8_lossy(&name).into_owned();
            Ok(caller
                .data()
                .resources
                .get(&name)
                .map(|b| b.len() as i32)
                .unwrap_or(-1))
        },
    )?;

    // resource_read(name_ptr, name_len, off, out_ptr, len) -> i32: copy
    // resource[off..off+len] into linear memory; returns the byte count, or -1
    // if the resource is absent.
    linker.func_wrap(
        "env",
        "resource_read",
        |mut caller: Caller<'_, HostCtx>,
         name_ptr: i32,
         name_len: i32,
         off: i32,
         out_ptr: i32,
         len: i32|
         -> Result<i32> {
            let name = read_wasm_bytes(&mut caller, name_ptr, name_len)?;
            let name = String::from_utf8_lossy(&name).into_owned();
            let chunk = match caller.data().resources.get(&name) {
                Some(bytes) => {
                    let off = off.max(0) as usize;
                    let end = off.saturating_add(len.max(0) as usize).min(bytes.len());
                    if off >= bytes.len() {
                        Vec::new()
                    } else {
                        bytes[off..end].to_vec()
                    }
                }
                None => return Ok(-1),
            };
            write_wasm_bytes(&mut caller, out_ptr, &chunk)?;
            Ok(chunk.len() as i32)
        },
    )?;

    if caps.dma {
        // dma_read(addr, ptr, len): Amiga[addr..] -> wasm linear memory[ptr..].
        linker.func_wrap(
            "env",
            "dma_read",
            |mut caller: Caller<'_, HostCtx>, addr: i32, ptr: i32, len: i32| -> Result<()> {
                if caller.data().mem == 0 {
                    anyhow::bail!("dma_read called outside of active host transaction");
                }
                // Validate the destination window before allocating, so a
                // plugin-controlled `len` can't force an oversized host
                // allocation (see `checked_wasm_window`). `write_wasm_bytes`
                // below re-validates the same window when it stores `buf`.
                let mem_size = caller_memory(&mut caller)?.data_size(&caller);
                let (_, len) = checked_wasm_window(ptr, len, mem_size)?;
                let mut buf = vec![0u8; len];
                with_amiga_memory(&caller, |amiga| {
                    DeviceHost::new(amiga).dma_read(addr as u32, &mut buf);
                });

                for (r_start, r_end, r_off) in dma_segments(addr as u32, len) {
                    for (p_addr, p_buf) in &caller.data().pending_dma {
                        for (p_start, p_end, p_off) in dma_segments(*p_addr, p_buf.len()) {
                            if r_start < p_end && p_start < r_end {
                                let overlap_start = r_start.max(p_start);
                                let overlap_end = r_end.min(p_end);
                                let overlap_len = (overlap_end - overlap_start) as usize;
                                let dst_offset = r_off + (overlap_start - r_start) as usize;
                                let src_offset = p_off + (overlap_start - p_start) as usize;
                                buf[dst_offset..dst_offset + overlap_len]
                                    .copy_from_slice(&p_buf[src_offset..src_offset + overlap_len]);
                            }
                        }
                    }
                }

                write_wasm_bytes(&mut caller, ptr, &buf)
            },
        )?;

        // dma_write(addr, ptr, len): wasm linear memory[ptr..] -> Amiga[addr..].
        linker.func_wrap(
            "env",
            "dma_write",
            |mut caller: Caller<'_, HostCtx>, addr: i32, ptr: i32, len: i32| -> Result<()> {
                if caller.data().mem == 0 {
                    anyhow::bail!("dma_write called outside of active host transaction");
                }
                let memory = caller_memory(&mut caller)?;
                let (ptr, len) = checked_wasm_window(ptr, len, memory.data_size(&caller))?;
                let host_ctx = caller.data();
                if host_ctx.pending_dma.len() >= MAX_PENDING_DMA_ENTRIES {
                    anyhow::bail!(
                        "WASM plugin exceeded maximum pending DMA entries ({MAX_PENDING_DMA_ENTRIES})"
                    );
                }
                let new_total = host_ctx
                    .pending_dma_bytes
                    .checked_add(len)
                    .ok_or_else(|| anyhow!("WASM plugin pending DMA byte count overflow"))?;
                if new_total > MAX_PENDING_DMA_BYTES {
                    anyhow::bail!(
                        "WASM plugin exceeded maximum pending DMA buffer size ({new_total} > {MAX_PENDING_DMA_BYTES} bytes)"
                    );
                }
                let mut buf = vec![0u8; len];
                memory
                    .read(&mut caller, ptr, &mut buf)
                    .context("reading WASM plugin memory")?;
                let ctx = caller.data_mut();
                ctx.pending_dma_bytes = new_total;
                ctx.pending_dma.push((addr as u32, buf));
                Ok(())
            },
        )?;
    }

    if caps.net {
        // net_send(ptr, len): transmit the Ethernet frame in linear memory.
        linker.func_wrap(
            "env",
            "net_send",
            |mut caller: Caller<'_, HostCtx>, ptr: i32, len: i32| -> Result<()> {
                let frame = read_wasm_bytes(&mut caller, ptr, len)?;
                if let Some(net) = caller.data_mut().net.as_mut() {
                    net.send(&frame);
                }
                Ok(())
            },
        )?;

        // net_recv(ptr, cap) -> i32: copy the next inbound frame into linear
        // memory at `ptr` (truncated to `cap` bytes) and return its length, or
        // 0 when none is waiting.
        linker.func_wrap(
            "env",
            "net_recv",
            |mut caller: Caller<'_, HostCtx>, ptr: i32, cap: i32| -> Result<i32> {
                let Some(frame) = caller.data_mut().net.as_mut().and_then(|n| n.poll()) else {
                    return Ok(0);
                };
                let n = frame.len().min(cap.max(0) as usize);
                write_wasm_bytes(&mut caller, ptr, &frame[..n])?;
                Ok(n as i32)
            },
        )?;
    }

    if caps.resolve {
        // resolve_start(name_ptr, name_len) -> i32: kick off a hostname
        // lookup via the host's own OS resolver (getaddrinfo) on a
        // short-lived background thread -- it blocks, and this store runs
        // synchronously on the main emulation thread (see CLAUDE.md), so it
        // cannot run inline. Returns a request id to poll with
        // `resolve_poll`, or -1 if the thread couldn't be spawned.
        // MAX_OUTSTANDING_RESOLVES bounds concurrent threads the same way
        // the NAT DNS forwarder's own `MAX_OUTSTANDING` does (a plugin bug
        // or a hostile module retrying `resolve_start` in a loop should
        // exhaust a small, fixed budget, not fork unboundedly).
        linker.func_wrap(
            "env",
            "resolve_start",
            |mut caller: Caller<'_, HostCtx>, name_ptr: i32, name_len: i32| -> Result<i32> {
                if caller.data().resolve_jobs.len() >= MAX_OUTSTANDING_RESOLVES {
                    return Ok(-1);
                }
                let name = read_wasm_bytes(&mut caller, name_ptr, name_len)?;
                let name = String::from_utf8_lossy(&name).into_owned();
                let (tx, rx) = mpsc::channel();
                let spawned = std::thread::Builder::new()
                    .name("wasm-plugin-resolve".into())
                    .spawn(move || {
                        let _ = tx.send(crate::net::nat::dns::resolve_a(&name));
                    });
                if spawned.is_err() {
                    return Ok(-1);
                }
                let ctx = caller.data_mut();
                let id = ctx.next_resolve_id;
                ctx.next_resolve_id = ctx.next_resolve_id.wrapping_add(1);
                ctx.resolve_jobs.insert(id, rx);
                Ok(id)
            },
        )?;

        // resolve_poll(id, out_ptr) -> i32: -2 while still pending, -1 on
        // failure (not found, an unrecognized/already-consumed id, or a
        // lookup that never survived a save-state restore -- see
        // `resolve_jobs`'s own comment), or 0 with `out_ptr`'s 4 bytes (in
        // the plugin's own linear memory, not Amiga memory -- this has
        // nothing to do with `dma_read`/`dma_write`) holding the resolved
        // address in the same big-endian byte order every other address
        // this ABI hands a plugin already uses.
        linker.func_wrap(
            "env",
            "resolve_poll",
            |mut caller: Caller<'_, HostCtx>, id: i32, out_ptr: i32| -> Result<i32> {
                let Some(rx) = caller.data().resolve_jobs.get(&id) else {
                    return Ok(-1);
                };
                match rx.try_recv() {
                    Ok(Some(addr)) => {
                        caller.data_mut().resolve_jobs.remove(&id);
                        write_wasm_bytes(&mut caller, out_ptr, &addr.octets())?;
                        Ok(0)
                    }
                    Ok(None) | Err(mpsc::TryRecvError::Disconnected) => {
                        caller.data_mut().resolve_jobs.remove(&id);
                        Ok(-1)
                    }
                    Err(mpsc::TryRecvError::Empty) => Ok(-2),
                }
            },
        )?;
    }

    if caps.host_sockets {
        // sock_open(domain, type_) -> i32: open a non-blocking host socket,
        // returning a plugin-scoped handle or a negative BSD-style errno.
        // Phase 1 of the host-socket backend (see
        // `HOSTSOCKET-HOST-BACKEND-PLAN.md`) only needs outbound TCP, so
        // only AF_INET/SOCK_STREAM is implemented; bind/listen/accept and
        // UDP are a later phase, hence ENOTSOCK/EOPNOTSUPP here rather than
        // NotSupported at instantiation time -- a plugin can hold the
        // capability and still try (and cleanly fail) other domains/types.
        linker.func_wrap(
            "env",
            "sock_open",
            |mut caller: Caller<'_, HostCtx>, domain: i32, type_: i32| -> Result<i32> {
                const AF_INET: i32 = 2;
                const SOCK_STREAM: i32 = 1;
                const SOCK_DGRAM: i32 = 2;
                if caller.data().sockets.len() >= MAX_OPEN_HOST_SOCKETS {
                    return Ok(-EMFILE);
                }
                if domain != AF_INET {
                    return Ok(-EOPNOTSUPP);
                }
                let sock_type = match type_ {
                    SOCK_STREAM => Type::STREAM,
                    SOCK_DGRAM => Type::DGRAM,
                    _ => return Ok(-EOPNOTSUPP),
                };
                let socket = match Socket::new(Domain::IPV4, sock_type, None) {
                    Ok(s) => s,
                    Err(e) => return Ok(-translate_errno(&e)),
                };
                if let Err(e) = socket.set_nonblocking(true) {
                    return Ok(-translate_errno(&e));
                }
                let ctx = caller.data_mut();
                let id = ctx.next_socket_id;
                ctx.next_socket_id = ctx.next_socket_id.wrapping_add(1);
                ctx.sockets.insert(id, socket);
                Ok(id)
            },
        )?;

        // sock_connect(h, ip, port) -> i32: like a real non-blocking BSD
        // connect() -- 0 on an immediate connect (rare for TCP),
        // -EINPROGRESS with completion to be observed via sock_poll, or
        // another negative errno. `ip` is a big-endian IPv4 address packed
        // into the i32, the same byte order every other address in this
        // ABI uses.
        linker.func_wrap(
            "env",
            "sock_connect",
            |caller: Caller<'_, HostCtx>, handle: i32, ip: i32, port: i32| -> Result<i32> {
                let Some(socket) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                let addr = SocketAddr::V4(SocketAddrV4::new(
                    Ipv4Addr::from((ip as u32).to_be_bytes()),
                    port as u16,
                ));
                match socket.connect(&SockAddr::from(addr)) {
                    Ok(()) => Ok(0),
                    Err(e) if is_connect_in_progress(&e) => Ok(-EINPROGRESS),
                    Err(e) => Ok(-translate_errno(&e)),
                }
            },
        )?;

        // sock_send(h, ptr, len) -> i32: like a non-blocking BSD send() --
        // a byte count, or a negative errno (-EAGAIN when it would block;
        // there is no separate blocking mode, see this module's own doc
        // comment on how the guest's blocking doorbell loop supplies that).
        linker.func_wrap(
            "env",
            "sock_send",
            |mut caller: Caller<'_, HostCtx>, handle: i32, ptr: i32, len: i32| -> Result<i32> {
                let buf = read_wasm_bytes(&mut caller, ptr, len)?;
                let Some(socket) = caller.data_mut().sockets.get_mut(&handle) else {
                    return Ok(-EBADF);
                };
                match socket.write(&buf) {
                    Ok(n) => Ok(n as i32),
                    Err(e) => Ok(-translate_errno(&e)),
                }
            },
        )?;

        // sock_recv(h, ptr, cap) -> i32: a byte count, 0 at EOF, or a
        // negative errno (-EAGAIN when it would block).
        linker.func_wrap(
            "env",
            "sock_recv",
            |mut caller: Caller<'_, HostCtx>, handle: i32, ptr: i32, cap: i32| -> Result<i32> {
                let mem_size = caller_memory(&mut caller)?.data_size(&caller);
                let (_, cap) = checked_wasm_window(ptr, cap, mem_size)?;
                let mut buf = vec![0u8; cap];
                let Some(socket) = caller.data_mut().sockets.get_mut(&handle) else {
                    return Ok(-EBADF);
                };
                let n = match socket.read(&mut buf) {
                    Ok(n) => n,
                    Err(e) => return Ok(-translate_errno(&e)),
                };
                write_wasm_bytes(&mut caller, ptr, &buf[..n])?;
                Ok(n as i32)
            },
        )?;

        // sock_poll(h) -> i32: a readiness bitmask (bit 0 readable, bit 1
        // writable, bit 2 error-pending, bit 3 peer hangup -- set alongside
        // bit 0, see SOCK_HUP's own comment), or -EBADF for an unknown
        // handle.
        // socket2 has no portable select()/poll(), so this approximates
        // the same way a caller without an event loop would: a
        // zero-consuming MSG_PEEK for readability (true at EOF too, same
        // as a real socket), and `take_error()` + `peer_addr()` for
        // connect-completion. Good enough for the guest's own
        // doorbell-retry polling loop; a true poll()-backed wakeup (so the
        // board can assert INT2 the instant a watched socket is ready,
        // instead of only when the guest happens to re-poll) is later
        // work -- see `HOSTSOCKET-HOST-BACKEND-PLAN.md`'s "wakeup poller".
        linker.func_wrap(
            "env",
            "sock_poll",
            |caller: Caller<'_, HostCtx>, handle: i32| -> Result<i32> {
                let Some(socket) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                Ok(poll_socket_mask(socket))
            },
        )?;

        // sock_close(h): drop the host socket. No-op on an unknown handle
        // (the plugin's own fd table is the source of truth for whether
        // closing twice is an error; this import just tears down the host
        // resource).
        linker.func_wrap(
            "env",
            "sock_close",
            |mut caller: Caller<'_, HostCtx>, handle: i32| {
                caller.data_mut().sockets.remove(&handle);
            },
        )?;

        // sock_bind(h, ip, port) -> i32: like BSD bind() -- 0, or a
        // negative errno. `ip`/`port` use the same big-endian-packed-i32
        // convention `sock_connect` already does.
        linker.func_wrap(
            "env",
            "sock_bind",
            |caller: Caller<'_, HostCtx>, handle: i32, ip: i32, port: i32| -> Result<i32> {
                let Some(socket) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                let addr = SocketAddr::V4(SocketAddrV4::new(
                    Ipv4Addr::from((ip as u32).to_be_bytes()),
                    port as u16,
                ));
                match socket.bind(&SockAddr::from(addr)) {
                    Ok(()) => Ok(0),
                    Err(e) => Ok(-translate_errno(&e)),
                }
            },
        )?;

        // sock_listen(h, backlog) -> i32: like BSD listen() -- 0, or a
        // negative errno.
        linker.func_wrap(
            "env",
            "sock_listen",
            |caller: Caller<'_, HostCtx>, handle: i32, backlog: i32| -> Result<i32> {
                let Some(socket) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                match socket.listen(backlog.max(1)) {
                    Ok(()) => Ok(0),
                    Err(e) => Ok(-translate_errno(&e)),
                }
            },
        )?;

        // sock_accept(h) -> i32: like non-blocking BSD accept() -- a new,
        // already-nonblocking handle for the accepted connection, or a
        // negative errno (-EAGAIN when nothing is waiting to be accepted
        // yet). The accepted peer's own address isn't returned here --
        // `sock_local_addr` doubles for querying it too (a connected
        // socket's "local" address from the *other* handle's point of
        // view isn't meaningful, but nothing here needs that; a future
        // `sock_peer_addr` would be the real fix if something does).
        linker.func_wrap(
            "env",
            "sock_accept",
            |mut caller: Caller<'_, HostCtx>, handle: i32| -> Result<i32> {
                if caller.data().sockets.len() >= MAX_OPEN_HOST_SOCKETS {
                    return Ok(-EMFILE);
                }
                let Some(listener) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                match listener.accept() {
                    Ok((accepted, _peer)) => {
                        if let Err(e) = accepted.set_nonblocking(true) {
                            return Ok(-translate_errno(&e));
                        }
                        let ctx = caller.data_mut();
                        let id = ctx.next_socket_id;
                        ctx.next_socket_id = ctx.next_socket_id.wrapping_add(1);
                        ctx.sockets.insert(id, accepted);
                        Ok(id)
                    }
                    Err(e) => Ok(-translate_errno(&e)),
                }
            },
        )?;

        // sock_local_addr(h, out_ptr) -> i32: writes the socket's bound
        // local address as 6 bytes (4-byte IPv4 + 2-byte port, both
        // big-endian) into the plugin's own linear memory at `out_ptr`,
        // returning 0 -- or a negative errno, writing nothing. Lets
        // `do_bind_host` resolve the real port the OS assigned a `bind()`
        // to port 0, the host-backend counterpart of the smoltcp path's
        // own `alloc_local_port()`.
        linker.func_wrap(
            "env",
            "sock_local_addr",
            |mut caller: Caller<'_, HostCtx>, handle: i32, out_ptr: i32| -> Result<i32> {
                let Some(socket) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                let addr = match socket.local_addr() {
                    Ok(addr) => addr,
                    Err(e) => return Ok(-translate_errno(&e)),
                };
                let Some(v4) = addr.as_socket_ipv4() else {
                    return Ok(-EOPNOTSUPP);
                };
                let mut buf = [0u8; 6];
                buf[0..4].copy_from_slice(&v4.ip().octets());
                buf[4..6].copy_from_slice(&v4.port().to_be_bytes());
                write_wasm_bytes(&mut caller, out_ptr, &buf)?;
                Ok(0)
            },
        )?;

        // sock_peer_addr(h, out_ptr) -> i32: `sock_local_addr`'s
        // counterpart for the *remote* end of a connected socket -- same
        // 6-byte layout, needed so `do_accept_host` can report the
        // accepted connection's real peer address instead of a fake
        // 0.0.0.0:0 when the guest actually asked for it (`addr_out != 0`
        // in `accept(sock, addr, addrlen)`).
        linker.func_wrap(
            "env",
            "sock_peer_addr",
            |mut caller: Caller<'_, HostCtx>, handle: i32, out_ptr: i32| -> Result<i32> {
                let Some(socket) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                let addr = match socket.peer_addr() {
                    Ok(addr) => addr,
                    Err(e) => return Ok(-translate_errno(&e)),
                };
                let Some(v4) = addr.as_socket_ipv4() else {
                    return Ok(-EOPNOTSUPP);
                };
                let mut buf = [0u8; 6];
                buf[0..4].copy_from_slice(&v4.ip().octets());
                buf[4..6].copy_from_slice(&v4.port().to_be_bytes());
                write_wasm_bytes(&mut caller, out_ptr, &buf)?;
                Ok(0)
            },
        )?;

        // sock_sendto(h, ptr, len, ip, port) -> i32: a single datagram to
        // an explicit destination, no `sock_connect` required -- UDP's
        // real `sendto()`. A byte count, or a negative errno (`-EAGAIN`
        // when it would block, same as `sock_send`).
        linker.func_wrap(
            "env",
            "sock_sendto",
            |mut caller: Caller<'_, HostCtx>,
             handle: i32,
             ptr: i32,
             len: i32,
             ip: i32,
             port: i32|
             -> Result<i32> {
                let buf = read_wasm_bytes(&mut caller, ptr, len)?;
                let Some(socket) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                let addr = SocketAddr::V4(SocketAddrV4::new(
                    Ipv4Addr::from((ip as u32).to_be_bytes()),
                    port as u16,
                ));
                match socket.send_to(&buf, &SockAddr::from(addr)) {
                    Ok(n) => Ok(n as i32),
                    Err(e) => Ok(-translate_errno(&e)),
                }
            },
        )?;

        // sock_recvfrom(h, ptr, cap, out_addr_ptr) -> i32: a byte count,
        // or a negative errno (`-EAGAIN` when nothing is waiting). On
        // success, also writes the sender's address into the plugin's
        // own linear memory at `out_addr_ptr` (same 6-byte layout as
        // `sock_local_addr`/`sock_peer_addr`) -- UDP's real `recvfrom()`.
        linker.func_wrap(
            "env",
            "sock_recvfrom",
            |mut caller: Caller<'_, HostCtx>,
             handle: i32,
             ptr: i32,
             cap: i32,
             out_addr_ptr: i32|
             -> Result<i32> {
                let mem_size = caller_memory(&mut caller)?.data_size(&caller);
                let (_, cap) = checked_wasm_window(ptr, cap, mem_size)?;
                let mut raw = vec![0u8; cap];
                // SAFETY: `MaybeUninit<u8>` has the same layout as `u8`,
                // and every element of `raw` is already a valid `u8`
                // (zero-initialized) -- reinterpreting it as
                // `&mut [MaybeUninit<u8>]` for socket2's own `recv_from`
                // API, then reading it back as plain bytes afterward, is
                // sound regardless of how much of it `recv_from` actually
                // writes.
                let uninit: &mut [MaybeUninit<u8>] =
                    unsafe { std::slice::from_raw_parts_mut(raw.as_mut_ptr().cast(), raw.len()) };
                let Some(socket) = caller.data_mut().sockets.get_mut(&handle) else {
                    return Ok(-EBADF);
                };
                let (n, from) = match socket.recv_from(uninit) {
                    Ok(v) => v,
                    Err(e) => return Ok(-translate_errno(&e)),
                };
                write_wasm_bytes(&mut caller, ptr, &raw[..n])?;
                if let Some(v4) = from.as_socket_ipv4() {
                    let mut addr_buf = [0u8; 6];
                    addr_buf[0..4].copy_from_slice(&v4.ip().octets());
                    addr_buf[4..6].copy_from_slice(&v4.port().to_be_bytes());
                    write_wasm_bytes(&mut caller, out_addr_ptr, &addr_buf)?;
                }
                Ok(n as i32)
            },
        )?;

        // sock_setopt(h, level, optname, value) -> i32: a small, real
        // subset of setsockopt() applied directly to the host socket --
        // SOL_SOCKET's SO_REUSEADDR/SO_KEEPALIVE/SO_RCVBUF/SO_SNDBUF and
        // IPPROTO_TCP's TCP_NODELAY, the same subset
        // `crates/hostsocket-plugin`'s own smoltcp path tracks, minus
        // SO_LINGER/SO_RCVTIMEO/SO_SNDTIMEO: SO_LINGER's own two-field
        // struct doesn't fit this single-`value` ABI, and
        // SO_RCVTIMEO/SO_SNDTIMEO have no real effect on a socket this
        // backend always keeps OS-level non-blocking -- both stay
        // plugin-side roundtrip storage regardless of backend. An
        // unrecognized (level, optname) is `-EINVAL`; the plugin is
        // expected to fall back to its own roundtrip storage for those,
        // not treat this as the sole source of truth for every option.
        linker.func_wrap(
            "env",
            "sock_setopt",
            |caller: Caller<'_, HostCtx>,
             handle: i32,
             level: i32,
             optname: i32,
             value: i32|
             -> Result<i32> {
                let Some(socket) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                let result = match (level, optname) {
                    (SOL_SOCKET, SO_REUSEADDR) => socket.set_reuse_address(value != 0),
                    (SOL_SOCKET, SO_KEEPALIVE) => socket.set_keepalive(value != 0),
                    (SOL_SOCKET, SO_RCVBUF) => socket.set_recv_buffer_size(value.max(0) as usize),
                    (SOL_SOCKET, SO_SNDBUF) => socket.set_send_buffer_size(value.max(0) as usize),
                    (IPPROTO_TCP, TCP_NODELAY) => socket.set_tcp_nodelay(value != 0),
                    _ => return Ok(-EINVAL),
                };
                match result {
                    Ok(()) => Ok(0),
                    Err(e) => Ok(-translate_errno(&e)),
                }
            },
        )?;

        // sock_getopt(h, level, optname, out_ptr) -> i32: `sock_setopt`'s
        // own counterpart, plus SOL_SOCKET's SO_ERROR -- a real
        // "get pending error and clear" via the host socket's actual
        // `SO_ERROR` (socket2's `take_error()`), translated to this
        // module's own BSD errno space same as everywhere else. Writes a
        // 4-byte big-endian value at `out_ptr` and returns 0 on success,
        // or a negative errno (`-EINVAL` for an unrecognized option,
        // same fallback contract as `sock_setopt`).
        linker.func_wrap(
            "env",
            "sock_getopt",
            |mut caller: Caller<'_, HostCtx>,
             handle: i32,
             level: i32,
             optname: i32,
             out_ptr: i32|
             -> Result<i32> {
                let Some(socket) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                let value: i32 = match (level, optname) {
                    (SOL_SOCKET, SO_REUSEADDR) => match socket.reuse_address() {
                        Ok(v) => v as i32,
                        Err(e) => return Ok(-translate_errno(&e)),
                    },
                    (SOL_SOCKET, SO_KEEPALIVE) => match socket.keepalive() {
                        Ok(v) => v as i32,
                        Err(e) => return Ok(-translate_errno(&e)),
                    },
                    (SOL_SOCKET, SO_RCVBUF) => match socket.recv_buffer_size() {
                        Ok(v) => v as i32,
                        Err(e) => return Ok(-translate_errno(&e)),
                    },
                    (SOL_SOCKET, SO_SNDBUF) => match socket.send_buffer_size() {
                        Ok(v) => v as i32,
                        Err(e) => return Ok(-translate_errno(&e)),
                    },
                    (IPPROTO_TCP, TCP_NODELAY) => match socket.tcp_nodelay() {
                        Ok(v) => v as i32,
                        Err(e) => return Ok(-translate_errno(&e)),
                    },
                    (SOL_SOCKET, SO_ERROR) => match socket.take_error() {
                        Ok(pending) => pending.as_ref().map_or(0, translate_errno),
                        Err(e) => return Ok(-translate_errno(&e)),
                    },
                    _ => return Ok(-EINVAL),
                };
                write_wasm_bytes(&mut caller, out_ptr, &value.to_be_bytes())?;
                Ok(0)
            },
        )?;

        // sock_dup(h) -> i32: a real `dup(2)` of the host socket -- a new,
        // genuinely independent handle sharing the same underlying open
        // file description (same eventual teardown timing: the OS keeps
        // the connection alive until the *last* real descriptor
        // referencing it closes). Backs Dup2Socket/ReleaseCopyOfSocket on
        // a host-backed fd (`crates/hostsocket-plugin`'s own
        // `do_dup2socket_host`/`do_release_copy_of_socket_host`) -- no
        // manual refcounting needed on either side of this ABI the way
        // the plugin's smoltcp path needs its own `Rc<()>` for, since the
        // kernel already does exactly that bookkeeping for a real socket.
        linker.func_wrap(
            "env",
            "sock_dup",
            |mut caller: Caller<'_, HostCtx>, handle: i32| -> Result<i32> {
                // Same cap `sock_open`/`sock_accept` enforce -- without it,
                // repeated `ReleaseCopyOfSocket` calls (each moving one more
                // duplicate into the plugin's own unbounded pool, see that
                // LVO's own doc comment) could grow this table past the
                // limit those two are supposed to guarantee.
                if caller.data().sockets.len() >= MAX_OPEN_HOST_SOCKETS {
                    return Ok(-EMFILE);
                }
                let Some(socket) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                let cloned = match socket.try_clone() {
                    Ok(s) => s,
                    Err(e) => return Ok(-translate_errno(&e)),
                };
                let ctx = caller.data_mut();
                let id = ctx.next_socket_id;
                ctx.next_socket_id = ctx.next_socket_id.wrapping_add(1);
                ctx.sockets.insert(id, cloned);
                Ok(id)
            },
        )?;

        // sock_shutdown(h, how) -> i32: real BSD shutdown() -- `how` is
        // 0 (SHUT_RD), 1 (SHUT_WR), or 2 (SHUT_RDWR). Found missing
        // entirely (`crates/hostsocket-plugin`'s `do_shutdown` had no
        // host-backed branch at all) running bsdsocktest for real
        // against this backend for the first time.
        linker.func_wrap(
            "env",
            "sock_shutdown",
            |caller: Caller<'_, HostCtx>, handle: i32, how: i32| -> Result<i32> {
                let Some(socket) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                let dir = match how {
                    0 => Shutdown::Read,
                    1 => Shutdown::Write,
                    2 => Shutdown::Both,
                    _ => return Ok(-EINVAL),
                };
                match socket.shutdown(dir) {
                    Ok(()) => Ok(0),
                    Err(e) => Ok(-translate_errno(&e)),
                }
            },
        )?;

        // sock_peek(h, ptr, cap) -> i32: like `sock_recv`, but a real
        // non-consuming peek (`socket2::Socket::peek`, `MSG_PEEK` at the
        // OS level) -- backs `recv(MSG_PEEK)`/`recvmsg(MSG_PEEK)` on a
        // host-backed fd, found missing entirely (rejected with a flat
        // `EOPNOTSUPP`) running bsdsocktest for real against this
        // backend for the first time.
        linker.func_wrap(
            "env",
            "sock_peek",
            |mut caller: Caller<'_, HostCtx>, handle: i32, ptr: i32, cap: i32| -> Result<i32> {
                let mem_size = caller_memory(&mut caller)?.data_size(&caller);
                let (_, cap) = checked_wasm_window(ptr, cap, mem_size)?;
                let mut raw = vec![0u8; cap];
                // SAFETY: same reasoning as `sock_recvfrom`'s own
                // identical cast -- `raw` is already fully initialized
                // (zeroed), so reinterpreting it as `&mut [MaybeUninit<u8>]`
                // for socket2's own `peek` API is sound.
                let uninit: &mut [MaybeUninit<u8>] =
                    unsafe { std::slice::from_raw_parts_mut(raw.as_mut_ptr().cast(), raw.len()) };
                let Some(socket) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                match socket.peek(uninit) {
                    Ok(n) => {
                        write_wasm_bytes(&mut caller, ptr, &raw[..n])?;
                        Ok(n as i32)
                    }
                    Err(e) => Ok(-translate_errno(&e)),
                }
            },
        )?;

        // sock_send_oob(h, ptr, len) -> i32: like `sock_send`, but a real
        // `send(MSG_OOB)` (`socket2::Socket::send_out_of_band`) -- backs
        // `send(MSG_OOB)` on a host-backed fd. Unlike the smoltcp path
        // (no urgent-pointer support in `socket::tcp` at all, a permanent
        // structural gap), a real host TCP socket genuinely supports this.
        linker.func_wrap(
            "env",
            "sock_send_oob",
            |mut caller: Caller<'_, HostCtx>, handle: i32, ptr: i32, len: i32| -> Result<i32> {
                let buf = read_wasm_bytes(&mut caller, ptr, len)?;
                let Some(socket) = caller.data_mut().sockets.get_mut(&handle) else {
                    return Ok(-EBADF);
                };
                match socket.send_out_of_band(&buf) {
                    Ok(n) => Ok(n as i32),
                    Err(e) => Ok(-translate_errno(&e)),
                }
            },
        )?;

        // sock_recv_oob(h, ptr, cap) -> i32: like `sock_recv`, but a real
        // `recv(MSG_OOB)` (`socket2::Socket::recv_out_of_band`) -- backs
        // `recv(MSG_OOB)`/`recvmsg(MSG_OOB)` on a host-backed fd, retrieving
        // real urgent data a plain `sock_recv`/`sock_peek` never surfaces.
        linker.func_wrap(
            "env",
            "sock_recv_oob",
            |mut caller: Caller<'_, HostCtx>, handle: i32, ptr: i32, cap: i32| -> Result<i32> {
                let mem_size = caller_memory(&mut caller)?.data_size(&caller);
                let (_, cap) = checked_wasm_window(ptr, cap, mem_size)?;
                let mut raw = vec![0u8; cap];
                // SAFETY: same reasoning as `sock_peek`'s own identical
                // cast -- `raw` is already fully initialized (zeroed).
                let uninit: &mut [MaybeUninit<u8>] =
                    unsafe { std::slice::from_raw_parts_mut(raw.as_mut_ptr().cast(), raw.len()) };
                let Some(socket) = caller.data_mut().sockets.get_mut(&handle) else {
                    return Ok(-EBADF);
                };
                match socket.recv_out_of_band(uninit) {
                    Ok(n) => {
                        write_wasm_bytes(&mut caller, ptr, &raw[..n])?;
                        Ok(n as i32)
                    }
                    Err(e) => Ok(-translate_errno(&e)),
                }
            },
        )?;

        // sock_nread(h) -> i32: bytes currently available to read without
        // blocking -- a real `ioctl(fd, FIONREAD, &n)`, backing
        // `IoctlSocket(FIONREAD)` on a host-backed fd (unix only; no
        // portable equivalent is wired up for other targets yet, same
        // caveat as this module's other unix-only paths).
        linker.func_wrap(
            "env",
            "sock_nread",
            |caller: Caller<'_, HostCtx>, handle: i32| -> Result<i32> {
                let Some(socket) = caller.data().sockets.get(&handle) else {
                    return Ok(-EBADF);
                };
                #[cfg(unix)]
                {
                    use std::os::unix::io::AsRawFd;
                    let mut n: libc::c_int = 0;
                    let rc = unsafe { libc::ioctl(socket.as_raw_fd(), libc::FIONREAD, &mut n) };
                    if rc < 0 {
                        return Ok(-translate_errno(&std::io::Error::last_os_error()));
                    }
                    Ok(n)
                }
                #[cfg(not(unix))]
                {
                    let _ = socket;
                    Ok(-EOPNOTSUPP)
                }
            },
        )?;
    }
    Ok(())
}

/// Caps concurrent background resolver threads a single plugin instance can
/// have in flight (see `resolve_start`'s own comment).
const MAX_OUTSTANDING_RESOLVES: usize = 8;

/// Caps concurrently open host sockets a single plugin instance can hold
/// (the `host_sockets` capability), the same bounded-resource reasoning as
/// [`MAX_OUTSTANDING_RESOLVES`].
const MAX_OPEN_HOST_SOCKETS: usize = 64;

const MAX_PENDING_DMA_ENTRIES: usize = 4096;
const MAX_PENDING_DMA_BYTES: usize = 16 * 1024 * 1024;

// BSD-style errno values the guest's bsdsocket.library expects, matching
// `crates/hostsocket-plugin`'s own copy of this table (see this module's
// `sock_*` doc comment for why the two are hand-kept in sync rather than
// shared -- the plugin compiles to wasm32 and cannot depend on this crate).
const EBADF: i32 = 9;
const EIO: i32 = 5;
const EMFILE: i32 = 24;
const EOPNOTSUPP: i32 = 45;
const EAGAIN: i32 = 35;
const EINPROGRESS: i32 = 36;
const EALREADY: i32 = 37;
const ENOTSOCK: i32 = 38;
const ENOTCONN: i32 = 57;
const ECONNREFUSED: i32 = 61;
const EPIPE: i32 = 32;
const ECONNRESET: i32 = 54;
const EADDRINUSE: i32 = 48;
const ETIMEDOUT: i32 = 60;
const ENETUNREACH: i32 = 51;
const EHOSTUNREACH: i32 = 65;
// Only produced by a second non-blocking `connect()` call on a socket whose
// first attempt already succeeded (POSIX-guaranteed: not a real error) --
// see `crates/hostsocket-plugin`'s own `do_connect_host` for the caller
// that relies on seeing this rather than a bare success from the repeat
// call.
const EISCONN: i32 = 56;
const EINVAL: i32 = 22;

// `sock_setopt`/`sock_getopt`'s own (level, optname) numbering -- BSD
// sockopt constants, matching `crates/hostsocket-plugin`'s own identical
// copy of this same small table (hand-kept in sync, same convention as
// this module's errno constants above).
const SOL_SOCKET: i32 = 0xFFFF;
const IPPROTO_TCP: i32 = 6;
const SO_ERROR: i32 = 0x1007;
const SO_RCVBUF: i32 = 0x1002;
const SO_SNDBUF: i32 = 0x1001;
const SO_REUSEADDR: i32 = 0x0004;
const SO_KEEPALIVE: i32 = 0x0008;
const TCP_NODELAY: i32 = 0x01;

/// Whether a non-blocking `connect()` is still in progress: `WouldBlock` on
/// every platform's `io::ErrorKind`, or the raw `EINPROGRESS`/`WSAEINPROGRESS`
/// errno on unix/Windows respectively (the kind Rust's std maps it to is not
/// consistent enough across platforms/versions to rely on alone).
fn is_connect_in_progress(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::EINPROGRESS)
    }
    #[cfg(windows)]
    {
        e.raw_os_error() == Some(WSAEINPROGRESS)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// The host's native errno for `raw`, translated to this module's BSD-style
/// constant.
#[cfg(unix)]
fn from_raw_errno(raw: i32) -> Option<i32> {
    Some(match raw {
        _ if raw == libc::EAGAIN || raw == libc::EWOULDBLOCK => EAGAIN,
        _ if raw == libc::EINPROGRESS => EINPROGRESS,
        _ if raw == libc::EALREADY => EALREADY,
        _ if raw == libc::ENOTSOCK => ENOTSOCK,
        _ if raw == libc::ENOTCONN => ENOTCONN,
        _ if raw == libc::ECONNREFUSED => ECONNREFUSED,
        _ if raw == libc::EPIPE => EPIPE,
        _ if raw == libc::ECONNRESET => ECONNRESET,
        _ if raw == libc::EADDRINUSE => EADDRINUSE,
        _ if raw == libc::EOPNOTSUPP => EOPNOTSUPP,
        _ if raw == libc::EMFILE => EMFILE,
        _ if raw == libc::ETIMEDOUT => ETIMEDOUT,
        _ if raw == libc::ENETUNREACH => ENETUNREACH,
        _ if raw == libc::EHOSTUNREACH => EHOSTUNREACH,
        _ if raw == libc::EISCONN => EISCONN,
        _ => return None,
    })
}

// Winsock (WSA*) error codes -- stable, documented Win32 API constants (MSDN
// `WSAGetLastError`), not available as named constants in `libc` on this
// target the way unix's own errno family is, so hand-listed here. Mapping
// these matters: `do_connect_host`'s retry loop (plugin crate) re-issues
// `sock_connect` on every retry and specifically checks for `EALREADY`
// (still pending) vs `EISCONN` (now connected) vs anything else (hard
// failure) -- without this table, every one of those raw WSA codes fell
// through to a generic `EIO` (no `ErrorKind` in `translate_errno`'s own
// fallback match captures them precisely either), and a retry loop that
// never sees its own expected `EALREADY` bails out as a hard failure on
// the very first retry. Found running this crate's own host-backend test
// suite for real on Windows CI for the first time (every test issuing a
// guest-side non-blocking TCP `connect()` failed outright; UDP `connect()`
// -- synchronous at the OS level, no retry loop involved -- and pure
// accept()-side tests were unaffected, isolating the fault to exactly
// this retry path).
#[cfg(windows)]
const WSAEWOULDBLOCK: i32 = 10035;
#[cfg(windows)]
const WSAEINPROGRESS: i32 = 10036;
#[cfg(windows)]
const WSAEALREADY: i32 = 10037;
#[cfg(windows)]
const WSAENOTSOCK: i32 = 10038;
#[cfg(windows)]
const WSAEOPNOTSUPP: i32 = 10045;
#[cfg(windows)]
const WSAEADDRINUSE: i32 = 10048;
#[cfg(windows)]
const WSAENETUNREACH: i32 = 10051;
#[cfg(windows)]
const WSAECONNRESET: i32 = 10054;
#[cfg(windows)]
const WSAEISCONN: i32 = 10056;
#[cfg(windows)]
const WSAENOTCONN: i32 = 10057;
#[cfg(windows)]
const WSAESHUTDOWN: i32 = 10058;
#[cfg(windows)]
const WSAETIMEDOUT: i32 = 10060;
#[cfg(windows)]
const WSAECONNREFUSED: i32 = 10061;
#[cfg(windows)]
const WSAEHOSTUNREACH: i32 = 10065;
#[cfg(windows)]
const WSAEMFILE: i32 = 10024;

#[cfg(windows)]
fn from_raw_errno(raw: i32) -> Option<i32> {
    Some(match raw {
        _ if raw == WSAEWOULDBLOCK => EAGAIN,
        _ if raw == WSAEINPROGRESS => EINPROGRESS,
        _ if raw == WSAEALREADY => EALREADY,
        _ if raw == WSAENOTSOCK => ENOTSOCK,
        _ if raw == WSAENOTCONN => ENOTCONN,
        _ if raw == WSAECONNREFUSED => ECONNREFUSED,
        // No distinct WSA "broken pipe" -- a write past a local shutdown or
        // a torn-down connection surfaces as WSAESHUTDOWN/WSAECONNRESET,
        // matching real BSD EPIPE/ECONNRESET closely enough for this ABI's
        // own error-reporting granularity.
        _ if raw == WSAESHUTDOWN => EPIPE,
        _ if raw == WSAECONNRESET => ECONNRESET,
        _ if raw == WSAEADDRINUSE => EADDRINUSE,
        _ if raw == WSAEOPNOTSUPP => EOPNOTSUPP,
        _ if raw == WSAEMFILE => EMFILE,
        _ if raw == WSAETIMEDOUT => ETIMEDOUT,
        _ if raw == WSAENETUNREACH => ENETUNREACH,
        _ if raw == WSAEHOSTUNREACH => EHOSTUNREACH,
        _ if raw == WSAEISCONN => EISCONN,
        _ => return None,
    })
}

#[cfg(not(any(unix, windows)))]
fn from_raw_errno(_raw: i32) -> Option<i32> {
    None
}

/// Normalize a host I/O error to this module's BSD-style errno space (see
/// the `sock_*` doc comment for why the guest never sees a platform-native
/// errno).
fn translate_errno(e: &std::io::Error) -> i32 {
    if let Some(code) = e.raw_os_error().and_then(from_raw_errno) {
        return code;
    }
    use std::io::ErrorKind::*;
    match e.kind() {
        WouldBlock => EAGAIN,
        ConnectionRefused => ECONNREFUSED,
        ConnectionReset => ECONNRESET,
        NotConnected => ENOTCONN,
        TimedOut => ETIMEDOUT,
        AddrInUse => EADDRINUSE,
        BrokenPipe => EPIPE,
        _ => EIO,
    }
}

/// `sock_poll`'s readiness bitmask bits (see this module's own `sock_*` doc
/// comment) -- hand-kept in sync with `crates/hostsocket-plugin`'s copy of
/// these same values.
const SOCK_READABLE: i32 = 1;
const SOCK_WRITABLE: i32 = 2;
const SOCK_ERROR: i32 = 4;
/// Set alongside `SOCK_READABLE` specifically on a peer hangup (`POLLHUP`,
/// or a zero-length `peek()` on the non-unix fallback) -- lets a caller
/// that needs to tell "there is real data" apart from "the peer is gone,
/// a `recv()` here would just return EOF" do so without consuming
/// anything, which `GetSocketEvents()`'s FD_CLOSE edge detection needs
/// (see `sample_event_level_host` in the plugin crate).
const SOCK_HUP: i32 = 8;

/// `sock_poll`'s readiness check for one socket. On unix, a real
/// non-blocking `poll(2)` -- correct for every socket state this module
/// needs it for, including a *listening* socket (POSIX defines `POLLIN` on
/// one to mean "a connection is ready to `accept()`", not "there is data to
/// read") and a still-connecting one (`POLLOUT`/`POLLERR` are the standard
/// way to detect a non-blocking `connect()`'s completion, exactly what
/// `do_connect_host`'s own repeated-`connect()` retry already relies on
/// `sock_poll` to schedule). `POLLHUP` counts as readable too: a
/// readable-at-EOF socket is exactly what a real `recv()` returning `0`
/// looks like, the same "readable" a data-bearing socket reports.
///
/// Deliberately does *not* surface `POLLPRI` (real TCP urgent/out-of-band
/// data pending) as a bit here: tried, and found unreliable on this
/// project's own macOS dev host -- `poll(2)` never reported it for a
/// genuine `MSG_OOB` send in isolation, but *did* report it spuriously
/// coincident with an unrelated `POLLHUP` (peer socket closing), which
/// would have made `WaitSelect()`'s `exceptfds` fire on the wrong
/// condition entirely. `recv(MSG_OOB)` itself (`sock_recv_oob`) works
/// fine without this -- a real, edge-triggered `MSG_OOB` byte is
/// retrievable directly, no polling needed -- only the non-consuming
/// "is one pending" signal `exceptfds` would need is what's missing.
///
/// Windows uses `WSAPoll`, whose listener/read/connect semantics match the
/// `poll(2)` bits used here. Remaining non-unix targets fall back to a
/// cruder `peek()`/`peer_addr()` heuristic.
#[cfg(unix)]
fn poll_socket_mask(socket: &Socket) -> i32 {
    use std::os::unix::io::AsRawFd;
    let mut pfd = libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLIN | libc::POLLOUT,
        revents: 0,
    };
    // timeout = 0: an immediate, non-blocking poll -- this runs inside a
    // wasmtime host call, which must never block the main emulation
    // thread.
    if unsafe { libc::poll(&mut pfd, 1, 0) } < 0 {
        // A `poll(2)` syscall failure (not a socket error -- e.g. EINTR)
        // is vanishingly unlikely for a single, valid, already-open fd;
        // report it as an error bit rather than silently claiming "not
        // ready", so a caller looping on this doesn't spin forever.
        return SOCK_ERROR;
    }
    let mut mask = 0;
    if pfd.revents & libc::POLLIN != 0 {
        mask |= SOCK_READABLE;
    }
    if pfd.revents & libc::POLLHUP != 0 {
        // A hung-up fd is done in both directions, not just readable: a
        // pending or future write would fail immediately (EPIPE/ECONNRESET),
        // so real select()/poll() convention reports it write-ready too --
        // otherwise a blocked send(), a WaitSelect(writefds), or SO_EVENTMASK's
        // own FD_CONNECT/FD_WRITE edge (both keyed on this same bit, see
        // `scan_select`'s and `sample_event_level_host`'s own write_ready
        // checks in the plugin crate) would never retry and discover the
        // real failure, the same livelock shape `do_connect_host`'s own
        // retry loop was fixed for (see this file's own comment on that).
        mask |= SOCK_READABLE | SOCK_WRITABLE | SOCK_HUP;
    }
    if pfd.revents & libc::POLLOUT != 0 {
        mask |= SOCK_WRITABLE;
    }
    if pfd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        mask |= SOCK_ERROR;
    }
    mask
}

#[cfg(windows)]
fn poll_socket_mask(socket: &Socket) -> i32 {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        WSAPoll, POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT, WSAPOLLFD,
    };

    let mut pfd = WSAPOLLFD {
        fd: socket.as_raw_socket() as usize,
        events: POLLIN | POLLOUT,
        revents: 0,
    };
    // Zero timeout: the WASM host call must never block the emulator thread.
    if unsafe { WSAPoll(&mut pfd, 1, 0) } < 0 {
        return SOCK_ERROR;
    }
    let mut mask = 0;
    if pfd.revents & POLLIN != 0 {
        mask |= SOCK_READABLE;
    }
    if pfd.revents & POLLHUP != 0 {
        mask |= SOCK_READABLE | SOCK_WRITABLE | SOCK_HUP;
    }
    if pfd.revents & POLLOUT != 0 {
        mask |= SOCK_WRITABLE;
    }
    if pfd.revents & (POLLERR | POLLNVAL) != 0 {
        mask |= SOCK_ERROR;
    }
    mask
}

#[cfg(not(any(unix, windows)))]
fn poll_socket_mask(socket: &Socket) -> i32 {
    let mut mask = 0;
    let mut probe = [MaybeUninit::new(0u8); 1];
    match socket.peek(&mut probe) {
        Ok(0) => mask |= SOCK_READABLE | SOCK_HUP,
        Ok(_) => mask |= SOCK_READABLE,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        // A listening socket's own `peek()` fails with `NotConnected` (it
        // is not a connected data socket at all to peek from) -- not a
        // real error and not evidence of anything readable, unlike every
        // other `peek()` failure. Treating it the same as a genuine error
        // (the old behavior here) made every listening socket poll as
        // permanently accept-ready, spuriously firing
        // `WaitSelect(readfds)`/`SO_EVENTMASK`'s `FD_ACCEPT` even with
        // nothing pending. Skipped entirely, at the cost of the same
        // "cannot detect accept-readiness on a listening socket" gap this
        // module's own doc comment on `poll_socket_mask` already
        // documents for non-unix.
        Err(e) if e.kind() == std::io::ErrorKind::NotConnected => {}
        Err(_) => mask |= SOCK_READABLE | SOCK_ERROR,
    }
    // Deliberately not `take_error()` here: that call is destructive (it
    // clears the pending `SO_ERROR`, real BSD `getsockopt(SO_ERROR)`
    // semantics), and this function runs on every readiness poll --
    // `WaitSelect()`, `SO_EVENTMASK`'s tick-driven sampling, every
    // `do_connect_host` retry -- far more often than the one place that's
    // actually supposed to consume it, the guest's own real
    // `getsockopt(SO_ERROR)` (`sock_getopt`'s own `SO_ERROR` arm, which
    // does call `take_error()`, correctly). Calling it here too raced
    // that real query and could silently clear the error first, making a
    // later `getsockopt(SO_ERROR)` report `0` for a connection that
    // genuinely failed. `peer_addr()` alone is enough for write-readiness
    // (only succeeds once actually connected), and a failed connect still
    // surfaces as `SOCK_ERROR` above: a broken socket's own `peek()`
    // fails too, just not with `NotConnected`.
    if socket.peer_addr().is_ok() {
        mask |= SOCK_WRITABLE;
    }
    mask
}

/// Run `f` with the live Amiga memory the store currently points at. No-op when
/// the pointer is unset (a plugin should only DMA from within a host call).
fn with_amiga_memory(caller: &Caller<'_, HostCtx>, f: impl FnOnce(&mut Memory)) {
    let mem = caller.data().mem;
    if mem == 0 {
        return;
    }
    // SAFETY: `mem` is the address of the `&mut Memory` set by `WasmRuntime::enter`
    // for the duration of this plugin call (see the ZorroDevice impl below). It is
    // not aliased while the plugin runs -- the outer DeviceHost is not touched
    // until the call returns -- and is cleared to 0 afterwards.
    let amiga = unsafe { &mut *(mem as *mut Memory) };
    f(amiga);
}

/// Validate a plugin-supplied `[ptr, ptr+len)` window against the plugin's
/// current linear-memory size, returning the clamped `(ptr, len)`. `ptr`
/// and `len` are plugin-controlled and unrelated to how much memory the
/// plugin actually has, so an out-of-range window is rejected *before* the
/// host allocates `len` bytes -- otherwise a huge `len` (up to ~2 GiB via
/// i32) would force an oversized host allocation per call. An out-of-range
/// window is a clean error, exactly as the previous trap-on-access was.
fn checked_wasm_window(ptr: i32, len: i32, mem_size: usize) -> Result<(usize, usize)> {
    let ptr = ptr.max(0) as usize;
    let len = len.max(0) as usize;
    if ptr.checked_add(len).is_none_or(|end| end > mem_size) {
        anyhow::bail!("WASM plugin memory window {ptr}+{len} exceeds {mem_size} bytes");
    }
    Ok((ptr, len))
}

fn dma_segments(addr: u32, len: usize) -> impl Iterator<Item = (u64, u64, usize)> {
    let (seg1, seg2) = if len == 0 {
        (None, None)
    } else {
        let first_len = len.min((u32::MAX - addr) as usize + 1);
        let start = addr as u64;
        let s1 = Some((start, start + first_len as u64, 0));
        let s2 = if first_len < len {
            Some((0, (len - first_len) as u64, first_len))
        } else {
            None
        };
        (s1, s2)
    };
    seg1.into_iter().chain(seg2)
}

/// Read `len` bytes from the plugin's linear memory at `ptr`.
fn read_wasm_bytes(caller: &mut Caller<'_, HostCtx>, ptr: i32, len: i32) -> Result<Vec<u8>> {
    let memory = caller_memory(caller)?;
    let (ptr, len) = checked_wasm_window(ptr, len, memory.data_size(&caller))?;
    let mut buf = vec![0u8; len];
    memory
        .read(&mut *caller, ptr, &mut buf)
        .context("reading WASM plugin memory")?;
    Ok(buf)
}

/// Write `buf` into the plugin's linear memory at `ptr`.
fn write_wasm_bytes(caller: &mut Caller<'_, HostCtx>, ptr: i32, buf: &[u8]) -> Result<()> {
    let memory = caller_memory(caller)?;
    memory
        .write(&mut *caller, ptr.max(0) as usize, buf)
        .context("writing WASM plugin memory")?;
    Ok(())
}

fn caller_memory(caller: &mut Caller<'_, HostCtx>) -> Result<WasmMemory> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| anyhow!("WASM plugin exports no `memory`"))
}

/// A functional Zorro board implemented by a WASM plugin module.
///
/// Serializes via a path-reopen shadow (like HDF/CD images): the snapshot
/// carries the module path, manifest, and a linear-memory image; on load the
/// module is recompiled from its path and the image replayed.
pub struct WasmBoard {
    module_path: PathBuf,
    rt: RefCell<WasmRuntime>,
}

impl WasmBoard {
    /// Load and instantiate a plugin module from a `.wasm` file.
    pub fn from_file(path: &Path, manifest: WasmManifest) -> Result<Self> {
        Self::from_file_with_mode(path, manifest, InstantiationMode::Active)
    }

    fn from_file_with_mode(
        path: &Path,
        manifest: WasmManifest,
        mode: InstantiationMode,
    ) -> Result<Self> {
        if mode == InstantiationMode::Active && manifest.net != NetConfig::None {
            log::warn!(
                "wasm[{}]: network backend {:?} active -- deterministic replay \
                 and save-state reproducibility are not guaranteed while \
                 traffic flows",
                manifest.name,
                manifest.net
            );
        } else if mode == InstantiationMode::Active && manifest.caps.resolve {
            // The resolve capability is just as non-deterministic as a net
            // backend (host-resolver answers arrive on the host's schedule
            // and vary with its DNS state), and a board can hold it without
            // any net backend at all -- warn for that shape too rather than
            // letting it break replay silently. One warning suffices when
            // both apply, hence the else-if.
            log::warn!(
                "wasm[{}]: host-resolver capability active -- deterministic \
                 replay and save-state reproducibility are not guaranteed \
                 while lookups run",
                manifest.name
            );
        } else if mode == InstantiationMode::Active && manifest.caps.host_sockets {
            // Same reasoning as the resolve branch above: host sockets are
            // non-deterministic (and reach further than either net or
            // resolve -- see `WasmCaps::host_sockets`'s own doc comment),
            // and a board can hold the capability without a net backend or
            // resolve capability at all.
            log::warn!(
                "wasm[{}]: host-socket capability active -- deterministic \
                 replay and save-state reproducibility are not guaranteed \
                 while connections are open",
                manifest.name
            );
        }
        let engine = make_engine()?;
        // The bundled HostSocket board's module is embedded in the binary;
        // its path is a sentinel, not a file (and stays one through a
        // save-state round trip, which reopens modules by path).
        let module = if path == Path::new(crate::hostsocket::BUNDLED_HOSTSOCKET_WASM) {
            Module::new(&engine, crate::hostsocket::HOSTSOCKET_WASM)
                .context("compiling the bundled HostSocket plugin")?
        } else {
            Module::from_file(&engine, path)
                .with_context(|| format!("compiling WASM plugin {}", path.display()))?
        };
        let rt = WasmRuntime::new(engine, module, manifest, mode)?;
        Ok(Self {
            module_path: path.to_path_buf(),
            rt: RefCell::new(rt),
        })
    }

    /// Call an exported function that takes no Amiga memory, returning its
    /// `i32` result (0 if the export is absent or traps).
    fn call_flag(&self, sel: impl FnOnce(&Exports) -> Option<TypedFunc<(), i32>>) -> bool {
        let mut rt = self.rt.borrow_mut();
        if rt.faulted {
            return false;
        }
        let Some(func) = sel(&rt.exports) else {
            return false;
        };
        refuel(&mut rt.store);
        let result = func.call(&mut rt.store, ());
        rt.store.data_mut().clear_pending_dma();
        match result {
            Ok(v) => v != 0,
            Err(e) => {
                rt.trigger_fault(&format!("int line query trapped: {e}"));
                false
            }
        }
    }
}

impl ZorroDevice for WasmBoard {
    fn read(&mut self, off: u32, size: usize, host: &mut DeviceHost) -> u32 {
        let rt = self.rt.get_mut();
        if rt.faulted {
            return 0xFFFF_FFFF;
        }
        let Some(func) = rt.exports.read.clone() else {
            return 0xFFFF_FFFF;
        };
        rt.enter(host.memory_mut());
        refuel(&mut rt.store);
        let result = func.call(&mut rt.store, (off as i32, size as i32));
        rt.leave();
        match result {
            Ok(v) => {
                rt.commit_dma(host);
                v as u32
            }
            Err(e) => {
                rt.store.data_mut().clear_pending_dma();
                rt.trigger_fault(&format!("read trapped: {e}"));
                0xFFFF_FFFF
            }
        }
    }

    fn write(&mut self, off: u32, size: usize, value: u32, host: &mut DeviceHost) {
        let rt = self.rt.get_mut();
        if rt.faulted {
            return;
        }
        let Some(func) = rt.exports.write.clone() else {
            return;
        };
        rt.enter(host.memory_mut());
        refuel(&mut rt.store);
        let result = func.call(&mut rt.store, (off as i32, size as i32, value as i32));
        rt.leave();
        match result {
            Ok(()) => {
                rt.commit_dma(host);
            }
            Err(e) => {
                rt.store.data_mut().clear_pending_dma();
                rt.trigger_fault(&format!("write trapped: {e}"));
            }
        }
    }

    fn tick(&mut self, cck: u32, host: &mut DeviceHost) {
        let rt = self.rt.get_mut();
        if rt.faulted {
            return;
        }
        let Some(func) = rt.exports.tick.clone() else {
            return;
        };
        rt.enter(host.memory_mut());
        refuel(&mut rt.store);
        let result = func.call(&mut rt.store, cck as i32);
        rt.leave();
        match result {
            Ok(()) => {
                rt.commit_dma(host);
            }
            Err(e) => {
                rt.store.data_mut().clear_pending_dma();
                rt.trigger_fault(&format!("tick trapped: {e}"));
            }
        }
    }

    fn int2_line(&self) -> bool {
        self.call_flag(|e| e.int2.clone())
    }

    fn int6_line(&self) -> bool {
        self.call_flag(|e| e.int6.clone())
    }

    // Ticked every slice (the plugin decides what to do); a sparse next_event
    // model is a future optimization.
    fn is_idle(&self) -> bool {
        false
    }

    fn reset(&mut self) {
        if let Err(e) = self.rt.get_mut().reset() {
            log::error!("wasm: plugin reset failed: {e}");
        }
    }

    fn kind(&self) -> &'static str {
        "wasm"
    }
}

/// The serialized form of a [`WasmBoard`]: enough to recompile the module from
/// its path and replay its linear memory.
#[derive(Serialize, Deserialize)]
struct WasmBoardState {
    module_path: PathBuf,
    manifest: WasmManifest,
    pages: u64,
    bytes: Vec<u8>,
    /// `HostCtx::next_socket_id` at snapshot time (default 0 so an old save
    /// file without this field still deserializes). `sockets` itself is a
    /// host resource and never serialized (every handle the plugin
    /// remembers in `bytes` above is deliberately stale after a restore,
    /// see `sockets`'s own doc comment) -- but the *counter* has to
    /// survive anyway: `Deserialize` below builds a brand new `HostCtx`
    /// via `WasmBoard::from_file`, whose own counter starts back at 0. If
    /// left there, the very next `sock_open`/`sock_accept`/`sock_dup`
    /// after a restore could hand out a low handle that collides with a
    /// stale one still sitting in the restored `host_fds` table -- the old
    /// (and now-unrelated) guest fd would silently start operating on the
    /// new socket instead of the intended clean `EBADF`. Restoring this
    /// value keeps the id sequence monotonic across the round trip, so no
    /// value handed out before the save is ever handed out again after.
    #[serde(default)]
    next_socket_id: i32,
    #[serde(default)]
    faulted: bool,
}

impl Serialize for WasmBoard {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut rt = self.rt.borrow_mut();
        let (pages, bytes) = rt.snapshot();
        let state = WasmBoardState {
            module_path: self.module_path.clone(),
            manifest: rt.manifest.clone(),
            pages,
            bytes,
            next_socket_id: rt.next_socket_id(),
            faulted: rt.faulted,
        };
        state.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WasmBoard {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let state = WasmBoardState::deserialize(deserializer)?;
        let mode = if state.faulted {
            InstantiationMode::FaultedRestore
        } else {
            InstantiationMode::Active
        };
        let board = WasmBoard::from_file_with_mode(&state.module_path, state.manifest, mode)
            .map_err(serde::de::Error::custom)?;
        let mut rt = board.rt.borrow_mut();
        rt.restore(state.pages, &state.bytes)
            .map_err(serde::de::Error::custom)?;
        rt.set_next_socket_id(state.next_socket_id);
        rt.faulted = state.faulted;
        drop(rt);
        Ok(board)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A golden test plugin: a 16-bit counter at window offset 0, incremented
    /// by `tick`, readable/writable, asserting INT2 once it passes a threshold.
    /// Its whole state is one i32 in linear memory at address 0.
    const COUNTER_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "read") (param $off i32) (param $size i32) (result i32)
            (i32.load (i32.const 0)))
          (func (export "write") (param $off i32) (param $size i32) (param $val i32)
            (i32.store (i32.const 0) (local.get $val)))
          (func (export "tick") (param $cck i32)
            (i32.store (i32.const 0)
              (i32.add (i32.load (i32.const 0)) (i32.const 1))))
          (func (export "int2") (result i32)
            (i32.gt_u (i32.load (i32.const 0)) (i32.const 3)))
        )
    "#;

    /// A DMA test plugin: `write(off, size, val)` reads 4 bytes from Amiga
    /// address `val` into linear memory and stores their big-endian sum back so
    /// `read` returns it; exercises the dma_read host import.
    const DMA_WAT: &str = r#"
        (module
          (import "env" "dma_read" (func $dma_read (param i32 i32 i32)))
          (memory (export "memory") 1)
          (func (export "read") (param $off i32) (param $size i32) (result i32)
            (i32.load (i32.const 0)))
          (func (export "write") (param $off i32) (param $size i32) (param $addr i32)
            ;; copy 4 bytes from Amiga[$addr] into linear memory at offset 16
            (call $dma_read (local.get $addr) (i32.const 16) (i32.const 4))
            ;; store the 32-bit big-endian value at offset 0
            (i32.store (i32.const 0)
              (i32.or
                (i32.or
                  (i32.shl (i32.load8_u (i32.const 16)) (i32.const 24))
                  (i32.shl (i32.load8_u (i32.const 17)) (i32.const 16)))
                (i32.or
                  (i32.shl (i32.load8_u (i32.const 18)) (i32.const 8))
                  (i32.load8_u (i32.const 19))))))
        )
    "#;

    fn write_wasm(name: &str, wat: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bytes = wat::parse_str(wat).expect("valid WAT");
        let path = std::env::temp_dir().join(format!(
            "copperline-wasmboard-{name}-{}-{seq}.wasm",
            std::process::id(),
        ));
        std::fs::write(&path, &bytes).expect("write wasm");
        path
    }

    fn manifest(name: &str, dma: bool) -> WasmManifest {
        WasmManifest {
            name: name.into(),
            caps: WasmCaps {
                dma,
                int2: true,
                int6: false,
                net: false,
                resolve: false,
                host_sockets: false,
            },
            net: NetConfig::None,
            config: BTreeMap::new(),
            file_keys: Vec::new(),
        }
    }

    fn net_manifest(name: &str) -> WasmManifest {
        WasmManifest {
            name: name.into(),
            caps: WasmCaps {
                dma: false,
                int2: false,
                int6: false,
                net: true,
                resolve: false,
                host_sockets: false,
            },
            net: NetConfig::Loopback,
            config: BTreeMap::new(),
            file_keys: Vec::new(),
        }
    }

    fn resolve_manifest(name: &str) -> WasmManifest {
        WasmManifest {
            name: name.into(),
            caps: WasmCaps {
                dma: false,
                int2: false,
                int6: false,
                net: false,
                resolve: true,
                host_sockets: false,
            },
            net: NetConfig::None,
            config: BTreeMap::new(),
            file_keys: Vec::new(),
        }
    }

    fn host_sockets_manifest(name: &str) -> WasmManifest {
        WasmManifest {
            name: name.into(),
            caps: WasmCaps {
                dma: false,
                int2: false,
                int6: false,
                net: false,
                resolve: false,
                host_sockets: true,
            },
            net: NetConfig::None,
            config: BTreeMap::new(),
            file_keys: Vec::new(),
        }
    }

    fn empty_memory() -> Memory {
        Memory {
            chip_ram: vec![0u8; 0x1000],
            slow_ram: Vec::new(),
            mb_ram: Vec::new(),
            accel_ram: Vec::new(),
            rom: Vec::new(),
            overlay: false,
            zorro: crate::zorro::ZorroChain::default(),
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs: Vec::new(),
            wcs_write_protected: false,
        }
    }

    #[test]
    fn counter_plugin_reads_writes_and_ticks() {
        let path = write_wasm("counter", COUNTER_WAT);
        let mut board = WasmBoard::from_file(&path, manifest("counter", false)).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        assert_eq!(board.read(0, 2, &mut host), 0);
        board.write(0, 2, 10, &mut host);
        assert_eq!(board.read(0, 2, &mut host), 10);

        // tick increments; int2 asserts once the counter passes 3.
        let mut fresh = WasmBoard::from_file(&path, manifest("counter", false)).unwrap();
        assert!(!fresh.int2_line());
        for _ in 0..5 {
            fresh.tick(1, &mut host);
        }
        assert_eq!(fresh.read(0, 2, &mut host), 5);
        assert!(fresh.int2_line());

        let _ = std::fs::remove_file(&path);
    }

    /// A runaway plugin: `tick` never returns. Copperline runs synchronously
    /// on the main thread, so without a fuel cap this would hang the whole
    /// emulator forever instead of trapping.
    const INFINITE_LOOP_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "tick") (param $cck i32)
            (loop $forever
              (br $forever)))
        )
    "#;

    const INIT_COUNTER_WAT: &str = r#"
        (module
          (global $init_count (mut i32) (i32.const 0))
          (memory (export "memory") 1)
          (func (export "init")
            (global.set $init_count
              (i32.add (global.get $init_count) (i32.const 1))))
          (func (export "read") (param $off i32) (param $size i32) (result i32)
            (global.get $init_count))
          (func (export "tick") (param $cck i32)
            (loop $forever
              (br $forever)))
        )
    "#;

    fn runtime_probe(board: &WasmBoard) -> (bool, bool, i32) {
        let mut rt = board.rt.borrow_mut();
        let faulted = rt.faulted;
        let has_network_backend = rt.store.data().net.is_some();
        let read = rt.exports.read.clone().unwrap();
        refuel(&mut rt.store);
        let init_count = read.call(&mut rt.store, (0, 0)).unwrap();
        (faulted, has_network_backend, init_count)
    }

    #[test]
    fn runaway_plugin_loop_traps_on_fuel_instead_of_hanging() {
        let path = write_wasm("infinite_loop", INFINITE_LOOP_WAT);
        let mut board = WasmBoard::from_file(&path, manifest("infinite_loop", false)).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        // Must return (the fuel budget traps the loop) rather than hang;
        // the test process itself would time out if it didn't.
        board.tick(1, &mut host);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fault_isolation_state_and_recovery_on_reset() {
        let path = write_wasm("infinite_loop_isolation", INFINITE_LOOP_WAT);
        let mut board =
            WasmBoard::from_file(&path, manifest("infinite_loop_isolation", false)).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        board.tick(1, &mut host);
        assert!(board.rt.borrow().faulted);

        board.tick(1, &mut host);
        assert!(board.rt.borrow().faulted);

        assert_eq!(board.read(0, 2, &mut host), 0xFFFF_FFFF);
        assert!(!board.int2_line());
        assert!(!board.int6_line());

        let snapshot = bincode::serialize(&board).unwrap();
        let restored: WasmBoard = bincode::deserialize(&snapshot).unwrap();
        assert!(restored.rt.borrow().faulted);

        board.reset();
        assert!(!board.rt.borrow().faulted);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fault_isolation_drops_and_reset_reopens_network_backend() {
        let path = write_wasm("network_fault_isolation", INFINITE_LOOP_WAT);
        let mut board =
            WasmBoard::from_file(&path, net_manifest("network_fault_isolation")).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        board.tick(1, &mut host);
        assert!(board.rt.borrow().store.data().net.is_none());

        board.reset();
        assert!(board.rt.borrow().store.data().net.is_some());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn faulted_restore_stays_inert_until_reset() {
        let path = write_wasm("faulted_restore", INIT_COUNTER_WAT);
        let mut board = WasmBoard::from_file(&path, net_manifest("faulted_restore")).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        board.tick(1, &mut host);
        let snapshot = bincode::serialize(&board).unwrap();
        let mut restored: WasmBoard = bincode::deserialize(&snapshot).unwrap();

        assert_eq!(runtime_probe(&restored), (true, false, 0));

        restored.reset();
        assert_eq!(runtime_probe(&restored), (false, true, 1));

        let _ = std::fs::remove_file(&path);
    }

    const DMA_TRAP_WAT: &str = r#"
        (module
          (import "env" "dma_write" (func $dma_write (param i32 i32 i32)))
          (memory (export "memory") 1)
          (func (export "write") (param $off i32) (param $size i32) (param $val i32)
            (i32.store (i32.const 0) (i32.const 16909060))
            (call $dma_write (i32.const 0) (i32.const 0) (i32.const 4))
            (unreachable))
        )
    "#;

    #[test]
    fn dma_write_rolls_back_on_plugin_trap() {
        let path = write_wasm("dma_trap", DMA_TRAP_WAT);
        let mut manifest = manifest("dma_trap", false);
        manifest.caps.dma = true;
        let mut board = WasmBoard::from_file(&path, manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        board.write(0, 4, 0, &mut host);
        assert!(board.rt.borrow().faulted);
        assert_eq!(mem.chip_ram[0..4], [0, 0, 0, 0]);
        assert_eq!(board.rt.borrow().store.data().pending_dma.len(), 0);
        assert_eq!(board.rt.borrow().store.data().pending_dma_bytes, 0);
        let _ = std::fs::remove_file(&path);
    }

    const DMA_COHERENCY_WAT: &str = r#"
        (module
          (import "env" "dma_write" (func $dma_write (param i32 i32 i32)))
          (import "env" "dma_read" (func $dma_read (param i32 i32 i32)))
          (memory (export "memory") 1)
          (func (export "write") (param $off i32) (param $size i32) (param $val i32)
            (i32.store (i32.const 0) (i32.const 16909060))
            (call $dma_write (i32.const 0) (i32.const 0) (i32.const 4))
            (call $dma_read (i32.const 0) (i32.const 16) (i32.const 4)))
        )
    "#;

    #[test]
    fn dma_read_overlays_pending_uncommitted_dma_writes() {
        let path = write_wasm("dma_coherency", DMA_COHERENCY_WAT);
        let mut manifest = manifest("dma_coherency", false);
        manifest.caps.dma = true;
        let mut board = WasmBoard::from_file(&path, manifest).unwrap();
        let mut mem = empty_memory();
        {
            let mut host = DeviceHost::new(&mut mem);
            board.write(0, 4, 0, &mut host);
        }
        assert!(!board.rt.borrow().faulted);

        let mut rt = board.rt.borrow_mut();
        let memory = rt.memory;
        let mut wasm_buf = vec![0u8; 4];
        memory.read(&mut rt.store, 16, &mut wasm_buf).unwrap();
        assert_eq!(wasm_buf, [4, 3, 2, 1]);
        drop(rt);

        assert_eq!(mem.chip_ram[0..4], [4, 3, 2, 1]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    #[ignore]
    fn bench_wasmboard_fault_isolation_performance() {
        let path = write_wasm("bench_infinite_loop", INFINITE_LOOP_WAT);
        let mut board =
            WasmBoard::from_file(&path, manifest("bench_infinite_loop", false)).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        board.tick(1, &mut host);
        assert!(board.rt.borrow().faulted);

        let iterations = 100_000;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            board.tick(1, &mut host);
        }
        let elapsed = start.elapsed();
        let nanos_per_op = elapsed.as_nanos() as f64 / iterations as f64;

        assert!(
            nanos_per_op < 1000.0,
            "faulted tick fast-path took too long: {nanos_per_op:.2} ns/tick"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    #[ignore]
    fn bench_wasmboard_normal_vs_faulted_performance() {
        let path_healthy = write_wasm("bench_healthy", COUNTER_WAT);
        let path_faulted = write_wasm("bench_faulted", INFINITE_LOOP_WAT);

        let mut board_healthy =
            WasmBoard::from_file(&path_healthy, manifest("bench_healthy", false)).unwrap();
        let mut board_faulted =
            WasmBoard::from_file(&path_faulted, manifest("bench_faulted", false)).unwrap();

        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        let iterations = 100_000;

        let start_healthy = std::time::Instant::now();
        for _ in 0..iterations {
            board_healthy.tick(1, &mut host);
        }
        let elapsed_healthy = start_healthy.elapsed();
        let nanos_healthy = elapsed_healthy.as_nanos() as f64 / iterations as f64;

        board_faulted.tick(1, &mut host);
        assert!(board_faulted.rt.borrow().faulted);

        let start_faulted = std::time::Instant::now();
        for _ in 0..iterations {
            board_faulted.tick(1, &mut host);
        }
        let elapsed_faulted = start_faulted.elapsed();
        let nanos_faulted = elapsed_faulted.as_nanos() as f64 / iterations as f64;

        assert!(nanos_faulted < nanos_healthy);

        let _ = std::fs::remove_file(&path_healthy);
        let _ = std::fs::remove_file(&path_faulted);
    }

    /// `write(off, size, len)` calls `dma_read` with a caller-supplied
    /// length instead of a fixed one, to exercise an oversized `len`.
    const DMA_OVERSIZED_LEN_WAT: &str = r#"
        (module
          (import "env" "dma_read" (func $dma_read (param i32 i32 i32)))
          (memory (export "memory") 1)
          (func (export "write") (param $off i32) (param $size i32) (param $len i32)
            (call $dma_read (i32.const 0) (i32.const 0) (local.get $len)))
        )
    "#;

    #[test]
    fn dma_read_with_oversized_len_does_not_allocate_unbounded_memory() {
        let path = write_wasm("dma_oversized", DMA_OVERSIZED_LEN_WAT);
        let mut board = WasmBoard::from_file(&path, manifest("dma_oversized", true)).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        // len = i32::MAX: without capping the allocation to the plugin's
        // actual (1-page = 64 KiB) linear memory, this would attempt a
        // ~2 GiB host allocation on every call.
        board.write(0, 0, i32::MAX as u32, &mut host);

        let _ = std::fs::remove_file(&path);
    }

    const DMA_JOURNAL_FLOOD_WAT: &str = r#"
        (module
          (import "env" "dma_write" (func $dma_write (param i32 i32 i32)))
          (memory (export "memory") 1)
          (func (export "write") (param $off i32) (param $size i32) (param $count i32)
            (local $i i32)
            (block $done
              (loop $loop
                (br_if $done (i32.ge_u (local.get $i) (local.get $count)))
                (call $dma_write (i32.const 0) (i32.const 0) (i32.const 4))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $loop)
              )
            )
          )
        )
    "#;

    #[test]
    fn dma_write_exceeding_journal_entry_limit_traps_and_faults_cleanly() {
        let path = write_wasm("dma_journal_flood", DMA_JOURNAL_FLOOD_WAT);
        let mut board = WasmBoard::from_file(&path, manifest("dma_journal_flood", true)).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        board.write(0, 0, 5000, &mut host);
        assert!(board.rt.borrow().faulted);
        assert_eq!(board.rt.borrow().store.data().pending_dma.len(), 0);
        assert_eq!(board.rt.borrow().store.data().pending_dma_bytes, 0);

        let _ = std::fs::remove_file(&path);
    }

    const DMA_JOURNAL_BYTE_FLOOD_WAT: &str = r#"
        (module
          (import "env" "dma_write" (func $dma_write (param i32 i32 i32)))
          (memory (export "memory") 1)
          (func (export "write") (param $off i32) (param $size i32) (param $count i32)
            (local $i i32)
            (block $done
              (loop $loop
                (br_if $done (i32.ge_u (local.get $i) (local.get $count)))
                (call $dma_write (i32.const 0) (i32.const 0) (i32.const 65536))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $loop)
              )
            )
          )
        )
    "#;

    #[test]
    fn dma_write_exceeding_journal_byte_limit_traps_and_faults_cleanly() {
        let path = write_wasm("dma_journal_byte_flood", DMA_JOURNAL_BYTE_FLOOD_WAT);
        let mut board =
            WasmBoard::from_file(&path, manifest("dma_journal_byte_flood", true)).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        board.write(0, 0, 300, &mut host);
        assert!(board.rt.borrow().faulted);
        assert_eq!(board.rt.borrow().store.data().pending_dma.len(), 0);
        assert_eq!(board.rt.borrow().store.data().pending_dma_bytes, 0);

        let _ = std::fs::remove_file(&path);
    }

    const DMA_INIT_WAT: &str = r#"
        (module
          (import "env" "dma_write" (func $dma_write (param i32 i32 i32)))
          (memory (export "memory") 1)
          (func (export "init")
            (call $dma_write (i32.const 0) (i32.const 0) (i32.const 4)))
        )
    "#;

    #[test]
    fn dma_write_during_init_fails_instantiation() {
        let path = write_wasm("dma_init", DMA_INIT_WAT);
        let mut manifest = manifest("dma_init", false);
        manifest.caps.dma = true;
        let res = WasmBoard::from_file(&path, manifest);
        assert!(res.is_err());
        let _ = std::fs::remove_file(&path);
    }

    const DMA_INT2_WAT: &str = r#"
        (module
          (import "env" "dma_write" (func $dma_write (param i32 i32 i32)))
          (memory (export "memory") 1)
          (func (export "int2") (result i32)
            (call $dma_write (i32.const 0) (i32.const 0) (i32.const 4))
            (i32.const 1))
          (func (export "write") (param $off i32) (param $size i32) (param $val i32))
        )
    "#;

    #[test]
    fn dma_write_during_int_line_query_traps_and_does_not_leak_dma() {
        let path = write_wasm("dma_int2", DMA_INT2_WAT);
        let mut manifest = manifest("dma_int2", false);
        manifest.caps.dma = true;
        let mut board = WasmBoard::from_file(&path, manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        assert!(!board.int2_line());
        assert!(board.rt.borrow().faulted);
        assert_eq!(board.rt.borrow().store.data().pending_dma.len(), 0);
        assert_eq!(board.rt.borrow().store.data().pending_dma_bytes, 0);

        board.write(0, 4, 123, &mut host);
        assert_eq!(mem.chip_ram[0..4], [0, 0, 0, 0]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dma_segments_handles_non_wrapping_and_wrapping_ranges() {
        let segs: Vec<_> = dma_segments(100, 4).collect();
        assert_eq!(segs, vec![(100, 104, 0)]);

        let segs: Vec<_> = dma_segments(0xFFFF_FFFE, 4).collect();
        assert_eq!(segs, vec![(0xFFFF_FFFE, 0x1_0000_0000, 0), (0, 2, 2)]);

        let segs: Vec<_> = dma_segments(0xFFFF_FFFF, 1).collect();
        assert_eq!(segs, vec![(0xFFFF_FFFF, 0x1_0000_0000, 0)]);

        let segs: Vec<_> = dma_segments(0, 0).collect();
        assert!(segs.is_empty());
    }

    const DMA_WRAP_WAT: &str = r#"
        (module
          (import "env" "dma_write" (func $dma_write (param i32 i32 i32)))
          (import "env" "dma_read" (func $dma_read (param i32 i32 i32)))
          (memory (export "memory") 1)
          (func (export "write") (param $off i32) (param $size i32) (param $val i32)
            (i32.store8 (i32.const 0) (i32.const 10))
            (i32.store8 (i32.const 1) (i32.const 20))
            (i32.store8 (i32.const 2) (i32.const 30))
            (i32.store8 (i32.const 3) (i32.const 40))
            (call $dma_write (i32.const -2) (i32.const 0) (i32.const 4))
            (call $dma_read (i32.const -1) (i32.const 16) (i32.const 3)))
        )
    "#;

    #[test]
    fn dma_read_overlays_pending_uncommitted_writes_with_wrapping_address() {
        let path = write_wasm("dma_wrap", DMA_WRAP_WAT);
        let mut manifest = manifest("dma_wrap", false);
        manifest.caps.dma = true;
        let mut board = WasmBoard::from_file(&path, manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        board.write(0, 4, 0, &mut host);
        assert!(!board.rt.borrow().faulted);

        let mut rt = board.rt.borrow_mut();
        let memory = rt.memory;
        let mut wasm_buf = vec![0u8; 3];
        memory.read(&mut rt.store, 16, &mut wasm_buf).unwrap();
        assert_eq!(wasm_buf, [20, 30, 40]);
        drop(rt);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reset_clears_plugin_memory() {
        let path = write_wasm("counter", COUNTER_WAT);
        let mut board = WasmBoard::from_file(&path, manifest("counter", false)).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        board.write(0, 2, 42, &mut host);
        assert_eq!(board.read(0, 2, &mut host), 42);
        board.reset();
        assert_eq!(board.read(0, 2, &mut host), 0);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_state_round_trip_is_byte_identical() {
        let path = write_wasm("counter", COUNTER_WAT);
        let mut board = WasmBoard::from_file(&path, manifest("counter", false)).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(0, 2, 99, &mut host);

        // Serialize, mutate the live board, then restore from the snapshot.
        let blob = bincode::serialize(&board).unwrap();
        board.write(0, 2, 7, &mut host);
        assert_eq!(board.read(0, 2, &mut host), 7);

        let restored: WasmBoard = bincode::deserialize(&blob).unwrap();
        let mut rmem = empty_memory();
        let mut rhost = DeviceHost::new(&mut rmem);
        let mut restored = restored;
        assert_eq!(restored.read(0, 2, &mut rhost), 99);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn memory_grow_round_trips_through_a_snapshot() {
        // A plugin that grows its memory then writes a marker high up; the
        // snapshot must capture the grown page count and the marker.
        let grow_wat = r#"
            (module
              (memory (export "memory") 1)
              (func (export "write") (param i32 i32 i32)
                (drop (memory.grow (i32.const 1)))
                (i32.store (i32.const 65540) (i32.const 12345)))
              (func (export "read") (param i32 i32) (result i32)
                (i32.load (i32.const 65540)))
            )
        "#;
        let path = write_wasm("grow", grow_wat);
        let mut board = WasmBoard::from_file(&path, manifest("grow", false)).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(0, 0, 0, &mut host); // grow + write marker in the new page
        assert_eq!(board.read(0, 0, &mut host), 12345);

        let blob = bincode::serialize(&board).unwrap();
        let mut restored: WasmBoard = bincode::deserialize(&blob).unwrap();
        let mut rmem = empty_memory();
        let mut rhost = DeviceHost::new(&mut rmem);
        assert_eq!(restored.read(0, 0, &mut rhost), 12345);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bundled_hostsocket_board_resolves_embedded_module_and_rom() {
        // The [hostsocket] board's module path and rom resource are
        // sentinels for embedded bytes; instantiation and a save-state
        // round trip (which reopens both by path) must resolve them
        // without touching the filesystem. Reading the stub ROM's first
        // byte back out of the board window proves the `rom` resource
        // actually reached the plugin, not just that the module compiled.
        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let rom_first = u32::from(crate::hostsocket::HOSTSOCKET_ROM[0]);
        assert_eq!(board.read(0x08, 1, &mut host), rom_first);

        let blob = bincode::serialize(&board).unwrap();
        let mut restored: WasmBoard = bincode::deserialize(&blob).unwrap();
        let mut rmem = empty_memory();
        let mut rhost = DeviceHost::new(&mut rmem);
        assert_eq!(restored.read(0x08, 1, &mut rhost), rom_first);
    }

    #[test]
    fn dma_read_import_reaches_amiga_chip_ram() {
        let path = write_wasm("dma", DMA_WAT);
        let mut board = WasmBoard::from_file(&path, manifest("dma", true)).unwrap();
        let mut mem = empty_memory();
        mem.chip_ram[0x40] = 0xDE;
        mem.chip_ram[0x41] = 0xAD;
        mem.chip_ram[0x42] = 0xBE;
        mem.chip_ram[0x43] = 0xEF;
        let mut host = DeviceHost::new(&mut mem);

        // write() triggers dma_read of 4 bytes from Amiga $40.
        board.write(0, 0, 0x40, &mut host);
        assert_eq!(board.read(0, 4, &mut host), 0xDEAD_BEEF);

        let _ = std::fs::remove_file(&path);
    }

    /// A NIC test plugin: `write(_, _, val)` transmits a 4-byte frame
    /// `[val, AA, BB, CC]`; `read` polls a frame into linear memory and returns
    /// `(len << 16) | first_byte`. With the loopback backend, what is sent
    /// comes straight back.
    const NET_WAT: &str = r#"
        (module
          (import "env" "net_send" (func $net_send (param i32 i32)))
          (import "env" "net_recv" (func $net_recv (param i32 i32) (result i32)))
          (memory (export "memory") 1)
          (func (export "write") (param $off i32) (param $size i32) (param $val i32)
            (i32.store8 (i32.const 32) (local.get $val))
            (i32.store8 (i32.const 33) (i32.const 0xAA))
            (i32.store8 (i32.const 34) (i32.const 0xBB))
            (i32.store8 (i32.const 35) (i32.const 0xCC))
            (call $net_send (i32.const 32) (i32.const 4)))
          (func (export "read") (param $off i32) (param $size i32) (result i32)
            (local $n i32)
            (local.set $n (call $net_recv (i32.const 64) (i32.const 128)))
            (i32.or
              (i32.shl (local.get $n) (i32.const 16))
              (i32.load8_u (i32.const 64))))
        )
    "#;

    /// A plugin that reads a setting and a file resource at init: `init` puts
    /// the config value's first byte at mem[256], the resource length at
    /// mem[257], and the resource's first 4 bytes at mem[258..]; `read(off)`
    /// returns mem[256 + off].
    const CONFIG_WAT: &str = r#"
        (module
          (import "env" "config_get" (func $config_get (param i32 i32 i32 i32) (result i32)))
          (import "env" "resource_len" (func $resource_len (param i32 i32) (result i32)))
          (import "env" "resource_read" (func $resource_read (param i32 i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "buffers")
          (data (i32.const 16) "rom")
          (func (export "init")
            (drop (call $config_get (i32.const 0) (i32.const 7) (i32.const 256) (i32.const 64)))
            (i32.store8 (i32.const 257) (call $resource_len (i32.const 16) (i32.const 3)))
            (drop (call $resource_read (i32.const 16) (i32.const 3) (i32.const 0) (i32.const 258) (i32.const 4))))
          (func (export "read") (param $off i32) (param $size i32) (result i32)
            (i32.load8_u (i32.add (i32.const 256) (local.get $off))))
        )
    "#;

    #[test]
    fn config_and_resource_imports_reach_the_plugin() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let rom_path = std::env::temp_dir().join(format!(
            "copperline-wasm-rom-{}-{nanos}.bin",
            std::process::id()
        ));
        std::fs::write(&rom_path, [0xCA, 0xFE, 0xBA, 0xBE]).unwrap();

        let path = write_wasm("cfg", CONFIG_WAT);
        let mut manifest = manifest("cfg", false);
        manifest.config.insert("buffers".into(), "8".into());
        manifest
            .config
            .insert("rom".into(), rom_path.to_string_lossy().into_owned());
        manifest.file_keys = vec!["rom".into()];

        let mut board = WasmBoard::from_file(&path, manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(0, 1, &mut host), 0x38); // config "buffers" = "8"
        assert_eq!(board.read(1, 1, &mut host), 4); // resource_len("rom")
        assert_eq!(board.read(2, 1, &mut host), 0xCA); // rom[0]

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&rom_path);
    }

    #[test]
    fn net_send_and_recv_round_trip_over_loopback() {
        let path = write_wasm("net", NET_WAT);
        let mut board = WasmBoard::from_file(&path, net_manifest("net")).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        // Transmit a frame; the loopback backend queues it straight back.
        board.write(0, 0, 0x5E, &mut host);
        // read() polls it: length 4, first byte 0x5E.
        assert_eq!(board.read(0, 0, &mut host), (4 << 16) | 0x5E);
        // No more frames waiting -> length 0 in the high half.
        assert_eq!(board.read(0, 0, &mut host) >> 16, 0);

        let _ = std::fs::remove_file(&path);
    }

    /// A plugin that resolves the fixed name "localhost" (offline-safe: no
    /// real network access needed, same as `net/nat/dns.rs`'s own
    /// `a_query_for_localhost_resolves_offline` test): `write` kicks off
    /// the lookup and stashes the request id at mem[0]; `read` polls it,
    /// returning the raw `resolve_poll` result, with the resolved address
    /// landing at mem[64..68] on success.
    const RESOLVE_WAT: &str = r#"
        (module
          (import "env" "resolve_start" (func $resolve_start (param i32 i32) (result i32)))
          (import "env" "resolve_poll" (func $resolve_poll (param i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 32) "localhost")
          (func (export "write") (param $off i32) (param $size i32) (param $val i32)
            (i32.store (i32.const 0) (call $resolve_start (i32.const 32) (i32.const 9))))
          (func (export "read") (param $off i32) (param $size i32) (result i32)
            (call $resolve_poll (i32.load (i32.const 0)) (i32.const 64)))
        )
    "#;

    #[test]
    fn resolve_start_and_poll_round_trip_a_real_background_lookup() {
        let path = write_wasm("resolve", RESOLVE_WAT);
        let mut board = WasmBoard::from_file(&path, resolve_manifest("resolve")).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        board.write(0, 0, 0, &mut host); // kicks off resolve_start("localhost")

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let result = loop {
            // board.read() returns the raw register value as u32; resolve_poll's
            // i32 sentinels round-trip through it bit-for-bit, so compare against
            // the same bit pattern rather than sign-extending back to i32.
            let r = board.read(0, 0, &mut host);
            if r != (-2i32) as u32 {
                break r;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "resolve_poll never left pending"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(result, 0, "localhost must resolve");

        // The address landed in the plugin's own linear memory (not Amiga
        // memory), which `read()`'s own return value doesn't expose -- a
        // save-state snapshot is the simplest way this test harness has to
        // inspect it directly, same as `memory_grow_round_trips_through_a_
        // snapshot` above does for its own marker value.
        let blob = bincode::serialize(&board).unwrap();
        let snap: WasmBoardState = bincode::deserialize(&blob).unwrap();
        assert_eq!(&snap.bytes[64..68], &[127, 0, 0, 1]);

        let _ = std::fs::remove_file(&path);
    }

    /// A host-socket test plugin: `write(_, _, port)` opens an AF_INET/
    /// SOCK_STREAM host socket and starts a non-blocking connect to
    /// 127.0.0.1:`port`. `read` drives the rest of the exchange one step
    /// per call -- once `sock_poll` reports writable it sends the 4-byte
    /// payload "PING" (once, via the mem[8] latch), then every call tries
    /// `sock_recv` into mem[64..68]. It returns -100 (a sentinel outside
    /// the real BSD errno range this ABI uses) while still waiting on
    /// either step, so the test harness's poll loop can tell "not ready
    /// yet" apart from a genuine `sock_recv` errno.
    const HOST_SOCKET_WAT: &str = r#"
        (module
          (import "env" "sock_open" (func $sock_open (param i32 i32) (result i32)))
          (import "env" "sock_connect" (func $sock_connect (param i32 i32 i32) (result i32)))
          (import "env" "sock_send" (func $sock_send (param i32 i32 i32) (result i32)))
          (import "env" "sock_recv" (func $sock_recv (param i32 i32 i32) (result i32)))
          (import "env" "sock_poll" (func $sock_poll (param i32) (result i32)))
          (memory (export "memory") 1)
          (func (export "write") (param $off i32) (param $size i32) (param $port i32)
            (local $h i32)
            (local.set $h (call $sock_open (i32.const 2) (i32.const 1)))
            (i32.store (i32.const 0) (local.get $h))
            (drop (call $sock_connect (local.get $h) (i32.const 0x7F000001) (local.get $port))))
          (func (export "read") (param $off i32) (param $size i32) (result i32)
            (local $h i32) (local $mask i32) (local $n i32)
            (local.set $h (i32.load (i32.const 0)))
            (block $sent
              (br_if $sent (i32.load8_u (i32.const 8)))
              (local.set $mask (call $sock_poll (local.get $h)))
              (br_if $sent (i32.eqz (i32.and (local.get $mask) (i32.const 2))))
              (i32.store8 (i32.const 32) (i32.const 0x50)) ;; 'P'
              (i32.store8 (i32.const 33) (i32.const 0x49)) ;; 'I'
              (i32.store8 (i32.const 34) (i32.const 0x4E)) ;; 'N'
              (i32.store8 (i32.const 35) (i32.const 0x47)) ;; 'G'
              (drop (call $sock_send (local.get $h) (i32.const 32) (i32.const 4)))
              (i32.store8 (i32.const 8) (i32.const 1))
              (return (i32.const -100)))
            (local.set $n (call $sock_recv (local.get $h) (i32.const 64) (i32.const 4)))
            (if (i32.eq (local.get $n) (i32.const -35)) ;; -EAGAIN: nothing yet
              (then (return (i32.const -100))))
            (local.get $n))
        )
    "#;

    #[test]
    fn sock_open_connect_send_recv_round_trip_a_real_tcp_echo() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4];
            std::io::Read::read_exact(&mut stream, &mut buf).unwrap();
            assert_eq!(&buf, b"PING");
            std::io::Write::write_all(&mut stream, b"PONG").unwrap();
        });

        let path = write_wasm("host_socket", HOST_SOCKET_WAT);
        let mut board = WasmBoard::from_file(&path, host_sockets_manifest("host_socket")).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);

        board.write(0, 0, port as u32, &mut host); // sock_open + sock_connect

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let result = loop {
            let r = board.read(0, 0, &mut host);
            if r != (-100i32) as u32 {
                break r;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "host-socket echo never completed"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(result, 4, "sock_recv must return the 4 bytes of \"PONG\"");

        let blob = bincode::serialize(&board).unwrap();
        let snap: WasmBoardState = bincode::deserialize(&blob).unwrap();
        assert_eq!(&snap.bytes[64..68], b"PONG");

        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    // -- HostSocket board: host-socket backend, driven through the real
    // dispatch/RPC protocol --------------------------------------------
    //
    // The test above exercises `sock_*` in isolation via a hand-written
    // WAT plugin. This one instead loads the REAL, compiled
    // `crates/hostsocket-plugin` module (the same artifact
    // `assets/hostsocket/hostsocket_plugin.wasm` embeds) with
    // `transport = "host"` selected, and drives it exactly the way the
    // real guest stub ROM would: stage an argblock in "Amiga" memory,
    // write REG_ARGPTR then REG_CALL (the RPC doorbell), and read
    // REG_RESULT back -- proving `crates/hostsocket-plugin`'s own
    // do_socket/do_connect_host/do_send_host/do_recv_host/do_close
    // routing (not just the raw host imports) against a real TCP peer.
    // See ../guest/hostsocket/hostsocket_board.h for the register/call
    // layout these constants mirror (kept in sync by hand, the same
    // convention that header's own comment and `board_layout_matches_
    // guest_header` in the plugin crate already follow).

    const HS_REG_ARGPTR: u32 = 0x7C00;
    const HS_REG_CALL: u32 = 0x7C04;
    const HS_REG_RESULT: u32 = 0x7C08;
    const HS_CALL_SOCKET: i32 = 0;
    const HS_CALL_CONNECT: i32 = 1;
    const HS_CALL_SEND: i32 = 2;
    const HS_CALL_RECV: i32 = 3;
    const HS_CALL_CLOSESOCKET: i32 = 4;
    const HS_CALL_BIND: i32 = 10;
    const HS_CALL_LISTEN: i32 = 11;
    const HS_CALL_ACCEPT: i32 = 12;
    const HS_CALL_WAITSELECT: i32 = 9;
    const HS_CALL_SENDTO: i32 = 13;
    const HS_CALL_RECVFROM: i32 = 14;
    const HS_CALL_GETSOCKNAME: i32 = 18;
    const HS_CALL_GETPEERNAME: i32 = 19;
    const HS_CALL_ERRNO: i32 = 8;
    const HS_CALL_SETSOCKOPT: i32 = 16;
    const HS_CALL_GETSOCKOPT: i32 = 17;
    const HS_SOL_SOCKET: i32 = 0xFFFF;
    const HS_IPPROTO_TCP: i32 = 6;
    const HS_SO_ERROR: i32 = 0x1007;
    const HS_SO_RCVBUF: i32 = 0x1002;
    const HS_SO_LINGER: i32 = 0x0080;
    const HS_TCP_NODELAY: i32 = 0x01;
    const HS_CALL_DUP2SOCKET: i32 = 20;
    const HS_CALL_OBTAINSOCKET: i32 = 36;
    const HS_CALL_RELEASESOCKET: i32 = 37;
    const HS_CALL_RELEASECOPYOFSOCKET: i32 = 38;
    const HS_CALL_SHUTDOWN: i32 = 15;
    const HS_CALL_IOCTLSOCKET: i32 = 6;
    const HS_CALL_SENDMSG: i32 = 33;
    const HS_CALL_RECVMSG: i32 = 34;
    const HS_FIONREAD: i32 = 0x4004667F;
    const HS_MSG_PEEK: i32 = 0x2;
    const HS_MSG_OOB: i32 = 0x1;
    const HS_AF_INET: i32 = 2;
    const HS_SOCK_STREAM: i32 = 1;
    const HS_RES_PENDING: i32 = -2;
    // Scratch "Amiga memory" addresses for this test's own argblock/
    // sockaddr/send/recv buffers -- kept well within `empty_memory`'s
    // 0x1000-byte chip_ram, with generous gaps between them.
    const HS_ARGBLOCK_ADDR: u32 = 0x100;

    /// Stages a `task` + up to 7 args as the plugin's own 32-byte
    /// argblock (big-endian LONGs, matching `Board::dispatch`'s own
    /// `arg(i)` reader) at [`HS_ARGBLOCK_ADDR`], rings the RPC doorbell
    /// (REG_ARGPTR then REG_CALL), and returns REG_RESULT.
    fn hs_call(
        board: &mut WasmBoard,
        host: &mut DeviceHost,
        task: u32,
        call: i32,
        args: [i32; 7],
    ) -> i32 {
        let mut block = [0u8; 32];
        block[0..4].copy_from_slice(&(task as i32).to_be_bytes());
        for (i, a) in args.iter().enumerate() {
            block[(i + 1) * 4..(i + 2) * 4].copy_from_slice(&a.to_be_bytes());
        }
        let base = HS_ARGBLOCK_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 32].copy_from_slice(&block);

        board.write(HS_REG_ARGPTR, 4, HS_ARGBLOCK_ADDR, host);
        board.write(HS_REG_CALL, 4, call as u32, host);
        board.read(HS_REG_RESULT, 4, host) as i32
    }

    /// Repeats `hs_call` until it stops returning [`HS_RES_PENDING`], for
    /// the same reason the real guest's own blocking-doorbell loop does:
    /// `do_connect_host`/`do_send_host`/`do_recv_host` register a wait
    /// and expect to be re-invoked with the identical args until the
    /// underlying host socket operation completes.
    fn hs_call_blocking(
        board: &mut WasmBoard,
        host: &mut DeviceHost,
        task: u32,
        call: i32,
        args: [i32; 7],
        deadline: std::time::Instant,
    ) -> i32 {
        loop {
            let r = hs_call(board, host, task, call, args);
            if r != HS_RES_PENDING {
                return r;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "hostsocket-plugin call {call} never left RES_PENDING"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn hostsocket_plugin_host_backend_round_trips_a_real_tcp_echo_via_dispatch() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4];
            std::io::Read::read_exact(&mut stream, &mut buf).unwrap();
            assert_eq!(&buf, b"PING");
            std::io::Write::write_all(&mut stream, b"PONG").unwrap();
        });

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000; // an arbitrary, never-dereferenced "task pointer"
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        // socket(AF_INET=2, SOCK_STREAM=1, 0)
        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [2, 1, 0, 0, 0, 0, 0],
        );
        assert!(
            fd > 0,
            "do_socket (host backend) should return a fd, got {fd}"
        );

        // Stage a sockaddr_in for 127.0.0.1:port at a second scratch
        // address (parse_sockaddr only reads bytes [2..8): port, then
        // the 4 IP octets).
        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);

        // connect(fd, &sockaddr, 16)
        let rc = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_CONNECT,
            [fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(rc, 0, "connect (host backend) should succeed");

        // send(fd, "PING", 4, 0)
        const SEND_BUF_ADDR: u32 = 0x300;
        let base = SEND_BUF_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(b"PING");
        let sent = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_SEND,
            [fd, SEND_BUF_ADDR as i32, 4, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(sent, 4, "send (host backend) should queue all 4 bytes");

        // recv(fd, buf, 4, 0)
        const RECV_BUF_ADDR: u32 = 0x400;
        let received = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_RECV,
            [fd, RECV_BUF_ADDR as i32, 4, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(
            received, 4,
            "recv (host backend) should return \"PONG\"'s 4 bytes"
        );
        let base = RECV_BUF_ADDR as usize;
        assert_eq!(&host.memory_mut().chip_ram[base..base + 4], b"PONG");

        // CloseSocket(fd)
        let closed = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [fd, 0, 0, 0, 0, 0, 0],
        );
        assert_eq!(closed, 0, "CloseSocket (host backend) should succeed");

        server.join().unwrap();
    }

    #[test]
    fn hostsocket_plugin_host_backend_bind_listen_accept_serves_a_real_tcp_client() {
        // A free port, released immediately so the dispatch-level bind()
        // below can claim the same one -- same "ask the OS, then reuse
        // it" pattern the other host-backend tests use, just via bind()
        // instead of a listener the test itself keeps open.
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        // socket(AF_INET=2, SOCK_STREAM=1, 0)
        let listen_fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [2, 1, 0, 0, 0, 0, 0],
        );
        assert!(listen_fd > 0);

        // bind(listen_fd, 127.0.0.1:port, 16)
        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);
        let rc = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_BIND,
            [listen_fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
        );
        assert_eq!(rc, 0, "bind (host backend) should succeed");

        // listen(listen_fd, 5)
        let rc = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_LISTEN,
            [listen_fd, 5, 0, 0, 0, 0, 0],
        );
        assert_eq!(rc, 0, "listen (host backend) should succeed");

        let client = std::thread::spawn(move || {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            std::io::Write::write_all(&mut stream, b"PING").unwrap();
            let mut buf = [0u8; 4];
            std::io::Read::read_exact(&mut stream, &mut buf).unwrap();
            assert_eq!(&buf, b"PONG");
        });

        // accept(listen_fd, &addr, &addrlen), addrlen in = 16
        const ADDR_OUT_ADDR: u32 = 0x500;
        const LEN_PTR_ADDR: u32 = 0x520;
        let base = LEN_PTR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(&16i32.to_be_bytes());
        let conn_fd = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_ACCEPT,
            [
                listen_fd,
                ADDR_OUT_ADDR as i32,
                LEN_PTR_ADDR as i32,
                0,
                0,
                0,
                0,
            ],
            deadline,
        );
        assert!(
            conn_fd > 0,
            "accept (host backend) should return a new fd, got {conn_fd}"
        );

        // accept() must have reported the real client address: AF_INET at
        // +0, port at +2, IP at +4 (see write_sockaddr_out's own layout).
        let base = ADDR_OUT_ADDR as usize;
        let reported = &host.memory_mut().chip_ram[base..base + 8];
        assert_eq!(
            &reported[4..8],
            &[127, 0, 0, 1],
            "accept should report the client's IP"
        );
        let reported_port = u16::from_be_bytes([reported[2], reported[3]]);
        assert_ne!(
            reported_port, 0,
            "accept should report the client's real port"
        );

        // recv(conn_fd, buf, 4, 0) -> "PING"
        const RECV_BUF_ADDR: u32 = 0x600;
        let received = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_RECV,
            [conn_fd, RECV_BUF_ADDR as i32, 4, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(received, 4);
        let base = RECV_BUF_ADDR as usize;
        assert_eq!(&host.memory_mut().chip_ram[base..base + 4], b"PING");

        // send(conn_fd, "PONG", 4, 0)
        const SEND_BUF_ADDR: u32 = 0x700;
        let base = SEND_BUF_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(b"PONG");
        let sent = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_SEND,
            [conn_fd, SEND_BUF_ADDR as i32, 4, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(sent, 4);

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [conn_fd, 0, 0, 0, 0, 0, 0],
        );
        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [listen_fd, 0, 0, 0, 0, 0, 0],
        );

        client.join().unwrap();
    }

    /// Exercises the `sock_poll`-backed `scan_select`/`WaitKind::Select`
    /// path this same host backend now supports for a real, non-blocking
    /// `poll(2)` readiness check (previously `WaitSelect()` silently never
    /// reported a host-backed fd ready at all): a host-backed *listening*
    /// socket, `WaitSelect()`ed on for read-readiness the same way
    /// bsdsocktest's own NULL-timeout test does for a smoltcp one, must
    /// report ready exactly when a real client connects -- not before,
    /// and not only via a direct `accept()` call.
    /// POSIX `poll(2)` and Windows `WSAPoll` both define listener
    /// read-readiness as a queued connection, so this covers every supported
    /// native desktop target rather than being a Unix-only accommodation.
    #[cfg(any(unix, windows))]
    #[test]
    fn hostsocket_plugin_host_backend_waitselect_reports_accept_readiness() {
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        let listen_fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [2, 1, 0, 0, 0, 0, 0],
        );
        assert!(listen_fd > 0);

        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);
        assert_eq!(
            hs_call(
                &mut board,
                &mut host,
                task,
                HS_CALL_BIND,
                [listen_fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            ),
            0
        );
        assert_eq!(
            hs_call(
                &mut board,
                &mut host,
                task,
                HS_CALL_LISTEN,
                [listen_fd, 5, 0, 0, 0, 0, 0],
            ),
            0
        );

        // A client connecting is the only thing that should ever make
        // this WaitSelect() stop blocking -- started only after bind()/
        // listen() above, so an early false-positive would mean the very
        // first hs_call_blocking retry already saw it ready.
        let client = std::thread::spawn(move || {
            std::net::TcpStream::connect(("127.0.0.1", port)).unwrap()
            // Held until this thread's own return value drops (after
            // `client.join()` below), keeping the connection open for the
            // whole WaitSelect/accept sequence.
        });

        // WaitSelect(nfds = listen_fd + 1, &readfds, &writefds, NULL, NULL
        // (block indefinitely), NULL): readfds/writefds are real Amiga
        // addresses of 4-byte bitmasks (bit N = fd N), per
        // `do_wait_select`'s own signature -- not raw values.
        const READ_MASK_ADDR: u32 = 0x800;
        const WRITE_MASK_ADDR: u32 = 0x810;
        let read_bit = 1u32 << listen_fd;
        let base = READ_MASK_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(&read_bit.to_be_bytes());
        let base = WRITE_MASK_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(&0u32.to_be_bytes());

        let ready_count = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_WAITSELECT,
            [
                listen_fd + 1,
                READ_MASK_ADDR as i32,
                WRITE_MASK_ADDR as i32,
                0,
                0,
                0,
                0,
            ],
            deadline,
        );
        assert_eq!(
            ready_count, 1,
            "WaitSelect should report exactly the listener ready for read"
        );
        let base = READ_MASK_ADDR as usize;
        let ready_read_mask = u32::from_be_bytes(
            host.memory_mut().chip_ram[base..base + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            ready_read_mask, read_bit,
            "the listener's own fd bit should be the one reported ready"
        );

        // Drain the pending connection so accept() doesn't itself block.
        let conn_fd = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_ACCEPT,
            [listen_fd, 0, 0, 0, 0, 0, 0],
            deadline,
        );
        assert!(conn_fd > 0);
        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [conn_fd, 0, 0, 0, 0, 0, 0],
        );
        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [listen_fd, 0, 0, 0, 0, 0, 0],
        );

        let stream = client.join().unwrap();
        drop(stream);
    }

    /// A connected-UDP round trip through the host backend: `socket(...,
    /// SOCK_DGRAM, ...)` + `connect()` (a real, immediate BSD UDP
    /// `connect()` just records a default peer -- no handshake, unlike
    /// TCP) + plain `send()`/`recv()` against that peer, exactly the same
    /// dispatch calls the TCP round-trip test above uses. The peer side is
    /// a raw `std::net::UdpSocket`, which -- like real UDP -- learns the
    /// dispatch-driven socket's own (OS-assigned, ephemeral) address
    /// purely from the first datagram's own sender address, with no
    /// `bind()`/`getsockname()` involved on either end.
    #[test]
    fn hostsocket_plugin_host_backend_udp_connect_send_recv_round_trips_a_real_datagram() {
        let peer = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = peer.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut buf = [0u8; 4];
            let (n, from) = peer.recv_from(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"PING");
            peer.send_to(b"PONG", from).unwrap();
        });

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        // socket(AF_INET=2, SOCK_DGRAM=2, 0)
        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [2, 2, 0, 0, 0, 0, 0],
        );
        assert!(
            fd > 0,
            "do_socket (host backend, UDP) should return a fd, got {fd}"
        );

        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);

        // connect(fd, &sockaddr, 16) -- records the default peer; a real
        // UDP connect() always completes on the very first call (no
        // handshake), so this shouldn't need more than one hs_call, but
        // hs_call_blocking is used anyway for uniformity with every other
        // host-backend test.
        let rc = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_CONNECT,
            [fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(
            rc, 0,
            "connect (host backend, UDP) should succeed immediately"
        );

        const SEND_BUF_ADDR: u32 = 0x300;
        let base = SEND_BUF_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(b"PING");
        let sent = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_SEND,
            [fd, SEND_BUF_ADDR as i32, 4, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(
            sent, 4,
            "send (host backend, UDP) should queue the whole datagram"
        );

        const RECV_BUF_ADDR: u32 = 0x400;
        let received = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_RECV,
            [fd, RECV_BUF_ADDR as i32, 4, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(
            received, 4,
            "recv (host backend, UDP) should return \"PONG\"'s 4 bytes"
        );
        let base = RECV_BUF_ADDR as usize;
        assert_eq!(&host.memory_mut().chip_ram[base..base + 4], b"PONG");

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [fd, 0, 0, 0, 0, 0, 0],
        );

        server.join().unwrap();
    }

    /// UDP `sendto()`/`recvfrom()` through the host backend, with no prior
    /// `connect()` at all: an explicit destination on the way out
    /// (`sendto`), and the sender's real address reported on the way back
    /// (`recvfrom`) -- unlike the connected-socket round trip above, the
    /// peer here never learns anything about the dispatch-driven socket in
    /// advance either; both directions carry an explicit address on every
    /// call, the same as real BSD UDP servers overwhelmingly use.
    #[test]
    fn hostsocket_plugin_host_backend_udp_sendto_recvfrom_round_trips_with_explicit_addresses() {
        let peer = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let peer_port = peer.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut buf = [0u8; 4];
            let (n, from) = peer.recv_from(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"PING");
            peer.send_to(b"PONG", from).unwrap();
        });

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        // socket(AF_INET=2, SOCK_DGRAM=2, 0) -- no bind()/connect() at all.
        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [2, 2, 0, 0, 0, 0, 0],
        );
        assert!(fd > 0);

        const TO_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&peer_port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = TO_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);

        const SEND_BUF_ADDR: u32 = 0x300;
        let base = SEND_BUF_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(b"PING");

        // sendto(fd, "PING", 4, flags(unused, arg4), &to, tolen)
        let sent = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_SENDTO,
            [fd, SEND_BUF_ADDR as i32, 4, 0, TO_ADDR as i32, 16, 0],
            deadline,
        );
        assert_eq!(
            sent, 4,
            "sendto (host backend) should queue the whole datagram"
        );

        // recvfrom(fd, buf, 4, flags(unused, arg4), &from, &fromlen)
        const RECV_BUF_ADDR: u32 = 0x400;
        const FROM_ADDR: u32 = 0x500;
        const FROM_LEN_ADDR: u32 = 0x520;
        let base = FROM_LEN_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(&16i32.to_be_bytes());
        let received = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_RECVFROM,
            [
                fd,
                RECV_BUF_ADDR as i32,
                4,
                0,
                FROM_ADDR as i32,
                FROM_LEN_ADDR as i32,
                0,
            ],
            deadline,
        );
        assert_eq!(
            received, 4,
            "recvfrom (host backend) should return \"PONG\"'s 4 bytes"
        );
        let base = RECV_BUF_ADDR as usize;
        assert_eq!(&host.memory_mut().chip_ram[base..base + 4], b"PONG");

        // The reported sender must be the real peer: 127.0.0.1:peer_port,
        // same sockaddr_in layout write_sockaddr_out uses (AF_INET at +0,
        // port at +2, IP at +4).
        let base = FROM_ADDR as usize;
        let reported = &host.memory_mut().chip_ram[base..base + 8];
        assert_eq!(&reported[4..8], &[127, 0, 0, 1]);
        let reported_port = u16::from_be_bytes([reported[2], reported[3]]);
        assert_eq!(reported_port, peer_port);

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [fd, 0, 0, 0, 0, 0, 0],
        );

        server.join().unwrap();
    }

    /// `getsockname()` resolving a `bind()` to port `0`, exercised the
    /// only way that's actually meaningful: the port it reports is then
    /// used to make a *real* connection, not just checked for being
    /// nonzero. This is the scenario `do_bind_host`'s own comment points
    /// to `do_getsockname_host` for -- the host backend never caches a
    /// resolved ephemeral port anywhere; `sock_local_addr` is asked fresh.
    #[test]
    fn hostsocket_plugin_host_backend_getsockname_resolves_a_bind_to_port_zero() {
        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        let listen_fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [2, 1, 0, 0, 0, 0, 0],
        );
        assert!(listen_fd > 0);

        // bind(listen_fd, 127.0.0.1:0, 16) -- port 0: let the OS choose.
        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);
        assert_eq!(
            hs_call(
                &mut board,
                &mut host,
                task,
                HS_CALL_BIND,
                [listen_fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            ),
            0
        );
        assert_eq!(
            hs_call(
                &mut board,
                &mut host,
                task,
                HS_CALL_LISTEN,
                [listen_fd, 5, 0, 0, 0, 0, 0],
            ),
            0
        );

        // getsockname(listen_fd, &addr, &addrlen), addrlen in = 16
        const NAME_ADDR: u32 = 0x600;
        const NAME_LEN_ADDR: u32 = 0x620;
        let base = NAME_LEN_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(&16i32.to_be_bytes());
        let rc = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_GETSOCKNAME,
            [
                listen_fd,
                NAME_ADDR as i32,
                NAME_LEN_ADDR as i32,
                0,
                0,
                0,
                0,
            ],
        );
        assert_eq!(rc, 0, "getsockname (host backend) should succeed");
        let base = NAME_ADDR as usize;
        let reported = &host.memory_mut().chip_ram[base..base + 8];
        assert_eq!(&reported[4..8], &[127, 0, 0, 1]);
        let resolved_port = u16::from_be_bytes([reported[2], reported[3]]);
        assert_ne!(
            resolved_port, 0,
            "getsockname should resolve the real port bind()-to-0 picked"
        );

        // Prove it's the *real* port: connect a real client to exactly it.
        let client = std::thread::spawn(move || {
            std::net::TcpStream::connect(("127.0.0.1", resolved_port)).unwrap()
        });
        let conn_fd = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_ACCEPT,
            [listen_fd, 0, 0, 0, 0, 0, 0],
            deadline,
        );
        assert!(conn_fd > 0, "accept should succeed on the resolved port");

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [conn_fd, 0, 0, 0, 0, 0, 0],
        );
        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [listen_fd, 0, 0, 0, 0, 0, 0],
        );

        let stream = client.join().unwrap();
        drop(stream);
    }

    /// `getpeername()` after a real `connect()`: reports the real peer's
    /// address, not a placeholder -- and fails cleanly (`ENOTCONN`, via a
    /// real host `getpeername()` on an unconnected socket) when there
    /// isn't one, exercised on a second, never-connected fd.
    #[test]
    fn hostsocket_plugin_host_backend_getpeername_reports_the_real_peer() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let _ = listener.accept().unwrap();
        });

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [2, 1, 0, 0, 0, 0, 0],
        );
        assert!(fd > 0);

        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);
        let rc = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_CONNECT,
            [fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(rc, 0);

        const NAME_ADDR: u32 = 0x600;
        const NAME_LEN_ADDR: u32 = 0x620;
        let base = NAME_LEN_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(&16i32.to_be_bytes());
        let rc = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_GETPEERNAME,
            [fd, NAME_ADDR as i32, NAME_LEN_ADDR as i32, 0, 0, 0, 0],
        );
        assert_eq!(
            rc, 0,
            "getpeername (host backend) should succeed after connect"
        );
        let base = NAME_ADDR as usize;
        let reported = &host.memory_mut().chip_ram[base..base + 8];
        assert_eq!(&reported[4..8], &[127, 0, 0, 1]);
        let reported_port = u16::from_be_bytes([reported[2], reported[3]]);
        assert_eq!(
            reported_port, port,
            "getpeername should report the real peer's port"
        );

        // A second, never-connected fd should fail cleanly instead of
        // reporting a stale or placeholder address.
        let idle_fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [2, 1, 0, 0, 0, 0, 0],
        );
        assert!(idle_fd > 0);
        let rc = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_GETPEERNAME,
            [idle_fd, NAME_ADDR as i32, NAME_LEN_ADDR as i32, 0, 0, 0, 0],
        );
        assert_eq!(rc, -1, "getpeername on an unconnected fd should fail");

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [idle_fd, 0, 0, 0, 0, 0, 0],
        );
        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [fd, 0, 0, 0, 0, 0, 0],
        );

        server.join().unwrap();
    }

    /// Host sockets are a host resource, not part of the plugin's own
    /// linear-memory snapshot (see `HostCtx.sockets`'s own doc comment in
    /// this file) -- a save-state restore starts with none open, so a
    /// fd the guest still remembers from before the snapshot must fail
    /// clean (`EBADF`) rather than resurrect a connection or panic. The
    /// plugin needs no code of its own for this: `sock_send`'s own
    /// "unknown handle" branch already reports `-EBADF`, so this is
    /// really testing that nothing *breaks* that default, not a feature
    /// that had to be built.
    #[test]
    fn hostsocket_plugin_host_backend_save_state_restore_tears_open_sockets_cleanly() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let _ = listener.accept().unwrap();
            // Held for the whole test so the connection stays up to the
            // point of the snapshot -- proving the fd was genuinely alive
            // before the restore, not just never connected.
            std::thread::sleep(std::time::Duration::from_millis(200));
        });

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [2, 1, 0, 0, 0, 0, 0],
        );
        assert!(fd > 0);
        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);
        let rc = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_CONNECT,
            [fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(
            rc, 0,
            "the fd must genuinely be connected before the snapshot"
        );

        // Snapshot, then restore into a fresh board -- `restored`'s own
        // `sockets` map starts empty (see the doc comment above).
        let blob = bincode::serialize(&board).unwrap();
        let mut restored: WasmBoard = bincode::deserialize(&blob).unwrap();

        const SEND_BUF_ADDR: u32 = 0x300;
        let base = SEND_BUF_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(b"PING");
        let rc = hs_call(
            &mut restored,
            &mut host,
            task,
            HS_CALL_SEND,
            [fd, SEND_BUF_ADDR as i32, 4, 0, 0, 0, 0],
        );
        assert_eq!(
            rc, -1,
            "send() on a pre-restore fd must fail, not silently succeed or hang"
        );
        const EBADF: i32 = 9; // BSD errno, same numbering this crate's own table uses
        let errno = hs_call(&mut restored, &mut host, task, HS_CALL_ERRNO, [0; 7]);
        assert_eq!(errno, EBADF, "the failure must be reported as EBADF");

        // The restored board itself must still be otherwise healthy: a
        // brand new socket (unrelated to the torn one) should work fine.
        let fresh_fd = hs_call(
            &mut restored,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [2, 1, 0, 0, 0, 0, 0],
        );
        assert!(
            fresh_fd > 0,
            "a restored board must still be able to open new sockets"
        );

        // The real regression this guards: the restored board's own host
        // handle counter used to restart at 0 (a fresh `HostCtx`, see
        // `WasmBoardState::next_socket_id`'s own comment), the exact same
        // value the very first socket ever opened (the now-torn `fd`
        // above) originally got. `fresh_fd` above is real evidence the
        // counter *was* reused this way pre-fix -- but the collision only
        // actually manifests once the *old* fd is touched again
        // afterward: connect `fresh_fd` to a second, distinct listener and
        // confirm `fd` still cleanly fails EBADF rather than silently
        // operating on `fresh_fd`'s own real connection (the failure mode
        // this test would not have caught otherwise, since everything
        // above happens while the restored socket table is still empty).
        let listener2 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port2 = listener2.local_addr().unwrap().port();
        let server2 = std::thread::spawn(move || {
            let (mut stream, _) = listener2.accept().unwrap();
            let mut buf = [0u8; 4];
            std::io::Read::read_exact(&mut stream, &mut buf).unwrap();
            assert_eq!(
                &buf, b"PONG",
                "fresh_fd's own real connection must see its own send"
            );
        });
        let mut sockaddr2 = [0u8; 16];
        sockaddr2[2..4].copy_from_slice(&port2.to_be_bytes());
        sockaddr2[4..8].copy_from_slice(&[127, 0, 0, 1]);
        const SOCKADDR2_ADDR: u32 = 0x400;
        let base = SOCKADDR2_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr2);
        let rc = hs_call_blocking(
            &mut restored,
            &mut host,
            task,
            HS_CALL_CONNECT,
            [fresh_fd, SOCKADDR2_ADDR as i32, 16, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(rc, 0, "fresh_fd must connect for real");

        const SEND2_BUF_ADDR: u32 = 0x500;
        let base = SEND2_BUF_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(b"PONG");
        let rc = hs_call(
            &mut restored,
            &mut host,
            task,
            HS_CALL_SEND,
            [fresh_fd, SEND2_BUF_ADDR as i32, 4, 0, 0, 0, 0],
        );
        assert_eq!(rc, 4, "fresh_fd's own real send must succeed");
        server2.join().unwrap();

        // Now the actual regression check: the torn pre-restore fd, tried
        // again now that a real socket exists at whatever host handle it
        // used to hold.
        let rc = hs_call(
            &mut restored,
            &mut host,
            task,
            HS_CALL_SEND,
            [fd, SEND_BUF_ADDR as i32, 4, 0, 0, 0, 0],
        );
        assert_eq!(
            rc, -1,
            "the pre-restore fd must still fail even after a new socket exists"
        );
        let errno = hs_call(&mut restored, &mut host, task, HS_CALL_ERRNO, [0; 7]);
        assert_eq!(
            errno, EBADF,
            "and specifically EBADF, not silently operating on fresh_fd's real socket"
        );

        server.join().unwrap();
    }

    /// `setsockopt()`/`getsockopt()` through the host backend: SO_RCVBUF
    /// and TCP_NODELAY reach the *real* host socket (not a plugin-side
    /// echo -- proven by checking a fresh socket's real kernel-assigned
    /// SO_RCVBUF default, which is always comfortably larger than
    /// anything a zeroed roundtrip-storage struct's own default could
    /// produce), while SO_LINGER stays plugin-side roundtrip storage
    /// (see `HostFdSlot::opts`'s own comment on why) and still round
    /// trips correctly.
    #[test]
    fn hostsocket_plugin_host_backend_setsockopt_getsockopt_reach_the_real_socket() {
        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;

        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [2, 1, 0, 0, 0, 0, 0],
        );
        assert!(fd > 0);

        const OPTVAL_ADDR: u32 = 0x200;
        const OPTLEN_ADDR: u32 = 0x220;

        let getopt = |board: &mut WasmBoard,
                      host: &mut DeviceHost,
                      level: i32,
                      optname: i32,
                      cap: i32|
         -> i32 {
            let base = OPTLEN_ADDR as usize;
            host.memory_mut().chip_ram[base..base + 4].copy_from_slice(&cap.to_be_bytes());
            hs_call(
                board,
                host,
                task,
                HS_CALL_GETSOCKOPT,
                [
                    fd,
                    level,
                    optname,
                    OPTVAL_ADDR as i32,
                    OPTLEN_ADDR as i32,
                    0,
                    0,
                ],
            )
        };
        let read_i32 = |host: &mut DeviceHost| -> i32 {
            let base = OPTVAL_ADDR as usize;
            i32::from_be_bytes(
                host.memory_mut().chip_ram[base..base + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        let setopt_i32 = |board: &mut WasmBoard,
                          host: &mut DeviceHost,
                          level: i32,
                          optname: i32,
                          value: i32|
         -> i32 {
            let base = OPTVAL_ADDR as usize;
            host.memory_mut().chip_ram[base..base + 4].copy_from_slice(&value.to_be_bytes());
            hs_call(
                board,
                host,
                task,
                HS_CALL_SETSOCKOPT,
                [fd, level, optname, OPTVAL_ADDR as i32, 4, 0, 0],
            )
        };

        // SO_RCVBUF's real kernel-assigned default, before any setsockopt
        // call at all -- a naive plugin-side echo/roundtrip struct would
        // default to 0 (Rust's own zeroed `Default`), so a large nonzero
        // value here is only possible via a real `getsockopt(2)`.
        assert_eq!(
            getopt(&mut board, &mut host, HS_SOL_SOCKET, HS_SO_RCVBUF, 4),
            0
        );
        let default_rcvbuf = read_i32(&mut host);
        assert!(
            default_rcvbuf > 1024,
            "a fresh socket's real SO_RCVBUF default should be well over 1024, got {default_rcvbuf}"
        );

        // Set it, then read back what the kernel actually applied (it may
        // round up, but never down).
        assert_eq!(
            setopt_i32(&mut board, &mut host, HS_SOL_SOCKET, HS_SO_RCVBUF, 65536),
            0
        );
        assert_eq!(
            getopt(&mut board, &mut host, HS_SOL_SOCKET, HS_SO_RCVBUF, 4),
            0
        );
        let new_rcvbuf = read_i32(&mut host);
        assert!(
            new_rcvbuf >= 65536,
            "SO_RCVBUF after setsockopt should be at least what was requested, got {new_rcvbuf}"
        );

        // TCP_NODELAY: off by default on a fresh TCP socket, on after an
        // explicit set -- a real, observable round trip through the OS.
        assert_eq!(
            getopt(&mut board, &mut host, HS_IPPROTO_TCP, HS_TCP_NODELAY, 4),
            0
        );
        assert_eq!(read_i32(&mut host), 0, "TCP_NODELAY should default to off");
        assert_eq!(
            setopt_i32(&mut board, &mut host, HS_IPPROTO_TCP, HS_TCP_NODELAY, 1),
            0
        );
        assert_eq!(
            getopt(&mut board, &mut host, HS_IPPROTO_TCP, HS_TCP_NODELAY, 4),
            0
        );
        assert_eq!(read_i32(&mut host), 1, "TCP_NODELAY should read back on");

        // SO_LINGER: plugin-side roundtrip storage (no single-`value` ABI
        // fits its two-field struct) -- still a real round trip through
        // do_setsockopt_host/do_getsockopt_host's own fallback.
        const LINGER_ADDR: u32 = 0x240;
        let base = LINGER_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(&1i32.to_be_bytes());
        host.memory_mut().chip_ram[base + 4..base + 8].copy_from_slice(&30i32.to_be_bytes());
        let rc = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SETSOCKOPT,
            [fd, HS_SOL_SOCKET, HS_SO_LINGER, LINGER_ADDR as i32, 8, 0, 0],
        );
        assert_eq!(rc, 0);
        let base = OPTLEN_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(&8i32.to_be_bytes());
        let rc = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_GETSOCKOPT,
            [
                fd,
                HS_SOL_SOCKET,
                HS_SO_LINGER,
                OPTVAL_ADDR as i32,
                OPTLEN_ADDR as i32,
                0,
                0,
            ],
        );
        assert_eq!(rc, 0);
        let base = OPTVAL_ADDR as usize;
        let onoff = i32::from_be_bytes(
            host.memory_mut().chip_ram[base..base + 4]
                .try_into()
                .unwrap(),
        );
        let secs = i32::from_be_bytes(
            host.memory_mut().chip_ram[base + 4..base + 8]
                .try_into()
                .unwrap(),
        );
        assert_eq!((onoff, secs), (1, 30));

        // SO_ERROR on a healthy, never-failed socket: 0, no pending error.
        assert_eq!(
            getopt(&mut board, &mut host, HS_SOL_SOCKET, HS_SO_ERROR, 4),
            0
        );
        assert_eq!(read_i32(&mut host), 0);

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [fd, 0, 0, 0, 0, 0, 0],
        );
    }

    /// Dup2Socket(fd, -1) on a host-backed fd: real `dup()` semantics via
    /// `sock_dup`, proven the way that matters -- not just that a second
    /// fd number comes back, but that closing the *original* fd leaves
    /// the duplicate's own connection genuinely alive and usable,
    /// because the OS itself (not this plugin) is what's tracking the
    /// shared open file description.
    #[test]
    fn hostsocket_plugin_host_backend_dup2socket_any_new_fd_survives_closing_the_original() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4];
            std::io::Read::read_exact(&mut stream, &mut buf).unwrap();
            assert_eq!(&buf, b"PING");
            std::io::Write::write_all(&mut stream, b"PONG").unwrap();
        });

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [HS_AF_INET, HS_SOCK_STREAM, 0, 0, 0, 0, 0],
        );
        assert!(fd > 0);
        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);
        let rc = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_CONNECT,
            [fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(rc, 0);

        // Dup2Socket(fd, -1): "any new fd" form.
        let dup_fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_DUP2SOCKET,
            [fd, -1, 0, 0, 0, 0, 0],
        );
        assert!(dup_fd > 0 && dup_fd != fd);

        // Close the *original* -- the real OS-level connection must
        // survive, since `dup_fd` still holds a genuinely independent
        // descriptor referencing it.
        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [fd, 0, 0, 0, 0, 0, 0],
        );

        const SEND_BUF_ADDR: u32 = 0x300;
        let base = SEND_BUF_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(b"PING");
        let sent = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_SEND,
            [dup_fd, SEND_BUF_ADDR as i32, 4, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(
            sent, 4,
            "the duplicate fd must still be able to send after the original was closed"
        );

        const RECV_BUF_ADDR: u32 = 0x400;
        let received = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_RECV,
            [dup_fd, RECV_BUF_ADDR as i32, 4, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(received, 4);
        let base = RECV_BUF_ADDR as usize;
        assert_eq!(&host.memory_mut().chip_ram[base..base + 4], b"PONG");

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [dup_fd, 0, 0, 0, 0, 0, 0],
        );
        server.join().unwrap();
    }

    /// ReleaseSocket()/ObtainSocket() on a host-backed fd: the released
    /// fd must become invalid immediately in the releasing task's own
    /// context (pure Rust-side data movement, no new host handle), and
    /// ObtainSocket() must hand back a fd for the *same* real connection
    /// -- proven by actually completing a round trip through it.
    #[test]
    fn hostsocket_plugin_host_backend_release_socket_then_obtain_socket_round_trips() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4];
            std::io::Read::read_exact(&mut stream, &mut buf).unwrap();
            assert_eq!(&buf, b"PING");
            std::io::Write::write_all(&mut stream, b"PONG").unwrap();
        });

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [HS_AF_INET, HS_SOCK_STREAM, 0, 0, 0, 0, 0],
        );
        assert!(fd > 0);
        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);
        let rc = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_CONNECT,
            [fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(rc, 0);

        // ReleaseSocket(fd, UNIQUE_ID = -1) -- returns the library-assigned id.
        let id = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_RELEASESOCKET,
            [fd, -1, 0, 0, 0, 0, 0],
        );
        assert!(id >= 0);

        // `fd` must be dead in this context now -- send() on it should
        // fail as a plain unknown descriptor, not silently still work.
        const SEND_BUF_ADDR: u32 = 0x300;
        let base = SEND_BUF_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(b"PING");
        let rc = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SEND,
            [fd, SEND_BUF_ADDR as i32, 4, 0, 0, 0, 0],
        );
        assert_eq!(rc, -1, "the released fd must no longer be usable");

        // ObtainSocket(id, AF_INET, SOCK_STREAM, 0) -- gets back a fd for
        // the same real connection.
        let new_fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_OBTAINSOCKET,
            [id, HS_AF_INET, HS_SOCK_STREAM, 0, 0, 0, 0],
        );
        assert!(new_fd > 0);

        let sent = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_SEND,
            [new_fd, SEND_BUF_ADDR as i32, 4, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(sent, 4);
        const RECV_BUF_ADDR: u32 = 0x400;
        let received = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_RECV,
            [new_fd, RECV_BUF_ADDR as i32, 4, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(received, 4);
        let base = RECV_BUF_ADDR as usize;
        assert_eq!(&host.memory_mut().chip_ram[base..base + 4], b"PONG");

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [new_fd, 0, 0, 0, 0, 0, 0],
        );
        server.join().unwrap();
    }

    /// ReleaseCopyOfSocket(): unlike ReleaseSocket(), the original fd
    /// must stay valid *and* the obtained copy must be a real,
    /// independent duplicate of the same connection -- proven by sending
    /// through the original and receiving the reply through the copy.
    #[test]
    fn hostsocket_plugin_host_backend_release_copy_of_socket_keeps_the_original_valid() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4];
            std::io::Read::read_exact(&mut stream, &mut buf).unwrap();
            assert_eq!(&buf, b"PING");
            std::io::Write::write_all(&mut stream, b"PONG").unwrap();
        });

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [HS_AF_INET, HS_SOCK_STREAM, 0, 0, 0, 0, 0],
        );
        assert!(fd > 0);
        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);
        let rc = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_CONNECT,
            [fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(rc, 0);

        let id = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_RELEASECOPYOFSOCKET,
            [fd, -1, 0, 0, 0, 0, 0],
        );
        assert!(id >= 0);

        let copy_fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_OBTAINSOCKET,
            [id, HS_AF_INET, HS_SOCK_STREAM, 0, 0, 0, 0],
        );
        assert!(copy_fd > 0 && copy_fd != fd);

        // Send through the original...
        const SEND_BUF_ADDR: u32 = 0x300;
        let base = SEND_BUF_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(b"PING");
        let sent = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_SEND,
            [fd, SEND_BUF_ADDR as i32, 4, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(sent, 4, "the original fd must still work after the release");

        // ...and receive the reply through the copy: only possible if
        // both really are descriptors onto the same underlying
        // connection.
        const RECV_BUF_ADDR: u32 = 0x400;
        let received = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_RECV,
            [copy_fd, RECV_BUF_ADDR as i32, 4, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(
            received, 4,
            "the copy must see the same connection's inbound data"
        );
        let base = RECV_BUF_ADDR as usize;
        assert_eq!(&host.memory_mut().chip_ram[base..base + 4], b"PONG");

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [fd, 0, 0, 0, 0, 0, 0],
        );
        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [copy_fd, 0, 0, 0, 0, 0, 0],
        );
        server.join().unwrap();
    }

    /// Shutdown(SHUT_WR) on a host-backed fd: a real half-close reaching
    /// the actual host socket -- proven by the *peer* observing EOF on
    /// its next read (only possible if the FIN genuinely left the host),
    /// not just that the call itself returns success. Found missing
    /// entirely (`do_shutdown` had no host-backed branch at all) running
    /// bsdsocktest for real against this backend for the first time.
    #[test]
    fn hostsocket_plugin_host_backend_shutdown_write_reaches_the_real_peer() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4];
            let n = std::io::Read::read(&mut stream, &mut buf).unwrap();
            assert_eq!(
                n, 0,
                "the peer must see a clean EOF after shutdown(SHUT_WR)"
            );
        });

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [HS_AF_INET, HS_SOCK_STREAM, 0, 0, 0, 0, 0],
        );
        assert!(fd > 0);
        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);
        let rc = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_CONNECT,
            [fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(rc, 0);

        // shutdown(fd, SHUT_WR = 1)
        let rc = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SHUTDOWN,
            [fd, 1, 0, 0, 0, 0, 0],
        );
        assert_eq!(rc, 0, "shutdown (host backend) should succeed");

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [fd, 0, 0, 0, 0, 0, 0],
        );
        server.join().unwrap();
    }

    /// `recv(MSG_PEEK)` on a host-backed fd: a real, non-consuming peek --
    /// proven by reading the *same* bytes twice, once via `MSG_PEEK` and
    /// once via a plain `recv()` right after, which only works if the
    /// peek genuinely left the data in the socket's own receive buffer.
    #[test]
    fn hostsocket_plugin_host_backend_recv_msg_peek_does_not_consume() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            std::io::Write::write_all(&mut stream, b"PING").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
        });

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [HS_AF_INET, HS_SOCK_STREAM, 0, 0, 0, 0, 0],
        );
        assert!(fd > 0);
        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);
        let rc = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_CONNECT,
            [fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(rc, 0);

        // recv(fd, buf, 4, MSG_PEEK)
        const PEEK_BUF_ADDR: u32 = 0x300;
        let peeked = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_RECV,
            [fd, PEEK_BUF_ADDR as i32, 4, HS_MSG_PEEK, 0, 0, 0],
            deadline,
        );
        assert_eq!(peeked, 4, "peek should see the 4 bytes already sent");
        let base = PEEK_BUF_ADDR as usize;
        assert_eq!(&host.memory_mut().chip_ram[base..base + 4], b"PING");

        // recv(fd, buf, 4, 0) -- must see the SAME bytes, not EOF/nothing.
        const RECV_BUF_ADDR: u32 = 0x400;
        let received = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_RECV,
            [fd, RECV_BUF_ADDR as i32, 4, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(
            received, 4,
            "a real recv() right after peeking must still see the data"
        );
        let base = RECV_BUF_ADDR as usize;
        assert_eq!(&host.memory_mut().chip_ram[base..base + 4], b"PING");

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [fd, 0, 0, 0, 0, 0, 0],
        );
        server.join().unwrap();
    }

    /// `recv(MSG_OOB)` on a host-backed fd retrieves real TCP urgent data
    /// -- unlike the smoltcp path (a permanent `EOPNOTSUPP`, no
    /// urgent-pointer support in `socket::tcp` at all), a real host socket
    /// genuinely supports this. The peer sends the urgent byte with a raw
    /// `libc::send(..., MSG_OOB)` (std's own `TcpStream` has no flags-aware
    /// send), matching exactly how bsdsocktest's own test 27 drives this.
    /// Unix-only: drives the peer side with a raw `libc::send`/`AsRawFd`,
    /// neither available on Windows.
    #[cfg(unix)]
    #[test]
    fn hostsocket_plugin_host_backend_recv_msg_oob_gets_real_urgent_byte() {
        use std::os::unix::io::AsRawFd;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let raw = stream.as_raw_fd();
            let byte = [0xABu8];
            let n =
                unsafe { libc::send(raw, byte.as_ptr() as *const libc::c_void, 1, libc::MSG_OOB) };
            assert_eq!(n, 1, "libc::send(MSG_OOB) itself must succeed");
            std::thread::sleep(std::time::Duration::from_millis(100));
        });

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [HS_AF_INET, HS_SOCK_STREAM, 0, 0, 0, 0, 0],
        );
        assert!(fd > 0);
        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);
        let rc = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_CONNECT,
            [fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(rc, 0);

        // recv(fd, buf, 1, MSG_OOB) -- must block/retry until the real
        // urgent byte lands, then deliver exactly it.
        const OOB_BUF_ADDR: u32 = 0x300;
        let n = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_RECV,
            [fd, OOB_BUF_ADDR as i32, 1, HS_MSG_OOB, 0, 0, 0],
            deadline,
        );
        assert_eq!(
            n, 1,
            "recv(MSG_OOB) should retrieve exactly the urgent byte"
        );
        let base = OOB_BUF_ADDR as usize;
        assert_eq!(host.memory_mut().chip_ram[base], 0xAB);

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [fd, 0, 0, 0, 0, 0, 0],
        );
        server.join().unwrap();
    }

    /// `IoctlSocket(FIONREAD)` on a host-backed fd: reports the real
    /// number of bytes the kernel actually has queued -- proven by
    /// checking it matches what was really sent, not a placeholder.
    /// Unix-only: `sock_nread` (see its own doc comment) has no non-unix
    /// implementation -- a real `ioctlsocket(FIONREAD)` FFI binding would
    /// need Windows verification this project doesn't have.
    #[cfg(unix)]
    #[test]
    fn hostsocket_plugin_host_backend_ioctl_fionread_reports_pending_bytes() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            std::io::Write::write_all(&mut stream, b"PING").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
        });

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [HS_AF_INET, HS_SOCK_STREAM, 0, 0, 0, 0, 0],
        );
        assert!(fd > 0);
        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);
        let rc = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_CONNECT,
            [fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(rc, 0);

        // Poll FIONREAD until the peer's 4 bytes have actually arrived
        // (a real network round trip, even over loopback, isn't
        // instantaneous relative to this call).
        const ARGP_ADDR: u32 = 0x300;
        let pending = {
            let poll_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let rc = hs_call(
                    &mut board,
                    &mut host,
                    task,
                    HS_CALL_IOCTLSOCKET,
                    [fd, HS_FIONREAD, ARGP_ADDR as i32, 0, 0, 0, 0],
                );
                assert_eq!(rc, 0, "IoctlSocket(FIONREAD) should succeed");
                let base = ARGP_ADDR as usize;
                let n = i32::from_be_bytes(
                    host.memory_mut().chip_ram[base..base + 4]
                        .try_into()
                        .unwrap(),
                );
                if n > 0 {
                    break n;
                }
                assert!(
                    std::time::Instant::now() < poll_deadline,
                    "FIONREAD never reported the peer's data arriving"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        assert_eq!(
            pending, 4,
            "FIONREAD should report the real pending byte count"
        );

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [fd, 0, 0, 0, 0, 0, 0],
        );
        server.join().unwrap();
    }

    /// `sendmsg()`/`recvmsg()` on a host-backed fd: a real round trip
    /// through the gather/scatter path (one iovec each, still exercising
    /// the real code, not the plain send()/recv() one).
    #[test]
    fn hostsocket_plugin_host_backend_sendmsg_recvmsg_round_trip() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4];
            std::io::Read::read_exact(&mut stream, &mut buf).unwrap();
            assert_eq!(&buf, b"PING");
            std::io::Write::write_all(&mut stream, b"PONG").unwrap();
        });

        let cfg = crate::hostsocket::board_config(
            crate::net::NetConfig::Loopback,
            None,
            None,
            None,
            None,
            None,
            Some("host"),
        );
        let mut board = WasmBoard::from_file(&cfg.wasm_path, cfg.manifest).unwrap();
        let mut mem = empty_memory();
        let mut host = DeviceHost::new(&mut mem);
        let task = 0x2000;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        let fd = hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_SOCKET,
            [HS_AF_INET, HS_SOCK_STREAM, 0, 0, 0, 0, 0],
        );
        assert!(fd > 0);
        const SOCKADDR_ADDR: u32 = 0x200;
        let mut sockaddr = [0u8; 16];
        sockaddr[2..4].copy_from_slice(&port.to_be_bytes());
        sockaddr[4..8].copy_from_slice(&[127, 0, 0, 1]);
        let base = SOCKADDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 16].copy_from_slice(&sockaddr);
        let rc = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_CONNECT,
            [fd, SOCKADDR_ADDR as i32, 16, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(rc, 0);

        // sendmsg: one iovec pointing at "PING", via a struct msghdr at
        // MSGHDR_ADDR (msg_iov at +8, msg_iovlen at +12, matching
        // read_iovec_descriptors's own layout).
        const SEND_DATA_ADDR: u32 = 0x300;
        const SEND_IOVEC_ADDR: u32 = 0x320;
        const SEND_MSGHDR_ADDR: u32 = 0x340;
        let base = SEND_DATA_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 4].copy_from_slice(b"PING");
        let mut iovec = [0u8; 8];
        iovec[0..4].copy_from_slice(&SEND_DATA_ADDR.to_be_bytes());
        iovec[4..8].copy_from_slice(&4u32.to_be_bytes());
        let base = SEND_IOVEC_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 8].copy_from_slice(&iovec);
        let mut msghdr = [0u8; 28];
        msghdr[8..12].copy_from_slice(&SEND_IOVEC_ADDR.to_be_bytes());
        msghdr[12..16].copy_from_slice(&1u32.to_be_bytes());
        let base = SEND_MSGHDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 28].copy_from_slice(&msghdr);

        let sent = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_SENDMSG,
            [fd, SEND_MSGHDR_ADDR as i32, 0, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(sent, 4, "sendmsg (host backend) should queue all 4 bytes");

        // recvmsg: one iovec to receive into.
        const RECV_DATA_ADDR: u32 = 0x400;
        const RECV_IOVEC_ADDR: u32 = 0x420;
        const RECV_MSGHDR_ADDR: u32 = 0x440;
        let mut iovec = [0u8; 8];
        iovec[0..4].copy_from_slice(&RECV_DATA_ADDR.to_be_bytes());
        iovec[4..8].copy_from_slice(&4u32.to_be_bytes());
        let base = RECV_IOVEC_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 8].copy_from_slice(&iovec);
        let mut msghdr = [0u8; 28];
        msghdr[8..12].copy_from_slice(&RECV_IOVEC_ADDR.to_be_bytes());
        msghdr[12..16].copy_from_slice(&1u32.to_be_bytes());
        let base = RECV_MSGHDR_ADDR as usize;
        host.memory_mut().chip_ram[base..base + 28].copy_from_slice(&msghdr);

        let received = hs_call_blocking(
            &mut board,
            &mut host,
            task,
            HS_CALL_RECVMSG,
            [fd, RECV_MSGHDR_ADDR as i32, 0, 0, 0, 0, 0],
            deadline,
        );
        assert_eq!(
            received, 4,
            "recvmsg (host backend) should return \"PONG\"'s 4 bytes"
        );
        let base = RECV_DATA_ADDR as usize;
        assert_eq!(&host.memory_mut().chip_ram[base..base + 4], b"PONG");

        hs_call(
            &mut board,
            &mut host,
            task,
            HS_CALL_CLOSESOCKET,
            [fd, 0, 0, 0, 0, 0, 0],
        );
        server.join().unwrap();
    }

    /// Materialise an inert example plugin `.wasm` (autoconfigures, answers a
    /// constant, no interrupts/DMA) to the path in `COPPERLINE_EMIT_WASM`, for
    /// end-to-end boot testing and as a starting point for plugin authors.
    /// Run with: `COPPERLINE_EMIT_WASM=/path/board.wasm cargo test --release \
    /// emit_example_plugin_wasm -- --ignored`.
    ///
    /// A generator rather than a check, so it skips cleanly when the path is
    /// unset: `cargo test -- --ignored` stops at the first failing binary, and
    /// panicking here would hide every asset-gated integration test behind it
    /// (see tests/README.md).
    #[test]
    #[ignore]
    fn emit_example_plugin_wasm() {
        let inert = r#"
            (module
              (memory (export "memory") 1)
              (func (export "read") (param i32 i32) (result i32)
                (i32.const 0x12345678))
              (func (export "write") (param i32 i32 i32))
              (func (export "tick") (param i32)))
        "#;
        let Some(out) = crate::envcfg::var("COPPERLINE_EMIT_WASM") else {
            eprintln!("skipping: set COPPERLINE_EMIT_WASM to a .wasm output path to run it");
            return;
        };
        std::fs::write(&out, wat::parse_str(inert).expect("valid WAT")).expect("write wasm");
        eprintln!("wrote example plugin to {out}");
    }
}
