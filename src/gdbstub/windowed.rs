// SPDX-License-Identifier: GPL-3.0-or-later

//! Socket plumbing for the windowed GDB stub (`--gdb-gui`): an accept
//! thread plus, per connection, a reader thread (RSP framing, checksum
//! verification, ack handling) and a writer thread. Framed packets cross
//! to the winit frame loop over an mpsc channel and are drained at the
//! top of `about_to_wait`, the same deterministic boundary the windowed
//! control server uses; replies travel back over a per-connection
//! channel of pre-framed strings, so the two socket threads never
//! interleave on the stream. This module holds no winit types -- the
//! frame loop passes a wake callback so a packet arriving while the
//! machine is paused (`ControlFlow::Wait`) still gets serviced promptly.
//!
//! One client at a time, like the headless stub: the accept thread
//! reads each connection to completion before accepting the next, so an
//! extra client waits in the listener backlog until the current one
//! detaches.

use super::core::{checksum, hex_encode, parse_hex_byte, MAX_PACKET_PAYLOAD_BYTES};
use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

/// A message from the socket threads to the frame loop.
pub enum GdbMsg {
    /// A client connected; the frame loop pauses the machine and builds
    /// a fresh [`super::core::GdbCore`].
    Connected,
    /// The client went away (EOF or error); the frame loop removes the
    /// session's machine-installed debug state and keeps serving.
    Disconnected,
    /// A framed, checksum-verified packet payload.
    Packet(String),
    /// The RSP interrupt byte (0x03) arrived between packets.
    Interrupt,
}

/// Outbound queue depth (pre-framed packets). Far more than a stopped
/// protocol exchange ever queues; a client this far behind is gone.
const OUTBOUND_PACKET_CAPACITY: usize = 256;

/// The frame loop's handle on the windowed GDB server.
pub struct GdbHandle {
    listener: Option<TcpListener>,
    cmd_tx: Sender<GdbMsg>,
    cmd_rx: Receiver<GdbMsg>,
    connection: Arc<Mutex<Option<Arc<OutboundConnection>>>>,
    connected: Arc<AtomicBool>,
}

struct OutboundConnection {
    frame_tx: SyncSender<String>,
    disconnect_stream: Option<TcpStream>,
}

impl GdbHandle {
    /// Bind the listener and announce the endpoint. Called from `main`
    /// before the window exists so a debugger can attach as soon as it
    /// opens; threads are spawned later by [`GdbHandle::start`].
    pub fn bind(config: &super::Config) -> Result<Self> {
        let bind = crate::debugger::normalize_listen_addr(&config.listen)?;
        let listener =
            TcpListener::bind(&bind).with_context(|| format!("binding GDB stub {bind}"))?;
        let local = listener
            .local_addr()
            .context("resolving GDB stub address")?;
        eprintln!("gdb: listening on {local}");
        log::info!("gdb: listening on {local}");
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        Ok(Self {
            listener: Some(listener),
            cmd_tx,
            cmd_rx,
            connection: Arc::new(Mutex::new(None)),
            connected: Arc::new(AtomicBool::new(false)),
        })
    }

    /// A loopback handle with no sockets, for driving the frame-loop
    /// drain directly in tests: messages are pushed through the returned
    /// sender, framed replies arrive on the returned receiver.
    pub fn test_pair() -> (Self, Sender<GdbMsg>, Receiver<String>) {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel(OUTBOUND_PACKET_CAPACITY);
        let connection = OutboundConnection {
            frame_tx,
            disconnect_stream: None,
        };
        let handle = Self {
            listener: None,
            cmd_tx: cmd_tx.clone(),
            cmd_rx,
            connection: Arc::new(Mutex::new(Some(Arc::new(connection)))),
            connected: Arc::new(AtomicBool::new(true)),
        };
        (handle, cmd_tx, frame_rx)
    }

    /// Spawn the accept thread. `wake` is invoked after every enqueued
    /// message so the event loop leaves `ControlFlow::Wait`.
    pub fn start(&mut self, wake: Box<dyn Fn() + Send + Sync>) {
        let Some(listener) = self.listener.take() else {
            return; // test handle, or started twice
        };
        let cmd_tx = self.cmd_tx.clone();
        let connection_slot = Arc::clone(&self.connection);
        let connected = Arc::clone(&self.connected);
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::from(wake);
        std::thread::Builder::new()
            .name("gdb-accept".into())
            .spawn(move || {
                accept_loop(listener, cmd_tx, connection_slot, connected, wake);
            })
            .expect("spawning gdb accept thread");
    }

    /// Whether a client is currently attached (status-bar indicator).
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Drain one queued message, non-blocking.
    pub(crate) fn try_recv(&self) -> Option<GdbMsg> {
        self.cmd_rx.try_recv().ok()
    }

    /// Frame `payload` as an RSP packet and enqueue it without blocking
    /// the emulator frame loop. A full queue disconnects the client:
    /// silently dropping a reply would leave the debugger hung.
    pub(crate) fn send_packet(&self, payload: &str) {
        let sum = checksum(payload.as_bytes());
        self.send_frame(format!("${payload}#{sum:02x}"));
    }

    /// Deliver console text as `O` packets (200-byte chunks), the RSP
    /// side channel gdb prints verbatim.
    pub(crate) fn send_console(&self, output: &str) {
        for chunk in output.as_bytes().chunks(200) {
            self.send_packet(&format!("O{}", hex_encode(chunk)));
        }
    }

    fn send_frame(&self, frame: String) {
        let Some(connection) = self.connection.lock().expect("connection lock").clone() else {
            return;
        };
        match connection.frame_tx.try_send(frame) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.disconnect_current(&connection, "outbound packet queue is full");
            }
            Err(TrySendError::Disconnected(_)) => {
                self.disconnect_current(&connection, "gdb writer stopped");
            }
        }
    }

    /// Tear down the socket side of the current connection (the `k`
    /// packet's detach). The reader sees EOF and the accept thread emits
    /// the normal [`GdbMsg::Disconnected`] cleanup.
    pub(crate) fn disconnect(&self, reason: &str) {
        let connection = self.connection.lock().expect("connection lock").clone();
        if let Some(connection) = connection {
            self.disconnect_current(&connection, reason);
        }
    }

    fn disconnect_current(&self, connection: &Arc<OutboundConnection>, reason: &str) {
        let removed = {
            let mut active = self.connection.lock().expect("connection lock");
            if active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, connection))
            {
                active.take();
                true
            } else {
                false
            }
        };
        if !removed {
            return;
        }
        log::warn!("gdb: {reason}; detaching client");
        self.connected.store(false, Ordering::Relaxed);
        if let Some(stream) = &connection.disconnect_stream {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

fn accept_loop(
    listener: TcpListener,
    cmd_tx: Sender<GdbMsg>,
    connection_slot: Arc<Mutex<Option<Arc<OutboundConnection>>>>,
    connected: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    loop {
        let (stream, peer) = match listener.accept() {
            Ok(conn) => conn,
            Err(e) => {
                log::warn!("gdb: accept failed: {e}");
                return;
            }
        };
        log::info!("gdb: connection from {peer}");
        stream.set_nodelay(true).ok();

        // Writer thread: owns the write half, drains the frame channel.
        let (frame_tx, frame_rx) =
            std::sync::mpsc::sync_channel::<String>(OUTBOUND_PACKET_CAPACITY);
        let disconnect_stream = match stream.try_clone() {
            Ok(clone) => clone,
            Err(e) => {
                log::warn!("gdb: cloning disconnect stream failed: {e}");
                continue;
            }
        };
        let write_stream = match stream.try_clone() {
            Ok(clone) => clone,
            Err(e) => {
                log::warn!("gdb: cloning stream failed: {e}");
                continue;
            }
        };
        write_stream
            .set_write_timeout(Some(std::time::Duration::from_millis(250)))
            .ok();
        let writer = std::thread::Builder::new()
            .name("gdb-write".into())
            .spawn(move || {
                let mut stream = write_stream;
                for frame in frame_rx {
                    if stream.write_all(frame.as_bytes()).is_err() {
                        break;
                    }
                    stream.flush().ok();
                }
            })
            .expect("spawning gdb writer thread");

        let connection = Arc::new(OutboundConnection {
            frame_tx,
            disconnect_stream: Some(disconnect_stream),
        });
        *connection_slot.lock().expect("connection lock") = Some(Arc::clone(&connection));
        connected.store(true, Ordering::Relaxed);
        let _ = cmd_tx.send(GdbMsg::Connected);
        wake();

        // Read this connection to completion on the accept thread; the
        // next client is only accepted afterwards (one at a time).
        read_connection(stream, &cmd_tx, &connection.frame_tx, &wake);

        let mut active = connection_slot.lock().expect("connection lock");
        if active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &connection))
        {
            active.take();
        }
        drop(active);
        connected.store(false, Ordering::Relaxed);
        let _ = cmd_tx.send(GdbMsg::Disconnected);
        wake();
        drop(connection); // writer thread drains and exits
        let _ = writer.join();
        log::info!("gdb: client detached; listening again");
    }
}

/// Read one connection until EOF: RSP framing, checksum verification,
/// and acks happen here, so the frame loop only ever sees whole valid
/// packets. Acks go through the writer channel (never written directly)
/// so the two threads cannot interleave bytes on the stream.
fn read_connection(
    mut stream: TcpStream,
    cmd_tx: &Sender<GdbMsg>,
    frame_tx: &SyncSender<String>,
    wake: &Arc<dyn Fn() + Send + Sync>,
) {
    // The client's own NoAckMode view: flipped as soon as the request is
    // read, since the drain always answers it with OK.
    let mut no_ack = false;
    let mut byte = [0u8; 1];
    'connection: loop {
        // Hunt for a packet start, surfacing interrupts along the way.
        loop {
            match stream.read(&mut byte) {
                Ok(0) => return,
                Ok(_) => {}
                Err(_) => return,
            }
            match byte[0] {
                b'$' => break,
                0x03 => {
                    let _ = cmd_tx.send(GdbMsg::Interrupt);
                    wake();
                }
                _ => {} // '+', '-', line noise
            }
        }
        let mut payload = Vec::new();
        loop {
            match stream.read(&mut byte) {
                Ok(0) => return,
                Ok(_) => {}
                Err(_) => return,
            }
            if byte[0] == b'#' {
                break;
            }
            payload.push(byte[0]);
            if payload.len() > MAX_PACKET_PAYLOAD_BYTES {
                log::warn!("gdb: packet exceeds payload limit; disconnecting");
                let _ = stream.shutdown(Shutdown::Both);
                return;
            }
        }
        let mut sum_bytes = [0u8; 2];
        if stream.read_exact(&mut sum_bytes).is_err() {
            return;
        }
        let Ok(expected) = parse_hex_byte(sum_bytes[0], sum_bytes[1]) else {
            continue 'connection;
        };
        if expected != checksum(&payload) {
            if !no_ack {
                let _ = frame_tx.try_send("-".to_string());
            }
            continue;
        }
        if !no_ack {
            let _ = frame_tx.try_send("+".to_string());
        }
        let Ok(text) = String::from_utf8(payload) else {
            continue;
        };
        if text == "QStartNoAckMode" {
            no_ack = true;
        }
        let _ = cmd_tx.send(GdbMsg::Packet(text));
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gdbstub::testkit::GdbClient;

    fn started_handle() -> (GdbHandle, std::net::SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let mut handle = GdbHandle {
            listener: Some(listener),
            cmd_tx,
            cmd_rx,
            connection: Arc::new(Mutex::new(None)),
            connected: Arc::new(AtomicBool::new(false)),
        };
        handle.start(Box::new(|| {}));
        (handle, addr)
    }

    fn recv(handle: &GdbHandle) -> GdbMsg {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(msg) = handle.try_recv() {
                return msg;
            }
            assert!(std::time::Instant::now() < deadline, "no gdb message");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn reader_frames_packets_acks_and_surfaces_interrupts() {
        let (handle, addr) = started_handle();
        let mut client = GdbClient::connect(addr);
        assert!(matches!(recv(&handle), GdbMsg::Connected));
        assert!(handle.connected());

        // A framed packet arrives whole, and the reader acks it.
        client.send("qC");
        assert!(matches!(recv(&handle), GdbMsg::Packet(p) if p == "qC"));
        // Answer through the handle; the client reads the framed reply
        // (read_reply also consumes the "+" ack transparently).
        handle.send_packet("QC1");
        // An interrupt byte between packets surfaces as its own message.
        client.raw(&[0x03]);
        assert!(matches!(recv(&handle), GdbMsg::Interrupt));
        // A bad checksum is dropped (nacked), not forwarded.
        client.raw(b"$qC#00");
        client.send("qAttached");
        assert!(matches!(recv(&handle), GdbMsg::Packet(p) if p == "qAttached"));
        assert_eq!(client.read_reply(), "QC1");
        drop(client);
        assert!(matches!(recv(&handle), GdbMsg::Disconnected));
        assert!(!handle.connected());
    }

    #[test]
    fn no_ack_mode_stops_acks_and_the_packet_still_reaches_the_drain() {
        let (handle, addr) = started_handle();
        let mut client = GdbClient::connect(addr);
        assert!(matches!(recv(&handle), GdbMsg::Connected));
        client.send("QStartNoAckMode");
        assert!(matches!(recv(&handle), GdbMsg::Packet(p) if p == "QStartNoAckMode"));
        handle.send_packet("OK");
        // From here the reader must not ack; the next reply the client
        // sees is the framed OK followed directly by the next packet's
        // reply, with no '+' in between.
        client.send("qC");
        assert!(matches!(recv(&handle), GdbMsg::Packet(p) if p == "qC"));
        handle.send_packet("QC1");
        // Exact wire bytes: the ack for QStartNoAckMode itself (sent in
        // the old mode), then the two framed replies with no ack bytes
        // in between.
        let expected = b"+$OK#9a$QC1#c5";
        assert_eq!(client.read_bytes(expected.len()), expected);
        drop(client);
        assert!(matches!(recv(&handle), GdbMsg::Disconnected));
    }

    #[test]
    fn a_dropped_client_frees_the_listener_for_the_next() {
        let (handle, addr) = started_handle();
        let first = GdbClient::connect(addr);
        assert!(matches!(recv(&handle), GdbMsg::Connected));
        drop(first);
        assert!(matches!(recv(&handle), GdbMsg::Disconnected));
        assert!(!handle.connected());
        // Reconnecting gets a fresh session on the same endpoint.
        let _second = GdbClient::connect(addr);
        assert!(matches!(recv(&handle), GdbMsg::Connected));
        assert!(handle.connected());
    }
}
