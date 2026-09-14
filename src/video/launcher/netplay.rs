// SPDX-License-Identifier: GPL-3.0-or-later

//! Editable connection details belong to this launcher session, not a machine file.

use super::*;
use crate::netplay::{Options, Role, WatchOptions};
#[cfg(feature = "netplay-internet")]
use anyhow::Context;

#[derive(Debug, Clone)]
pub struct NetplaySetup {
    pub enabled: bool,
    pub internet: bool,
    pub relay: String,
    pub relay_only: bool,
    #[cfg(feature = "netplay-internet")]
    pub internet_host: Option<crate::netplay::internet::Options>,
    pub bind: String,
    pub peer: String,
    pub player: usize,
    /// Watch the host's game without owning a controller port.
    pub spectator: bool,
    /// Spectators a host admits (0 = none).
    pub spectators: u8,
    pub code: String,
    /// The host's Internet spectator invitation, generated with the code.
    pub spectator_code: String,
    pub delay: u8,
    pub rollback: u8,
}

impl Default for NetplaySetup {
    fn default() -> Self {
        Self {
            enabled: false,
            internet: false,
            relay: String::new(),
            relay_only: false,
            #[cfg(feature = "netplay-internet")]
            internet_host: None,
            bind: "0.0.0.0:19732".into(),
            peer: String::new(),
            player: 0,
            spectator: false,
            spectators: 0,
            code: String::new(),
            spectator_code: String::new(),
            delay: 2,
            rollback: 8,
        }
    }
}

fn hex(session: &[u8; 16]) -> String {
    session.iter().map(|b| format!("{b:02x}")).collect()
}

impl From<&Options> for NetplaySetup {
    fn from(options: &Options) -> Self {
        Self {
            enabled: true,
            bind: options.bind.to_string(),
            peer: options.peer.to_string(),
            player: options.player,
            spectators: options.spectators,
            code: hex(&options.session),
            delay: options.input_delay,
            rollback: options.rollback_frames,
            ..Self::default()
        }
    }
}

impl From<&WatchOptions> for NetplaySetup {
    fn from(options: &WatchOptions) -> Self {
        Self {
            enabled: true,
            bind: options.bind.to_string(),
            peer: options.host.to_string(),
            spectator: true,
            code: hex(&options.session),
            ..Self::default()
        }
    }
}

#[cfg(feature = "netplay-internet")]
fn single_relay(endpoint: &iroh::EndpointAddr) -> String {
    if endpoint.relay_urls().count() == 1 {
        endpoint.relay_urls().next().unwrap().to_string()
    } else {
        String::new()
    }
}

impl From<&crate::netplay::ConnectionOptions> for NetplaySetup {
    fn from(options: &crate::netplay::ConnectionOptions) -> Self {
        match options {
            crate::netplay::ConnectionOptions::Direct(options) => Self::from(options),
            crate::netplay::ConnectionOptions::Watch(options) => Self::from(options),
            #[cfg(feature = "netplay-internet")]
            crate::netplay::ConnectionOptions::Internet(options) => Self {
                enabled: true,
                internet: true,
                player: options.settings().player,
                spectators: options.spectators,
                code: options.invitation.encode().expect("validated invitation"),
                spectator_code: options
                    .spectator_invitation()
                    .and_then(|invitation| invitation.encode().ok())
                    .unwrap_or_default(),
                delay: options.invitation.delay,
                rollback: options.invitation.window,
                relay_only: options.relay_only,
                relay: single_relay(&options.invitation.endpoint),
                internet_host: options.host_key.as_ref().map(|_| options.as_ref().clone()),
                ..Self::default()
            },
            #[cfg(feature = "netplay-internet")]
            crate::netplay::ConnectionOptions::WatchInternet(options) => Self {
                enabled: true,
                internet: true,
                spectator: true,
                code: options.invitation.encode().expect("validated invitation"),
                relay_only: options.relay_only,
                relay: single_relay(&options.invitation.endpoint),
                ..Self::default()
            },
        }
    }
}

impl NetplaySetup {
    pub fn role(&self) -> Role {
        if self.spectator {
            Role::Spectator
        } else if self.player == 0 {
            Role::Host
        } else {
            Role::Guest
        }
    }

    pub fn connection_options(&self) -> Result<Option<crate::netplay::ConnectionOptions>> {
        if !self.enabled {
            return Ok(None);
        }
        if self.internet {
            #[cfg(feature = "netplay-internet")]
            {
                use crate::netplay::{internet, ConnectionOptions};
                let options = match self.role() {
                    Role::Host => {
                        let mut host = self
                            .internet_host
                            .clone()
                            .context("Create a new invitation first")?;
                        anyhow::ensure!(
                            host.invitation.encode()? == self.code
                                && host.invitation.delay == self.delay
                                && host.invitation.window == self.rollback
                                && host.spectators == self.spectators,
                            "Create a new invitation after changing host settings"
                        );
                        host.relay_only = self.relay_only;
                        ConnectionOptions::Internet(Box::new(host))
                    }
                    Role::Guest => ConnectionOptions::Internet(Box::new(internet::Options::join(
                        &self.code,
                        self.relay_only,
                    )?)),
                    Role::Spectator => ConnectionOptions::WatchInternet(Box::new(
                        internet::SpectatorOptions::watch(&self.code, self.relay_only)?,
                    )),
                };
                options.validate()?;
                return Ok(Some(options));
            }
            #[cfg(not(feature = "netplay-internet"))]
            anyhow::bail!("This build does not include Internet netplay");
        }
        if self.spectator {
            Ok(self.watch_options()?.map(Into::into))
        } else {
            Ok(self.options()?.map(Into::into))
        }
    }

    pub fn generate_code(&mut self) -> Result<()> {
        if self.internet {
            #[cfg(feature = "netplay-internet")]
            {
                anyhow::ensure!(
                    self.role() == Role::Host,
                    "Only the host creates an invitation"
                );
                let host = crate::netplay::internet::Options::host(
                    self.delay,
                    self.rollback,
                    &self.relay,
                    self.relay_only,
                    self.spectators,
                )?;
                self.code = host.invitation.encode()?;
                self.spectator_code = host
                    .spectator_invitation()
                    .map(|invitation| invitation.encode())
                    .transpose()?
                    .unwrap_or_default();
                self.internet_host = Some(host);
                return Ok(());
            }
            #[cfg(not(feature = "netplay-internet"))]
            anyhow::bail!("This build does not include Internet netplay");
        }
        self.new_code();
        Ok(())
    }

    pub fn field_enabled(&self, field: F) -> bool {
        if field == F::NetplayEnabled {
            return true;
        }
        if !self.enabled {
            return false;
        }
        let host = self.role() == Role::Host;
        match field {
            F::NetplayMode => cfg!(feature = "netplay-internet"),
            F::NetplayBind | F::NetplayPeer => !self.internet,
            F::NetplayRelay => self.internet && host,
            F::NetplayRelayOnly => self.internet,
            F::NetplayNewCode | F::NetplayDelay | F::NetplayRollback => {
                !self.spectator && (!self.internet || host)
            }
            F::NetplaySpectators => host,
            F::NetplayCopySpectatorCode => self.internet && host && self.spectators > 0,
            _ => true,
        }
    }
    fn adopt_invitation(&mut self) {
        #[cfg(feature = "netplay-internet")]
        if self.internet && self.role() == Role::Guest {
            if let Ok(invitation) = crate::netplay::internet::Invitation::decode(&self.code) {
                self.delay = invitation.delay;
                self.rollback = invitation.window;
            }
        }
    }

    /// Direct player options; none for a spectator.
    pub fn options(&self) -> Result<Option<Options>> {
        use anyhow::Context;
        if !self.enabled || self.spectator {
            return Ok(None);
        }
        let options = Options {
            bind: self.bind.parse().context("Local address needs IP:port")?,
            peer: self.peer.parse().context("Peer address needs IP:port")?,
            player: self.player,
            session: crate::netplay::parse_session_id(&self.code)?,
            input_delay: self.delay,
            rollback_frames: self.rollback,
            spectators: if self.player == 0 { self.spectators } else { 0 },
        };
        options.validate()?;
        Ok(Some(options))
    }

    /// Direct spectator options: the peer address is the host's.
    pub fn watch_options(&self) -> Result<Option<WatchOptions>> {
        use anyhow::Context;
        if !self.enabled || !self.spectator {
            return Ok(None);
        }
        let options = WatchOptions {
            bind: self.bind.parse().context("Local address needs IP:port")?,
            host: self.peer.parse().context("Host address needs IP:port")?,
            session: crate::netplay::parse_session_id(&self.code)?,
        };
        options.validate()?;
        Ok(Some(options))
    }

    pub fn new_code(&mut self) {
        // RandomState obtains fresh host seeds. This is a collision-resistant
        // session label, not a secret or a peer-authentication credential.
        use std::hash::{BuildHasher, Hasher};
        let words: [u64; 2] = std::array::from_fn(|_| {
            let mut hash = std::collections::hash_map::RandomState::new().build_hasher();
            hash.write(b"Copperline netplay session");
            hash.finish()
        });
        self.code = format!("{:016x}{:016x}", words[0], words[1]);
    }

    pub fn value(&self, field: F) -> String {
        match field {
            F::NetplayMode => if self.internet {
                "Internet"
            } else {
                "Direct IP (UDP)"
            }
            .into(),
            F::NetplayRelay => self.relay.clone(),
            F::NetplayRelayOnly => if self.relay_only {
                "Relay only"
            } else {
                "Automatic"
            }
            .into(),
            F::NetplayBind => self.bind.clone(),
            F::NetplayPeer => self.peer.clone(),
            F::NetplayCode => self.code.clone(),
            F::NetplayPlayer => match (self.role(), self.internet) {
                (Role::Spectator, true) => "Watch".into(),
                (Role::Spectator, false) => "Spectator".into(),
                (Role::Host, true) => "Host (port 1)".into(),
                (Role::Guest, true) => "Join (port 2)".into(),
                (_, false) => format!("{} (port {})", self.player + 1, self.player + 1),
            },
            F::NetplaySpectators => match self.spectators {
                0 => "Off".into(),
                n => format!("Up to {n}"),
            },
            F::NetplayDelay => format!("{} frames", self.delay),
            F::NetplayRollback => format!("{} frames", self.rollback),
            F::NetplayNewCode => if self.internet {
                "New invitation"
            } else {
                "New code"
            }
            .into(),
            F::NetplayCopyCode => "Copy code".into(),
            F::NetplayCopySpectatorCode => "Copy spectator code".into(),
            _ => String::new(),
        }
    }

    pub fn cycle(&mut self, field: F, forward: bool) {
        if !self.field_enabled(field) {
            return;
        }
        match field {
            F::NetplayMode => {
                self.internet = !self.internet;
                self.code.clear();
                self.spectator_code.clear();
                #[cfg(feature = "netplay-internet")]
                {
                    self.internet_host = None;
                }
            }
            F::NetplayRelayOnly => self.relay_only = !self.relay_only,
            F::NetplayPlayer => {
                // Host, join, watch, and round again.
                let index = match self.role() {
                    Role::Host => 0,
                    Role::Guest => 1,
                    Role::Spectator => 2,
                };
                let next = (index + if forward { 1 } else { 2 }) % 3;
                self.spectator = next == 2;
                self.player = usize::from(next == 1);
                self.adopt_invitation();
            }
            F::NetplaySpectators => {
                self.spectators =
                    cycle_slice(&[0, 1, 2, 3, 4, 5, 6, 7, 8], self.spectators, forward)
            }
            F::NetplayDelay => {
                self.delay = cycle_slice(&[0, 1, 2, 3, 4, 5, 6], self.delay, forward)
            }
            F::NetplayRollback => {
                self.rollback = cycle_slice(
                    &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
                    self.rollback,
                    forward,
                )
            }
            _ => {}
        }
    }
}

impl LauncherField {
    pub fn is_netplay(self) -> bool {
        matches!(
            self,
            F::NetplayEnabled
                | F::NetplayMode
                | F::NetplayRelay
                | F::NetplayRelayOnly
                | F::NetplayBind
                | F::NetplayPeer
                | F::NetplayPlayer
                | F::NetplaySpectators
                | F::NetplayCode
                | F::NetplayDelay
                | F::NetplayRollback
                | F::NetplayNewCode
                | F::NetplayCopyCode
                | F::NetplayCopySpectatorCode
        )
    }
}

impl LauncherState {
    pub fn rows(&self) -> std::borrow::Cow<'static, [Row]> {
        if self.tab == LauncherTab::Netplay && self.netplay.internet {
            std::borrow::Cow::Borrowed(&fields::INTERNET_NETPLAY_ROWS)
        } else {
            rows(
                self.tab,
                self.setup.parallel_device(),
                self.setup.serial_mode(),
                self.setup.midi_out_is_mt32(),
                self.setup.midi_out_is_csynth(),
            )
        }
    }

    pub fn toggle_netplay(&mut self) {
        self.netplay.enabled = !self.netplay.enabled;
        self.prepare_netplay_machine();
    }

    pub fn prepare_netplay_machine(&mut self) {
        if self.netplay.enabled {
            // These session requirements are visible on the other pages too.
            self.setup.serial_mode = SerialMode::Off;
            self.setup.jit = false;
            self.setup.power_on = true;
            self.setup.run_ahead_frames = 0;
            self.setup.warp_boot = false;
            self.setup.warp_until = None;
            for port in &mut self.setup.port_devices {
                if !matches!(
                    port,
                    PortDevice::Mouse | PortDevice::Joystick | PortDevice::Cd32Pad
                ) {
                    *port = PortDevice::Joystick;
                }
            }
        }
    }

    pub fn begin_edit_netplay(&mut self, field: F) {
        if !self.netplay.field_enabled(field)
            || !matches!(
                field,
                F::NetplayBind | F::NetplayPeer | F::NetplayCode | F::NetplayRelay
            )
        {
            return;
        }
        self.edit_buffer = self.netplay.value(field);
        self.editing = Some(EditTarget::Netplay(field));
        self.edit_caret = Caret::end_of(&self.edit_buffer);
        self.status = None;
    }

    pub fn clear_netplay_edit(&mut self) {
        if matches!(self.editing, Some(EditTarget::Netplay(_))) {
            self.edit_buffer.clear();
            self.edit_caret = Caret::default();
        }
    }

    pub(super) fn commit_netplay_edit(&mut self, field: F) {
        let value = self.edit_buffer.trim().to_string();
        match field {
            F::NetplayBind => self.netplay.bind = value,
            F::NetplayPeer => self.netplay.peer = value,
            F::NetplayCode => {
                self.netplay.code = value;
                self.netplay.adopt_invitation();
            }
            F::NetplayRelay => {
                if self.netplay.relay != value {
                    #[cfg(feature = "netplay-internet")]
                    {
                        self.netplay.internet_host = None;
                    }
                }
                self.netplay.relay = value;
            }
            _ => {}
        }
        self.editing = None;
        self.edit_buffer.clear();
    }
}
