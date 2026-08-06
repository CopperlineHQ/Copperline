// SPDX-License-Identifier: GPL-3.0-or-later

//! Linux ALSA sequencer backend, linked straight against `libasound` with no
//! wrapper crate. Output rides a real-time queue: each message is scheduled at a
//! host instant so the ALSA server dispatches it then, no matter when the
//! emulation thread called `snd_seq_event_output`. That is what keeps the guest's
//! byte timing intact -- the direct analogue of a CoreMIDI packet timestamp.
//! Input runs on a dedicated thread that decodes incoming events to raw MIDI
//! bytes and pushes them to the lock-free ring Paula's receiver drains.
//!
//! Two sequencer clients are used, each touched by a single thread: `out_seq`
//! (output + the queue + subscription changes) belongs to the emulation thread;
//! the input handle belongs to its own thread. libasound is not safe to drive
//! from two threads at once, so nothing crosses that line -- an input retarget is
//! posted to the input thread rather than applied on the caller's.
//!
//! The `snd_seq_ev_*` scheduling helpers are `static inline` in the ALSA headers,
//! so they are not in the shared library: their field writes are replicated by
//! hand against a Rust mirror of `snd_seq_event_t` (see [`SeqEvent`]).

use std::ffi::{c_char, c_int, c_long, c_short, c_uint, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use ringbuf::traits::Producer;

use super::{InputProducer, MidiBackend, MidiEndpoint, MidiEndpoints};

// --- ALSA sequencer FFI -------------------------------------------------

// Opaque library handles; only ever held behind pointers.
enum SndSeq {}
enum SndSeqClientInfo {}
enum SndSeqPortInfo {}
enum SndMidiEvent {}

// `snd_seq_addr_t`: a (client, port) pair. The library stores each as a byte.
#[repr(C)]
#[derive(Clone, Copy)]
struct SeqAddr {
    client: u8,
    port: u8,
}

// `snd_seq_real_time_t`. Also stands in for the 8-byte `snd_seq_timestamp_t`
// union in the event below: both variants are two 32-bit words, and we only ever
// schedule in real time, so there is no need to model the union.
#[repr(C)]
#[derive(Clone, Copy)]
struct SeqRealTime {
    tv_sec: c_uint,
    tv_nsec: c_uint,
}

// Mirror of `snd_seq_event_t` (28 bytes, 4-aligned). The encoder fills `type_`,
// `flags`, and the 12-byte `data` union; we set the schedule fields by hand. The
// `data` union is opaque here -- we never read it -- so a byte array of the right
// size suffices, and keeping the `time` field 4-aligned keeps the whole struct's
// layout identical to the C one.
#[repr(C)]
struct SeqEvent {
    type_: u8,
    flags: u8,
    tag: u8,
    queue: u8,
    time: SeqRealTime,
    source: SeqAddr,
    dest: SeqAddr,
    data: [u8; 12],
}

impl SeqEvent {
    /// An all-zero event is a valid `SND_SEQ_EVENT_NONE` with no schedule flags.
    fn blank() -> Self {
        // SAFETY: the struct is plain old data; all-zero is a valid instance.
        unsafe { std::mem::zeroed() }
    }
}

// Lock the mirror to the C layout the headers define (verified by offsetof). A
// mismatch would mis-place the schedule fields the encoder does not set, the way
// natural alignment mis-located CoreMIDI's received packets.
const _: () = {
    assert!(std::mem::size_of::<SeqAddr>() == 2);
    assert!(std::mem::size_of::<SeqRealTime>() == 8);
    assert!(std::mem::size_of::<SeqEvent>() == 28);
    assert!(std::mem::offset_of!(SeqEvent, time) == 4);
    assert!(std::mem::offset_of!(SeqEvent, source) == 12);
    assert!(std::mem::offset_of!(SeqEvent, dest) == 14);
    assert!(std::mem::offset_of!(SeqEvent, data) == 16);
};

const SND_SEQ_OPEN_DUPLEX: c_int = 3;

const SND_SEQ_PORT_CAP_READ: c_uint = 1 << 0;
const SND_SEQ_PORT_CAP_WRITE: c_uint = 1 << 1;
const SND_SEQ_PORT_CAP_SUBS_READ: c_uint = 1 << 5;
const SND_SEQ_PORT_CAP_SUBS_WRITE: c_uint = 1 << 6;

const SND_SEQ_PORT_TYPE_MIDI_GENERIC: c_uint = 1 << 1;
const SND_SEQ_PORT_TYPE_APPLICATION: c_uint = 1 << 20;

// The sequencer's own system client (Timer/Announce ports). Always present and
// never a MIDI device, so it is left out of the device list, as MIDI apps do.
const SND_SEQ_CLIENT_SYSTEM: c_int = 0;

const SND_SEQ_ADDRESS_UNKNOWN: u8 = 253;
const SND_SEQ_ADDRESS_SUBSCRIBERS: u8 = 254;
const SND_SEQ_QUEUE_DIRECT: u8 = 253;

// Event mode flag bits (see snd_seq_ev_schedule_real / _set_direct).
const SND_SEQ_TIME_STAMP_REAL: u8 = 1 << 0;
const SND_SEQ_TIME_STAMP_MASK: u8 = 1 << 0;
const SND_SEQ_TIME_MODE_REL: u8 = 1 << 1;
const SND_SEQ_TIME_MODE_MASK: u8 = 1 << 1;

const SND_SEQ_EVENT_START: c_int = 30;
const SND_SEQ_EVENT_NONE: u8 = 255;

// A destination we can send to is writable and takes write-subscriptions; a
// source we can receive from is readable and takes read-subscriptions.
const DEST_CAPS: c_uint = SND_SEQ_PORT_CAP_WRITE | SND_SEQ_PORT_CAP_SUBS_WRITE;
const SOURCE_CAPS: c_uint = SND_SEQ_PORT_CAP_READ | SND_SEQ_PORT_CAP_SUBS_READ;

// Encoder/decoder scratch, and the ceiling on a single SysEx message. The macOS
// backend's one-packet scratch caps SysEx at roughly the same size.
const MIDI_BUF: usize = 4096;

#[link(name = "asound")]
extern "C" {
    fn snd_seq_open(
        handle: *mut *mut SndSeq,
        name: *const c_char,
        streams: c_int,
        mode: c_int,
    ) -> c_int;
    fn snd_seq_close(handle: *mut SndSeq) -> c_int;
    fn snd_seq_set_client_name(seq: *mut SndSeq, name: *const c_char) -> c_int;
    fn snd_seq_client_id(seq: *mut SndSeq) -> c_int;
    fn snd_seq_nonblock(seq: *mut SndSeq, nonblock: c_int) -> c_int;
    fn snd_seq_create_simple_port(
        seq: *mut SndSeq,
        name: *const c_char,
        caps: c_uint,
        type_: c_uint,
    ) -> c_int;
    fn snd_seq_alloc_queue(seq: *mut SndSeq) -> c_int;
    fn snd_seq_control_queue(
        seq: *mut SndSeq,
        q: c_int,
        type_: c_int,
        value: c_int,
        ev: *mut SeqEvent,
    ) -> c_int;
    fn snd_seq_connect_to(seq: *mut SndSeq, my_port: c_int, client: c_int, port: c_int) -> c_int;
    fn snd_seq_disconnect_to(seq: *mut SndSeq, my_port: c_int, client: c_int, port: c_int)
        -> c_int;
    fn snd_seq_connect_from(seq: *mut SndSeq, my_port: c_int, client: c_int, port: c_int) -> c_int;
    fn snd_seq_disconnect_from(
        seq: *mut SndSeq,
        my_port: c_int,
        client: c_int,
        port: c_int,
    ) -> c_int;
    fn snd_seq_event_output(seq: *mut SndSeq, ev: *mut SeqEvent) -> c_int;
    fn snd_seq_drain_output(seq: *mut SndSeq) -> c_int;
    fn snd_seq_event_input(seq: *mut SndSeq, ev: *mut *mut SeqEvent) -> c_int;
    fn snd_seq_poll_descriptors_count(seq: *mut SndSeq, events: c_short) -> c_int;
    fn snd_seq_poll_descriptors(
        seq: *mut SndSeq,
        pfds: *mut libc::pollfd,
        space: c_uint,
        events: c_short,
    ) -> c_int;

    fn snd_seq_client_info_malloc(ptr: *mut *mut SndSeqClientInfo) -> c_int;
    fn snd_seq_client_info_free(ptr: *mut SndSeqClientInfo);
    fn snd_seq_client_info_set_client(info: *mut SndSeqClientInfo, client: c_int);
    fn snd_seq_client_info_get_client(info: *const SndSeqClientInfo) -> c_int;
    fn snd_seq_client_info_get_name(info: *mut SndSeqClientInfo) -> *const c_char;
    fn snd_seq_query_next_client(seq: *mut SndSeq, info: *mut SndSeqClientInfo) -> c_int;

    fn snd_seq_port_info_malloc(ptr: *mut *mut SndSeqPortInfo) -> c_int;
    fn snd_seq_port_info_free(ptr: *mut SndSeqPortInfo);
    fn snd_seq_port_info_set_client(info: *mut SndSeqPortInfo, client: c_int);
    fn snd_seq_port_info_set_port(info: *mut SndSeqPortInfo, port: c_int);
    fn snd_seq_port_info_get_port(info: *const SndSeqPortInfo) -> c_int;
    fn snd_seq_port_info_get_name(info: *const SndSeqPortInfo) -> *const c_char;
    fn snd_seq_port_info_get_capability(info: *const SndSeqPortInfo) -> c_uint;
    fn snd_seq_query_next_port(seq: *mut SndSeq, info: *mut SndSeqPortInfo) -> c_int;

    fn snd_midi_event_new(bufsize: usize, rdev: *mut *mut SndMidiEvent) -> c_int;
    fn snd_midi_event_free(dev: *mut SndMidiEvent);
    fn snd_midi_event_no_status(dev: *mut SndMidiEvent, on: c_int);
    fn snd_midi_event_reset_encode(dev: *mut SndMidiEvent);
    fn snd_midi_event_encode(
        dev: *mut SndMidiEvent,
        buf: *const u8,
        count: c_long,
        ev: *mut SeqEvent,
    ) -> c_long;
    fn snd_midi_event_decode(
        dev: *mut SndMidiEvent,
        buf: *mut u8,
        count: c_long,
        ev: *const SeqEvent,
    ) -> c_long;
}

// --- helpers ------------------------------------------------------------

/// A resolved host endpoint: where to connect, plus the display name to show.
#[derive(Clone)]
struct Endpoint {
    client: c_int,
    port: c_int,
    name: String,
}

/// Copy a C string into a Rust `String`.
fn cstr_to_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(p).to_str().ok().map(String::from) }
}

/// Open a sequencer client on the default device. Duplex because even the input
/// handle sends subscription commands, and the output handle drives the queue.
fn open_seq() -> Result<*mut SndSeq> {
    let name = CString::new("default").unwrap();
    let mut seq: *mut SndSeq = std::ptr::null_mut();
    let rc = unsafe { snd_seq_open(&mut seq, name.as_ptr(), SND_SEQ_OPEN_DUPLEX, 0) };
    if rc < 0 || seq.is_null() {
        bail!("snd_seq_open failed ({rc})");
    }
    Ok(seq)
}

/// Our own client ids, so [`query_ports`] hides Copperline's ports from the
/// device list (they would otherwise appear as a source and a destination).
/// Reset each time a backend opens; only one is live at a time.
fn own_clients() -> &'static Mutex<Vec<c_int>> {
    static OWN: OnceLock<Mutex<Vec<c_int>>> = OnceLock::new();
    OWN.get_or_init(|| Mutex::new(Vec::new()))
}

/// Every port on the system whose capabilities include `caps`, skipping our own
/// client. Opens a short-lived handle, so it is safe from any thread and always
/// reflects the current device set (freshly connected devices included).
fn query_ports(caps: c_uint) -> Vec<Endpoint> {
    let mut out = Vec::new();
    let Ok(seq) = open_seq() else {
        return out;
    };
    let own = own_clients().lock().unwrap().clone();
    unsafe {
        let mut cinfo: *mut SndSeqClientInfo = std::ptr::null_mut();
        let mut pinfo: *mut SndSeqPortInfo = std::ptr::null_mut();
        if snd_seq_client_info_malloc(&mut cinfo) >= 0 && snd_seq_port_info_malloc(&mut pinfo) >= 0
        {
            snd_seq_client_info_set_client(cinfo, -1);
            while snd_seq_query_next_client(seq, cinfo) >= 0 {
                let client = snd_seq_client_info_get_client(cinfo);
                if client == SND_SEQ_CLIENT_SYSTEM || own.contains(&client) {
                    continue;
                }
                let cname = cstr_to_string(snd_seq_client_info_get_name(cinfo)).unwrap_or_default();
                snd_seq_port_info_set_client(pinfo, client);
                snd_seq_port_info_set_port(pinfo, -1);
                while snd_seq_query_next_port(seq, pinfo) >= 0 {
                    if snd_seq_port_info_get_capability(pinfo) & caps != caps {
                        continue;
                    }
                    let port = snd_seq_port_info_get_port(pinfo);
                    let pname =
                        cstr_to_string(snd_seq_port_info_get_name(pinfo)).unwrap_or_default();
                    out.push(Endpoint {
                        client,
                        port,
                        name: format!("{cname}:{pname}"),
                    });
                }
            }
        }
        if !cinfo.is_null() {
            snd_seq_client_info_free(cinfo);
        }
        if !pinfo.is_null() {
            snd_seq_port_info_free(pinfo);
        }
        snd_seq_close(seq);
    }
    out
}

/// First endpoint whose display name contains `want` (case-insensitive), the
/// same match the CoreMIDI backend uses.
fn find_named(ports: Vec<Endpoint>, want: &str) -> Option<Endpoint> {
    let needle = want.to_lowercase();
    ports
        .into_iter()
        .find(|e| e.name.to_lowercase().contains(&needle))
}

/// Log the first input event so a silent MIDI-in path (nothing arriving) is
/// distinguishable from a consumption problem (arriving but never drained).
fn log_first_input(n: usize) {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        log::info!("midi: input decoded {n} byte(s) from host");
    }
}

/// Log a send failure once (errors typically repeat every message).
fn log_send_error(msg: &str) {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        log::warn!("midi: {msg}");
    }
}

/// Hand decoded input bytes to the guest, honouring the Active Sensing opt-out.
fn push_input(producer: &mut InputProducer, data: &[u8]) {
    if super::strip_active_sense() {
        for &b in data {
            if b != super::ACTIVE_SENSE {
                producer.push_slice(std::slice::from_ref(&b));
            }
        }
    } else {
        producer.push_slice(data);
    }
}

// --- input thread -------------------------------------------------------

/// Retarget request handed from the emulation thread to the input thread, which
/// owns the input handle. `None` outer = nothing pending; `Some(None)` = detach;
/// `Some(Some(ep))` = attach to `ep`.
struct InputControl {
    request: Mutex<Option<Option<Endpoint>>>,
    current: Mutex<Option<Endpoint>>,
    stop: AtomicBool,
}

/// Move a raw handle across the thread boundary. Only the input thread touches
/// it once handed over, so single-owner access still holds.
struct SeqPtr(*mut SndSeq);
unsafe impl Send for SeqPtr {}

/// Apply a pending retarget on the input thread. Disconnects the old source and
/// connects the new one, keeping `current` in step for `current_input`.
fn apply_input_request(seq: *mut SndSeq, in_port: c_int, control: &InputControl) {
    let Some(target) = control.request.lock().unwrap().take() else {
        return;
    };
    let mut current = control.current.lock().unwrap();
    if let Some(old) = current.take() {
        unsafe { snd_seq_disconnect_from(seq, in_port, old.client, old.port) };
    }
    if let Some(ep) = target {
        let rc = unsafe { snd_seq_connect_from(seq, in_port, ep.client, ep.port) };
        if rc < 0 {
            log::warn!("midi: could not connect input from {:?} ({rc})", ep.name);
        } else {
            *current = Some(ep);
        }
    }
}

/// Input loop: wait for events, decode them to raw MIDI, push to the ring. Polls
/// with a timeout rather than blocking outright so retargets and shutdown are
/// picked up promptly. Owns and closes the input handle.
fn input_loop(
    in_seq: SeqPtr,
    in_port: c_int,
    control: Arc<InputControl>,
    mut producer: InputProducer,
) {
    let seq = in_seq.0;
    let mut parser: *mut SndMidiEvent = std::ptr::null_mut();
    if unsafe { snd_midi_event_new(MIDI_BUF, &mut parser) } < 0 {
        log::warn!("midi: could not create ALSA decoder; input disabled");
        unsafe { snd_seq_close(seq) };
        return;
    }
    // Emit a status byte per message; the guest's framer tracks running status.
    unsafe { snd_midi_event_no_status(parser, 1) };

    let events = libc::POLLIN as c_short;
    let nfds = unsafe { snd_seq_poll_descriptors_count(seq, events) }.max(0) as usize;
    let mut pfds = vec![
        libc::pollfd {
            fd: 0,
            events: 0,
            revents: 0
        };
        nfds
    ];
    if nfds > 0 {
        unsafe { snd_seq_poll_descriptors(seq, pfds.as_mut_ptr(), nfds as c_uint, events) };
    }

    let mut buf = [0u8; MIDI_BUF];
    while !control.stop.load(Ordering::Relaxed) {
        apply_input_request(seq, in_port, &control);
        // Wake at least every 100ms to re-check the stop flag and any retarget.
        if nfds > 0 {
            unsafe { libc::poll(pfds.as_mut_ptr(), nfds as libc::nfds_t, 100) };
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
        // Drain everything the poll made ready; the handle is non-blocking, so
        // the loop ends with -EAGAIN once the buffer empties.
        loop {
            let mut ev: *mut SeqEvent = std::ptr::null_mut();
            if unsafe { snd_seq_event_input(seq, &mut ev) } < 0 || ev.is_null() {
                break;
            }
            let len =
                unsafe { snd_midi_event_decode(parser, buf.as_mut_ptr(), buf.len() as c_long, ev) };
            // Non-MIDI events (subscription notices, and the like) decode to a
            // negative error and are skipped.
            if len > 0 {
                let data = &buf[..len as usize];
                log_first_input(data.len());
                push_input(&mut producer, data);
            }
        }
    }
    unsafe {
        snd_midi_event_free(parser);
        snd_seq_close(seq);
    }
}

// --- backend ------------------------------------------------------------

pub struct AlsaBackend {
    out_seq: *mut SndSeq,
    out_port: c_int,
    queue: c_int,
    // MIDI-byte encoder, touched only by the emulation thread via `send`.
    parser: *mut SndMidiEvent,
    dest: Option<Endpoint>,
    // `COPPERLINE_MIDI_IMMEDIATE=1`: dispatch directly ("now") instead of on the
    // queue, to isolate a scheduling problem from a routing one.
    immediate: bool,
    input: Arc<InputControl>,
    input_thread: Option<JoinHandle<()>>,
}

// The handles are only ever touched by the thread that owns the backend; the
// input handle lives on its own thread behind `SeqPtr`.
unsafe impl Send for AlsaBackend {}

impl MidiBackend for AlsaBackend {
    fn send(&mut self, data: &[u8], at: Instant) {
        if self.dest.is_none() || data.is_empty() {
            return;
        }
        let mut ev = SeqEvent::blank();
        unsafe { snd_midi_event_reset_encode(self.parser) };
        let n = unsafe {
            snd_midi_event_encode(self.parser, data.as_ptr(), data.len() as c_long, &mut ev)
        };
        if n <= 0 || ev.type_ == SND_SEQ_EVENT_NONE {
            log_send_error("snd_midi_event_encode produced no event");
            return;
        }
        // Route from our port to whoever we are connected to. The encoder may
        // have set the length bits in `flags` (SysEx), so only the schedule bits
        // are touched below.
        ev.source.port = self.out_port as u8;
        ev.dest.client = SND_SEQ_ADDRESS_SUBSCRIBERS;
        ev.dest.port = SND_SEQ_ADDRESS_UNKNOWN;
        if self.immediate {
            ev.queue = SND_SEQ_QUEUE_DIRECT;
        } else {
            // Schedule in real time, relative to the queue's now (== host now):
            // a past instant collapses to a zero delay, i.e. as soon as possible.
            let delay = at.saturating_duration_since(Instant::now());
            ev.flags = (ev.flags & !(SND_SEQ_TIME_STAMP_MASK | SND_SEQ_TIME_MODE_MASK))
                | SND_SEQ_TIME_STAMP_REAL
                | SND_SEQ_TIME_MODE_REL;
            ev.time = SeqRealTime {
                tv_sec: delay.as_secs() as c_uint,
                tv_nsec: delay.subsec_nanos() as c_uint,
            };
            ev.queue = self.queue as u8;
        }
        if unsafe { snd_seq_event_output(self.out_seq, &mut ev) } < 0 {
            log_send_error("snd_seq_event_output failed");
            return;
        }
        if unsafe { snd_seq_drain_output(self.out_seq) } < 0 {
            log_send_error("snd_seq_drain_output failed");
        }
    }

    fn set_output(&mut self, endpoint: Option<&str>) {
        // Drop the current connection first so a retarget never leaves two.
        if let Some(old) = self.dest.take() {
            unsafe { snd_seq_disconnect_to(self.out_seq, self.out_port, old.client, old.port) };
        }
        if let Some(name) = endpoint {
            if let Some(ep) = find_named(query_ports(DEST_CAPS), name) {
                let rc =
                    unsafe { snd_seq_connect_to(self.out_seq, self.out_port, ep.client, ep.port) };
                if rc < 0 {
                    log::warn!("midi: could not connect output to {:?} ({rc})", ep.name);
                } else {
                    self.dest = Some(ep);
                }
            }
        }
    }

    fn set_input(&mut self, endpoint: Option<&str>) {
        // Resolve on this thread, hand the address to the thread that owns the
        // input handle.
        let target = endpoint.and_then(|name| find_named(query_ports(SOURCE_CAPS), name));
        *self.input.request.lock().unwrap() = Some(target);
    }

    fn current_output(&self) -> Option<String> {
        self.dest.as_ref().map(|e| e.name.clone())
    }

    fn current_input(&self) -> Option<String> {
        self.input
            .current
            .lock()
            .unwrap()
            .as_ref()
            .map(|e| e.name.clone())
    }
}

impl Drop for AlsaBackend {
    fn drop(&mut self) {
        // Stop and join the input thread (it closes its own handle) before the
        // output handle goes.
        self.input.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.input_thread.take() {
            let _ = t.join();
        }
        unsafe {
            if !self.parser.is_null() {
                snd_midi_event_free(self.parser);
            }
            if !self.out_seq.is_null() {
                snd_seq_close(self.out_seq);
            }
        }
    }
}

pub fn enumerate() -> MidiEndpoints {
    MidiEndpoints {
        inputs: query_ports(SOURCE_CAPS)
            .into_iter()
            .map(|e| MidiEndpoint { name: e.name })
            .collect(),
        outputs: query_ports(DEST_CAPS)
            .into_iter()
            .map(|e| MidiEndpoint { name: e.name })
            .collect(),
    }
}

pub fn open(
    midi_out: Option<&str>,
    midi_in: Option<&str>,
    input: InputProducer,
) -> Result<Box<dyn MidiBackend>> {
    // Fresh own-client set for this backend so enumerate() hides our ports.
    own_clients().lock().unwrap().clear();

    let immediate = crate::envcfg::flag("COPPERLINE_MIDI_IMMEDIATE");
    if immediate {
        log::info!("midi: immediate send mode (COPPERLINE_MIDI_IMMEDIATE), scheduling bypassed");
    }

    // Output client: our source port plus the real-time queue scheduled sends
    // ride. The ports are named so users can spot us in `aconnect`.
    let out_seq = open_seq()?;
    let client_name = CString::new("Copperline").unwrap();
    unsafe { snd_seq_set_client_name(out_seq, client_name.as_ptr()) };
    own_clients()
        .lock()
        .unwrap()
        .push(unsafe { snd_seq_client_id(out_seq) });
    let out_port = create_port(
        out_seq,
        "Copperline out",
        SND_SEQ_PORT_CAP_READ | SND_SEQ_PORT_CAP_SUBS_READ,
    )?;
    let queue = unsafe { snd_seq_alloc_queue(out_seq) };
    if queue < 0 {
        unsafe { snd_seq_close(out_seq) };
        bail!("snd_seq_alloc_queue failed ({queue})");
    }
    // Start the queue clock so scheduled events are actually dispatched.
    unsafe {
        snd_seq_control_queue(out_seq, queue, SND_SEQ_EVENT_START, 0, std::ptr::null_mut());
        snd_seq_drain_output(out_seq);
    }

    let mut parser: *mut SndMidiEvent = std::ptr::null_mut();
    if unsafe { snd_midi_event_new(MIDI_BUF, &mut parser) } < 0 {
        unsafe { snd_seq_close(out_seq) };
        bail!("snd_midi_event_new failed");
    }
    unsafe { snd_midi_event_no_status(parser, 1) };

    // Input client, driven on its own thread. Non-blocking so a burst drains
    // without the thread ever parking inside the library.
    let in_seq = open_seq()?;
    unsafe {
        snd_seq_set_client_name(in_seq, client_name.as_ptr());
        snd_seq_nonblock(in_seq, 1);
    }
    own_clients()
        .lock()
        .unwrap()
        .push(unsafe { snd_seq_client_id(in_seq) });
    let in_port = create_port(
        in_seq,
        "Copperline in",
        SND_SEQ_PORT_CAP_WRITE | SND_SEQ_PORT_CAP_SUBS_WRITE,
    )?;

    let dest = match midi_out {
        Some(want) => {
            let ep = find_named(query_ports(DEST_CAPS), want)
                .ok_or_else(|| anyhow!("no MIDI output endpoint matches {want:?}"))?;
            let rc = unsafe { snd_seq_connect_to(out_seq, out_port, ep.client, ep.port) };
            if rc < 0 {
                bail!("could not connect MIDI output to {:?} ({rc})", ep.name);
            }
            log::info!("midi: output connected to {:?}", ep.name);
            Some(ep)
        }
        None => None,
    };

    let control = Arc::new(InputControl {
        request: Mutex::new(None),
        current: Mutex::new(None),
        stop: AtomicBool::new(false),
    });
    if let Some(want) = midi_in {
        let ep = find_named(query_ports(SOURCE_CAPS), want)
            .ok_or_else(|| anyhow!("no MIDI input endpoint matches {want:?}"))?;
        let rc = unsafe { snd_seq_connect_from(in_seq, in_port, ep.client, ep.port) };
        if rc < 0 {
            bail!("could not connect MIDI input from {:?} ({rc})", ep.name);
        }
        log::info!("midi: input connected from {:?}", ep.name);
        *control.current.lock().unwrap() = Some(ep);
    }

    let input_thread = {
        let control = Arc::clone(&control);
        let in_ptr = SeqPtr(in_seq);
        std::thread::Builder::new()
            .name("copperline-midi-in".into())
            .spawn(move || input_loop(in_ptr, in_port, control, input))
            .map_err(|e| anyhow!("could not spawn MIDI input thread: {e}"))?
    };

    Ok(Box::new(AlsaBackend {
        out_seq,
        out_port,
        queue,
        parser,
        dest,
        immediate,
        input: control,
        input_thread: Some(input_thread),
    }))
}

/// Create one of our sequencer ports, generic MIDI plus application type.
fn create_port(seq: *mut SndSeq, name: &str, caps: c_uint) -> Result<c_int> {
    let cname = CString::new(name).unwrap();
    let port = unsafe {
        snd_seq_create_simple_port(
            seq,
            cname.as_ptr(),
            caps,
            SND_SEQ_PORT_TYPE_MIDI_GENERIC | SND_SEQ_PORT_TYPE_APPLICATION,
        )
    };
    if port < 0 {
        unsafe { snd_seq_close(seq) };
        bail!("snd_seq_create_simple_port {name:?} failed ({port})");
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::{Consumer, Split};
    use ringbuf::HeapRb;

    /// Round-trip a scheduled message through the kernel "Midi Through" port,
    /// which echoes what it is sent back to its readers. Proves the whole live
    /// path at once: queue scheduling, the output subscription, input decode, and
    /// the ring push -- and that the schedule is honoured rather than collapsed to
    /// "now". Ignored by default: it needs a running sequencer with the
    /// snd-seq-dummy "Midi Through" client, so run it explicitly with
    /// `cargo test --features midi -- --ignored`.
    #[test]
    #[ignore]
    fn midi_through_loopback() {
        const THROUGH: &str = "Midi Through";
        let (producer, mut consumer) = HeapRb::<u8>::new(64).split();
        let mut backend = match open(Some(THROUGH), Some(THROUGH), producer) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skipping midi_through_loopback: {e}");
                return;
            }
        };

        // Schedule far enough ahead to clear the input thread's poll interval, so
        // a broken schedule (delivered immediately) is distinguishable from a
        // honoured one purely by arrival time.
        let note = [0x90u8, 0x3C, 0x64];
        let lead = Duration::from_millis(300);
        let sent = Instant::now();
        backend.send(&note, sent + lead);

        let mut got = Vec::new();
        let mut arrived = None;
        let deadline = Instant::now() + Duration::from_secs(2);
        while got.len() < note.len() && Instant::now() < deadline {
            match consumer.try_pop() {
                Some(b) => {
                    arrived.get_or_insert_with(Instant::now);
                    got.push(b);
                }
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        }

        assert_eq!(got, note, "looped-back bytes should match what was sent");
        let delay = arrived.unwrap().duration_since(sent);
        assert!(
            delay >= Duration::from_millis(250),
            "scheduled send arrived too early ({delay:?}); timing was not honoured"
        );
    }
}
