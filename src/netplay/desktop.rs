// SPDX-License-Identifier: GPL-3.0-or-later

use super::{
    control::{Control, ROLE_GUEST, ROLE_HOST, ROLE_SPECTATOR},
    setup::{Bundle, Staged},
    spectate::{self, Feed, FeedCursor, FeedDecoder, FeedMessage, Spectator, SwapRecord},
    transport::{LinkTransport, NativeTransport, PeerTransport, UdpTransport},
    *,
};
use crate::{
    config::Config,
    emulator::Emulator,
    timebase::{Duration, Instant},
};
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Control-message kinds: JSON setup, the machine bundle, replacement disk
/// bytes, and spectator feed bytes.
const KIND_JSON: u8 = 1;
const KIND_BUNDLE: u8 = 2;
const KIND_DISK: u8 = 3;
const KIND_FEED: u8 = 4;

const SETUP_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const SPECTATOR_TIMEOUT: Duration = Duration::from_secs(10);
const KEEPALIVE: Duration = Duration::from_secs(1);

/// A player's link, and the host's link to each guest.
type PlayerLink = Control<LinkTransport>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Message {
    /// A guest announces itself, naming the port it was configured for.
    /// Internet guests ask for none and take whatever the host has free.
    Hello {
        build: String,
        player: Option<usize>,
    },
    /// The host's timeline settings and the port it seated this guest on.
    Offer {
        delay: u8,
        window: u8,
        players: usize,
        player: usize,
    },
    Verified {
        identity: [u8; 32],
    },
    Start,
    Swap {
        id: u64,
        event: SwapMessage,
    },
    /// A spectator announces itself; the host answers with its bundle.
    Watch {
        build: String,
    },
    /// The host declines a spectator or a guest.
    Refused {
        reason: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum SwapMessage {
    Begin {
        drive: usize,
        size: usize,
        hash: [u8; 32],
        writable: bool,
    },
    Held {
        frame: u64,
    },
    Target {
        frame: u64,
    },
    Ready {
        hash: [u8; 32],
    },
    Prepared,
    Apply,
    Applied {
        hash: [u8; 32],
    },
    Resume,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SwapPhase {
    HostHeld,
    HostReady,
    HostPrepared,
    HostApplied,
    GuestTarget,
    GuestReady,
    GuestBytes,
    GuestApply,
    GuestResume,
}

struct Swap {
    phase: SwapPhase,
    stop: u64,
    drive: usize,
    writable: bool,
    size: usize,
    hash: [u8; 32],
    bytes: Option<Arc<Vec<u8>>>,
    /// The host's own digest at the stopped boundary, kept for spectators.
    own_digest: Option<[u8; 32]>,
    started: Instant,
    /// What each guest has reported at the current stage (host only).
    held: BTreeMap<usize, u64>,
    ready: BTreeMap<usize, [u8; 32]>,
    prepared: BTreeSet<usize>,
    applied: BTreeMap<usize, [u8; 32]>,
}

impl Swap {
    fn new(
        phase: SwapPhase,
        stop: u64,
        drive: usize,
        writable: bool,
        size: usize,
        hash: [u8; 32],
    ) -> Self {
        Self {
            phase,
            stop,
            drive,
            writable,
            size,
            hash,
            bytes: None,
            own_digest: None,
            started: Instant::now(),
            held: BTreeMap::new(),
            ready: BTreeMap::new(),
            prepared: BTreeSet::new(),
            applied: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The host is waiting for its guests to finish setup.
    HostSetup,
    GuestOffer,
    GuestBundle,
    GuestStart,
    WatchBundle,
    WatchStart,
    Running,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WatchPhase {
    Hello,
    Verified,
    Streaming,
}

/// Where one guest is in the host's setup.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GuestPhase {
    AwaitVerified,
    Running,
}

struct GuestState {
    player: usize,
    phase: GuestPhase,
}

/// A connection the host has accepted but not yet seated: it has no port
/// until its `Hello` says which one it wants. Seating moves the link into
/// the timeline and leaves nothing behind.
struct PendingGuest {
    control: Option<PlayerLink>,
    started: Instant,
}

impl PendingGuest {
    fn link(&mut self) -> Result<&mut PlayerLink> {
        self.control.as_mut().context("guest link was taken")
    }
}

/// The host's link to one spectator.
struct SpectatorLink {
    control: Control<PeerTransport>,
    phase: WatchPhase,
    cursor: FeedCursor,
    started: Instant,
    last_seen: Instant,
    last_sent: Instant,
}

/// A spectator's link to the host and its confirmed-only timeline.
struct Watcher {
    control: Control<NativeTransport>,
    spectator: Option<Spectator>,
    last_received: Instant,
    last_status: Instant,
    catching_up: bool,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if let Some(spectator) = &self.spectator {
            log::info!(
                "netplay: spectating finished frames={} checked={} swaps={}",
                spectator.executed(),
                spectator.checked(),
                spectator.swaps_applied()
            );
        }
    }
}

enum Timeline {
    Play(Box<Connection<PlayerLink>>),
    Watch(Box<Watcher>),
}

fn send_message<T: Transport>(control: &mut Control<T>, message: &Message) -> Result<()> {
    let mut bytes = vec![KIND_JSON];
    bytes.extend(serde_json::to_vec(message)?);
    control.send_message(bytes)
}

fn send_feed<T: Transport>(control: &mut Control<T>, message: &FeedMessage) -> Result<()> {
    let mut bytes = vec![KIND_FEED];
    message.encode_into(&mut bytes);
    control.send_message(bytes)
}

fn decode_json(bytes: &[u8]) -> Result<Message> {
    ensure!(
        bytes.first() == Some(&KIND_JSON) && bytes.len() <= 2048,
        "invalid netplay setup message"
    );
    Ok(serde_json::from_slice(&bytes[1..])?)
}

impl SpectatorLink {
    fn step(
        &mut self,
        now: Instant,
        identity: [u8; 32],
        feed: Option<&Feed>,
        bundle: Option<&[Arc<Vec<u8>>]>,
    ) -> Result<()> {
        self.control.poll()?;
        while let Some(bytes) = self.control.take_message() {
            match (bytes.first(), self.phase) {
                (Some(&KIND_JSON), _) => match (decode_json(&bytes)?, self.phase) {
                    (Message::Watch { build }, WatchPhase::Hello) => {
                        ensure!(
                            build == env!("COPPERLINE_DISPLAY_VERSION"),
                            "spectator uses a different Copperline build"
                        );
                        if feed.is_some_and(Feed::full) {
                            let _ = send_message(
                                &mut self.control,
                                &Message::Refused {
                                    reason: "the game's history is too large to replay".into(),
                                },
                            );
                            bail!("spectator refused: the retained history is too large");
                        }
                        let bundle = bundle.context("no setup bundle retained for spectators")?;
                        let mut parts = vec![Arc::new(vec![KIND_BUNDLE])];
                        parts.extend(bundle.iter().cloned());
                        self.control.send_parts(parts)?;
                        self.phase = WatchPhase::Verified;
                    }
                    (Message::Verified { identity: theirs }, WatchPhase::Verified) => {
                        ensure!(theirs == identity, "spectator built a different machine");
                        send_message(&mut self.control, &Message::Start)?;
                        self.phase = WatchPhase::Streaming;
                        self.last_seen = now;
                    }
                    _ => bail!("unexpected spectator setup message"),
                },
                (Some(&KIND_FEED), WatchPhase::Streaming) => {
                    let mut decoder = FeedDecoder::default();
                    decoder.push(&bytes[1..])?;
                    while let Some(message) = decoder.next_message()? {
                        ensure!(
                            matches!(message, FeedMessage::Status { .. }),
                            "unexpected spectator feed message"
                        );
                    }
                    self.last_seen = now;
                }
                _ => bail!("unexpected spectator message"),
            }
        }
        if self.phase != WatchPhase::Streaming {
            ensure!(
                now.duration_since(self.started) < SETUP_TIMEOUT,
                "spectator setup timed out"
            );
            return Ok(());
        }
        ensure!(
            now.duration_since(self.last_seen) < SPECTATOR_TIMEOUT,
            "spectator timed out"
        );
        let Some(feed) = feed else {
            return Ok(());
        };
        let mut sent = false;
        while self.control.can_send() {
            let Some((message, next)) = feed.next_message(self.cursor, spectate::MAX_BATCH) else {
                break;
            };
            send_feed(&mut self.control, &message)?;
            self.cursor = next;
            sent = true;
        }
        if sent {
            self.last_sent = now;
        } else if now.duration_since(self.last_sent) >= KEEPALIVE && self.control.can_send() {
            send_feed(&mut self.control, &feed.head())?;
            self.last_sent = now;
        }
        Ok(())
    }
}

/// Desktop setup and media coordination around the shared rollback protocol,
/// or a spectator's lockstep replay of the host's confirmed history.
pub struct Session {
    role: Role,
    options: ConnectionOptions,
    timeline: Timeline,
    /// The host's socket or endpoint, which admits guests and spectators.
    listener: Option<NativeTransport>,
    phase: Phase,
    /// The host's guests, in the order they were seated.
    guests: Vec<GuestState>,
    /// Connections the host has accepted but not yet seated.
    pending: Vec<PendingGuest>,
    /// Retained while guests or spectators may still join.
    bundle: Option<Vec<Arc<Vec<u8>>>>,
    directory: Option<tempfile::TempDir>,
    changed_config: Option<Config>,
    started: Instant,
    progress: Option<String>,
    last_progress: Instant,
    failure: Option<String>,
    swap: Option<Swap>,
    swap_id: u64,
    spectators: Vec<SpectatorLink>,
    spectator_limit: usize,
    spectator_tag: [u8; 16],
}

impl Session {
    pub fn new(
        options: impl Into<ConnectionOptions>,
        emu: &mut Emulator,
        cfg: &Config,
    ) -> Result<Self> {
        let options = options.into();
        options.validate()?;
        validate_config(cfg)?;
        ensure!(
            emu.bus().emulated_cck() == 0,
            "netplay must start before the machine runs"
        );
        match options.role() {
            Role::Spectator => Self::watch(options),
            role => Self::play(options, role, emu, cfg),
        }
    }

    fn play(
        options: ConnectionOptions,
        role: Role,
        emu: &mut Emulator,
        cfg: &Config,
    ) -> Result<Self> {
        let settings = options.settings().expect("players negotiate settings");
        settings.validate()?;
        let spectator_limit = options.spectators();
        // Finish every fallible preparation before replacing the live machine
        // or moving its output sink. The host also uses the transmitted setup.
        let staged = if role == Role::Host {
            let bundle = Bundle::capture(cfg, emu)?;
            Some((bundle.stage()?, bundle.into_parts()?))
        } else {
            None
        };
        let session_id = settings.session;
        let (transport, spectator_tag) = match &options {
            ConnectionOptions::Direct(direct) => {
                let transport = if role == Role::Host {
                    UdpTransport::listen(direct.clone())?
                } else {
                    UdpTransport::connect(direct.clone())?
                };
                (NativeTransport::Udp(transport), session_id)
            }
            #[cfg(feature = "netplay-internet")]
            ConnectionOptions::Internet(internet_options) => {
                let tag = internet_options
                    .spectator_capability
                    .unwrap_or(internet_options.invitation.session);
                (
                    NativeTransport::Internet(Box::new(internet::InternetTransport::new(
                        (**internet_options).clone(),
                    )?)),
                    tag,
                )
            }
            _ => unreachable!("players use player options"),
        };
        let (mut connection, listener, directory, bundle, changed_config) =
            if let Some((mut staged, bytes)) = staged {
                staged.emu.set_paced(emu.paced());
                validate_player_ports(&staged.emu, settings.players)?;
                let connection =
                    Connection::without_links(settings.clone(), &mut staged.emu, &staged.cfg)?;
                std::mem::swap(
                    &mut staged.emu.bus_mut().paula.audio,
                    &mut emu.bus_mut().paula.audio,
                );
                *emu = *staged.emu;
                (
                    connection,
                    Some(transport),
                    Some(staged.directory),
                    Some(bytes.into_iter().map(Arc::new).collect()),
                    Some(staged.cfg),
                )
            } else {
                let control = Control::new(
                    LinkTransport::Direct(transport),
                    session_id,
                    ROLE_GUEST,
                    ROLE_HOST,
                );
                (
                    Connection::with_transport(settings.clone(), control, emu, cfg)?,
                    None,
                    None,
                    None,
                    None,
                )
            };
        if spectator_limit > 0 {
            connection.enable_feed(spectate::NATIVE_FEED_LIMIT)?;
        }
        let now = Instant::now();
        let requested_player = (role == Role::Guest).then_some(settings.player);
        let mut session = Self {
            role,
            options,
            timeline: Timeline::Play(Box::new(connection)),
            listener,
            phase: if role == Role::Host {
                Phase::HostSetup
            } else {
                Phase::GuestOffer
            },
            guests: Vec::new(),
            pending: Vec::new(),
            bundle,
            directory,
            changed_config,
            started: now,
            progress: Some("Waiting for the other players...".into()),
            last_progress: now,
            failure: None,
            swap: None,
            swap_id: 0,
            spectators: Vec::new(),
            spectator_limit,
            spectator_tag,
        };
        if role == Role::Guest {
            // A direct-IP guest was configured for a port; an Internet guest
            // takes whichever port the host still has free.
            let player = requested_player.filter(|_| session.options.names_player());
            session.send_to_host(Message::Hello {
                build: env!("COPPERLINE_DISPLAY_VERSION").into(),
                player,
            })?;
        }
        Ok(session)
    }

    fn watch(options: ConnectionOptions) -> Result<Self> {
        let (transport, tag) = match &options {
            ConnectionOptions::Watch(watch) => {
                let tag = watch.session;
                (
                    NativeTransport::Udp(UdpTransport::watch(watch.clone())?),
                    tag,
                )
            }
            #[cfg(feature = "netplay-internet")]
            ConnectionOptions::WatchInternet(watch) => {
                let tag = watch.invitation.capability;
                (
                    NativeTransport::Internet(Box::new(internet::InternetTransport::watch(
                        (**watch).clone(),
                    )?)),
                    tag,
                )
            }
            _ => unreachable!("spectators use watch options"),
        };
        let now = Instant::now();
        let mut session = Self {
            role: Role::Spectator,
            options,
            timeline: Timeline::Watch(Box::new(Watcher {
                control: Control::new(transport, tag, ROLE_SPECTATOR, ROLE_HOST),
                spectator: None,
                last_received: now,
                last_status: now,
                catching_up: false,
            })),
            listener: None,
            phase: Phase::WatchBundle,
            guests: Vec::new(),
            pending: Vec::new(),
            bundle: None,
            directory: None,
            changed_config: None,
            started: now,
            progress: Some("Connecting to the host...".into()),
            last_progress: now,
            failure: None,
            swap: None,
            swap_id: 0,
            spectators: Vec::new(),
            spectator_limit: 0,
            spectator_tag: tag,
        };
        let watcher = match &mut session.timeline {
            Timeline::Watch(watcher) => watcher,
            Timeline::Play(_) => unreachable!("spectators own a watcher timeline"),
        };
        send_message(
            &mut watcher.control,
            &Message::Watch {
                build: env!("COPPERLINE_DISPLAY_VERSION").into(),
            },
        )?;
        Ok(session)
    }

    fn connection(&self) -> &Connection<PlayerLink> {
        match &self.timeline {
            Timeline::Play(connection) => connection,
            Timeline::Watch(_) => unreachable!("spectators have no rollback timeline"),
        }
    }

    fn connection_mut(&mut self) -> &mut Connection<PlayerLink> {
        match &mut self.timeline {
            Timeline::Play(connection) => connection,
            Timeline::Watch(_) => unreachable!("spectators have no rollback timeline"),
        }
    }

    pub fn options(&self) -> ConnectionOptions {
        self.options.clone()
    }
    pub fn role(&self) -> Role {
        self.role
    }
    /// The controller port this participant owns; spectators own none.
    pub fn port(&self) -> Option<usize> {
        match &self.timeline {
            Timeline::Play(connection) => Some(connection.player()),
            Timeline::Watch(_) => None,
        }
    }
    /// Controller ports the session drives.
    pub fn players(&self) -> usize {
        match &self.timeline {
            Timeline::Play(connection) => connection.players(),
            Timeline::Watch(watcher) => watcher
                .spectator
                .as_ref()
                .and_then(Spectator::players)
                .unwrap_or(2),
        }
    }
    pub fn status(&self) -> Status {
        match &self.timeline {
            Timeline::Play(connection) => connection.status(),
            Timeline::Watch(watcher) => {
                let (executed, checked, behind) = watcher
                    .spectator
                    .as_ref()
                    .map_or((0, 0, 0), |s| (s.executed(), s.checked(), s.behind()));
                Status {
                    connected: self.phase == Phase::Running,
                    frame: executed,
                    confirmed_frame: executed,
                    acknowledged_frame: executed,
                    rollbacks: 0,
                    replayed_frames: 0,
                    checked_frame: checked,
                    behind,
                }
            }
        }
    }
    /// Confirmed host frames a spectator has yet to execute.
    pub fn behind(&self) -> u64 {
        self.status().behind
    }
    /// A spectator running unpaced through its backlog.
    pub fn catching_up(&self) -> bool {
        matches!(&self.timeline, Timeline::Watch(w) if w.catching_up)
    }
    pub fn set_catching_up(&mut self, catching_up: bool) {
        if let Timeline::Watch(watcher) = &mut self.timeline {
            watcher.catching_up = catching_up;
        }
    }
    /// Spectators the host is currently serving (any setup phase).
    pub fn spectator_count(&self) -> usize {
        self.spectators.len()
    }
    /// Guests the host has seated so far.
    pub fn guest_count(&self) -> usize {
        self.guests.len()
    }
    pub fn route(&self) -> &'static str {
        match (&self.timeline, &self.listener) {
            (_, Some(listener)) => listener.route(),
            (Timeline::Play(connection), None) => connection.route(),
            (Timeline::Watch(watcher), None) => watcher.control.route(),
        }
    }
    pub fn confirmed_state_digest(&self, emu: &Emulator) -> Result<[u8; 32]> {
        match &self.timeline {
            Timeline::Play(connection) => connection.confirmed_state_digest(emu),
            Timeline::Watch(_) => {
                ensure!(self.failure.is_none(), "netplay session has failed");
                ensure!(self.ready_to_capture(), "spectator frame is not settled");
                Ok(digest(&emu.netplay_snapshot()?))
            }
        }
    }
    pub fn take_config(&mut self) -> Option<Config> {
        self.changed_config.take()
    }
    pub fn take_progress(&mut self) -> Option<String> {
        self.progress.take()
    }
    /// Whether any link still has setup or media bytes in flight.
    fn sending(&self) -> bool {
        match &self.timeline {
            Timeline::Play(connection) => {
                connection.transports().any(|(_, link)| link.sending())
                    || self
                        .pending
                        .iter()
                        .any(|guest| guest.control.as_ref().is_some_and(Control::sending))
            }
            Timeline::Watch(watcher) => watcher.control.sending(),
        }
    }
    pub fn ready_to_capture(&self) -> bool {
        match &self.timeline {
            Timeline::Play(_) => {
                self.swap.is_none() && !self.sending() && self.status().ready_to_capture()
            }
            Timeline::Watch(watcher) => {
                self.phase == Phase::Running
                    && !watcher.control.sending()
                    && watcher
                        .spectator
                        .as_ref()
                        .is_some_and(|s| s.due_swap().is_none())
            }
        }
    }
    pub fn can_change_disk(&self) -> bool {
        self.role == Role::Host
            && self.phase == Phase::Running
            && self.status().connected
            && self.swap.is_none()
            && !self.sending()
    }

    /// Queue one host-controlled insertion or eject. Decode the local image
    /// before stopping the game, so a bad selection leaves play untouched.
    pub fn change_disk(
        &mut self,
        emu: &Emulator,
        drive: usize,
        bytes: Vec<u8>,
        writable: bool,
    ) -> Result<()> {
        ensure!(
            self.can_change_disk(),
            "wait for the host's connected, idle netplay session"
        );
        Self::validate_disk(emu, drive, &bytes, writable)?;
        let size = bytes.len();
        let hash = digest(&bytes);
        self.swap_id = self
            .swap_id
            .checked_add(1)
            .context("disk change identifier exhausted")?;
        let mut swap = Swap::new(
            SwapPhase::HostHeld,
            self.status().frame,
            drive,
            writable,
            size,
            hash,
        );
        swap.bytes = Some(Arc::new(bytes));
        self.swap = Some(swap);
        self.swap_send(SwapMessage::Begin {
            drive,
            size,
            hash,
            writable,
        })?;
        self.progress = Some(format!("Pausing every player for DF{drive}..."));
        Ok(())
    }

    fn validate_disk(emu: &Emulator, drive: usize, bytes: &[u8], writable: bool) -> Result<()> {
        ensure!(
            drive < 4 && emu.bus().floppy.drive_connected(drive),
            "floppy drive is not connected"
        );
        ensure!(
            bytes.len() <= setup::FLOPPY_LIMIT && (!bytes.is_empty() || !writable),
            "invalid replacement disk size or write protection"
        );
        if !bytes.is_empty() {
            crate::floppy::FloppyController::default().insert_memory_disk_image_bytes_with_limit(
                0,
                bytes.to_vec(),
                "replacement".into(),
                !writable,
                setup::FLOPPY_LIMIT,
            )?;
        }
        Ok(())
    }

    /// Send a setup message to every seated guest, or to the host.
    fn send_all(&mut self, message: Message) -> Result<()> {
        if self.role == Role::Host {
            let connection = self.connection_mut();
            for (_, link) in connection.links_mut() {
                send_message(link, &message)?;
            }
            Ok(())
        } else {
            self.send_to_host(message)
        }
    }

    fn send_to_host(&mut self, message: Message) -> Result<()> {
        let link = self
            .connection_mut()
            .link_mut(0)
            .context("no link to the netplay host")?;
        send_message(link, &message)
    }

    fn swap_send(&mut self, event: SwapMessage) -> Result<()> {
        self.send_all(Message::Swap {
            id: self.swap_id,
            event,
        })
    }

    /// Guests the host is coordinating a media change with.
    fn guest_players(&self) -> Vec<usize> {
        self.guests.iter().map(|guest| guest.player).collect()
    }

    fn swap_message(
        &mut self,
        emu: &mut Emulator,
        from: usize,
        id: u64,
        event: SwapMessage,
    ) -> Result<()> {
        if let SwapMessage::Begin {
            drive,
            size,
            hash,
            writable,
        } = event
        {
            ensure!(
                self.role == Role::Guest
                    && self.status().connected
                    && self.swap.is_none()
                    && self.swap_id.checked_add(1) == Some(id),
                "unexpected disk change request"
            );
            ensure!(
                drive < 4
                    && emu.bus().floppy.drive_connected(drive)
                    && size <= setup::FLOPPY_LIMIT
                    && (size > 0 || !writable),
                "invalid disk change description"
            );
            self.swap_id = id;
            let frame = self.status().frame;
            self.swap = Some(Swap::new(
                SwapPhase::GuestTarget,
                frame,
                drive,
                writable,
                size,
                hash,
            ));
            self.progress = Some(format!("Host is changing DF{drive}..."));
            return self.swap_send(SwapMessage::Held { frame });
        }
        ensure!(id == self.swap_id, "unexpected disk change identifier");
        let frame = self.status().frame;
        let guests = self.guest_players();
        let swap = self.swap.as_mut().context("no disk change in progress")?;
        match event {
            SwapMessage::Held { frame: peer } if swap.phase == SwapPhase::HostHeld => {
                ensure!(peer.abs_diff(frame) <= 32, "invalid peer disk change frame");
                swap.held.insert(from, peer);
                if guests.iter().all(|player| swap.held.contains_key(player)) {
                    // Everyone stops at the furthest frame any player has
                    // already reached, so nobody has to rewind to get there.
                    let target = swap
                        .held
                        .values()
                        .copied()
                        .max()
                        .unwrap_or(frame)
                        .max(frame);
                    swap.stop = target;
                    swap.phase = SwapPhase::HostReady;
                    self.swap_send(SwapMessage::Target { frame: target })?;
                }
            }
            SwapMessage::Target { frame: target } if swap.phase == SwapPhase::GuestTarget => {
                ensure!(
                    target >= frame && target - frame <= 32,
                    "invalid disk change target"
                );
                swap.stop = target;
                swap.phase = SwapPhase::GuestReady;
            }
            SwapMessage::Ready { hash } if swap.phase == SwapPhase::HostReady => {
                swap.ready.insert(from, hash);
            }
            SwapMessage::Prepared if swap.phase == SwapPhase::HostPrepared => {
                swap.prepared.insert(from);
                if guests.iter().all(|player| swap.prepared.contains(player)) {
                    self.apply_disk(emu)?;
                    self.swap.as_mut().unwrap().phase = SwapPhase::HostApplied;
                    self.swap_send(SwapMessage::Apply)?;
                }
            }
            SwapMessage::Apply if swap.phase == SwapPhase::GuestApply => {
                self.apply_disk(emu)?;
                self.swap.as_mut().unwrap().phase = SwapPhase::GuestResume;
                self.swap_send(SwapMessage::Applied {
                    hash: self.confirmed_state_digest(emu)?,
                })?;
            }
            SwapMessage::Applied { hash } if swap.phase == SwapPhase::HostApplied => {
                swap.applied.insert(from, hash);
                if guests
                    .iter()
                    .all(|player| swap.applied.contains_key(player))
                {
                    let own = self.confirmed_state_digest(emu)?;
                    let swap = self.swap.as_ref().unwrap();
                    ensure!(
                        swap.applied.values().all(|hash| *hash == own),
                        "players differ after the disk change"
                    );
                    // Every player agrees on the change; spectators replay it
                    // at this frame with the same digests on either side.
                    let record = SwapRecord {
                        frame: swap.stop,
                        drive: swap.drive,
                        writable: swap.writable,
                        before: swap
                            .own_digest
                            .context("disk change digest was not captured")?,
                        after: own,
                        bytes: swap
                            .bytes
                            .clone()
                            .context("replacement disk was not retained")?,
                    };
                    self.connection_mut().feed_swap(record)?;
                    self.swap_send(SwapMessage::Resume)?;
                    self.finish_swap();
                }
            }
            SwapMessage::Resume if swap.phase == SwapPhase::GuestResume => {
                self.finish_swap();
            }
            _ => anyhow::bail!("unexpected disk change phase"),
        }
        Ok(())
    }

    fn apply_disk(&mut self, emu: &mut Emulator) -> Result<()> {
        let status = self.status();
        let swap = self.swap.as_ref().context("no disk change in progress")?;
        ensure!(
            status.ready_to_capture() && status.frame == swap.stop,
            "disk change is not at a confirmed boundary"
        );
        let bytes = swap
            .bytes
            .as_ref()
            .context("replacement disk is not verified")?;
        spectate::change_floppy(emu, swap.drive, bytes.to_vec(), swap.writable)
    }

    fn finish_swap(&mut self) {
        let swap = self.swap.take().unwrap();
        self.progress = Some(format!(
            "DF{} {} on every player",
            swap.drive,
            if swap.size == 0 {
                "ejected"
            } else {
                "inserted"
            }
        ));
    }

    pub fn step(&mut self, emu: &mut Emulator, input: Input, advance: bool) -> Result<bool> {
        self.step_local(emu, &mut input.into(), advance)
    }

    pub fn step_local(
        &mut self,
        emu: &mut Emulator,
        input: &mut LocalInput,
        advance: bool,
    ) -> Result<bool> {
        if let Some(error) = &self.failure {
            anyhow::bail!("{error}");
        }
        let result = match self.role {
            Role::Spectator => self.step_watcher(emu, advance),
            _ => self.step_player(emu, input, advance),
        };
        if let Err(error) = &result {
            self.failure = Some(format!("{error:#}"));
        }
        result
    }

    /// Admit guests and spectators arriving on the host's socket or endpoint.
    fn service_listener(&mut self) -> Result<()> {
        let Some(listener) = &mut self.listener else {
            return Ok(());
        };
        listener.ready()?;
        listener.pump()?;
        let arrivals = listener.take_players();
        let watchers = listener.take_spectators();
        let now = Instant::now();
        let free = self.connection().free_players().count();
        for transport in arrivals {
            if self.pending.len() >= free {
                log::info!("netplay: guest refused; every controller port is taken");
                let mut control = Control::new(
                    LinkTransport::Peer(transport),
                    self.connection().settings.session,
                    ROLE_HOST,
                    ROLE_GUEST,
                );
                let _ = send_message(
                    &mut control,
                    &Message::Refused {
                        reason: "every controller port is taken".into(),
                    },
                );
                let _ = control.poll();
                continue;
            }
            log::info!("netplay: guest connecting ({})", transport.route());
            self.pending.push(PendingGuest {
                control: Some(Control::new(
                    LinkTransport::Peer(transport),
                    self.connection().settings.session,
                    ROLE_HOST,
                    ROLE_GUEST,
                )),
                started: now,
            });
        }
        for transport in watchers {
            if self.spectators.len() >= self.spectator_limit {
                log::info!(
                    "netplay: spectator refused; all {} places are taken",
                    self.spectator_limit
                );
                continue;
            }
            log::info!("netplay: spectator connecting ({})", transport.route());
            self.spectators.push(SpectatorLink {
                control: Control::new(transport, self.spectator_tag, ROLE_HOST, ROLE_SPECTATOR),
                phase: WatchPhase::Hello,
                cursor: FeedCursor::default(),
                started: now,
                last_seen: now,
                last_sent: now,
            });
        }
        Ok(())
    }

    /// Seat guests that have introduced themselves, then hand them the setup.
    fn service_pending(&mut self) -> Result<()> {
        let mut index = 0;
        while index < self.pending.len() {
            let result = self.pending[index].link()?.poll();
            let message = match result {
                Ok(()) => self.pending[index].link()?.take_message(),
                Err(error) => {
                    log::info!("netplay: guest left during setup: {error:#}");
                    self.pending.remove(index);
                    continue;
                }
            };
            let Some(bytes) = message else {
                if self.pending[index].started.elapsed() >= SETUP_TIMEOUT {
                    log::info!("netplay: guest setup timed out");
                    self.pending.remove(index);
                    continue;
                }
                index += 1;
                continue;
            };
            let mut guest = self.pending.remove(index);
            // A caller that cannot be seated -- the wrong build, a port
            // someone else already holds -- is turned away on its own link.
            // Nothing a stranger sends may end the game for the players.
            if let Err(error) = self.seat_guest(&mut guest, &bytes) {
                log::info!("netplay: guest refused: {error:#}");
                if let Some(control) = &mut guest.control {
                    let _ = send_message(
                        control,
                        &Message::Refused {
                            reason: format!("{error:#}"),
                        },
                    );
                    let _ = control.poll();
                }
            }
        }
        Ok(())
    }

    fn seat_guest(&mut self, guest: &mut PendingGuest, bytes: &[u8]) -> Result<()> {
        let Message::Hello { build, player } = decode_json(bytes)? else {
            bail!("a netplay guest must introduce itself first");
        };
        ensure!(
            build == env!("COPPERLINE_DISPLAY_VERSION"),
            "netplay requires the same Copperline build on every peer"
        );
        let connection = self.connection_mut();
        let seat = match player {
            Some(wanted) => {
                ensure!(
                    connection.free_players().any(|free| free == wanted),
                    "netplay port {} is taken; choose another player number",
                    wanted + 1
                );
                wanted
            }
            None => connection
                .free_players()
                .next()
                .context("netplay session is full")?,
        };
        let (delay, window, players) = {
            let settings = &connection.settings;
            (
                settings.input_delay,
                settings.rollback_frames,
                settings.players,
            )
        };
        let control = guest.control.take().context("guest link was taken")?;
        connection.add_link(seat, control)?;
        self.guests.push(GuestState {
            player: seat,
            phase: GuestPhase::AwaitVerified,
        });
        let link = self
            .connection_mut()
            .link_mut(seat)
            .expect("the link was just added");
        send_message(
            link,
            &Message::Offer {
                delay,
                window,
                players,
                player: seat,
            },
        )?;
        let mut parts = vec![Arc::new(vec![KIND_BUNDLE])];
        parts.extend(self.bundle.as_ref().unwrap().iter().cloned());
        self.connection_mut()
            .link_mut(seat)
            .expect("the link was just added")
            .send_parts(parts)?;
        log::info!("netplay: guest seated on controller port {}", seat + 1);
        self.progress = Some(format!(
            "Sending machine configuration and game files to player {}...",
            seat + 1
        ));
        Ok(())
    }

    fn step_player(
        &mut self,
        emu: &mut Emulator,
        input: &mut LocalInput,
        advance: bool,
    ) -> Result<bool> {
        if self.role == Role::Host {
            self.service_listener()?;
            self.service_pending()?;
        }
        let players: Vec<usize> = self
            .connection()
            .transports()
            .map(|(player, _)| player)
            .collect();
        for player in &players {
            self.connection_mut()
                .link_mut(*player)
                .expect("link exists")
                .poll()?;
        }
        ensure!(
            self.phase != Phase::GuestOffer
                || !self
                    .connection()
                    .link(0)
                    .is_some_and(Control::has_game_packets),
            "peer does not support desktop setup transfer; use the same Copperline build"
        );
        for player in players {
            while let Some(bytes) = self
                .connection_mut()
                .link_mut(player)
                .expect("link exists")
                .take_message()
            {
                self.handle_message(emu, player, input, bytes)?;
            }
        }
        if self.phase != Phase::Running {
            ensure!(
                self.started.elapsed() < SETUP_TIMEOUT,
                "netplay setup timed out"
            );
            self.connection_mut().started = Instant::now();
            if self.phase == Phase::GuestBundle
                && self.last_progress.elapsed() > Duration::from_secs(1)
            {
                let received = self.connection().link(0).map_or(0, Control::received_bytes);
                self.progress = Some(format!("Receiving game files: {} KiB", received / 1024));
                self.last_progress = Instant::now();
            }
            self.service_spectators();
            return Ok(false);
        }
        let advance = advance
            && self
                .swap
                .as_ref()
                .is_none_or(|swap| self.status().frame < swap.stop);
        let stepped = self.connection_mut().step_local(emu, input, advance)?;
        self.service_swap(emu)?;
        self.service_spectators();
        Ok(stepped)
    }

    /// Progress a media change that is waiting on the stopped boundary.
    fn service_swap(&mut self, emu: &mut Emulator) -> Result<()> {
        let status = self.status();
        let guests = self.guest_players();
        let Some(swap) = &self.swap else {
            return Ok(());
        };
        ensure!(
            swap.started.elapsed() < Duration::from_secs(180),
            "disk change timed out"
        );
        if status.frame != swap.stop || !status.ready_to_capture() {
            return Ok(());
        }
        let own = self.confirmed_state_digest(emu)?;
        let swap = self.swap.as_ref().unwrap();
        match swap.phase {
            SwapPhase::GuestReady => {
                self.swap.as_mut().unwrap().phase = SwapPhase::GuestBytes;
                self.swap_send(SwapMessage::Ready { hash: own })?;
            }
            SwapPhase::HostReady if guests.iter().all(|p| swap.ready.contains_key(p)) => {
                ensure!(
                    swap.ready.values().all(|hash| *hash == own),
                    "players differ before the disk change"
                );
                let mut bytes = vec![KIND_DISK];
                bytes.extend_from_slice(swap.bytes.as_ref().unwrap());
                let swap = self.swap.as_mut().unwrap();
                swap.own_digest = Some(own);
                swap.phase = SwapPhase::HostPrepared;
                let connection = self.connection_mut();
                for (_, link) in connection.links_mut() {
                    link.send_message(bytes.clone())?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_message(
        &mut self,
        emu: &mut Emulator,
        from: usize,
        input: &mut LocalInput,
        bytes: Vec<u8>,
    ) -> Result<()> {
        if bytes.first() == Some(&KIND_DISK) {
            let swap = self.swap.as_mut().context("unexpected replacement disk")?;
            ensure!(
                swap.phase == SwapPhase::GuestBytes
                    && bytes.len() == swap.size + 1
                    && digest(&bytes[1..]) == swap.hash,
                "invalid replacement disk transfer"
            );
            Self::validate_disk(emu, swap.drive, &bytes[1..], swap.writable)?;
            swap.bytes = Some(Arc::new(bytes[1..].to_vec()));
            swap.phase = SwapPhase::GuestApply;
            return self.swap_send(SwapMessage::Prepared);
        }
        if self.phase == Phase::GuestBundle {
            ensure!(
                bytes.first() == Some(&KIND_BUNDLE),
                "expected host setup bundle"
            );
            let bundle = Bundle::decode(&bytes[1..])?;
            drop(bytes);
            let Staged {
                emu: mut received,
                cfg,
                directory,
            } = bundle.stage()?;
            // Reuse the same validation as initial session construction.
            let connection = self.connection_mut();
            validate_player_ports(&received, connection.settings.players)?;
            connection.identity = initial_identity(&connection.settings, &mut received, &cfg)?;
            connection.rollback = Rollback::new(
                connection.settings.player,
                connection.settings.players,
                connection.settings.input_delay,
                connection.settings.rollback_frames,
            );
            received.set_paced(emu.paced());
            std::mem::swap(
                &mut received.bus_mut().paula.audio,
                &mut emu.bus_mut().paula.audio,
            );
            *emu = *received;
            *input = LocalInput::default();
            self.directory = Some(directory);
            self.changed_config = Some(cfg);
            let identity = self.connection().identity();
            self.send_to_host(Message::Verified { identity })?;
            self.phase = Phase::GuestStart;
            self.progress = Some("Host setup verified; waiting to start...".into());
            return Ok(());
        }
        let message = decode_json(&bytes)?;
        match message {
            Message::Offer {
                delay,
                window,
                players,
                player,
            } if self.phase == Phase::GuestOffer => {
                let connection = self.connection_mut();
                let mut settings = connection.settings.clone();
                settings.input_delay = delay;
                settings.rollback_frames = window;
                settings.players = players;
                settings.player = player;
                settings.validate()?;
                ensure!(player != 0, "the host owns controller port 1");
                connection.settings = settings;
                self.phase = Phase::GuestBundle;
                self.progress = Some(format!(
                    "Joining as player {}; receiving machine configuration and game files...",
                    player + 1
                ));
            }
            Message::Verified { identity } if self.role == Role::Host => {
                ensure!(
                    identity == self.connection().identity(),
                    "received setup produced a different machine"
                );
                let guest = self
                    .guests
                    .iter_mut()
                    .find(|guest| guest.player == from)
                    .context("unknown netplay guest")?;
                ensure!(
                    guest.phase == GuestPhase::AwaitVerified,
                    "unexpected netplay setup message"
                );
                guest.phase = GuestPhase::Running;
                let link = self
                    .connection_mut()
                    .link_mut(from)
                    .context("unknown netplay guest")?;
                send_message(link, &Message::Start)?;
                // The game starts once every port has a player: a latecomer
                // could not reproduce the frames already executed.
                if self.guests.len() == self.connection().settings.expected_links()
                    && self
                        .guests
                        .iter()
                        .all(|guest| guest.phase == GuestPhase::Running)
                {
                    self.phase = Phase::Running;
                    self.progress = Some(format!(
                        "All {} players are ready",
                        self.connection().settings.players
                    ));
                }
            }
            Message::Start if self.phase == Phase::GuestStart => {
                self.phase = Phase::Running;
            }
            Message::Refused { reason } => bail!("the host declined this player: {reason}"),
            Message::Swap { id, event } if self.phase == Phase::Running => {
                self.swap_message(emu, from, id, event)?;
            }
            _ => anyhow::bail!("unexpected netplay setup message"),
        }
        Ok(())
    }

    /// A host that is about to end its run keeps serving spectators until each
    /// connected one has received and acknowledged the whole feed, or until
    /// `timeout` passes. Spectators still in setup are not waited for.
    pub fn flush_spectators(&mut self, timeout: Duration) {
        if self.role != Role::Host || self.spectators.is_empty() {
            return;
        }
        let deadline = Instant::now() + timeout;
        while !self.spectators_delivered() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// One flush pass: read the host socket (direct-UDP spectators share it,
    /// so their acknowledgements only reach their slots when it is polled),
    /// advance every link, and report whether each connected spectator has
    /// received and acknowledged the whole feed.
    pub fn spectators_delivered(&mut self) -> bool {
        if self.role != Role::Host {
            return true;
        }
        // The players' links may already be gone; that is not this pass's
        // concern.
        let _ = self.service_listener();
        let players: Vec<usize> = self
            .connection()
            .transports()
            .map(|(player, _)| player)
            .collect();
        for player in players {
            let _ = self
                .connection_mut()
                .link_mut(player)
                .expect("link exists")
                .poll();
        }
        self.service_spectators();
        let end = self.connection().feed().map_or(0, Feed::frames);
        self.spectators.iter().all(|link| {
            link.phase != WatchPhase::Streaming
                || (link.cursor.frame >= end && !link.control.sending())
        })
    }

    /// Advance every spectator link. A spectator's failure drops that link
    /// only; the players' session is never affected.
    fn service_spectators(&mut self) {
        let Timeline::Play(connection) = &mut self.timeline else {
            return;
        };
        if self.spectator_limit == 0 {
            return;
        }
        let now = Instant::now();
        let identity = connection.identity();
        let feed = connection.feed();
        let bundle = self.bundle.as_deref();
        let mut changed = false;
        let mut index = 0;
        while index < self.spectators.len() {
            let link = &mut self.spectators[index];
            let was = link.phase;
            match link.step(now, identity, feed, bundle) {
                Ok(()) => {
                    if was != WatchPhase::Streaming && link.phase == WatchPhase::Streaming {
                        log::info!("netplay: spectator watching");
                        changed = true;
                    }
                    index += 1;
                }
                Err(error) => {
                    log::info!("netplay: spectator left: {error:#}");
                    self.spectators.remove(index);
                    changed = true;
                }
            }
        }
        if changed {
            let watching = self
                .spectators
                .iter()
                .filter(|link| link.phase == WatchPhase::Streaming)
                .count();
            self.progress = Some(match watching {
                0 => "No spectators watching".into(),
                1 => "1 spectator watching".into(),
                n => format!("{n} spectators watching"),
            });
        }
    }

    fn step_watcher(&mut self, emu: &mut Emulator, advance: bool) -> Result<bool> {
        let now = Instant::now();
        let Timeline::Watch(watcher) = &mut self.timeline else {
            unreachable!("spectators own a watcher timeline");
        };
        watcher.control.poll()?;
        while let Some(bytes) = watcher.control.take_message() {
            match (bytes.first(), self.phase) {
                (Some(&KIND_BUNDLE), Phase::WatchBundle) => {
                    let bundle = Bundle::decode(&bytes[1..])?;
                    drop(bytes);
                    let Staged {
                        emu: mut received,
                        cfg,
                        directory,
                    } = bundle.stage()?;
                    let identity = machine_identity(&mut received, &cfg)?;
                    received.set_paced(emu.paced());
                    std::mem::swap(
                        &mut received.bus_mut().paula.audio,
                        &mut emu.bus_mut().paula.audio,
                    );
                    *emu = *received;
                    self.directory = Some(directory);
                    self.changed_config = Some(cfg);
                    watcher.spectator = Some(Spectator::new(identity));
                    send_message(&mut watcher.control, &Message::Verified { identity })?;
                    self.phase = Phase::WatchStart;
                    self.progress = Some("Host setup verified; waiting for the game...".into());
                }
                (Some(&KIND_JSON), _) => match (decode_json(&bytes)?, self.phase) {
                    (Message::Start, Phase::WatchStart) => {
                        self.phase = Phase::Running;
                        watcher.last_received = now;
                        watcher.last_status = now - KEEPALIVE;
                        emu.reanchor_realtime_clock();
                        self.progress = Some(format!(
                            "Spectating ({}); F11 leaves",
                            watcher.control.route()
                        ));
                    }
                    (Message::Refused { reason }, _) => {
                        bail!("the host declined the spectator: {reason}")
                    }
                    _ => bail!("unexpected netplay setup message"),
                },
                (Some(&KIND_FEED), Phase::Running) => {
                    watcher
                        .spectator
                        .as_mut()
                        .context("spectator timeline is not ready")?
                        .push(&bytes[1..])?;
                    watcher.last_received = now;
                }
                _ => bail!("unexpected netplay message for a spectator"),
            }
        }
        if self.phase != Phase::Running {
            ensure!(
                self.started.elapsed() < SETUP_TIMEOUT,
                "netplay setup timed out"
            );
            if self.phase == Phase::WatchBundle
                && self.last_progress.elapsed() > Duration::from_secs(1)
            {
                self.progress = Some(format!(
                    "Receiving game files: {} KiB",
                    watcher.control.received_bytes() / 1024
                ));
                self.last_progress = Instant::now();
            }
            return Ok(false);
        }
        ensure!(
            now.duration_since(watcher.last_received) < SPECTATOR_TIMEOUT,
            "netplay host timed out"
        );
        let spectator = watcher
            .spectator
            .as_mut()
            .context("spectator timeline is not ready")?;
        // A checkpoint due at this frame is compared before a disk change
        // at the same frame changes the machine, as the host did.
        let settled = spectator.verify_frame(emu)?;
        if now.duration_since(watcher.last_status) >= KEEPALIVE && watcher.control.can_send() {
            send_feed(
                &mut watcher.control,
                &FeedMessage::Status {
                    frame: spectator.executed(),
                },
            )?;
            watcher.last_status = now;
        }
        if let Some(swap) = spectator.due_swap().filter(|_| settled) {
            let (drive, ejected) = (swap.drive, swap.bytes.is_empty());
            spectate::apply_swap(emu, swap)?;
            spectator.swap_applied();
            self.progress = Some(format!(
                "DF{drive} {} by the host",
                if ejected { "ejected" } else { "changed" }
            ));
        }
        let mut stepped = false;
        if advance {
            stepped = spectator.step(&mut EmulatedMachine(emu))?;
        }
        Ok(stepped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Addresses for `count` peers, reserved together and released before
    /// the sessions bind them.
    pub(super) fn addresses(count: usize) -> Result<Vec<std::net::SocketAddr>> {
        let reserve: Vec<_> = (0..count)
            .map(|_| std::net::UdpSocket::bind("127.0.0.1:0"))
            .collect::<std::io::Result<_>>()?;
        let addresses: Vec<_> = reserve
            .iter()
            .map(|s| s.local_addr())
            .collect::<std::io::Result<_>>()?;
        drop(reserve);
        Ok(addresses)
    }

    /// Direct-UDP options for one player of a `players`-player session. The
    /// host lists every guest address; each guest names the host.
    pub(super) fn options(
        addresses: &[std::net::SocketAddr],
        player: usize,
        players: usize,
        session: [u8; 16],
        spectators: u8,
    ) -> Options {
        Options {
            bind: addresses[player],
            peers: if player == 0 {
                addresses[1..players].to_vec()
            } else {
                vec![addresses[0]]
            },
            player,
            players,
            session,
            input_delay: 2,
            rollback_frames: 8,
            spectators: if player == 0 { spectators } else { 0 },
        }
    }

    #[test]
    fn guest_adopts_host_setup_and_both_peers_commit_insert_and_eject() -> Result<()> {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| -> Result<()> {
                let addresses = addresses(2)?;
                let mut machines = [
                    super::super::tests::emulator()?,
                    super::super::tests::emulator()?,
                ];
                let mut cfg = super::super::tests::safe_config()?;
                cfg.floppy_connected = [true; 4];
                prepare_config(&mut cfg)?;
                let mut guest_cfg = cfg.clone();
                guest_cfg.chip_ram_bytes *= 2;
                let mut peers = [
                    Session::new(
                        options(&addresses, 0, 2, [17; 16], 0),
                        &mut machines[0],
                        &cfg,
                    )?,
                    Session::new(
                        options(&addresses, 1, 2, [17; 16], 0),
                        &mut machines[1],
                        &guest_cfg,
                    )?,
                ];
                let deadline = Instant::now() + Duration::from_secs(90);
                while !peers.iter().all(|p| p.status().connected) {
                    for n in 0..2 {
                        peers[n].step(&mut machines[n], Input::default(), false)?;
                    }
                    ensure!(Instant::now() < deadline, "setup did not connect");
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(peers[0].guest_count(), 1);
                assert_eq!(peers[1].port(), Some(1));
                assert_eq!(
                    peers[1].take_config().unwrap().chip_ram_bytes,
                    cfg.chip_ram_bytes
                );
                assert_eq!(
                    machines[0].netplay_snapshot()?,
                    machines[1].netplay_snapshot()?
                );
                assert!(!peers[1].can_change_disk());
                assert!(
                    peers[0].bundle.is_some(),
                    "the host keeps its bundle while ports remain open"
                );
                assert!(machines.iter().all(|emu| !emu.paced()));
                let before = machines[0].netplay_snapshot()?;
                assert!(peers[0]
                    .change_disk(&machines[0], 0, vec![1, 2, 3], true)
                    .is_err());
                assert_eq!(before, machines[0].netplay_snapshot()?);
                for (drive, bytes) in
                    (0..4).flat_map(|drive| [(drive, vec![0; 901_120]), (drive, Vec::new())])
                {
                    let inserted = !bytes.is_empty();
                    peers[0].change_disk(&machines[0], drive, bytes, inserted)?;
                    while peers.iter().any(|p| p.swap.is_some()) || !peers[0].can_change_disk() {
                        for n in 0..2 {
                            peers[n].step(&mut machines[n], Input::default(), true)?;
                        }
                        ensure!(Instant::now() < deadline, "disk change did not finish");
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    // Stop both on a common frame before comparing full state.
                    let target = peers.iter().map(|p| p.status().frame).max().unwrap() + 2;
                    while !peers
                        .iter()
                        .all(|p| p.status().frame == target && p.ready_to_capture())
                    {
                        for n in 0..2 {
                            let advance = peers[n].status().frame < target;
                            peers[n].step(&mut machines[n], Input::default(), advance)?;
                        }
                        ensure!(Instant::now() < deadline, "confirmation did not finish");
                    }
                    assert_eq!(machines[0].bus().floppy.disk_inserted(drive), inserted);
                    assert_eq!(machines[1].bus().floppy.disk_inserted(drive), inserted);
                    assert_eq!(
                        machines[0].netplay_snapshot()?,
                        machines[1].netplay_snapshot()?
                    );
                }
                Ok(())
            })?
            .join()
            .unwrap()
    }

    /// Four peers, one controller port each: the two game ports and both
    /// sockets of the parallel-port adapter. Guests never address each
    /// other, so every switch a guest sees travelled through the host.
    #[test]
    fn four_players_share_one_machine_through_the_host() -> Result<()> {
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(|| -> Result<()> {
                let addresses = addresses(4)?;
                let mut machines: Vec<_> = (0..4)
                    .map(|_| super::super::tests::emulator_with_adapter())
                    .collect::<Result<_>>()?;
                let mut cfg = super::super::tests::adapter_config()?;
                cfg.floppy_connected = [true; 4];
                prepare_config(&mut cfg)?;
                let mut peers: Vec<Session> = Vec::new();
                for player in 0..4 {
                    peers.push(Session::new(
                        options(&addresses, player, 4, [43; 16], 0),
                        &mut machines[player],
                        &cfg,
                    )?);
                }
                assert_eq!(peers[0].role(), Role::Host);
                assert!(peers[1..].iter().all(|p| p.role() == Role::Guest));
                let deadline = Instant::now() + Duration::from_secs(120);
                while !peers.iter().all(|p| p.status().connected) {
                    for n in 0..4 {
                        peers[n].step(&mut machines[n], Input::default(), false)?;
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "four-player setup did not connect"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(peers[0].guest_count(), 3);
                for (player, peer) in peers.iter().enumerate() {
                    assert_eq!(peer.port(), Some(player));
                    assert_eq!(peer.players(), 4);
                }
                // One direction per player: up, down, left, right.
                let held = |player: usize| Input {
                    buttons: 1 << player,
                    ..Default::default()
                };
                let target = 90;
                loop {
                    let mut done = true;
                    for n in 0..4 {
                        let advance = peers[n].status().frame < target;
                        peers[n].step(&mut machines[n], held(n), advance)?;
                        let status = peers[n].status();
                        done &= status.frame == target && peers[n].ready_to_capture();
                    }
                    if done {
                        break;
                    }
                    if Instant::now() >= deadline {
                        let stuck: Vec<_> = peers
                            .iter()
                            .enumerate()
                            .map(|(n, peer)| {
                                let s = peer.status();
                                format!(
                                    "player {}: frame {} confirmed {} acknowledged {}",
                                    n + 1,
                                    s.frame,
                                    s.confirmed_frame,
                                    s.acknowledged_frame
                                )
                            })
                            .collect();
                        bail!("players did not reach frame {target}: {}", stuck.join("; "));
                    }
                }
                for (player, machine) in machines.iter().enumerate() {
                    let input = &machine.bus().input;
                    assert!(input.ports[0].up, "player 1 on machine {player}");
                    assert!(input.ports[1].down, "player 2 on machine {player}");
                    assert!(
                        input.parallel_joysticks[0].fitted && input.parallel_joysticks[0].left,
                        "player 3 on machine {player}"
                    );
                    assert!(
                        input.parallel_joysticks[1].fitted && input.parallel_joysticks[1].right,
                        "player 4 on machine {player}"
                    );
                }
                let host_state = machines[0].netplay_snapshot()?;
                for (player, machine) in machines.iter().enumerate().skip(1) {
                    assert_eq!(
                        machine.netplay_snapshot()?,
                        host_state,
                        "player {} diverged from the host",
                        player + 1
                    );
                }
                assert!(peers.iter().all(|p| p.status().checked_frame == 60));
                assert!(peers.iter().all(|p| p.failure.is_none()));
                // A disk change waits for every guest at each stage, not
                // just the first to answer.
                assert!(peers[0].can_change_disk());
                assert!(peers[1..].iter().all(|p| !p.can_change_disk()));
                peers[0].change_disk(&machines[0], 1, vec![7; 901_120], false)?;
                while peers.iter().any(|p| p.swap.is_some()) || !peers[0].can_change_disk() {
                    for n in 0..4 {
                        peers[n].step(&mut machines[n], held(n), true)?;
                    }
                    ensure!(Instant::now() < deadline, "disk change did not finish");
                    std::thread::sleep(Duration::from_millis(1));
                }
                let settled = peers.iter().map(|p| p.status().frame).max().unwrap() + 2;
                run_until(&mut peers, &mut machines, settled, deadline)?;
                let host_state = machines[0].netplay_snapshot()?;
                for (player, machine) in machines.iter().enumerate() {
                    assert!(
                        machine.bus().floppy.disk_inserted(1),
                        "player {} missed the disk change",
                        player + 1
                    );
                    assert_eq!(
                        machine.netplay_snapshot()?,
                        host_state,
                        "player {} diverged over the disk change",
                        player + 1
                    );
                }
                Ok(())
            })?
            .join()
            .unwrap()
    }

    /// Run the players until `frames` frames are confirmed on each, servicing
    /// any spectators, then hold everyone on that frame.
    pub(super) fn run_until(
        peers: &mut [Session],
        machines: &mut [Emulator],
        frames: u64,
        deadline: Instant,
    ) -> Result<()> {
        loop {
            let mut done = true;
            for n in 0..peers.len() {
                let advance = peers[n].status().frame < frames;
                peers[n].step(&mut machines[n], Input::default(), advance)?;
                let status = peers[n].status();
                done &= status.connected && status.frame == frames && peers[n].ready_to_capture();
            }
            if done {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "peers did not reach frame {frames}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn spectator_joins_late_replays_backlog_and_follows_disk_swaps() -> Result<()> {
        std::thread::Builder::new()
            .stack_size(48 * 1024 * 1024)
            .spawn(|| -> Result<()> {
                let addresses = addresses(3)?;
                let mut machines = vec![
                    super::super::tests::emulator()?,
                    super::super::tests::emulator()?,
                ];
                let mut cfg = super::super::tests::safe_config()?;
                cfg.floppy_connected = [true; 4];
                prepare_config(&mut cfg)?;
                let mut peers = vec![
                    Session::new(
                        options(&addresses, 0, 2, [23; 16], 1),
                        &mut machines[0],
                        &cfg,
                    )?,
                    Session::new(
                        options(&addresses, 1, 2, [23; 16], 0),
                        &mut machines[1],
                        &cfg,
                    )?,
                ];
                assert_eq!(peers[0].role(), Role::Host);
                assert_eq!(peers[1].role(), Role::Guest);
                let deadline = Instant::now() + Duration::from_secs(120);
                run_until(&mut peers, &mut machines, 70, deadline)?;
                assert!(
                    peers[0].bundle.is_some(),
                    "the host keeps its bundle for spectators"
                );
                // A disk change before the spectator exists must be replayed
                // from the host's history at the same frame.
                peers[0].change_disk(&machines[0], 1, vec![0; 901_120], false)?;
                while peers.iter().any(|p| p.swap.is_some()) || !peers[0].can_change_disk() {
                    for n in 0..2 {
                        peers[n].step(&mut machines[n], Input::default(), true)?;
                    }
                    ensure!(Instant::now() < deadline, "disk change did not finish");
                    std::thread::sleep(Duration::from_millis(1));
                }
                run_until(&mut peers, &mut machines, 130, deadline)?;
                machines.push(super::super::tests::emulator()?);
                peers.push(Session::new(
                    WatchOptions {
                        bind: addresses[2],
                        host: addresses[0],
                        session: [23; 16],
                    },
                    &mut machines[2],
                    &cfg,
                )?);
                assert_eq!(peers[2].role(), Role::Spectator);
                assert!(peers[2].port().is_none());
                assert!(!peers[2].can_change_disk());
                // The spectator catches up to the held frame while the
                // players stay put.
                while peers[2].status().frame < 130 {
                    for n in 0..3 {
                        peers[n].step(&mut machines[n], Input::default(), n == 2)?;
                    }
                    ensure!(Instant::now() < deadline, "spectator did not catch up");
                    if !peers[2].status().connected {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                assert_eq!(peers[0].spectator_count(), 1);
                assert_eq!(
                    peers[2].take_config().unwrap().chip_ram_bytes,
                    cfg.chip_ram_bytes
                );
                assert!(machines[2].bus().floppy.disk_inserted(1));
                assert_eq!(
                    machines[2].netplay_snapshot()?,
                    machines[0].netplay_snapshot()?,
                    "late joiner replayed the backlog and the disk change"
                );
                assert_eq!(peers[2].status().checked_frame, 120);
                // Live: an eject and an insert while the spectator follows.
                for (drive, bytes) in [(1, Vec::new()), (0, vec![1; 901_120])] {
                    peers[0].change_disk(&machines[0], drive, bytes, false)?;
                    while peers[..2].iter().any(|p| p.swap.is_some()) || !peers[0].can_change_disk()
                    {
                        for n in 0..3 {
                            peers[n].step(&mut machines[n], Input::default(), true)?;
                        }
                        ensure!(Instant::now() < deadline, "disk change did not finish");
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                let target = peers[..2].iter().map(|p| p.status().frame).max().unwrap() + 65;
                run_until(&mut peers, &mut machines, target, deadline)?;
                assert!(peers[2].status().behind == 0 && peers[2].status().frame == target);
                assert!(!machines[2].bus().floppy.disk_inserted(1));
                assert!(machines[2].bus().floppy.disk_inserted(0));
                assert_eq!(
                    machines[2].netplay_snapshot()?,
                    machines[0].netplay_snapshot()?
                );
                assert_eq!(
                    peers[2].status().checked_frame,
                    peers[0].status().checked_frame
                );
                // Losing the spectator never touches the players: the host
                // drops the silent link after its timeout while play goes on.
                let spectator = peers.pop().unwrap();
                drop(spectator);
                machines.pop();
                let leave = Instant::now() + Duration::from_secs(30);
                run_until(&mut peers, &mut machines, target + 20, leave)?;
                while peers[0].spectator_count() > 0 {
                    for n in 0..2 {
                        peers[n].step(&mut machines[n], Input::default(), false)?;
                    }
                    ensure!(Instant::now() < leave, "the host kept a vanished spectator");
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert!(peers[0].failure.is_none() && peers[1].failure.is_none());
                assert_eq!(peers[0].status().frame, target + 20);
                Ok(())
            })?
            .join()
            .unwrap()
    }
}

#[cfg(test)]
mod flush_tests {
    use super::*;

    /// A host that ends its run keeps the feed flowing to a direct-UDP
    /// spectator, whose acknowledgements arrive on the host's own socket and
    /// so depend on the flush polling it.
    #[test]
    fn host_flush_delivers_the_feed_to_a_udp_spectator() -> Result<()> {
        std::thread::Builder::new()
            .stack_size(48 * 1024 * 1024)
            .spawn(|| -> Result<()> {
                let addresses = super::tests::addresses(3)?;
                let mut machines = vec![
                    super::super::tests::emulator()?,
                    super::super::tests::emulator()?,
                    super::super::tests::emulator()?,
                ];
                let mut cfg = super::super::tests::safe_config()?;
                prepare_config(&mut cfg)?;
                let mut sessions = vec![
                    Session::new(
                        super::tests::options(&addresses, 0, 2, [29; 16], 1),
                        &mut machines[0],
                        &cfg,
                    )?,
                    Session::new(
                        super::tests::options(&addresses, 1, 2, [29; 16], 0),
                        &mut machines[1],
                        &cfg,
                    )?,
                ];
                let deadline = Instant::now() + Duration::from_secs(120);
                super::tests::run_until(&mut sessions, &mut machines, 90, deadline)?;
                sessions.push(Session::new(
                    WatchOptions {
                        bind: addresses[2],
                        host: addresses[0],
                        session: [29; 16],
                    },
                    &mut machines[2],
                    &cfg,
                )?);
                // Admit the spectator and let it finish setup while the
                // players hold their frame.
                while !sessions[2].status().connected {
                    for n in 0..3 {
                        sessions[n].step(&mut machines[n], Input::default(), n == 2)?;
                    }
                    ensure!(Instant::now() < deadline, "the spectator did not connect");
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(sessions[0].spectator_count(), 1);
                // The players' run is over: only the flush passes service
                // the host from here on, while the spectator keeps polling.
                let mut delivered = false;
                while !delivered {
                    delivered = sessions[0].spectators_delivered();
                    sessions[2].step(&mut machines[2], Input::default(), true)?;
                    ensure!(
                        Instant::now() < deadline,
                        "the flush never delivered the feed"
                    );
                }
                assert_eq!(sessions[0].spectator_count(), 1);
                while sessions[2].status().behind > 0 {
                    sessions[2].step(&mut machines[2], Input::default(), true)?;
                    ensure!(Instant::now() < deadline, "the spectator did not finish");
                }
                assert_eq!(sessions[2].status().frame, 90);
                assert_eq!(sessions[2].status().checked_frame, 60);
                assert_eq!(
                    machines[2].netplay_snapshot()?,
                    machines[0].netplay_snapshot()?
                );
                Ok(())
            })?
            .join()
            .unwrap()
    }
}
