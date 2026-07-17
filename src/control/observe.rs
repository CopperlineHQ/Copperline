// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-connection CCP event subscriptions. Sampling happens only at the
//! deterministic command/driver boundaries used by the two control-server
//! modes. Paula's serial tap is separately bounded at the point of capture;
//! windowed delivery adds another bounded queue at the socket boundary.

use super::{exec, proto};
use crate::emulator::Emulator;
use crate::serial::SERIAL_OBSERVATION_CAPACITY;
use serde_json::{json, Value};

pub const MAX_FRAME_INTERVAL: u64 = 1_000_000;
pub const OUTBOUND_NOTIFICATION_CAPACITY: usize = 256;

const FRAME: u8 = 1 << 0;
const SERIAL: u8 = 1 << 1;
const INTERRUPT: u8 = 1 << 2;
const MEDIA: u8 = 1 << 3;

/// Event families exposed by `events.subscribe`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Frame,
    Serial,
    Interrupt,
    Media,
}

impl EventKind {
    pub const ALL: [Self; 4] = [Self::Frame, Self::Serial, Self::Interrupt, Self::Media];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "frame" => Some(Self::Frame),
            "serial" => Some(Self::Serial),
            "interrupt" => Some(Self::Interrupt),
            "media" => Some(Self::Media),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Frame => "frame",
            Self::Serial => "serial",
            Self::Interrupt => "interrupt",
            Self::Media => "media",
        }
    }

    fn bit(self) -> u8 {
        match self {
            Self::Frame => FRAME,
            Self::Serial => SERIAL,
            Self::Interrupt => INTERRUPT,
            Self::Media => MEDIA,
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
            }
        }
        self.list_value()
    }

    pub fn unsubscribe(&mut self, emu: &mut Emulator, events: Option<&[EventKind]>) -> Value {
        let remove = events
            .map(|events| events.iter().fold(0, |bits, event| bits | event.bit()))
            .unwrap_or(FRAME | SERIAL | INTERRUPT | MEDIA);
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

        if self.active & FRAME != 0 {
            let current = emu.bus().emulated_frames();
            let previous = self.last_frame.unwrap_or(current);
            if current != previous && current.abs_diff(previous) >= self.frame_interval {
                let mut params = json!({
                    "position": position(emu),
                    "previous_frame": previous,
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

fn position(emu: &Emulator) -> Value {
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
}
