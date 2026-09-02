// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-connection CCP event subscriptions. Sampling happens only at the
//! deterministic command/driver boundaries used by the two control-server
//! modes. Paula's serial tap is separately bounded at the point of capture;
//! windowed delivery adds another bounded queue at the socket boundary.

use super::{exec, proto};
use crate::emulator::Emulator;
use crate::serial::SERIAL_OBSERVATION_CAPACITY;
use crate::uaelib::DebugEvent;
use serde_json::{json, Value};

pub const MAX_FRAME_INTERVAL: u64 = 1_000_000;
pub const OUTBOUND_NOTIFICATION_CAPACITY: usize = 256;

const FRAME: u8 = 1 << 0;
const SERIAL: u8 = 1 << 1;
const INTERRUPT: u8 = 1 << 2;
const MEDIA: u8 = 1 << 3;
const DEBUG: u8 = 1 << 4;

/// Event families exposed by `events.subscribe`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Frame,
    Serial,
    Interrupt,
    Media,
    /// Guest debug output through the uaelib trap (`crate::uaelib`).
    Debug,
}

impl EventKind {
    pub const ALL: [Self; 5] = [
        Self::Frame,
        Self::Serial,
        Self::Interrupt,
        Self::Media,
        Self::Debug,
    ];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "frame" => Some(Self::Frame),
            "serial" => Some(Self::Serial),
            "interrupt" => Some(Self::Interrupt),
            "media" => Some(Self::Media),
            "debug" => Some(Self::Debug),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Frame => "frame",
            Self::Serial => "serial",
            Self::Interrupt => "interrupt",
            Self::Media => "media",
            Self::Debug => "debug",
        }
    }

    fn bit(self) -> u8 {
        match self {
            Self::Frame => FRAME,
            Self::Serial => SERIAL,
            Self::Interrupt => INTERRUPT,
            Self::Media => MEDIA,
            Self::Debug => DEBUG,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InterruptState {
    intena: u16,
    intreq: u16,
    cpu_visible: u16,
    enabled_pending: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MediaState {
    floppy: [Option<String>; 4],
    cd_inserted: bool,
}

/// Subscription and sampling state for one authenticated connection.
pub struct Observer {
    active: u8,
    frame_interval: u64,
    frame_digest: bool,
    last_frame: Option<u64>,
    last_interrupt: Option<InterruptState>,
    last_media: Option<MediaState>,
    dropped_notifications: u64,
}

impl Observer {
    pub fn new() -> Self {
        Self {
            active: 0,
            frame_interval: 1,
            frame_digest: false,
            last_frame: None,
            last_interrupt: None,
            last_media: None,
            dropped_notifications: 0,
        }
    }

    pub fn subscribe(
        &mut self,
        emu: &mut Emulator,
        events: &[EventKind],
        frame_interval: Option<u64>,
        frame_digest: Option<bool>,
    ) -> Value {
        if let Some(interval) = frame_interval {
            self.frame_interval = interval;
        }
        if let Some(digest) = frame_digest {
            self.frame_digest = digest;
        }

        for event in events {
            let newly_active = self.active & event.bit() == 0;
            self.active |= event.bit();
            if !newly_active {
                continue;
            }
            match event {
                EventKind::Frame => self.last_frame = Some(emu.bus().emulated_frames()),
                EventKind::Serial => emu.bus_mut().paula.set_serial_observation_enabled(true),
                EventKind::Interrupt => self.last_interrupt = Some(interrupt_state(emu)),
                EventKind::Media => self.last_media = Some(media_state(emu)),
                // The queue is always on; a fresh subscription starts clean.
                EventKind::Debug => emu.clear_uaelib_debug_events(),
            }
        }
        self.list_value()
    }

    pub fn unsubscribe(&mut self, emu: &mut Emulator, events: Option<&[EventKind]>) -> Value {
        let remove = events
            .map(|events| events.iter().fold(0, |bits, event| bits | event.bit()))
            .unwrap_or(FRAME | SERIAL | INTERRUPT | MEDIA | DEBUG);
        let serial_was_active = self.active & SERIAL != 0;
        self.active &= !remove;

        if remove & FRAME != 0 {
            self.last_frame = None;
            self.frame_digest = false;
        }
        if remove & INTERRUPT != 0 {
            self.last_interrupt = None;
        }
        if remove & MEDIA != 0 {
            self.last_media = None;
        }
        if serial_was_active && self.active & SERIAL == 0 {
            emu.bus_mut().paula.set_serial_observation_enabled(false);
        }
        self.list_value()
    }

    pub fn disable(&mut self, emu: &mut Emulator) {
        self.unsubscribe(emu, None);
    }

    pub fn list_value(&self) -> Value {
        let supported: Vec<&str> = EventKind::ALL.iter().map(|event| event.name()).collect();
        let active: Vec<&str> = EventKind::ALL
            .iter()
            .filter(|event| self.active & event.bit() != 0)
            .map(|event| event.name())
            .collect();
        json!({
            "supported": supported,
            "active": active,
            "frame_interval": self.frame_interval,
            "frame_digest": self.frame_digest,
            "dropped_notifications": self.dropped_notifications,
            "limits": {
                "serial_records": SERIAL_OBSERVATION_CAPACITY,
                "debug_events": crate::uaelib::DEBUG_EVENT_CAPACITY,
                "debug_resources": crate::uaelib::RESOURCE_MAX,
                "windowed_outbound_notifications": OUTBOUND_NOTIFICATION_CAPACITY,
            },
        })
    }

    pub fn note_notification_dropped(&mut self) {
        self.dropped_notifications = self.dropped_notifications.saturating_add(1);
    }

    /// Sample all active families and return complete JSON-RPC notification
    /// lines. The caller owns transport-specific bounded delivery.
    pub fn poll(&mut self, emu: &mut Emulator) -> Vec<String> {
        let mut events = Vec::new();

        if self.active & SERIAL != 0 {
            let (records, dropped) = emu.bus_mut().paula.take_serial_observations();
            if !records.is_empty() || dropped != 0 {
                let words: Vec<Value> = records
                    .into_iter()
                    .map(|record| {
                        json!({
                            "word": record.word,
                            "long": record.long,
                            "at_cck": record.at_cck,
                        })
                    })
                    .collect();
                events.push(proto::event_line(
                    "event.serial",
                    json!({
                        "position": position(emu),
                        "words": words,
                        "dropped_words": dropped,
                        "dropped_notifications": self.dropped_notifications,
                    }),
                ));
            }
        }

        if self.active & INTERRUPT != 0 {
            let current = interrupt_state(emu);
            if self
                .last_interrupt
                .is_some_and(|previous| previous != current)
            {
                let previous = self.last_interrupt.expect("checked above");
                events.push(proto::event_line(
                    "event.interrupt",
                    json!({
                        "position": position(emu),
                        "previous": interrupt_value(previous),
                        "current": interrupt_value(current),
                        "asserted": current.intreq & !previous.intreq,
                        "cleared": previous.intreq & !current.intreq,
                        "dropped_notifications": self.dropped_notifications,
                    }),
                ));
            }
            self.last_interrupt = Some(current);
        }

        if self.active & MEDIA != 0 {
            let current = media_state(emu);
            if let Some(previous) = self.last_media.as_ref() {
                for drive in 0..4 {
                    if previous.floppy[drive] != current.floppy[drive] {
                        let name = current.floppy[drive].clone();
                        events.push(proto::event_line(
                            "event.media",
                            json!({
                                "position": position(emu),
                                "kind": "floppy",
                                "drive": drive,
                                "action": if name.is_some() { "inserted" } else { "ejected" },
                                "name": name,
                                "dropped_notifications": self.dropped_notifications,
                            }),
                        ));
                    }
                }
                if previous.cd_inserted != current.cd_inserted {
                    events.push(proto::event_line(
                        "event.media",
                        json!({
                            "position": position(emu),
                            "kind": "cd",
                            "action": if current.cd_inserted { "inserted" } else { "ejected" },
                            "dropped_notifications": self.dropped_notifications,
                        }),
                    ));
                }
            }
            self.last_media = Some(current);
        }

        if self.active & DEBUG != 0 {
            let (drained, dropped) = emu.take_uaelib_debug_events();
            if !drained.is_empty() {
                let (frame, seconds) = {
                    let bus = emu.bus();
                    (bus.emulated_frames(), bus.emulated_seconds())
                };
                for event in drained {
                    let mut params = match event {
                        DebugEvent::Log(text) => json!({ "kind": "log", "text": text }),
                        DebugEvent::Resource { action, resource } => json!({
                            "kind": "resource",
                            "action": action.name(),
                            "resource": exec::resource_value(&resource),
                        }),
                    };
                    params["seconds"] = json!(seconds);
                    params["frame"] = json!(frame);
                    params["dropped_events"] = json!(dropped);
                    params["dropped_notifications"] = json!(self.dropped_notifications);
                    events.push(proto::event_line("event.debug", params));
                }
            }
        }

        if self.active & FRAME != 0 {
            let current = emu.bus().emulated_frames();
            let previous = self.last_frame.unwrap_or(current);
            if current != previous && current.abs_diff(previous) >= self.frame_interval {
                let mut params = json!({
                    "position": position(emu),
                    "previous_frame": previous,
                    "guest_idle_cck": emu.uaelib_idle().and_then(|idle| idle.last_frame_idle_cck()),
                    "dropped_notifications": self.dropped_notifications,
                });
                if self.frame_digest {
                    params["digest"] = exec::digest_value(emu);
                }
                events.push(proto::event_line("event.frame", params));
                self.last_frame = Some(current);
            } else if self.last_frame.is_none() {
                self.last_frame = Some(current);
            }
        }

        events
    }
}

impl Default for Observer {
    fn default() -> Self {
        Self::new()
    }
}

fn interrupt_state(emu: &Emulator) -> InterruptState {
    let bus = emu.bus();
    let intena = bus.paula.intena;
    let intreq = bus.paula.intreq;
    let cpu_visible = bus.cpu_visible_intreq();
    InterruptState {
        intena,
        intreq,
        cpu_visible,
        enabled_pending: intena & cpu_visible,
    }
}

fn interrupt_value(state: InterruptState) -> Value {
    json!({
        "intena": state.intena,
        "intreq": state.intreq,
        "cpu_visible": state.cpu_visible,
        "enabled_pending": state.enabled_pending,
    })
}

fn media_state(emu: &Emulator) -> MediaState {
    let bus = emu.bus();
    MediaState {
        floppy: std::array::from_fn(|drive| bus.floppy.inserted_disk_name(drive)),
        cd_inserted: bus.cd_disc_inserted(),
    }
}

pub(crate) fn position(emu: &Emulator) -> Value {
    let bus = emu.bus();
    json!({
        "frame": bus.emulated_frames(),
        "cck": bus.emulated_cck(),
        "seconds": bus.emulated_seconds(),
        "vpos": bus.agnus.vpos,
        "hpos": bus.agnus.hpos,
        "pc": emu.machine.pc(),
        "retired_instructions": emu.retired_instructions(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::test_emulator;

    #[test]
    fn frame_subscription_is_baselined_and_interval_limited() {
        let mut emu = test_emulator();
        let mut observer = Observer::new();
        observer.subscribe(&mut emu, &[EventKind::Frame], Some(2), Some(false));

        assert!(observer.poll(&mut emu).is_empty());

        // Exercise the sampler directly at deterministic frame values; frame
        // production itself is covered by the emulator scheduler tests.
        let start = emu.bus().emulated_frames();
        observer.last_frame = Some(start + 2);
        let events = observer.poll(&mut emu);
        assert_eq!(events.len(), 1);
        let event: Value = serde_json::from_str(&events[0]).unwrap();
        assert_eq!(event["method"], "event.frame");
        assert_eq!(event["params"]["position"]["frame"], start);
    }

    #[test]
    fn interrupt_subscription_reports_state_changes() {
        let mut emu = test_emulator();
        let mut observer = Observer::new();
        observer.subscribe(&mut emu, &[EventKind::Interrupt], None, None);
        emu.bus_mut().paula.intreq ^= 1 << 5;

        let events = observer.poll(&mut emu);
        assert_eq!(events.len(), 1);
        let event: Value = serde_json::from_str(&events[0]).unwrap();
        assert_eq!(event["method"], "event.interrupt");
        assert_eq!(event["params"]["asserted"], 1 << 5);
    }

    #[test]
    fn unsubscribe_disables_all_families() {
        let mut emu = test_emulator();
        let mut observer = Observer::new();
        observer.subscribe(&mut emu, &EventKind::ALL, Some(3), Some(true));
        let state = observer.unsubscribe(&mut emu, None);
        assert_eq!(state["active"], json!([]));
        assert_eq!(state["frame_digest"], false);
    }

    fn uaelib_emulator() -> Emulator {
        let mut emu = test_emulator();
        let mut lib = crate::uaelib::UaeLib::new();
        lib.mute_stdout();
        emu.bus_mut().attach_uaelib(lib);
        emu
    }

    /// Register a copper list called "cop" the way the guest does.
    fn register_copperlist(emu: &mut Emulator) {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x0004_0000u32.to_be_bytes());
        bytes.extend_from_slice(&1000u32.to_be_bytes());
        let mut name = [0u8; 32];
        name[..3].copy_from_slice(b"cop");
        bytes.extend_from_slice(&name);
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&[0; 6]);
        let bus = emu.bus_mut();
        bus.mem.chip_ram[0x5000..0x5000 + bytes.len()].copy_from_slice(&bytes);
        let mem = &mut bus.mem;
        let lib = bus.uaelib.as_mut().unwrap();
        lib.call(
            crate::uaelib::FN_DEBUG_CMD,
            [crate::uaelib::CMD_REGISTER_RESOURCE, 0x5000, 0, 0, 0],
            mem,
            0x00FF_FFFF,
            0,
            0,
        );
    }

    #[test]
    fn debug_family_is_named_listed_and_cleared() {
        assert_eq!(EventKind::from_name("debug"), Some(EventKind::Debug));
        assert_eq!(EventKind::Debug.name(), "debug");
        assert_eq!(EventKind::ALL.len(), 5);
        let mut emu = uaelib_emulator();
        let mut observer = Observer::new();
        let state = observer.subscribe(&mut emu, &[EventKind::Debug], None, None);
        assert_eq!(state["active"], json!(["debug"]));
        assert_eq!(
            state["limits"]["debug_events"],
            crate::uaelib::DEBUG_EVENT_CAPACITY
        );
        assert_eq!(
            state["limits"]["debug_resources"],
            crate::uaelib::RESOURCE_MAX
        );
        let state = observer.unsubscribe(&mut emu, None);
        assert_eq!(state["active"], json!([]));
    }

    #[test]
    fn debug_subscription_streams_one_notification_per_event() {
        let mut emu = uaelib_emulator();
        let mut observer = Observer::new();
        // Queued before the subscription: a fresh subscription starts clean.
        emu.bus_mut()
            .uaelib
            .as_mut()
            .unwrap()
            .queue_debug_line("stale");
        observer.subscribe(&mut emu, &[EventKind::Debug], None, None);
        assert!(observer.poll(&mut emu).is_empty());

        {
            let lib = emu.bus_mut().uaelib.as_mut().unwrap();
            lib.queue_debug_line("first");
            lib.queue_debug_line("second");
        }
        register_copperlist(&mut emu);
        let events = observer.poll(&mut emu);
        assert_eq!(events.len(), 3);
        let first: Value = serde_json::from_str(&events[0]).unwrap();
        assert_eq!(first["method"], "event.debug");
        assert_eq!(first["params"]["kind"], "log");
        assert_eq!(first["params"]["text"], "first");
        assert_eq!(first["params"]["frame"], emu.bus().emulated_frames());
        assert_eq!(first["params"]["dropped_events"], 0);
        let second: Value = serde_json::from_str(&events[1]).unwrap();
        assert_eq!(second["params"]["text"], "second");
        let third: Value = serde_json::from_str(&events[2]).unwrap();
        assert_eq!(third["params"]["kind"], "resource");
        assert_eq!(third["params"]["action"], "registered");
        assert_eq!(third["params"]["resource"]["name"], "cop");
        assert_eq!(third["params"]["resource"]["type"], "copperlist");
        assert_eq!(third["params"]["resource"]["size"], 1000);
        assert!(observer.poll(&mut emu).is_empty());

        // An unsubscribed observer leaves the queue to whoever asks next.
        observer.unsubscribe(&mut emu, Some(&[EventKind::Debug]));
        emu.bus_mut()
            .uaelib
            .as_mut()
            .unwrap()
            .queue_debug_line("later");
        assert!(observer.poll(&mut emu).is_empty());
        assert_eq!(emu.take_uaelib_debug_events().0.len(), 1);
    }

    #[test]
    fn frame_events_carry_guest_idle_cck_once_used() {
        let mut emu = uaelib_emulator();
        let mut observer = Observer::new();
        observer.subscribe(&mut emu, &[EventKind::Frame], Some(1), Some(false));
        let start = emu.bus().emulated_frames();
        observer.last_frame = Some(start + 1);
        let events = observer.poll(&mut emu);
        assert_eq!(events.len(), 1);
        let event: Value = serde_json::from_str(&events[0]).unwrap();
        assert!(event["params"]["guest_idle_cck"].is_null());

        {
            let bus = emu.bus_mut();
            let mem = &mut bus.mem;
            let lib = bus.uaelib.as_mut().unwrap();
            let cmd = crate::uaelib::FN_DEBUG_CMD;
            let set_idle = crate::uaelib::CMD_SET_IDLE;
            lib.call(cmd, [set_idle, 1, 0, 0, 0], mem, 0x00FF_FFFF, 100, 0);
            lib.call(cmd, [set_idle, 0, 0, 0, 0], mem, 0x00FF_FFFF, 400, 0);
            lib.note_frame_start(1000);
        }
        observer.last_frame = Some(start + 1);
        let events = observer.poll(&mut emu);
        let event: Value = serde_json::from_str(&events[0]).unwrap();
        assert_eq!(event["params"]["guest_idle_cck"], 300);
    }
}
