// SPDX-License-Identifier: GPL-3.0-or-later

use super::{
    control::{Control, ROLE_GUEST, ROLE_HOST, ROLE_SPECTATOR},
    setup::{Bundle, Staged},
    spectate::{self, Feed, FeedCursor, FeedDecoder, FeedMessage, Spectator, SwapRecord},
    transport::{NativeTransport, SpectatorTransport, UdpTransport},
    *,
};
use crate::{
    config::Config,
    emulator::Emulator,
    timebase::{Duration, Instant},
};
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Message {
    Hello {
        build: String,
    },
    Offer {
        delay: u8,
        window: u8,
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
    /// The host declines a spectator.
    Refused {
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_adopts_host_setup_and_both_peers_commit_insert_and_eject() -> Result<()> {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| -> Result<()> {
                let reserve = [
                    std::net::UdpSocket::bind("127.0.0.1:0")?,
                    std::net::UdpSocket::bind("127.0.0.1:0")?,
                ];
                let addresses = [reserve[0].local_addr()?, reserve[1].local_addr()?];
                drop(reserve);
                let mut machines = [
                    super::super::tests::emulator()?,
                    super::super::tests::emulator()?,
                ];
                let mut cfg = super::super::tests::safe_config()?;
                cfg.floppy_connected = [true; 4];
                prepare_config(&mut cfg)?;
                let mut guest_cfg = cfg.clone();
                guest_cfg.chip_ram_bytes *= 2;
                let options = |player| Options {
                    bind: addresses[player],
                    peer: addresses[1 - player],
                    player,
                    session: [17; 16],
                    input_delay: 2,
                    rollback_frames: 8,
                    spectators: 0,
                };
                let mut peers = [
                    Session::new(options(0), &mut machines[0], &cfg)?,
                    Session::new(options(1), &mut machines[1], &guest_cfg)?,
                ];
                let deadline = Instant::now() + Duration::from_secs(90);
                while !peers.iter().all(|p| p.status().connected) {
                    for n in 0..2 {
                        peers[n].step(&mut machines[n], Input::default(), false)?;
                    }
                    ensure!(Instant::now() < deadline, "setup did not connect");
                    std::thread::sleep(Duration::from_millis(1));
                }
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
                    peers[0].bundle.is_none(),
                    "no spectators: the bundle is released"
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

    /// Run the players until `frames` frames are confirmed on both, servicing
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
                let reserve: Vec<_> = (0..3)
                    .map(|_| std::net::UdpSocket::bind("127.0.0.1:0"))
                    .collect::<std::io::Result<_>>()?;
                let addresses: Vec<_> = reserve
                    .iter()
                    .map(|s| s.local_addr())
                    .collect::<std::io::Result<_>>()?;
                drop(reserve);
                let mut machines = vec![
                    super::super::tests::emulator()?,
                    super::super::tests::emulator()?,
                ];
                let mut cfg = super::super::tests::safe_config()?;
                cfg.floppy_connected = [true; 4];
                prepare_config(&mut cfg)?;
                let options = |player: usize| Options {
                    bind: addresses[player],
                    peer: addresses[1 - player],
                    player,
                    session: [23; 16],
                    input_delay: 2,
                    rollback_frames: 8,
                    spectators: if player == 0 { 1 } else { 0 },
                };
                let mut peers = vec![
                    Session::new(options(0), &mut machines[0], &cfg)?,
                    Session::new(options(1), &mut machines[1], &cfg)?,
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

#[derive(PartialEq, Eq)]
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
    peer_digest: Option<[u8; 32]>,
    /// The host's own digest at the stopped boundary, kept for spectators.
    own_digest: Option<[u8; 32]>,
    started: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    HostHello,
    GuestOffer,
    GuestBundle,
    HostVerified,
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

/// The host's link to one spectator.
struct SpectatorLink {
    control: Control<SpectatorTransport>,
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
    Play(Box<Connection<Control<NativeTransport>>>),
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
    timeline: Timeline,
    phase: Phase,
    /// Retained while spectators may still join.
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
        let (transport, spectator_tag) = match options {
            ConnectionOptions::Direct(options) => {
                let tag = options.session;
                (NativeTransport::Udp(UdpTransport::new(options)?), tag)
            }
            #[cfg(feature = "netplay-internet")]
            ConnectionOptions::Internet(options) => {
                let tag = options
                    .spectator_capability
                    .unwrap_or(options.invitation.session);
                (
                    NativeTransport::Internet(Box::new(internet::InternetTransport::new(
                        *options,
                    )?)),
                    tag,
                )
            }
            _ => unreachable!("players use player options"),
        };
        let (mine, theirs) = if role == Role::Host {
            (ROLE_HOST, ROLE_GUEST)
        } else {
            (ROLE_GUEST, ROLE_HOST)
        };
        let control = Control::new(transport, settings.session, mine, theirs);
        let (mut connection, directory, bundle, changed_config) =
            if let Some((mut staged, bytes)) = staged {
                staged.emu.set_paced(emu.paced());
                let connection = Connection::with_transport(
                    settings.clone(),
                    control,
                    &mut staged.emu,
                    &staged.cfg,
                )?;
                std::mem::swap(
                    &mut staged.emu.bus_mut().paula.audio,
                    &mut emu.bus_mut().paula.audio,
                );
                *emu = *staged.emu;
                (
                    connection,
                    Some(staged.directory),
                    Some(bytes.into_iter().map(Arc::new).collect()),
                    Some(staged.cfg),
                )
            } else {
                (
                    Connection::with_transport(settings.clone(), control, emu, cfg)?,
                    None,
                    None,
                    None,
                )
            };
        if spectator_limit > 0 {
            connection.enable_feed(spectate::NATIVE_FEED_LIMIT)?;
        }
        let now = Instant::now();
        let mut session = Self {
            role,
            timeline: Timeline::Play(Box::new(connection)),
            phase: if role == Role::Host {
                Phase::HostHello
            } else {
                Phase::GuestOffer
            },
            bundle,
            directory,
            changed_config,
            started: now,
            progress: Some("Waiting for the other player...".into()),
            last_progress: now,
            failure: None,
            swap: None,
            swap_id: 0,
            spectators: Vec::new(),
            spectator_limit,
            spectator_tag,
        };
        if role == Role::Guest {
            session.send(Message::Hello {
                build: env!("COPPERLINE_DISPLAY_VERSION").into(),
            })?;
        }
        Ok(session)
    }

    fn watch(options: ConnectionOptions) -> Result<Self> {
        let (transport, tag) = match options {
            ConnectionOptions::Watch(options) => {
                let tag = options.session;
                (NativeTransport::Udp(UdpTransport::watch(options)?), tag)
            }
            #[cfg(feature = "netplay-internet")]
            ConnectionOptions::WatchInternet(options) => {
                let tag = options.invitation.capability;
                (
                    NativeTransport::Internet(Box::new(internet::InternetTransport::watch(
                        *options,
                    )?)),
                    tag,
                )
            }
            _ => unreachable!("spectators use watch options"),
        };
        let now = Instant::now();
        let mut session = Self {
            role: Role::Spectator,
            timeline: Timeline::Watch(Box::new(Watcher {
                control: Control::new(transport, tag, ROLE_SPECTATOR, ROLE_HOST),
                spectator: None,
                last_received: now,
                last_status: now,
                catching_up: false,
            })),
            phase: Phase::WatchBundle,
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
        session.send(Message::Watch {
            build: env!("COPPERLINE_DISPLAY_VERSION").into(),
        })?;
        Ok(session)
    }

    fn control(&self) -> &Control<NativeTransport> {
        match &self.timeline {
            Timeline::Play(connection) => &connection.transport,
            Timeline::Watch(watcher) => &watcher.control,
        }
    }

    fn control_mut(&mut self) -> &mut Control<NativeTransport> {
        match &mut self.timeline {
            Timeline::Play(connection) => &mut connection.transport,
            Timeline::Watch(watcher) => &mut watcher.control,
        }
    }

    fn connection(&self) -> &Connection<Control<NativeTransport>> {
        match &self.timeline {
            Timeline::Play(connection) => connection,
            Timeline::Watch(_) => unreachable!("spectators have no rollback timeline"),
        }
    }

    fn connection_mut(&mut self) -> &mut Connection<Control<NativeTransport>> {
        match &mut self.timeline {
            Timeline::Play(connection) => connection,
            Timeline::Watch(_) => unreachable!("spectators have no rollback timeline"),
        }
    }

    pub fn options(&self) -> ConnectionOptions {
        self.control().inner.options()
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
    pub fn route(&self) -> &'static str {
        self.control().route()
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
    pub fn ready_to_capture(&self) -> bool {
        match &self.timeline {
            Timeline::Play(connection) => {
                self.swap.is_none()
                    && !connection.transport.sending()
                    && self.status().ready_to_capture()
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
            && !self.control().sending()
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
        self.swap = Some(Swap {
            phase: SwapPhase::HostHeld,
            stop: self.status().frame,
            drive,
            writable,
            size,
            hash,
            bytes: Some(Arc::new(bytes)),
            peer_digest: None,
            own_digest: None,
            started: Instant::now(),
        });
        self.swap_send(SwapMessage::Begin {
            drive,
            size,
            hash,
            writable,
        })?;
        self.progress = Some(format!("Pausing both players for DF{drive}..."));
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

    fn swap_send(&mut self, event: SwapMessage) -> Result<()> {
        self.send(Message::Swap {
            id: self.swap_id,
            event,
        })
    }

    fn swap_message(&mut self, emu: &mut Emulator, id: u64, event: SwapMessage) -> Result<()> {
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
            self.swap = Some(Swap {
                phase: SwapPhase::GuestTarget,
                stop: frame,
                drive,
                size,
                hash,
                writable,
                bytes: None,
                peer_digest: None,
                own_digest: None,
                started: Instant::now(),
            });
            self.progress = Some(format!("Host is changing DF{drive}..."));
            return self.swap_send(SwapMessage::Held { frame });
        }
        ensure!(id == self.swap_id, "unexpected disk change identifier");
        let frame = self.status().frame;
        let swap = self.swap.as_mut().context("no disk change in progress")?;
        match event {
            SwapMessage::Held { frame: peer } if swap.phase == SwapPhase::HostHeld => {
                ensure!(peer.abs_diff(frame) <= 32, "invalid peer disk change frame");
                let target = peer.max(frame);
                swap.stop = target;
                swap.phase = SwapPhase::HostReady;
                self.swap_send(SwapMessage::Target { frame: target })?;
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
                swap.peer_digest = Some(hash);
            }
            SwapMessage::Prepared if swap.phase == SwapPhase::HostPrepared => {
                self.apply_disk(emu)?;
                self.swap.as_mut().unwrap().phase = SwapPhase::HostApplied;
                self.swap_send(SwapMessage::Apply)?;
            }
            SwapMessage::Apply if swap.phase == SwapPhase::GuestApply => {
                self.apply_disk(emu)?;
                self.swap.as_mut().unwrap().phase = SwapPhase::GuestResume;
                self.swap_send(SwapMessage::Applied {
                    hash: self.confirmed_state_digest(emu)?,
                })?;
            }
            SwapMessage::Applied { hash } if swap.phase == SwapPhase::HostApplied => {
                ensure!(
                    self.confirmed_state_digest(emu)? == hash,
                    "players differ after the disk change"
                );
                // Both players agree on the change; spectators replay it at
                // this frame with the same digests on either side.
                let swap = self.swap.as_ref().unwrap();
                let record = SwapRecord {
                    frame: swap.stop,
                    drive: swap.drive,
                    writable: swap.writable,
                    before: swap
                        .own_digest
                        .context("disk change digest was not captured")?,
                    after: hash,
                    bytes: swap
                        .bytes
                        .clone()
                        .context("replacement disk was not retained")?,
                };
                self.connection_mut().feed_swap(record)?;
                self.swap_send(SwapMessage::Resume)?;
                self.finish_swap();
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
            "DF{} {} on both players",
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

    fn send(&mut self, message: Message) -> Result<()> {
        send_message(self.control_mut(), &message)
    }

    fn step_player(
        &mut self,
        emu: &mut Emulator,
        input: &mut LocalInput,
        advance: bool,
    ) -> Result<bool> {
        self.control_mut().poll()?;
        ensure!(
            !matches!(self.phase, Phase::HostHello | Phase::GuestOffer)
                || !self.control().has_game_packets(),
            "peer does not support desktop setup transfer; use the same Copperline build"
        );
        while let Some(bytes) = self.control_mut().take_message() {
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
                self.swap_send(SwapMessage::Prepared)?;
                continue;
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
                connection.identity = initial_identity(&connection.settings, &mut received, &cfg)?;
                connection.rollback = Rollback::new(
                    connection.settings.player,
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
                self.send(Message::Verified { identity })?;
                self.phase = Phase::GuestStart;
                self.progress = Some("Host setup verified; waiting to start...".into());
                continue;
            }
            let message = decode_json(&bytes)?;
            match message {
                Message::Hello { build } if self.phase == Phase::HostHello => {
                    ensure!(
                        build == env!("COPPERLINE_DISPLAY_VERSION"),
                        "netplay requires the same Copperline build on both peers"
                    );
                    let (delay, window) = {
                        let settings = &self.connection().settings;
                        (settings.input_delay, settings.rollback_frames)
                    };
                    self.send(Message::Offer { delay, window })?;
                    let mut parts = vec![Arc::new(vec![KIND_BUNDLE])];
                    parts.extend(self.bundle.as_ref().unwrap().iter().cloned());
                    self.control_mut().send_parts(parts)?;
                    if self.spectator_limit == 0 {
                        self.bundle = None;
                    }
                    self.phase = Phase::HostVerified;
                    self.progress = Some("Sending machine configuration and game files...".into());
                }
                Message::Offer { delay, window } if self.phase == Phase::GuestOffer => {
                    let connection = self.connection_mut();
                    let mut settings = connection.settings.clone();
                    settings.input_delay = delay;
                    settings.rollback_frames = window;
                    settings.validate()?;
                    connection.settings = settings;
                    self.phase = Phase::GuestBundle;
                    self.progress =
                        Some("Receiving machine configuration and game files...".into());
                }
                Message::Verified { identity } if self.phase == Phase::HostVerified => {
                    ensure!(
                        identity == self.connection().identity(),
                        "received setup produced a different machine"
                    );
                    self.send(Message::Start)?;
                    self.phase = Phase::Running;
                }
                Message::Start if self.phase == Phase::GuestStart => {
                    self.phase = Phase::Running;
                }
                Message::Swap { id, event } if self.phase == Phase::Running => {
                    self.swap_message(emu, id, event)?;
                }
                _ => anyhow::bail!("unexpected netplay setup message"),
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
                self.progress = Some(format!(
                    "Receiving game files: {} KiB",
                    self.control().received_bytes() / 1024
                ));
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
        let status = self.status();
        if let Some(swap) = &self.swap {
            ensure!(
                swap.started.elapsed() < Duration::from_secs(180),
                "disk change timed out"
            );
            if status.frame == swap.stop && status.ready_to_capture() {
                let own = self.confirmed_state_digest(emu)?;
                if swap.phase == SwapPhase::GuestReady {
                    self.swap.as_mut().unwrap().phase = SwapPhase::GuestBytes;
                    self.swap_send(SwapMessage::Ready { hash: own })?;
                } else if swap.phase == SwapPhase::HostReady {
                    if let Some(peer) = swap.peer_digest {
                        ensure!(peer == own, "players differ before the disk change");
                        let swap = self.swap.as_mut().unwrap();
                        let mut bytes = vec![KIND_DISK];
                        bytes.extend_from_slice(swap.bytes.as_ref().unwrap());
                        swap.own_digest = Some(own);
                        swap.phase = SwapPhase::HostPrepared;
                        self.control_mut().send_message(bytes)?;
                    }
                }
            }
        }
        self.service_spectators();
        Ok(stepped)
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
        // The players' link may already be gone; that is not this pass's
        // concern.
        let _ = self.control_mut().poll();
        self.service_spectators();
        let end = self.connection().feed().map_or(0, Feed::frames);
        self.spectators.iter().all(|link| {
            link.phase != WatchPhase::Streaming
                || (link.cursor.frame >= end && !link.control.sending())
        })
    }

    /// Admit newly connected spectators and advance every link. A spectator's
    /// failure drops that link only; the players' session is never affected.
    fn service_spectators(&mut self) {
        let Timeline::Play(connection) = &mut self.timeline else {
            return;
        };
        if self.spectator_limit == 0 {
            return;
        }
        let now = Instant::now();
        for transport in connection.transport.inner.take_spectators() {
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
                let reserve: Vec<_> = (0..3)
                    .map(|_| std::net::UdpSocket::bind("127.0.0.1:0"))
                    .collect::<std::io::Result<_>>()?;
                let addresses: Vec<_> = reserve
                    .iter()
                    .map(|s| s.local_addr())
                    .collect::<std::io::Result<_>>()?;
                drop(reserve);
                let mut machines = vec![
                    super::super::tests::emulator()?,
                    super::super::tests::emulator()?,
                    super::super::tests::emulator()?,
                ];
                let mut cfg = super::super::tests::safe_config()?;
                prepare_config(&mut cfg)?;
                let options = |player: usize| Options {
                    bind: addresses[player],
                    peer: addresses[1 - player],
                    player,
                    session: [29; 16],
                    input_delay: 2,
                    rollback_frames: 8,
                    spectators: if player == 0 { 1 } else { 0 },
                };
                let mut sessions = vec![
                    Session::new(options(0), &mut machines[0], &cfg)?,
                    Session::new(options(1), &mut machines[1], &cfg)?,
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
