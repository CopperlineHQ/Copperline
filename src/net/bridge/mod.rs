// SPDX-License-Identifier: GPL-3.0-or-later

//! Direct host-adapter Ethernet bridge.
//!
//! The emulator thread never blocks on host I/O. A small worker owns the
//! platform capture/injection handle and exchanges complete Ethernet frames
//! with the emulated board over bounded channels. Receive filtering is
//! applied in the platform capture engine where possible and repeated here as
//! a safety boundary.

use super::NetBackend;
use anyhow::{Context, Result};
use std::sync::mpsc;
use std::time::Duration;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

/// One adapter selectable for direct bridging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInterface {
    /// Stable platform identifier accepted by `interface = "..."`.
    pub name: String,
    /// Human-facing description, when the platform provides one.
    pub description: Option<String>,
    pub up: bool,
    pub running: bool,
    pub loopback: bool,
    pub wireless: bool,
}

impl HostInterface {
    /// Concise label for CLI and launcher pickers.
    pub fn label(&self) -> String {
        match self.description.as_deref() {
            Some(description) if description != self.name => {
                format!("{description} [{}]", self.name)
            }
            _ => self.name.clone(),
        }
    }
}

/// Enumerate adapters available to the bridge backend.
pub fn list_interfaces() -> Result<Vec<HostInterface>> {
    let mut interfaces = platform::list_interfaces()
        .context("enumerating host network interfaces for bridged networking")?;
    interfaces.sort_by(|a, b| {
        b.running
            .cmp(&a.running)
            .then_with(|| b.up.cmp(&a.up))
            .then_with(|| a.loopback.cmp(&b.loopback))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(interfaces)
}

trait AdapterIo: Send {
    fn send(&mut self, frame: &[u8]) -> Result<()>;
    fn receive(&mut self) -> Result<Option<Vec<u8>>>;
    fn set_guest_mac(&mut self, mac: [u8; 6]) -> Result<()>;
}

const TO_ADAPTER_CAPACITY: usize = 256;
const TO_GUEST_CAPACITY: usize = 512;
const MAX_RX_BURST: usize = 128;

enum Command {
    Frame(Vec<u8>),
    GuestMac([u8; 6]),
}

/// Emulator-side handle for a direct adapter bridge.
pub struct BridgeBackend {
    to_adapter: Option<mpsc::SyncSender<Command>>,
    from_adapter: mpsc::Receiver<Vec<u8>>,
    thread: Option<std::thread::JoinHandle<()>>,
    guest_mac: Option<[u8; 6]>,
}

impl BridgeBackend {
    /// Open `interface` synchronously, then hand it to a non-blocking worker.
    /// Opening before the thread starts is intentional: configuration,
    /// permission, and missing-driver errors reach the startup caller.
    pub fn new(interface: &str, guest_mac: Option<[u8; 6]>) -> Result<Self> {
        let adapter = platform::open(interface, guest_mac)
            .with_context(|| format!("opening bridge interface {interface:?}"))?;
        Self::from_adapter(interface, adapter, guest_mac)
    }

    /// Finish backend construction around an already-open adapter. Keeping
    /// the worker boundary independent of the platform opener lets tests use
    /// an in-memory Ethernet peer while production still opens synchronously.
    fn from_adapter(
        interface: &str,
        mut adapter: impl AdapterIo + 'static,
        guest_mac: Option<[u8; 6]>,
    ) -> Result<Self> {
        if let Some(mac) = guest_mac {
            adapter
                .set_guest_mac(mac)
                .context("installing initial bridge receive filter")?;
        } else {
            // Generic plugin NICs reveal their address in their first TX
            // frame. Until then, an all-zero unicast placeholder leaves only
            // multicast/broadcast useful and keeps platform capture bounded.
            adapter
                .set_guest_mac([0; 6])
                .context("installing initial multicast bridge receive filter")?;
        }

        let (in_tx, in_rx) = mpsc::sync_channel(TO_ADAPTER_CAPACITY);
        let (out_tx, out_rx) = mpsc::sync_channel(TO_GUEST_CAPACITY);
        let interface_name = interface.to_string();
        let thread = std::thread::Builder::new()
            .name("a2065-bridge".into())
            .spawn(move || run_worker(&interface_name, adapter, guest_mac, in_rx, out_tx))
            .context("starting bridge worker thread")?;
        Ok(Self {
            to_adapter: Some(in_tx),
            from_adapter: out_rx,
            thread: Some(thread),
            guest_mac,
        })
    }
}

impl NetBackend for BridgeBackend {
    fn send(&mut self, frame: &[u8]) {
        if let Some(tx) = &self.to_adapter {
            // A2065 supplies PADR explicitly. A generic WASM NIC has no
            // separate MAC-setting import, so learn its station address from
            // the first transmitted Ethernet frame.
            if frame.len() >= 12 {
                let mut source = [0u8; 6];
                source.copy_from_slice(&frame[6..12]);
                if source[0] & 1 == 0
                    && source != [0; 6]
                    && self.guest_mac != Some(source)
                    && tx.try_send(Command::GuestMac(source)).is_ok()
                {
                    self.guest_mac = Some(source);
                }
            }
            // A full queue is ordinary Ethernet loss, never an emulator stall.
            let _ = tx.try_send(Command::Frame(frame.to_vec()));
        }
    }

    fn poll(&mut self) -> Option<Vec<u8>> {
        self.from_adapter.try_recv().ok()
    }

    fn set_guest_mac(&mut self, mac: [u8; 6]) {
        if let Some(tx) = &self.to_adapter {
            if tx.try_send(Command::GuestMac(mac)).is_ok() {
                self.guest_mac = Some(mac);
            }
        }
    }
}

impl Drop for BridgeBackend {
    fn drop(&mut self) {
        self.to_adapter = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_worker(
    interface: &str,
    mut adapter: impl AdapterIo,
    mut guest_mac: Option<[u8; 6]>,
    in_rx: mpsc::Receiver<Command>,
    out_tx: mpsc::SyncSender<Vec<u8>>,
) {
    loop {
        match in_rx.recv_timeout(Duration::from_millis(1)) {
            Ok(command) => {
                if !handle_command(interface, &mut adapter, &mut guest_mac, command) {
                    break;
                }
                while let Ok(command) = in_rx.try_recv() {
                    if !handle_command(interface, &mut adapter, &mut guest_mac, command) {
                        return;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        for _ in 0..MAX_RX_BURST {
            match adapter.receive() {
                Ok(Some(frame)) => {
                    if accept_frame(&frame, guest_mac) && out_tx.try_send(frame).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    log::error!(
                        "bridge interface {interface:?} went down: {error}; \
                         guest link is now disconnected"
                    );
                    return;
                }
            }
        }
    }
}

fn handle_command(
    interface: &str,
    adapter: &mut impl AdapterIo,
    guest_mac: &mut Option<[u8; 6]>,
    command: Command,
) -> bool {
    match command {
        Command::Frame(frame) => {
            if let Err(error) = adapter.send(&frame) {
                log::error!(
                    "bridge interface {interface:?} transmit failed: {error}; \
                     guest link is now disconnected"
                );
                return false;
            }
        }
        Command::GuestMac(mac) => {
            *guest_mac = Some(mac);
            if let Err(error) = adapter.set_guest_mac(mac) {
                log::error!(
                    "bridge interface {interface:?} receive-filter update failed: {error}; \
                     guest link is now disconnected"
                );
                return false;
            }
        }
    }
    true
}

/// Accept frames addressed to the guest plus all Ethernet multicast/broadcast
/// traffic. Frames sourced by the guest are suppressed to avoid capture echo.
fn accept_frame(frame: &[u8], guest_mac: Option<[u8; 6]>) -> bool {
    if frame.len() < 14 {
        return false;
    }
    if guest_mac.is_some_and(|mac| frame[6..12] == mac) {
        return false;
    }
    frame[0] & 1 != 0 || guest_mac.is_some_and(|mac| frame[..6] == mac)
}

#[cfg(any(test, target_os = "macos", target_os = "windows"))]
fn mac_filter(mac: [u8; 6]) -> String {
    format!(
        "(ether dst {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} or ether multicast) \
         and not ether src {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5],
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    const MAC: [u8; 6] = [0x02, 0, 0x10, 0, 0, 1];

    fn frame(dst: [u8; 6], src: [u8; 6]) -> Vec<u8> {
        let mut frame = vec![0; 60];
        frame[..6].copy_from_slice(&dst);
        frame[6..12].copy_from_slice(&src);
        frame
    }

    #[derive(Default)]
    struct FakeAdapterState {
        sent: Vec<Vec<u8>>,
        received: VecDeque<Vec<u8>>,
        guest_macs: Vec<[u8; 6]>,
    }

    struct FakeAdapter {
        state: Arc<Mutex<FakeAdapterState>>,
    }

    impl AdapterIo for FakeAdapter {
        fn send(&mut self, frame: &[u8]) -> Result<()> {
            self.state.lock().unwrap().sent.push(frame.to_vec());
            Ok(())
        }

        fn receive(&mut self) -> Result<Option<Vec<u8>>> {
            Ok(self.state.lock().unwrap().received.pop_front())
        }

        fn set_guest_mac(&mut self, mac: [u8; 6]) -> Result<()> {
            self.state.lock().unwrap().guest_macs.push(mac);
            Ok(())
        }
    }

    fn wait_until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready() {
            assert!(Instant::now() < deadline, "bridge worker timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn worker_exchanges_frames_with_a_synthetic_adapter() {
        let state = Arc::new(Mutex::new(FakeAdapterState::default()));
        let adapter = FakeAdapter {
            state: Arc::clone(&state),
        };
        let mut backend = BridgeBackend::from_adapter("synthetic", adapter, Some(MAC)).unwrap();
        assert_eq!(state.lock().unwrap().guest_macs, vec![MAC]);

        let outbound = frame([0xff; 6], MAC);
        backend.send(&outbound);
        wait_until(|| !state.lock().unwrap().sent.is_empty());
        assert_eq!(state.lock().unwrap().sent, vec![outbound]);

        let captured_echo = frame([0xff; 6], MAC);
        let inbound = frame(MAC, [0x02, 0, 0, 0, 0, 2]);
        {
            let mut state = state.lock().unwrap();
            state.received.push_back(captured_echo);
            state.received.push_back(inbound.clone());
        }
        let mut delivered = None;
        wait_until(|| {
            delivered = backend.poll();
            delivered.is_some()
        });
        assert_eq!(delivered, Some(inbound));
        assert!(backend.poll().is_none(), "capture echo must be suppressed");

        let replacement = [0x02, 0, 0x20, 0, 0, 1];
        backend.set_guest_mac(replacement);
        wait_until(|| state.lock().unwrap().guest_macs.contains(&replacement));
    }

    #[test]
    fn software_filter_accepts_guest_and_group_destinations() {
        assert!(accept_frame(&frame(MAC, [1; 6]), Some(MAC)));
        assert!(accept_frame(&frame([0xff; 6], [1; 6]), Some(MAC)));
        assert!(accept_frame(
            &frame([0x01, 0, 0x5e, 0, 0, 1], [1; 6]),
            Some(MAC)
        ));
        assert!(!accept_frame(
            &frame([0x02, 3, 4, 5, 6, 7], [1; 6]),
            Some(MAC)
        ));
    }

    #[test]
    fn software_filter_suppresses_guest_capture_echo_and_junk() {
        assert!(!accept_frame(&frame([0xff; 6], MAC), Some(MAC)));
        assert!(!accept_frame(&[0; 13], Some(MAC)));
        assert!(accept_frame(&frame([0xff; 6], [1; 6]), None));
    }

    #[test]
    fn pcap_filter_contains_both_mac_directions() {
        let filter = mac_filter(MAC);
        assert!(filter.contains("ether dst 02:00:10:00:00:01"));
        assert!(filter.contains("not ether src 02:00:10:00:00:01"));
    }
}
