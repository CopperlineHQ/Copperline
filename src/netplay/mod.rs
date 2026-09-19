// SPDX-License-Identifier: GPL-3.0-or-later

//! GGPO-style netplay for two to four players. Gameplay exchanges inputs and
//! state digests; desktop setup and floppy changes also transfer the host's
//! settings and media.
//!
//! Each player owns one controller port, so a session with the four-player
//! adapter fitted carries ports 3 and 4 as well. Players past the second
//! connect to the host, which relays every port's input to every other
//! player: guests never address each other.

#[cfg(not(target_arch = "wasm32"))]
mod control;
#[cfg(not(target_arch = "wasm32"))]
mod desktop;
#[cfg(all(feature = "netplay-internet", not(target_arch = "wasm32")))]
pub mod internet;
mod rollback;
#[cfg(not(target_arch = "wasm32"))]
mod setup;
#[cfg(not(target_arch = "wasm32"))]
pub use setup::guest_config;
pub mod spectate;
pub use spectate::{Feed, FeedCursor, Spectator, SwapRecord};
#[cfg(test)]
mod tests;
mod transport;
mod wire;
#[cfg(not(target_arch = "wasm32"))]
use transport::{NativeTransport, UdpTransport};
pub use transport::{PacketQueue, Transport};
/// Fixed wire layout, also exposed to browser glue for compatibility checks.
pub use wire::{HEADER as PACKET_HEADER, INPUT_RECORD, MAX_PACKET, VERSION as PROTOCOL_VERSION};
/// Default seed for a fitted clock in deterministic netplay sessions.
pub const RTC_SEED: u64 = 946684800;
/// Controller ports a session can drive: the two game ports plus the
/// four-player adapter's two sockets.
pub const MAX_PLAYERS: usize = crate::bus::PORT_COUNT;

use crate::emulator::Emulator;
use crate::timebase::{Duration, Instant};
use anyhow::{ensure, Context, Result};
use rollback::{Machine, Rollback};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
#[cfg(not(target_arch = "wasm32"))]
use std::net::SocketAddr;

/// Controller buttons, relative mouse motion and held keys for one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Input {
    /// Up, down, left, right, red, blue, play, rewind, forward, green, yellow.
    pub buttons: u16,
    pub keys: [u8; 16],
    pub mouse_dx: i16,
    pub mouse_dy: i16,
    /// Left, right and middle mouse buttons.
    pub mouse_buttons: u8,
}

impl Input {
    pub const BUTTONS: u16 = 0x7ff;

    pub fn set_mouse_button(&mut self, button: u8, pressed: bool) {
        if button < 3 {
            let mask = 1 << button;
            self.mouse_buttons = (self.mouse_buttons & !mask) | (u8::from(pressed) << button);
        }
    }

    fn without_motion(mut self) -> Self {
        self.mouse_dx = 0;
        self.mouse_dy = 0;
        self
    }

    /// Direction switches, red/fire and blue/second button, in wire order.
    pub fn set_joystick(&mut self, held: [bool; 6]) {
        self.buttons = (self.buttons & !0x3f) | Self::pack_buttons(held);
    }

    /// Play, rewind, forward, green and yellow, in wire order.
    pub fn set_cd32_buttons(&mut self, held: [bool; 5]) {
        self.buttons = (self.buttons & 0x3f) | (Self::pack_buttons(held) << 6);
    }

    fn pack_buttons<const N: usize>(held: [bool; N]) -> u16 {
        held.into_iter()
            .enumerate()
            .fold(0, |bits, (bit, on)| bits | (u16::from(on) << bit))
    }

    pub fn set_key(&mut self, key: u8, pressed: bool) {
        if key >= 128 {
            return;
        }
        let mask = 1 << (key % 8);
        if pressed {
            self.keys[usize::from(key / 8)] |= mask;
        } else {
            self.keys[usize::from(key / 8)] &= !mask;
        }
    }

    /// A key is held while any player holds it.
    pub(crate) fn merged_keys(inputs: &[Self]) -> [u8; 16] {
        inputs.iter().fold([0; 16], |mut keys, input| {
            for (byte, held) in keys.iter_mut().zip(input.keys) {
                *byte |= held;
            }
            keys
        })
    }
}

/// Frontend-held controls and unsampled motion, outside the wire timeline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalInput {
    pub held: Input,
    pub mouse_pending: (i32, i32),
}

impl LocalInput {
    pub fn add_mouse_delta(&mut self, dx: i32, dy: i32) {
        self.mouse_pending.0 = self.mouse_pending.0.saturating_add(dx);
        self.mouse_pending.1 = self.mouse_pending.1.saturating_add(dy);
    }

    fn sample(&self) -> Input {
        // Stay below the signed wrap limit of the 8-bit JOYDAT counters.
        Input {
            mouse_dx: self.mouse_pending.0.clamp(-100, 100) as i16,
            mouse_dy: self.mouse_pending.1.clamp(-100, 100) as i16,
            ..self.held
        }
    }
}

impl From<Input> for LocalInput {
    fn from(input: Input) -> Self {
        Self {
            held: input.without_motion(),
            mouse_pending: (i32::from(input.mouse_dx), i32::from(input.mouse_dy)),
        }
    }
}

/// Whether a pasted code is an Internet spectator invitation.
pub fn is_spectator_code(code: &str) -> bool {
    code.trim().starts_with("CLNS1.")
}

/// Decode the shared game identifier used by both CLI and GUI setup.
pub fn parse_session_id(code: &str) -> Result<[u8; 16]> {
    ensure!(code.len() == 32 && code.bytes().all(|b| b.is_ascii_hexdigit()),
        "Session code needs exactly 32 hexadecimal digits; create a new code or paste your peer's code");
    let mut session = [0; 16];
    for (i, byte) in session.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&code[i * 2..i * 2 + 2], 16)?;
    }
    Ok(session)
}

/// What a participant contributes to the session. Players own a controller
/// port; the host also serves any spectators, who own nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Host,
    Guest,
    Spectator,
}

/// Negotiated timeline settings, shared by every transport.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Zero-based controller port owned by this peer. Player 1 (index 0)
    /// hosts; ports 3 and 4 are the four-player adapter's sockets.
    pub player: usize,
    /// Controller ports the session drives, 2..=[`MAX_PLAYERS`].
    pub players: usize,
    pub session: [u8; 16],
    pub input_delay: u8,
    pub rollback_frames: u8,
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (2..=MAX_PLAYERS).contains(&self.players),
            "netplay needs 2 to {MAX_PLAYERS} players"
        );
        ensure!(
            self.player < self.players,
            "netplay player must be 1..{}",
            self.players
        );
        ensure!(
            self.input_delay <= 6,
            "netplay input delay must be 0..6 frames"
        );
        ensure!(
            (1..=12).contains(&self.rollback_frames),
            "netplay rollback window must be 1..12 frames"
        );
        Ok(())
    }

    /// The host owns port 1 and relays for everybody else.
    pub fn hosting(&self) -> bool {
        self.player == 0
    }

    /// Links this peer holds once the session is complete: a guest talks
    /// only to the host, and the host to every guest.
    pub fn expected_links(&self) -> usize {
        if self.hosting() {
            self.players - 1
        } else {
            1
        }
    }
}

/// Direct UDP endpoints. A guest names the host; a host may name the guests
/// it will accept, or leave the list empty to admit any source presenting
/// the session ID, as it already does for spectators.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct Options {
    pub bind: SocketAddr,
    pub peers: Vec<SocketAddr>,
    /// Zero-based controller port owned by this peer.
    pub player: usize,
    /// Controller ports the session drives, 2..=[`MAX_PLAYERS`].
    pub players: usize,
    pub session: [u8; 16],
    pub input_delay: u8,
    pub rollback_frames: u8,
    /// Spectators the host admits on the same socket (0 = none).
    pub spectators: u8,
}

/// A spectator of a direct UDP session addresses the host directly.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct WatchOptions {
    pub bind: SocketAddr,
    pub host: SocketAddr,
    pub session: [u8; 16],
}

#[cfg(not(target_arch = "wasm32"))]
impl WatchOptions {
    pub fn validate(&self) -> Result<()> {
        validate_peer(self.bind, self.host)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn validate_peer(bind: SocketAddr, peer: SocketAddr) -> Result<()> {
    ensure!(
        bind.is_ipv4() == peer.is_ipv4(),
        "netplay addresses must use the same IP family"
    );
    ensure!(
        peer.port() != 0 && !peer.ip().is_unspecified() && !peer.ip().is_multicast(),
        "netplay peer must be a unicast address with a nonzero port"
    );
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub enum ConnectionOptions {
    Direct(Options),
    #[cfg(feature = "netplay-internet")]
    Internet(Box<internet::Options>),
    Watch(WatchOptions),
    #[cfg(feature = "netplay-internet")]
    WatchInternet(Box<internet::SpectatorOptions>),
}

#[cfg(not(target_arch = "wasm32"))]
impl From<Options> for ConnectionOptions {
    fn from(options: Options) -> Self {
        Self::Direct(options)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<WatchOptions> for ConnectionOptions {
    fn from(options: WatchOptions) -> Self {
        Self::Watch(options)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ConnectionOptions {
    /// Timeline settings of a player; spectators negotiate none.
    pub fn settings(&self) -> Option<Settings> {
        match self {
            Self::Direct(options) => Some(options.settings()),
            #[cfg(feature = "netplay-internet")]
            Self::Internet(options) => Some(options.settings()),
            Self::Watch(_) => None,
            #[cfg(feature = "netplay-internet")]
            Self::WatchInternet(_) => None,
        }
    }

    /// Whether this peer was configured for a particular controller port.
    /// Direct-IP guests name their player number; an Internet guest takes
    /// whichever port the host still has free.
    pub fn names_player(&self) -> bool {
        matches!(self, Self::Direct(_))
    }

    pub fn role(&self) -> Role {
        match self {
            Self::Direct(options) => options.role(),
            #[cfg(feature = "netplay-internet")]
            Self::Internet(options) => options.role(),
            Self::Watch(_) => Role::Spectator,
            #[cfg(feature = "netplay-internet")]
            Self::WatchInternet(_) => Role::Spectator,
        }
    }

    /// Whether these options host an Internet session, the only kind that
    /// writes a spectator invitation file.
    pub fn hosts_internet(&self) -> bool {
        match self {
            #[cfg(feature = "netplay-internet")]
            Self::Internet(options) => options.host_key.is_some(),
            _ => false,
        }
    }

    /// Spectators a hosting player admits.
    pub fn spectators(&self) -> usize {
        match self {
            Self::Direct(options) if options.settings().hosting() => {
                usize::from(options.spectators)
            }
            #[cfg(feature = "netplay-internet")]
            Self::Internet(options) if options.host_key.is_some() => {
                usize::from(options.spectators)
            }
            _ => 0,
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Direct(options) => options.validate(),
            #[cfg(feature = "netplay-internet")]
            Self::Internet(options) => options.validate(),
            Self::Watch(options) => options.validate(),
            #[cfg(feature = "netplay-internet")]
            Self::WatchInternet(options) => options.validate(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Options {
    pub fn settings(&self) -> Settings {
        Settings {
            player: self.player,
            players: self.players,
            session: self.session,
            input_delay: self.input_delay,
            rollback_frames: self.rollback_frames,
        }
    }
    pub fn role(&self) -> Role {
        if self.player == 0 {
            Role::Host
        } else {
            Role::Guest
        }
    }
    /// The one endpoint a guest sends to.
    pub fn host_address(&self) -> Option<SocketAddr> {
        (!self.settings().hosting()).then(|| self.peers[0])
    }
    pub fn validate(&self) -> Result<()> {
        let settings = self.settings();
        settings.validate()?;
        ensure!(
            usize::from(self.spectators) <= spectate::MAX_SPECTATORS
                && (self.spectators == 0 || settings.hosting()),
            "netplay spectators are served by player 1, up to {}",
            spectate::MAX_SPECTATORS
        );
        if settings.hosting() {
            ensure!(
                self.peers.len() <= settings.expected_links(),
                "a netplay host lists at most one peer address per guest"
            );
        } else {
            ensure!(
                self.peers.len() == 1,
                "a netplay guest needs the host's peer address"
            );
        }
        for peer in &self.peers {
            validate_peer(self.bind, *peer)?;
        }
        Ok(())
    }
}

pub(crate) fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Validate static host dependencies before building or connecting a machine.
pub fn validate_config(cfg: &crate::config::Config) -> Result<()> {
    let mut dependencies = cfg.clone();
    if cfg.netplay_storage {
        dependencies.ide.master = None;
        dependencies.ide.slave = None;
        dependencies.scsi.units.fill(None);
        dependencies.lide.drives.fill(None);
    }
    if let Some(reason) = dependencies.runahead_machine_block_reason() {
        anyhow::bail!("netplay cannot use {reason}");
    }
    ensure!(
        cfg.run_program_dir.is_none() && !cfg.emulation.uaelib_files,
        "netplay cannot use host file commands"
    );
    ensure!(cfg.cd_image_path.is_none(), "netplay cannot use CD images");
    ensure!(
        [cfg.ide.master.as_ref(), cfg.ide.slave.as_ref()]
            .into_iter()
            .chain(cfg.scsi.units.iter().map(Option::as_ref))
            .chain(cfg.lide.drives.iter().map(Option::as_ref))
            .flatten()
            .all(|drive| !crate::config::is_cd_image_path(&drive.path)),
        "netplay cannot use ATAPI or SCSI CD images"
    );
    // The sampler attaches in the frontend after session construction; reject
    // it here, before fingerprinting or opening any parallel host device. The
    // four-player adapter is passive wiring with no host peripheral behind it,
    // so it stays: ports 3 and 4 are exactly what a multitap session plays on.
    ensure!(
        matches!(
            cfg.parallel.device,
            crate::config::ParallelDevice::None | crate::config::ParallelDevice::JoystickAdapter
        ),
        "netplay requires the parallel port device to be none or the four-player adapter"
    );
    // Its rate-specific resamplers serialize from a randomized HashMap, so
    // equivalent boards cannot yet guarantee byte-identical checkpoints.
    ensure!(!cfg.toccata, "netplay cannot use the Toccata sound board");
    // A card image is already a "hard-drive image" above, but a ROM-only
    // board still autoconfigs on the chain, and the `Hardware` manifest
    // records neither the board nor its ROM (which is the SF2000 firmware
    // author's, not ours to bundle) -- the peer would build a machine
    // without it and diverge from the first frame.
    ensure!(
        !cfg.sf2000sd.enabled(),
        "netplay cannot use the SF2000 SD card controller"
    );
    ensure!(
        !cfg.cpu_jit
            && cfg.emulation.power_on
            && !cfg.emulation.rewind
            && cfg.emulation.run_ahead_frames == 0,
        "netplay requires power on, interpreter execution, rewind off and run-ahead off"
    );
    ensure!(
        !cfg.emulation.warp_boot && cfg.emulation.warp_until.is_none(),
        "netplay cannot use warp boot"
    );
    ensure!(
        matches!(cfg.serial.mode, crate::config::SerialMode::Off),
        "netplay requires --serial off"
    );
    ensure!(
        cfg.floppy.bridges.iter().all(Option::is_none),
        "netplay cannot use physical floppy drives"
    );
    Ok(())
}

/// Apply the deterministic clock default before constructing a netplay machine.
pub fn prepare_config(cfg: &mut crate::config::Config) -> Result<()> {
    #[cfg(not(target_arch = "wasm32"))]
    setup::prepare_sources(cfg)?;
    validate_config(cfg)?;
    if cfg.rtc_present && cfg.rtc_seed_unix.is_none() {
        cfg.rtc_seed_unix = Some(RTC_SEED);
        log::info!("netplay: guest clock starts at 2000-01-01 00:00:00 UTC");
    }
    Ok(())
}

/// A session is serviced on the emulation thread; socket I/O never blocks it.
#[cfg(not(target_arch = "wasm32"))]
pub use desktop::Session;

/// One peer's link. A guest holds a single link to the host; the host holds
/// one per guest and relays between them.
struct Link<T> {
    player: usize,
    transport: T,
    /// A valid packet has arrived from this peer.
    seen: bool,
    /// The peer reports that it has everyone it is waiting for.
    ready: bool,
    /// What this peer still needs from each player.
    acks: [u64; MAX_PLAYERS],
    last_received: Instant,
    last_sent: Option<Instant>,
}

/// Packets one service call may send to one link. Redundant unacknowledged
/// inputs for three other ports can outgrow a single datagram.
const MAX_SEND_PACKETS: usize = 4;

pub struct Connection<T: Transport> {
    links: Vec<Link<T>>,
    settings: Settings,
    identity: [u8; 32],
    rollback: Rollback,
    connected: bool,
    started: Instant,
    peer_hashes: BTreeMap<(u64, usize), [u8; 32]>,
    last_checked: u64,
    failure: Option<String>,
    feed: Option<Feed>,
}

#[derive(Clone, Copy, Debug)]
pub struct Status {
    pub connected: bool,
    pub frame: u64,
    pub confirmed_frame: u64,
    /// All local inputs below this frame have reached every peer.
    pub acknowledged_frame: u64,
    pub rollbacks: u64,
    pub replayed_frames: u64,
    pub checked_frame: u64,
    /// Confirmed host frames a spectator has yet to execute.
    pub behind: u64,
}

impl Status {
    /// A capture may end this process, so every peer needs its frame's inputs.
    pub fn ready_to_capture(&self) -> bool {
        self.connected
            && self.frame == self.confirmed_frame
            && self.frame <= self.acknowledged_frame
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Connection<NativeTransport> {
    pub fn new(
        options: impl Into<ConnectionOptions>,
        emu: &mut Emulator,
        cfg: &crate::config::Config,
    ) -> Result<Self> {
        let (settings, transport) = match options.into() {
            ConnectionOptions::Direct(options) => {
                options.validate()?;
                ensure!(
                    options.players == 2 && options.peers.len() == 1,
                    "a session with more than two players is coordinated by its host"
                );
                (
                    options.settings(),
                    NativeTransport::Udp(UdpTransport::connect(options)?),
                )
            }
            #[cfg(feature = "netplay-internet")]
            ConnectionOptions::Internet(options) => (
                options.settings(),
                NativeTransport::Internet(Box::new(internet::InternetTransport::new(*options)?)),
            ),
            ConnectionOptions::Watch(_) => {
                anyhow::bail!("spectators follow the host's timeline instead of running one")
            }
            #[cfg(feature = "netplay-internet")]
            ConnectionOptions::WatchInternet(_) => {
                anyhow::bail!("spectators follow the host's timeline instead of running one")
            }
        };
        Self::with_transport(settings, transport, emu, cfg)
    }

    pub fn options(&self) -> ConnectionOptions {
        self.links[0].transport.options()
    }
}

fn initial_identity(
    settings: &Settings,
    emu: &mut Emulator,
    cfg: &crate::config::Config,
) -> Result<[u8; 32]> {
    settings.validate()?;
    let identity = machine_identity(emu, cfg)?;
    validate_player_ports(emu, settings.players)?;
    Ok(identity)
}

/// Every player needs a controller its inputs can reach. Ports 3 and 4 are
/// sockets on the passive four-player adapter, so the adapter must be
/// plugged in with a joystick in each socket the session uses.
pub fn validate_player_ports(emu: &Emulator, players: usize) -> Result<()> {
    for player in crate::bus::PARALLEL_PORT_FIRST..players {
        ensure!(
            emu.bus().input.device(player) == crate::bus::PortDevice::Joystick,
            "netplay player {} needs a joystick in the four-player adapter's socket; set port{} = joystick",
            player + 1,
            player + 1
        );
    }
    Ok(())
}

/// Fingerprint a cold machine every participant must reproduce exactly:
/// the build plus the complete normalized initial snapshot.
pub fn machine_identity(emu: &mut Emulator, cfg: &crate::config::Config) -> Result<[u8; 32]> {
    validate_config(cfg)?;
    ensure!(
        emu.bus().emulated_cck() == 0,
        "netplay must start before the machine runs"
    );
    // Paths are host metadata; normalize only after adopting the complete
    // images into memory so replay cannot reopen or overwrite local files.
    emu.bus_mut().floppy.prepare_netplay_images();
    if let Some(reason) = emu
        .bus()
        .runahead_host_block_reason()
        .or_else(|| emu.machine.runahead_debug_block_reason())
    {
        anyhow::bail!("netplay cannot use {reason}");
    }
    ensure!(
        !emu.time_travel_enabled(),
        "netplay cannot record reverse history"
    );
    ensure!(
        emu.bus().input.ports.iter().all(|p| matches!(
            p.device,
            crate::bus::PortDevice::Mouse
                | crate::bus::PortDevice::Joystick
                | crate::bus::PortDevice::Cd32Pad
        )),
        "netplay requires mouse, joystick or CD32 controllers on both game ports"
    );
    let mut identity_hash = Sha256::new();
    identity_hash.update(env!("COPPERLINE_DISPLAY_VERSION").as_bytes());
    identity_hash.update(emu.netplay_snapshot()?);
    Ok(identity_hash.finalize().into())
}

impl<T: Transport> Connection<T> {
    pub fn route(&self) -> &'static str {
        self.links
            .first()
            .map_or("connecting", |link| link.transport.route())
    }

    /// A peer whose links are added as the other players arrive.
    pub fn without_links(
        settings: Settings,
        emu: &mut Emulator,
        cfg: &crate::config::Config,
    ) -> Result<Self> {
        let identity = initial_identity(&settings, emu, cfg)?;
        let rollback = Rollback::new(
            settings.player,
            settings.players,
            settings.input_delay,
            settings.rollback_frames,
        );
        Ok(Self {
            links: Vec::new(),
            settings,
            identity,
            rollback,
            connected: false,
            started: Instant::now(),
            peer_hashes: BTreeMap::new(),
            last_checked: 0,
            failure: None,
            feed: None,
        })
    }

    /// A complete two-player session, or a guest's single link to the host.
    pub fn with_transport(
        settings: Settings,
        transport: T,
        emu: &mut Emulator,
        cfg: &crate::config::Config,
    ) -> Result<Self> {
        let peer = if settings.hosting() { 1 } else { 0 };
        ensure!(
            !settings.hosting() || settings.players == 2,
            "a host with more than two players holds one link per guest"
        );
        let mut connection = Self::without_links(settings, emu, cfg)?;
        connection.add_link(peer, transport)?;
        Ok(connection)
    }

    /// Admit another player's link. Every player must be present before the
    /// first frame: the timeline has no way to seat a latecomer.
    pub fn add_link(&mut self, player: usize, transport: T) -> Result<()> {
        ensure!(
            self.rollback.current == 0,
            "netplay players must all join before the game starts"
        );
        ensure!(
            player < self.settings.players && player != self.settings.player,
            "netplay player {} is not part of this session",
            player + 1
        );
        ensure!(
            !self.links.iter().any(|link| link.player == player),
            "netplay port {} already has a player",
            player + 1
        );
        ensure!(
            self.links.len() < self.settings.expected_links(),
            "netplay session is full"
        );
        let now = Instant::now();
        self.links.push(Link {
            player,
            transport,
            seen: false,
            ready: false,
            acks: [0; MAX_PLAYERS],
            last_received: now,
            last_sent: None,
        });
        Ok(())
    }

    /// Ports still waiting for a player, lowest first.
    pub fn free_players(&self) -> impl Iterator<Item = usize> + use<'_, T> {
        (0..self.settings.players).filter(|player| {
            *player != self.settings.player && !self.links.iter().any(|link| link.player == *player)
        })
    }

    pub fn link_mut(&mut self, player: usize) -> Option<&mut T> {
        self.links
            .iter_mut()
            .find(|link| link.player == player)
            .map(|link| &mut link.transport)
    }

    pub fn link(&self, player: usize) -> Option<&T> {
        self.links
            .iter()
            .find(|link| link.player == player)
            .map(|link| &link.transport)
    }

    /// Every peer's link, in the order they joined.
    pub fn links_mut(&mut self) -> impl Iterator<Item = (usize, &mut T)> {
        self.links
            .iter_mut()
            .map(|link| (link.player, &mut link.transport))
    }

    pub fn transports(&self) -> impl Iterator<Item = (usize, &T)> {
        self.links.iter().map(|link| (link.player, &link.transport))
    }

    /// The only link of a two-player peer, which the browser owns directly.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.links[0].transport
    }

    pub fn player(&self) -> usize {
        self.settings.player
    }

    pub fn players(&self) -> usize {
        self.settings.players
    }

    pub fn identity(&self) -> [u8; 32] {
        self.identity
    }

    /// Retain the confirmed history for spectators. Must precede the first
    /// frame so late joiners can replay from cold boot.
    pub fn enable_feed(&mut self, limit: usize) -> Result<()> {
        ensure!(
            self.rollback.current == 0 && self.rollback.confirmed == 0,
            "spectator history must start at frame zero"
        );
        self.rollback.log = Some(Default::default());
        self.feed = Some(Feed::new(limit, self.settings.players));
        Ok(())
    }

    pub fn feed(&self) -> Option<&Feed> {
        self.feed.as_ref()
    }

    /// Record a media change applied at the current confirmed boundary.
    pub fn feed_swap(&mut self, swap: SwapRecord) -> Result<()> {
        self.drain_feed()?;
        match &mut self.feed {
            Some(feed) => feed.record_swap(swap),
            None => Ok(()),
        }
    }

    fn drain_feed(&mut self) -> Result<()> {
        if let (Some(log), Some(feed)) = (&mut self.rollback.log, &mut self.feed) {
            for (frame, inputs) in log.inputs.drain(..) {
                feed.record_frame(frame, inputs)?;
            }
            for (frame, hash) in log.hashes.drain(..) {
                feed.record_checkpoint(frame, hash)?;
            }
        }
        Ok(())
    }

    pub fn status(&self) -> Status {
        Status {
            connected: self.connected,
            frame: self.rollback.current,
            confirmed_frame: self.rollback.confirmed,
            acknowledged_frame: self.rollback.ack_frontier(),
            rollbacks: self.rollback.rollbacks,
            replayed_frames: self.rollback.replayed_frames,
            checked_frame: self.last_checked,
            behind: 0,
        }
    }

    /// A transport-coordinated media change may only touch a fully confirmed
    /// boundary. No retained prediction can subsequently restore older media.
    pub fn confirmed_state_digest(&self, emu: &Emulator) -> Result<[u8; 32]> {
        ensure!(self.failure.is_none(), "netplay session has failed");
        ensure!(
            self.status().ready_to_capture(),
            "netplay frame is not confirmed"
        );
        Ok(digest(&emu.netplay_snapshot()?))
    }

    /// Poll, repair late input, and optionally advance a frame. `false` is a
    /// normal wait for handshake/input. Continue polling while waiting.
    pub fn step(&mut self, emu: &mut Emulator, input: Input, advance: bool) -> Result<bool> {
        self.step_local(emu, &mut input.into(), advance)
    }

    /// Consume mouse motion only when a new local frame is sampled. Motion
    /// arriving during a handshake or a repeated stalled poll stays pending.
    pub fn step_local(
        &mut self,
        emu: &mut Emulator,
        input: &mut LocalInput,
        advance: bool,
    ) -> Result<bool> {
        if let Some(error) = &self.failure {
            anyhow::bail!("{error}");
        }
        let result = self.step_inner(emu, input, advance);
        if let Err(error) = &result {
            self.failure = Some(format!("{error:#}"));
        }
        result
    }

    /// This peer has every link it is waiting for and has heard from each.
    fn locally_ready(&self) -> bool {
        self.links.len() == self.settings.expected_links()
            && self.links.iter().all(|link| link.seen)
    }

    fn step_inner(
        &mut self,
        emu: &mut Emulator,
        input: &mut LocalInput,
        advance: bool,
    ) -> Result<bool> {
        let mut ready = self.links.len() == self.settings.expected_links();
        for link in &mut self.links {
            if !link.transport.ready()? {
                ready = false;
            }
        }
        if !ready {
            self.started = Instant::now();
            for link in &mut self.links {
                link.last_received = self.started;
            }
            return Ok(false);
        }
        self.receive_packets(emu)?;
        ensure!(
            input.held.buttons & !Input::BUTTONS == 0 && input.held.mouse_buttons & !7 == 0,
            "invalid local netplay controller input"
        );
        let now = Instant::now();
        if self.connected {
            for link in &self.links {
                ensure!(
                    now.duration_since(link.last_received) < Duration::from_secs(10),
                    "netplay player {} timed out",
                    link.player + 1
                );
            }
        } else {
            ensure!(
                now.duration_since(self.started) < Duration::from_secs(60),
                "netplay peer timed out"
            );
        }
        let mut stepped = false;
        let sampled = input.sample();
        if self.connected && advance {
            if self.rollback.submit_local(sampled) {
                input.mouse_pending.0 -= i32::from(sampled.mouse_dx);
                input.mouse_pending.1 -= i32::from(sampled.mouse_dy);
            }
            // Send sampled input before replay, emulation, or pacing can add
            // another frame of avoidable network latency.
            self.send_packets(true)?;
        }
        if self.connected {
            self.update_relay_floor();
            let mut machine = EmulatedMachine(emu);
            self.rollback.reconcile(&mut machine)?;
            if advance {
                stepped = self.rollback.advance(&mut machine, sampled)?;
            }
            self.drain_feed()?;
            for (&(frame, player), expected) in &self.peer_hashes {
                if let Some(actual) = self.rollback.hashes.get(&frame) {
                    ensure!(
                        expected == actual,
                        "netplay desynchronized with player {} at confirmed frame {frame}",
                        player + 1
                    );
                    self.last_checked = self.last_checked.max(frame);
                }
            }
            let checked = self.last_checked;
            self.peer_hashes.retain(|(frame, _), _| *frame > checked);
        }
        self.send_packets(!advance)?;
        Ok(stepped)
    }

    fn receive_packets(&mut self, emu: &mut Emulator) -> Result<()> {
        // A finite receive budget keeps window input responsive under a burst.
        let mut buffer = [0; wire::MAX_PACKET + 1];
        for index in 0..self.links.len() {
            for _ in 0..64 {
                match self.links[index].transport.receive(&mut buffer) {
                    Ok(Some(len)) => {
                        let Some(bytes) = buffer.get(..len) else {
                            continue;
                        };
                        wire::Packet::check_version(bytes, &self.settings.session)?;
                        let Some(packet) = wire::Packet::decode(bytes) else {
                            continue;
                        };
                        if packet.session != self.settings.session {
                            continue;
                        }
                        self.accept(emu, index, packet)?;
                    }
                    Ok(None) => break,
                    Err(e) => return Err(e).context("receiving netplay input"),
                }
            }
        }
        Ok(())
    }

    fn accept(&mut self, emu: &mut Emulator, index: usize, packet: wire::Packet) -> Result<()> {
        let player = self.links[index].player;
        ensure!(
            packet.player == player,
            "netplay packet claims port {} on player {}'s link",
            packet.player + 1,
            player + 1
        );
        ensure!(packet.players == self.settings.players && packet.delay == self.settings.input_delay && packet.window == self.settings.rollback_frames,
            "netplay settings differ: use the same number of players and identical delay/rollback values");
        ensure!(packet.identity == self.identity, "netplay initial machine mismatch: use the same build, ROM, disks, floppy sounds and deterministic machine settings");
        self.links[index].seen = true;
        self.links[index].ready = packet.ready;
        self.links[index].acks = packet.acks;
        self.links[index].last_received = Instant::now();
        self.rollback
            .acknowledge(player, packet.acks[self.settings.player])?;
        for (source, frame, input) in packet.inputs {
            // A guest reports only its own port; the host relays every other
            // port to it and never echoes back what it just sent.
            if self.settings.hosting() {
                ensure!(
                    source == player,
                    "netplay player {} submitted input for port {}",
                    player + 1,
                    source + 1
                );
            } else {
                ensure!(
                    source != self.settings.player,
                    "netplay host echoed this player's own input"
                );
            }
            self.rollback.receive(source, frame, input)?;
        }
        if let Some((frame, hash)) = packet.checksum {
            ensure!(
                frame <= self.rollback.current + u64::from(self.settings.input_delay) + 1,
                "netplay checksum is too far in the future"
            );
            if frame > self.last_checked {
                self.peer_hashes.insert((frame, player), hash);
            }
        }
        // The host starts once every guest is present; a guest starts when
        // the host says so, which is the same instant for all of them.
        let connected = if self.settings.hosting() {
            self.locally_ready()
        } else {
            self.links[index].ready
        };
        if connected && !self.connected {
            self.connected = true;
            emu.reanchor_realtime_clock();
            log::info!(
                "netplay: connected; {} players, local controller port {}",
                self.settings.players,
                self.settings.player + 1
            );
        }
        Ok(())
    }

    /// A host keeps each port's inputs until the last link that still needs
    /// them has acknowledged them, since it is the only route between guests.
    fn update_relay_floor(&mut self) {
        if !self.settings.hosting() {
            return;
        }
        let mut floor = [u64::MAX; MAX_PLAYERS];
        for source in 0..self.settings.players {
            for link in self.links.iter().filter(|link| link.player != source) {
                floor[source] = floor[source].min(link.acks[source]);
            }
        }
        self.rollback.relay_floor = floor;
    }

    fn send_packets(&mut self, force: bool) -> Result<()> {
        let now = Instant::now();
        for index in 0..self.links.len() {
            let due = force
                || self.links[index]
                    .last_sent
                    .is_none_or(|last| now.duration_since(last) >= Duration::from_millis(10));
            if !due {
                continue;
            }
            let player = self.links[index].player;
            let acks = self.links[index].acks;
            // Records stay ordered by port and then frame, which is how the
            // wire format requires them, and how chunking preserves them.
            let mut records = Vec::new();
            for source in 0..self.settings.players {
                if source == player || (!self.settings.hosting() && source != self.settings.player)
                {
                    continue;
                }
                records.extend(
                    self.rollback
                        .pending(source, acks[source])
                        .map(|(frame, input)| (source, frame, input)),
                );
            }
            let mut sent = false;
            if records.is_empty() {
                sent = self.send_one(index, Vec::new())?;
            } else {
                for chunk in records.chunks(wire::MAX_INPUTS).take(MAX_SEND_PACKETS) {
                    if !self.send_one(index, chunk.to_vec())? {
                        break;
                    }
                    sent = true;
                }
            }
            if sent {
                self.links[index].last_sent = Some(now);
            }
        }
        Ok(())
    }

    fn send_one(&mut self, index: usize, inputs: Vec<(usize, u64, Input)>) -> Result<bool> {
        let mut acks = self.rollback.received;
        // A peer never reports what it needs from itself, and ports outside
        // the session never acknowledge anything.
        for (player, ack) in acks.iter_mut().enumerate() {
            if player == self.settings.player || player >= self.settings.players {
                *ack = 0;
            }
        }
        let packet = wire::Packet {
            session: self.settings.session,
            identity: self.identity,
            player: self.settings.player,
            players: self.settings.players,
            ready: self.locally_ready(),
            delay: self.settings.input_delay,
            window: self.settings.rollback_frames,
            acks,
            inputs,
            checksum: self.rollback.hashes.last_key_value().map(|(&f, &h)| (f, h)),
        }
        .encode();
        self.links[index]
            .transport
            .send(&packet)
            .context("sending netplay input")
    }
}

impl<T: Transport> Drop for Connection<T> {
    fn drop(&mut self) {
        let status = self.status();
        log::info!(
            "netplay: finished frames={} confirmed={} checked={} rollbacks={} replayed={}",
            status.frame,
            status.confirmed_frame,
            status.checked_frame,
            status.rollbacks,
            status.replayed_frames
        );
    }
}

pub(super) struct EmulatedMachine<'a>(pub(super) &'a mut Emulator);
impl Machine for EmulatedMachine<'_> {
    fn save(&self) -> Result<Vec<u8>> {
        self.0.netplay_snapshot()
    }
    fn load(&mut self, state: &[u8]) -> Result<()> {
        self.0.netplay_restore(state)
    }
    fn frame(&mut self, inputs: &[Input], previous_keys: [u8; 16], replay: bool) -> Result<()> {
        for (port, input) in inputs.iter().enumerate() {
            // A mouse port takes motion and buttons; a digital controller
            // update must not replace the device plugged into it. Ports 3
            // and 4 are adapter sockets, which carry switch joysticks only.
            if port < crate::bus::PARALLEL_PORT_FIRST
                && self.0.bus().input.ports[port].device.is_mouse()
            {
                let hardware = &mut self.0.bus_mut().input;
                hardware.add_mouse_delta(
                    port,
                    i32::from(input.mouse_dx),
                    i32::from(input.mouse_dy),
                );
                for button in 0..3 {
                    hardware.set_mouse_button(
                        port,
                        button,
                        input.mouse_buttons & (1 << button) != 0,
                    );
                }
                continue;
            }
            let on = |bit: u32| input.buttons & (1u16 << bit) != 0u16;
            self.0
                .bus_mut()
                .input
                .set_joystick(port, on(0), on(1), on(2), on(3), on(4), on(5));
            self.0
                .bus_mut()
                .input
                .set_cd32_buttons(port, on(6), on(7), on(8), on(9), on(10));
        }
        let keys = Input::merged_keys(inputs);
        for key in 0..128u8 {
            let mask = 1 << (key % 8);
            let index = usize::from(key / 8);
            if (keys[index] ^ previous_keys[index]) & mask != 0 {
                self.0
                    .bus_mut()
                    .enqueue_key_event(key, keys[index] & mask != 0);
            }
        }
        self.0.step_netplay_frame(replay)
    }
}
