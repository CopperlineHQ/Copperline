// SPDX-License-Identifier: GPL-3.0-or-later

//! Native encrypted QUIC datagrams, with NAT traversal and HTTPS relay fallback.
//! Network discovery, timers and credentials never enter the emulated machine.

use super::{PacketQueue, Role, Settings, Transport, MAX_PACKET};
use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use iroh::{endpoint::presets, Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinSet;

const ALPN: &[u8] = b"copperline/netplay/1";
pub const CODE_LIMIT: usize = 4096;
const PREFIX: &str = "CLNI1.";
const SPECTATOR_PREFIX: &str = "CLNS1.";
const SETUP_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    pub endpoint: EndpointAddr,
    pub session: [u8; 16],
    pub delay: u8,
    pub window: u8,
}

/// A separate capability admits spectators without exposing the player slot.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpectatorInvitation {
    pub endpoint: EndpointAddr,
    pub capability: [u8; 16],
}

fn decode_code<T: DeserializeOwned>(code: &str, prefix: &str, what: &str) -> Result<T> {
    let code = code.trim();
    ensure!(code.len() <= CODE_LIMIT, "{what} is too long");
    let encoded = code
        .strip_prefix(prefix)
        .with_context(|| format!("Paste a desktop {}", what.to_ascii_lowercase()))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .with_context(|| format!("Invalid {what}"))?;
    serde_json::from_slice(&bytes).with_context(|| format!("Invalid {what}"))
}

fn encode_code<T: Serialize>(value: &T, prefix: &str, what: &str) -> Result<String> {
    let code = format!(
        "{prefix}{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(value)?)
    );
    ensure!(code.len() <= CODE_LIMIT, "{what} is too long");
    Ok(code)
}

fn validate_endpoint(endpoint: &EndpointAddr) -> Result<()> {
    ensure!(
        !endpoint.is_empty() && endpoint.addrs.len() <= 8,
        "Invitation has no usable route"
    );
    for address in &endpoint.addrs {
        match address {
            iroh::TransportAddr::Relay(url) => {
                validate_relay(url.as_str())?;
            }
            iroh::TransportAddr::Ip(addr) => ensure!(
                addr.port() != 0 && !addr.ip().is_unspecified() && !addr.ip().is_multicast(),
                "Invalid invitation address"
            ),
            _ => bail!("Unsupported invitation route"),
        }
    }
    Ok(())
}

impl Invitation {
    pub fn decode(code: &str) -> Result<Self> {
        let invitation: Self = decode_code(code, PREFIX, "Internet invitation")?;
        invitation.settings(1).validate()?;
        validate_endpoint(&invitation.endpoint)?;
        Ok(invitation)
    }

    pub fn encode(&self) -> Result<String> {
        encode_code(self, PREFIX, "Internet invitation")
    }

    pub fn settings(&self, player: usize) -> Settings {
        Settings {
            player,
            session: self.session,
            input_delay: self.delay,
            rollback_frames: self.window,
        }
    }
}

impl SpectatorInvitation {
    pub fn decode(code: &str) -> Result<Self> {
        let invitation: Self = decode_code(code, SPECTATOR_PREFIX, "Spectator invitation")?;
        validate_endpoint(&invitation.endpoint)?;
        Ok(invitation)
    }

    pub fn encode(&self) -> Result<String> {
        encode_code(self, SPECTATOR_PREFIX, "Spectator invitation")
    }

    /// Whether a code is a spectator invitation rather than a player's.
    pub fn is_code(code: &str) -> bool {
        code.trim().starts_with(SPECTATOR_PREFIX)
    }
}

/// A private host key is kept in memory separately from the shareable invitation.
#[derive(Clone, Debug)]
pub struct Options {
    pub invitation: Invitation,
    pub host_key: Option<SecretKey>,
    pub relay_only: bool,
    /// Spectators the host admits (0 = none).
    pub spectators: u8,
    /// The host's spectator capability, generated with the invitation.
    pub spectator_capability: Option<[u8; 16]>,
}

pub fn validate_relay(value: &str) -> Result<RelayUrl> {
    let url: RelayUrl = value.parse().context("Relay needs an HTTPS URL")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Relay needs an HTTPS URL without credentials, query or fragment"
    );
    Ok(url)
}

impl Options {
    pub fn host(
        delay: u8,
        window: u8,
        relay: &str,
        relay_only: bool,
        spectators: u8,
    ) -> Result<Self> {
        let key = SecretKey::generate();
        let mut endpoint = EndpointAddr::new(key.public());
        if relay.trim().is_empty() {
            for url in iroh::defaults::prod::default_relay_map().urls::<Vec<_>>() {
                endpoint = endpoint.with_relay_url(url);
            }
        } else {
            endpoint = endpoint.with_relay_url(validate_relay(relay.trim())?);
        }
        // The invitation is also a capability: knowing the endpoint ID alone
        // must not let an unrelated client claim the second controller port.
        // Spectators hold a different secret, so neither code opens the
        // other's role.
        let random = SecretKey::generate().to_bytes();
        let invitation = Invitation {
            endpoint,
            session: random[..16].try_into()?,
            delay,
            window,
        };
        let options = Self {
            invitation,
            host_key: Some(key),
            relay_only,
            spectators,
            spectator_capability: (spectators > 0).then_some(random[16..].try_into()?),
        };
        options.validate()?;
        Ok(options)
    }

    pub fn join(code: &str, relay_only: bool) -> Result<Self> {
        Ok(Self {
            invitation: Invitation::decode(code)?,
            host_key: None,
            relay_only,
            spectators: 0,
            spectator_capability: None,
        })
    }

    pub fn settings(&self) -> Settings {
        self.invitation
            .settings(usize::from(self.host_key.is_none()))
    }

    pub fn role(&self) -> Role {
        if self.host_key.is_some() {
            Role::Host
        } else {
            Role::Guest
        }
    }

    /// The code spectators paste; only a host that admits spectators has one.
    pub fn spectator_invitation(&self) -> Option<SpectatorInvitation> {
        self.spectator_capability
            .map(|capability| SpectatorInvitation {
                endpoint: self.invitation.endpoint.clone(),
                capability,
            })
    }

    pub fn validate(&self) -> Result<()> {
        Invitation::decode(&self.invitation.encode()?)?;
        if let Some(key) = &self.host_key {
            ensure!(
                key.public() == self.invitation.endpoint.id,
                "Host key does not match the invitation"
            );
        }
        ensure!(
            !self.relay_only || self.invitation.endpoint.relay_urls().next().is_some(),
            "Relay-only mode needs a relay"
        );
        ensure!(
            usize::from(self.spectators) <= super::spectate::MAX_SPECTATORS
                && (self.spectators == 0 || self.host_key.is_some())
                && (self.spectators > 0) == self.spectator_capability.is_some(),
            "Spectators are admitted by the host, up to {}",
            super::spectate::MAX_SPECTATORS
        );
        Ok(())
    }
}

/// A spectator's connection details: the host's endpoint and the spectator
/// capability, never the players' session.
#[derive(Clone, Debug)]
pub struct SpectatorOptions {
    pub invitation: SpectatorInvitation,
    pub relay_only: bool,
}

impl SpectatorOptions {
    pub fn watch(code: &str, relay_only: bool) -> Result<Self> {
        Ok(Self {
            invitation: SpectatorInvitation::decode(code)?,
            relay_only,
        })
    }

    pub fn validate(&self) -> Result<()> {
        SpectatorInvitation::decode(&self.invitation.encode()?)?;
        ensure!(
            !self.relay_only || self.invitation.endpoint.relay_urls().next().is_some(),
            "Relay-only mode needs a relay"
        );
        Ok(())
    }
}

#[derive(Default)]
struct Shared {
    packets: PacketQueue,
    ready: bool,
    route: Option<&'static str>,
    failure: Option<String>,
    /// The emulation thread dropped its handle; the worker closes the link.
    closed: bool,
    /// Spectator connections accepted since the emulation thread last looked.
    spectators: Vec<Arc<Mutex<Shared>>>,
}

#[derive(Clone, Debug)]
enum Kind {
    Play(Box<Options>),
    Watch(SpectatorOptions),
}

pub struct InternetTransport {
    kind: Kind,
    shared: Arc<Mutex<Shared>>,
    cancel: Option<oneshot::Sender<()>>,
}

impl InternetTransport {
    pub(super) fn new(options: Options) -> Result<Self> {
        options.validate()?;
        Self::start(Kind::Play(Box::new(options)))
    }

    pub(super) fn watch(options: SpectatorOptions) -> Result<Self> {
        options.validate()?;
        Self::start(Kind::Watch(options))
    }

    fn start(kind: Kind) -> Result<Self> {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (cancel, cancelled) = oneshot::channel();
        let worker_state = shared.clone();
        let worker_kind = kind.clone();
        std::thread::Builder::new()
            .name("netplay-internet".into())
            .spawn(move || {
                let result = (|| -> Result<()> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(worker(worker_kind, &worker_state, cancelled))
                })();
                if let Err(error) = result {
                    let mut state = worker_state.lock().unwrap();
                    if state.failure.is_none() {
                        state.failure = Some(format!("{error:#}"));
                    }
                }
            })
            .context("Starting Internet netplay")?;
        Ok(Self {
            kind,
            shared,
            cancel: Some(cancel),
        })
    }

    pub(super) fn connection_options(&self) -> super::ConnectionOptions {
        match &self.kind {
            Kind::Play(options) => super::ConnectionOptions::Internet(options.clone()),
            Kind::Watch(options) => {
                super::ConnectionOptions::WatchInternet(Box::new(options.clone()))
            }
        }
    }

    pub(super) fn take_spectators(&mut self) -> Vec<SpectatorPeer> {
        std::mem::take(&mut self.shared.lock().unwrap().spectators)
            .into_iter()
            .map(|shared| SpectatorPeer { shared })
            .collect()
    }
}

impl Drop for InternetTransport {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

fn shared_route(shared: &Mutex<Shared>) -> &'static str {
    shared.lock().unwrap().route.unwrap_or("connecting")
}

fn shared_ready(shared: &Mutex<Shared>) -> Result<bool> {
    let shared = shared.lock().unwrap();
    if let Some(error) = &shared.failure {
        if shared.packets.has_incoming() {
            return Ok(shared.ready);
        }
        bail!("{error}");
    }
    Ok(shared.ready)
}

fn shared_send(shared: &Mutex<Shared>, packet: &[u8]) -> Result<bool> {
    let mut shared = shared.lock().unwrap();
    if !shared.ready || shared.failure.is_some() || shared.closed {
        return Ok(false);
    }
    shared.packets.send(packet)
}

impl Transport for InternetTransport {
    fn route(&self) -> &'static str {
        shared_route(&self.shared)
    }
    fn ready(&mut self) -> Result<bool> {
        shared_ready(&self.shared)
    }

    fn receive(&mut self, buffer: &mut [u8]) -> Result<Option<usize>> {
        // Deliver the final acknowledgement before reporting a peer close.
        // The next ready() poll surfaces failure after the queue is drained.
        self.shared.lock().unwrap().packets.receive(buffer)
    }

    fn send(&mut self, packet: &[u8]) -> Result<bool> {
        shared_send(&self.shared, packet)
    }
}

/// The host's handle for one accepted spectator connection. Dropping it
/// closes that connection without touching the players' link.
pub struct SpectatorPeer {
    shared: Arc<Mutex<Shared>>,
}

impl Drop for SpectatorPeer {
    fn drop(&mut self) {
        self.shared.lock().unwrap().closed = true;
    }
}

impl Transport for SpectatorPeer {
    fn route(&self) -> &'static str {
        shared_route(&self.shared)
    }
    fn ready(&mut self) -> Result<bool> {
        shared_ready(&self.shared)
    }
    fn receive(&mut self, buffer: &mut [u8]) -> Result<Option<usize>> {
        self.shared.lock().unwrap().packets.receive(buffer)
    }
    fn send(&mut self, packet: &[u8]) -> Result<bool> {
        shared_send(&self.shared, packet)
    }
}

async fn worker(
    kind: Kind,
    shared: &Arc<Mutex<Shared>>,
    mut cancelled: oneshot::Receiver<()>,
) -> Result<()> {
    let (address, host_key, relay_only) = match &kind {
        Kind::Play(options) => (
            &options.invitation.endpoint,
            options.host_key.clone(),
            options.relay_only,
        ),
        Kind::Watch(options) => (&options.invitation.endpoint, None, options.relay_only),
    };
    let relay_map = address.relay_urls().cloned().collect();
    let config = iroh::endpoint::QuicTransportConfig::builder()
        .datagram_receive_buffer_size(Some(64 * MAX_PACKET))
        .datagram_send_buffer_size(64 * MAX_PACKET)
        .max_concurrent_bidi_streams(1u32.into())
        .max_concurrent_uni_streams(0u32.into())
        .keep_alive_interval(Duration::from_secs(2))
        .build();
    let mut builder = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Custom(relay_map))
        .alpns(vec![ALPN.to_vec()])
        .transport_config(config);
    if let Some(key) = &host_key {
        builder = builder.secret_key(key.clone());
    }
    if relay_only {
        builder = builder.clear_ip_transports();
    }
    let endpoint = tokio::select! {
        _ = &mut cancelled => return Ok(()),
        result = builder.bind() => result.context("Opening Internet netplay endpoint")?,
    };
    let result = tokio::select! {
        _ = &mut cancelled => Ok(()),
        result = async {
            match &kind {
                Kind::Play(options) if options.host_key.is_some() => serve(&endpoint, options, shared).await,
                Kind::Play(options) => {
                    let connection = tokio::time::timeout(SETUP_TIMEOUT, connect(&endpoint, address, &options.invitation.session))
                        .await.context("Internet invitation timed out; start a new session")??;
                    shared.lock().unwrap().ready = true;
                    pump(connection, shared.clone()).await
                }
                Kind::Watch(options) => {
                    let connection = tokio::time::timeout(SETUP_TIMEOUT, connect(&endpoint, address, &options.invitation.capability))
                        .await.context("Spectator invitation timed out; ask the host for a new code")??;
                    shared.lock().unwrap().ready = true;
                    pump(connection, shared.clone()).await
                }
            }
        } => result,
    };
    // A cancelled or failed session releases sockets and relay registration.
    let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.close()).await;
    result
}

/// Move datagrams between one connection and its queues until the peer
/// closes or the emulation thread drops its handle.
async fn pump(connection: iroh::endpoint::Connection, shared: Arc<Mutex<Shared>>) -> Result<()> {
    let result = async {
        let mut tick = tokio::time::interval(Duration::from_millis(2));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut route_tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = route_tick.tick() => {
                    if let Some(path) = connection.paths().iter().find(|path| path.is_selected()) {
                        let route = if path.is_relay() { "relay" } else { "direct" };
                        let mut state = shared.lock().unwrap();
                        if state.route != Some(route) {
                            log::info!("netplay: Internet route is {route}");
                            state.route = Some(route);
                        }
                    }
                }
                packet = connection.read_datagram() => {
                    let packet = packet.context("Internet peer disconnected")?;
                    shared.lock().unwrap().packets.push(&packet)?;
                }
                _ = tick.tick() => {
                    let mut state = shared.lock().unwrap();
                    if state.closed {
                        drop(state);
                        connection.close(0u32.into(), b"done");
                        return Ok(());
                    }
                    while let Some(packet) = state.packets.pop() {
                        connection.send_datagram(packet.into()).context("Sending Internet netplay packet")?;
                    }
                }
            }
        }
    }
    .await;
    if let Err(error) = &result {
        let mut state = shared.lock().unwrap();
        if state.failure.is_none() {
            state.failure = Some(format!("{error:#}"));
        }
    }
    result
}

/// Connect to a host and present a capability over a bounded stream.
async fn connect(
    endpoint: &Endpoint,
    address: &EndpointAddr,
    capability: &[u8; 16],
) -> Result<iroh::endpoint::Connection> {
    let connection = endpoint
        .connect(address.clone(), ALPN)
        .await
        .context("Connecting to host; check that the host has pressed Run")?;
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut send, mut recv) = connection.open_bi().await?;
        send.write_all(capability).await?;
        send.finish()?;
        let reply = recv.read_to_end(1).await?;
        ensure!(reply == [1], "Host rejected the invitation");
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("Host did not accept the invitation")??;
    ensure!(
        connection
            .max_datagram_size()
            .is_some_and(|size| size >= MAX_PACKET),
        "Peer cannot carry netplay packets"
    );
    Ok(connection)
}

enum Admitted {
    Player(iroh::endpoint::Connection),
    Spectator(iroh::endpoint::Connection),
}

/// Accept connections until one presents an admissible capability. Failed,
/// unrelated or currently unwanted handshakes never claim a slot.
async fn admit(
    endpoint: &Endpoint,
    session: &[u8; 16],
    spectator: Option<&[u8; 16]>,
    want_player: bool,
    want_spectator: bool,
) -> Result<Admitted> {
    loop {
        let incoming = endpoint
            .accept()
            .await
            .context("Internet endpoint closed")?;
        let accepted = tokio::time::timeout(Duration::from_secs(5), async {
            let connection = incoming.await?;
            let (send, mut recv) = connection.accept_bi().await?;
            let capability = recv.read_to_end(16).await?;
            Ok::<_, anyhow::Error>((connection, send, capability))
        })
        .await;
        let Ok(Ok((connection, mut send, capability))) = accepted else {
            continue;
        };
        let admitted = if want_player && capability == session {
            Admitted::Player(connection)
        } else if want_spectator && spectator.is_some_and(|c| capability == c) {
            Admitted::Spectator(connection)
        } else {
            connection.close(1u32.into(), b"Invalid invitation");
            continue;
        };
        let connection = match &admitted {
            Admitted::Player(c) | Admitted::Spectator(c) => c,
        };
        let welcomed = async {
            send.write_all(&[1]).await?;
            send.finish()?;
            Ok::<_, anyhow::Error>(())
        }
        .await
        .is_ok()
            && connection
                .max_datagram_size()
                .is_some_and(|size| size >= MAX_PACKET);
        if !welcomed {
            connection.close(1u32.into(), b"Peer cannot carry netplay packets");
            continue;
        }
        return Ok(admitted);
    }
}

/// The host: admit the one player, then keep admitting spectators while the
/// session lasts. Each connection is pumped by its own task; a spectator's
/// failure is confined to its queues.
async fn serve(endpoint: &Endpoint, options: &Options, shared: &Arc<Mutex<Shared>>) -> Result<()> {
    let session = options.invitation.session;
    let spectator = options.spectator_capability;
    let cap = usize::from(options.spectators);
    let mut tasks: JoinSet<(bool, Result<()>)> = JoinSet::new();
    let mut player = false;
    let mut spectators = 0usize;
    let deadline = tokio::time::sleep(SETUP_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline, if !player => {
                bail!("Internet invitation timed out; start a new session");
            }
            admitted = admit(endpoint, &session, spectator.as_ref(), !player, spectators < cap) => {
                match admitted? {
                    Admitted::Player(connection) => {
                        player = true;
                        shared.lock().unwrap().ready = true;
                        let queues = shared.clone();
                        tasks.spawn(async move { (true, pump(connection, queues).await) });
                    }
                    Admitted::Spectator(connection) => {
                        spectators += 1;
                        let queues = Arc::new(Mutex::new(Shared {
                            ready: true,
                            ..Default::default()
                        }));
                        shared.lock().unwrap().spectators.push(queues.clone());
                        log::info!("netplay: spectator connected");
                        tasks.spawn(async move { (false, pump(connection, queues).await) });
                    }
                }
            }
            Some(finished) = tasks.join_next(), if !tasks.is_empty() => {
                match finished {
                    Ok((true, result)) => return result.and_then(|()| bail!("Internet peer disconnected")),
                    Ok((false, result)) => {
                        spectators -= 1;
                        if let Err(error) = result {
                            log::info!("netplay: spectator left: {error:#}");
                        }
                    }
                    Err(error) => bail!("Internet netplay task failed: {error}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_final_packets_are_delivered_before_disconnect() -> Result<()> {
        let mut state = Shared {
            ready: true,
            failure: Some("peer closed".into()),
            ..Default::default()
        };
        state.packets.push(&[1, 2, 3])?;
        let mut transport = InternetTransport {
            kind: Kind::Play(Box::new(Options::host(2, 8, "", false, 0)?)),
            shared: Arc::new(Mutex::new(state)),
            cancel: None,
        };
        assert!(transport.ready()?);
        assert!(!transport.send(&[4])?);
        let mut bytes = [0; 16];
        assert_eq!(transport.receive(&mut bytes)?, Some(3));
        assert_eq!(&bytes[..3], &[1, 2, 3]);
        assert_eq!(transport.receive(&mut bytes)?, None);
        assert!(transport.ready().is_err());
        Ok(())
    }

    #[test]
    fn invitations_keep_host_keys_private_and_validate_routes_and_settings() -> Result<()> {
        let host = Options::host(6, 12, "https://relay.example.com", false, 0)?;
        let code = host.invitation.encode()?;
        let guest = Options::join(&code, true)?;
        assert_eq!(guest.settings().player, 1);
        assert_eq!(guest.role(), Role::Guest);
        assert_eq!(host.role(), Role::Host);
        assert_eq!(guest.invitation.session, host.invitation.session);
        assert_eq!(
            guest.invitation.endpoint.id,
            host.host_key.as_ref().unwrap().public()
        );
        assert!(guest.host_key.is_none());
        assert_eq!(guest.settings().input_delay, 6);
        assert_eq!(guest.settings().rollback_frames, 12);
        assert!(host.spectator_invitation().is_none());
        let decoded = URL_SAFE_NO_PAD.decode(code.strip_prefix(PREFIX).unwrap())?;
        assert!(!String::from_utf8(decoded)?.contains("host_key"));
        for bad in ["", "CLNP1.bad", "CLNI1.bad", &"X".repeat(CODE_LIMIT + 1)] {
            assert!(Invitation::decode(bad).is_err());
        }
        for bad in [
            "http://relay.example.com",
            "https://user:secret@relay.example.com",
            "https://relay.example.com?token=1",
            "https://relay.example.com#fragment",
        ] {
            assert!(Options::host(2, 8, bad, false, 0).is_err());
        }
        let mut invalid = host.clone();
        invalid.invitation.delay = 7;
        assert!(invalid.validate().is_err());
        invalid = host.clone();
        invalid.invitation.window = 0;
        assert!(invalid.validate().is_err());
        invalid = host.clone();
        invalid.spectators = 1;
        assert!(invalid.validate().is_err(), "spectators need a capability");
        invalid = host;
        invalid.host_key = Some(SecretKey::generate());
        assert!(invalid.validate().is_err());
        Ok(())
    }

    #[test]
    fn spectator_invitations_carry_their_own_capability() -> Result<()> {
        let host = Options::host(2, 8, "https://relay.example.com", false, 3)?;
        let spectator = host.spectator_invitation().unwrap();
        let code = spectator.encode()?;
        assert!(SpectatorInvitation::is_code(&code));
        assert!(!SpectatorInvitation::is_code(&host.invitation.encode()?));
        let watch = SpectatorOptions::watch(&code, true)?;
        assert_eq!(watch.invitation.endpoint.id, host.invitation.endpoint.id);
        assert_ne!(watch.invitation.capability, host.invitation.session);
        assert!(watch.validate().is_ok());
        let decoded = URL_SAFE_NO_PAD.decode(code.strip_prefix(SPECTATOR_PREFIX).unwrap())?;
        let text = String::from_utf8(decoded)?;
        assert!(!text.contains("host_key") && !text.contains("session"));
        assert!(
            Options::join(&code, false).is_err(),
            "a spectator code is not a player invitation"
        );
        assert!(SpectatorOptions::watch(&host.invitation.encode()?, false).is_err());
        assert!(Options::host(2, 8, "", false, 9).is_err());
        let mut invalid = Options::host(2, 8, "", false, 0)?;
        invalid.spectator_capability = Some([1; 16]);
        assert!(invalid.validate().is_err());
        Ok(())
    }

    #[test]
    fn encrypted_loopback_admits_player_and_spectator_by_capability() -> Result<()> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(30), async {
                    let host_ep = Endpoint::builder(presets::Minimal)
                        .relay_mode(RelayMode::Disabled)
                        .alpns(vec![ALPN.to_vec()])
                        .clear_ip_transports()
                        .bind_addr("127.0.0.1:0")?
                        .bind()
                        .await?;
                    let client_ep = Endpoint::builder(presets::Minimal)
                        .relay_mode(RelayMode::Disabled)
                        .clear_ip_transports()
                        .bind_addr("127.0.0.1:0")?
                        .bind()
                        .await?;
                    let mut host = Options::host(0, 8, "", false, 1)?;
                    host.host_key = Some(host_ep.secret_key().clone());
                    host.invitation.endpoint = host_ep.addr();
                    host.validate()?;
                    let session = host.invitation.session;
                    let capability = host.spectator_capability.unwrap();
                    let address = host.invitation.endpoint.clone();
                    let (accepted, joined) = tokio::join!(
                        admit(&host_ep, &session, Some(&capability), true, true),
                        async {
                            let mut wrong = session;
                            wrong[0] ^= 1;
                            assert!(connect(&client_ep, &address, &wrong).await.is_err());
                            connect(&client_ep, &address, &session).await
                        }
                    );
                    let Admitted::Player(accepted) = accepted? else {
                        panic!("the session capability admits the player");
                    };
                    let joined = joined?;
                    let packet = vec![0x5a; MAX_PACKET];
                    joined.send_datagram(packet.clone().into())?;
                    assert_eq!(accepted.read_datagram().await?.as_ref(), packet);
                    accepted.send_datagram(vec![0xa5; MAX_PACKET].into())?;
                    assert_eq!(
                        joined.read_datagram().await?.as_ref(),
                        vec![0xa5; MAX_PACKET]
                    );
                    // With the player slot taken, only the spectator
                    // capability is admitted; the session code no longer is.
                    let (admitted, watching) = tokio::join!(
                        admit(&host_ep, &session, Some(&capability), false, true),
                        async {
                            assert!(connect(&client_ep, &address, &session).await.is_err());
                            connect(&client_ep, &address, &capability).await
                        }
                    );
                    let Admitted::Spectator(spectator) = admitted? else {
                        panic!("the spectator capability admits a spectator");
                    };
                    let watching = watching?;
                    spectator.send_datagram(vec![7; 8].into())?;
                    assert_eq!(watching.read_datagram().await?.as_ref(), vec![7; 8]);
                    watching.close(0u32.into(), b"done");
                    assert!(spectator.read_datagram().await.is_err());
                    // The player link is unaffected by the spectator's close.
                    joined.send_datagram(vec![1; 4].into())?;
                    assert_eq!(accepted_read(&accepted).await?, vec![1; 4]);
                    joined.close(0u32.into(), b"done");
                    assert!(accepted.read_datagram().await.is_err());
                    host_ep.close().await;
                    client_ep.close().await;
                    Ok::<_, anyhow::Error>(())
                })
                .await
                .context("Loopback connection timed out")?
            })
    }

    async fn accepted_read(connection: &iroh::endpoint::Connection) -> Result<Vec<u8>> {
        Ok(connection.read_datagram().await?.to_vec())
    }
}
