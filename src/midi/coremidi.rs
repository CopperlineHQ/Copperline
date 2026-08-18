// SPDX-License-Identifier: GPL-3.0-or-later

//! macOS CoreMIDI backend, linked straight against the CoreMIDI and
//! CoreFoundation frameworks with no wrapper crate. Output carries a CoreMIDI
//! packet timestamp, so the MIDIServer delivers each byte at its scheduled
//! instant no matter when the emulation thread called `MIDISend`. That is what
//! keeps the guest's byte timing intact. Input arrives on a CoreMIDI thread and
//! is pushed to the lock-free ring Paula's receiver drains.
//!
//! The process holds exactly one `MIDIClient`, created on first use and never
//! disposed (see [`shared_client`]); each machine's backend owns only its
//! ports.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::OnceLock;
use std::time::Instant;

use anyhow::{anyhow, bail, Result};
use ringbuf::traits::Producer;

use super::{InputProducer, MidiBackend, MidiEndpoint, MidiEndpoints};

// --- CoreMIDI / CoreFoundation FFI types --------------------------------

type OSStatus = i32;
type MidiObjectRef = u32;
type MidiClientRef = MidiObjectRef;
type MidiPortRef = MidiObjectRef;
type MidiEndpointRef = MidiObjectRef;
type MidiTimeStamp = u64;
type ItemCount = usize;
type ByteCount = usize;
type CFStringRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFTypeRef = *const c_void;
type CFIndex = isize;
type Boolean = u8;

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

// MIDIPacket / MIDIPacketList mirror the C layout. The `data` array is nominal:
// a real packet carries `length` bytes contiguously from `data`, which may be
// more or fewer than 256. Packet construction goes through the SDK's
// Init/Add helpers, so the only manual layout use is walking received packets.
//
// CoreMIDI packs these to 4 bytes (`#pragma pack(4)`), so `packet` follows
// `num_packets` with no 8-byte alignment padding: the first packet is at
// offset 4, not 8. Natural alignment here would mis-locate every received
// packet and read zeros.
#[repr(C, packed(4))]
struct MidiPacket {
    time_stamp: MidiTimeStamp,
    length: u16,
    data: [u8; 256],
}

#[repr(C, packed(4))]
struct MidiPacketList {
    num_packets: u32,
    packet: [MidiPacket; 1],
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

type MidiReadProc =
    extern "C" fn(pkt_list: *const MidiPacketList, read_ref: *mut c_void, src_ref: *mut c_void);
type MidiNotifyProc = extern "C" fn(message: *const c_void, ref_con: *mut c_void);

#[link(name = "CoreMIDI", kind = "framework")]
extern "C" {
    static kMIDIPropertyDisplayName: CFStringRef;

    fn MIDIClientCreate(
        name: CFStringRef,
        notify_proc: Option<MidiNotifyProc>,
        notify_ref_con: *mut c_void,
        out_client: *mut MidiClientRef,
    ) -> OSStatus;
    fn MIDIPortDispose(port: MidiPortRef) -> OSStatus;
    fn MIDIOutputPortCreate(
        client: MidiClientRef,
        port_name: CFStringRef,
        out_port: *mut MidiPortRef,
    ) -> OSStatus;
    fn MIDIInputPortCreate(
        client: MidiClientRef,
        port_name: CFStringRef,
        read_proc: MidiReadProc,
        ref_con: *mut c_void,
        out_port: *mut MidiPortRef,
    ) -> OSStatus;
    fn MIDIPortConnectSource(
        port: MidiPortRef,
        source: MidiEndpointRef,
        conn_ref_con: *mut c_void,
    ) -> OSStatus;
    fn MIDIPortDisconnectSource(port: MidiPortRef, source: MidiEndpointRef) -> OSStatus;
    fn MIDIGetNumberOfSources() -> ItemCount;
    fn MIDIGetSource(index: ItemCount) -> MidiEndpointRef;
    fn MIDIGetNumberOfDestinations() -> ItemCount;
    fn MIDIGetDestination(index: ItemCount) -> MidiEndpointRef;
    fn MIDIObjectGetStringProperty(
        obj: MidiObjectRef,
        property_id: CFStringRef,
        out_str: *mut CFStringRef,
    ) -> OSStatus;
    fn MIDISend(
        port: MidiPortRef,
        dest: MidiEndpointRef,
        pkt_list: *const MidiPacketList,
    ) -> OSStatus;
    fn MIDIPacketListInit(pkt_list: *mut MidiPacketList) -> *mut MidiPacket;
    fn MIDIPacketListAdd(
        pkt_list: *mut MidiPacketList,
        list_size: ByteCount,
        cur_packet: *mut MidiPacket,
        time: MidiTimeStamp,
        n_data: ByteCount,
        data: *const u8,
    ) -> *mut MidiPacket;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFStringGetCStringPtr(the_string: CFStringRef, encoding: u32) -> *const c_char;
    fn CFStringGetCString(
        the_string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: CFIndex,
        encoding: u32,
    ) -> Boolean;
    fn CFRelease(cf: CFTypeRef);
}

// mach timebase lives in libSystem, which is always linked.
extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

// --- helpers ------------------------------------------------------------

/// Wrap a Rust string as a CFString for the CoreMIDI name arguments. The caller
/// must `CFRelease` the result.
fn cfstring(s: &str) -> CFStringRef {
    let c = CString::new(s).unwrap_or_default();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), K_CF_STRING_ENCODING_UTF8) }
}

/// Copy a CFString into a Rust String.
fn cfstring_to_string(s: CFStringRef) -> Option<String> {
    if s.is_null() {
        return None;
    }
    unsafe {
        let ptr = CFStringGetCStringPtr(s, K_CF_STRING_ENCODING_UTF8);
        if !ptr.is_null() {
            return CStr::from_ptr(ptr).to_str().ok().map(String::from);
        }
        // No direct pointer available (the common case): copy into a buffer.
        let mut buf = [0 as c_char; 512];
        if CFStringGetCString(
            s,
            buf.as_mut_ptr(),
            buf.len() as CFIndex,
            K_CF_STRING_ENCODING_UTF8,
        ) != 0
        {
            return CStr::from_ptr(buf.as_ptr()).to_str().ok().map(String::from);
        }
    }
    None
}

/// Read an endpoint's display name.
fn endpoint_name(endpoint: MidiEndpointRef) -> Option<String> {
    if endpoint == 0 {
        return None;
    }
    let mut cf: CFStringRef = std::ptr::null();
    let status =
        unsafe { MIDIObjectGetStringProperty(endpoint, kMIDIPropertyDisplayName, &mut cf) };
    if status != 0 || cf.is_null() {
        return None;
    }
    let name = cfstring_to_string(cf);
    unsafe { CFRelease(cf) };
    name
}

fn sources() -> impl Iterator<Item = (MidiEndpointRef, String)> {
    (0..unsafe { MIDIGetNumberOfSources() })
        .map(|i| unsafe { MIDIGetSource(i) })
        .filter_map(|e| endpoint_name(e).map(|n| (e, n)))
}

fn destinations() -> impl Iterator<Item = (MidiEndpointRef, String)> {
    (0..unsafe { MIDIGetNumberOfDestinations() })
        .map(|i| unsafe { MIDIGetDestination(i) })
        .filter_map(|e| endpoint_name(e).map(|n| (e, n)))
}

fn find_endpoint(
    it: impl Iterator<Item = (MidiEndpointRef, String)>,
    want: &str,
) -> Option<MidiEndpointRef> {
    let needle = want.to_lowercase();
    it.filter(|(_, name)| name.to_lowercase().contains(&needle))
        .map(|(e, _)| e)
        .next()
}

/// Cached mach timebase: mach ticks -> nanoseconds is `ticks * numer / denom`.
fn timebase() -> (u64, u64) {
    static TB: OnceLock<(u64, u64)> = OnceLock::new();
    *TB.get_or_init(|| {
        let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
        unsafe { mach_timebase_info(&mut info) };
        let numer = if info.numer == 0 {
            1
        } else {
            info.numer as u64
        };
        let denom = if info.denom == 0 {
            1
        } else {
            info.denom as u64
        };
        (numer, denom)
    })
}

/// Convert a host [`Instant`] into a CoreMIDI timestamp (mach absolute time).
/// A timestamp of 0 means "now"; an instant already in the past collapses to it.
fn host_timestamp(at: Instant) -> MidiTimeStamp {
    match at.checked_duration_since(Instant::now()) {
        None => 0,
        Some(delta) => {
            let (numer, denom) = timebase();
            // nanoseconds -> mach ticks is the inverse of ticks -> ns.
            let ns = delta.as_nanos();
            let ticks = ns.saturating_mul(denom as u128) / (numer as u128);
            unsafe { mach_absolute_time() }.saturating_add(ticks as u64)
        }
    }
}

// --- input callback -----------------------------------------------------

/// Kept alive for the port's lifetime; its address is the read-callback refCon.
/// Only the CoreMIDI callback thread touches the producer, so the single-writer
/// invariant of the SPSC ring holds.
struct InputState {
    producer: InputProducer,
}

/// Address of the next packet after one of `length` data bytes. Packets are
/// 4-byte aligned (pack(4)), so the end of the data is rounded up to 4.
unsafe fn next_packet(pkt: *const MidiPacket, length: usize) -> *const MidiPacket {
    // offsetof(MIDIPacket, data) = 8 (timeStamp) + 2 (length).
    let offset = (10 + length + 3) & !3;
    (pkt as *const u8).add(offset) as *const MidiPacket
}

extern "C" fn read_proc(pkt_list: *const MidiPacketList, read_ref: *mut c_void, _src: *mut c_void) {
    if pkt_list.is_null() || read_ref.is_null() {
        return;
    }
    // Sole writer: CoreMIDI serializes a port's read callback. Read the packed
    // fields through raw pointers -- a reference to a misaligned field is UB.
    let state = unsafe { &mut *(read_ref as *mut InputState) };
    let num_packets = unsafe { std::ptr::addr_of!((*pkt_list).num_packets).read_unaligned() };
    let mut pkt = unsafe { std::ptr::addr_of!((*pkt_list).packet) as *const MidiPacket };
    for _ in 0..num_packets {
        let length = unsafe { std::ptr::addr_of!((*pkt).length).read_unaligned() } as usize;
        let data_ptr = unsafe { std::ptr::addr_of!((*pkt).data) as *const u8 };
        let data = unsafe { std::slice::from_raw_parts(data_ptr, length) };
        log_first_input(length);
        // Overflow (guest not draining fast enough) drops the tail, mirroring a
        // real UART receive overrun rather than blocking the MIDI thread.
        // Active Sensing is forwarded by default; only dropped when opted in
        // (see strip_active_sense).
        if super::strip_active_sense() {
            for &b in data {
                if b != super::ACTIVE_SENSE {
                    state.producer.push_slice(std::slice::from_ref(&b));
                }
            }
        } else {
            state.producer.push_slice(data);
        }
        pkt = unsafe { next_packet(pkt, length) };
    }
}

/// Log the first input callback so a silent MIDI-in path (nothing arriving from
/// the host) is distinguishable from a consumption problem (arriving but the
/// guest's serial receiver never runs).
fn log_first_input(n: usize) {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::info!("midi: input callback received {n} byte(s) from host");
    }
}

/// Log a send failure once (errors typically repeat every byte, so only the
/// first is worth surfacing).
fn log_send_error(msg: &str) {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::warn!("midi: {msg}");
    }
}

// --- backend ------------------------------------------------------------

pub struct CoreMidiBackend {
    // Both ports are created up front so the device can be switched at runtime
    // without making ports mid-session. `dest`/`source` hold the current
    // endpoints (0 = none); switching just retargets them.
    out_port: MidiPortRef,
    dest: MidiEndpointRef,
    in_port: MidiPortRef,
    source: MidiEndpointRef,
    // `COPPERLINE_MIDI_IMMEDIATE=1`: send with timestamp 0 ("now") instead of a
    // scheduled host time, to isolate a scheduling problem from a routing one.
    immediate: bool,
    // Kept alive so the read-callback refCon stays valid; dropped after the
    // ports are disposed (see Drop) -- the client outlives them all.
    _input_state: Box<InputState>,
}

// The stored handles are integer references and a ring producer; the raw pointer
// handed to CoreMIDI is derived from the boxed state, which outlives the port.
// Safe to move between threads.
unsafe impl Send for CoreMidiBackend {}

impl MidiBackend for CoreMidiBackend {
    fn send(&mut self, data: &[u8], at: Instant) {
        if self.dest == 0 || data.is_empty() {
            return;
        }
        let ts = if self.immediate {
            0
        } else {
            host_timestamp(at)
        };
        // 8-aligned scratch large enough for one short packet.
        let mut storage = [0u64; 128];
        let list = storage.as_mut_ptr() as *mut MidiPacketList;
        unsafe {
            let cur = MIDIPacketListInit(list);
            let cur = MIDIPacketListAdd(
                list,
                std::mem::size_of_val(&storage),
                cur,
                ts,
                data.len(),
                data.as_ptr(),
            );
            if cur.is_null() {
                log_send_error("MIDIPacketListAdd overflow");
                return;
            }
            let status = MIDISend(self.out_port, self.dest, list);
            if status != 0 {
                log_send_error(&format!("MIDISend failed (OSStatus {status})"));
            }
        }
    }

    fn set_output(&mut self, endpoint: Option<&str>) {
        self.dest = endpoint
            .and_then(|name| find_endpoint(destinations(), name))
            .unwrap_or(0);
    }

    fn set_input(&mut self, endpoint: Option<&str>) {
        if self.source != 0 {
            unsafe { MIDIPortDisconnectSource(self.in_port, self.source) };
            self.source = 0;
        }
        if let Some(src) = endpoint.and_then(|name| find_endpoint(sources(), name)) {
            if unsafe { MIDIPortConnectSource(self.in_port, src, std::ptr::null_mut()) } == 0 {
                self.source = src;
            }
        }
    }

    fn current_output(&self) -> Option<String> {
        (self.dest != 0).then(|| endpoint_name(self.dest)).flatten()
    }

    fn current_input(&self) -> Option<String> {
        (self.source != 0)
            .then(|| endpoint_name(self.source))
            .flatten()
    }
}

impl Drop for CoreMidiBackend {
    fn drop(&mut self) {
        // The ports are this machine's; the client is the process's and
        // stays (see [`shared_client`]). Disposing the input port stops
        // the read callback before the boxed input state is freed.
        unsafe {
            MIDIPortDispose(self.in_port);
            MIDIPortDispose(self.out_port);
        }
    }
}

/// The process's one CoreMIDI client, created on first use and never
/// disposed. CoreMIDI's link to the MIDIServer daemon is per-process and
/// unrecoverable: the daemon exits a few seconds after the system-wide
/// last client is disposed, and a process whose link dies that way can
/// never create a client again -- every later `MIDIClientCreate` returns
/// OSStatus -2 until the app is relaunched. (Apple's `MIDIClientDispose`
/// documentation describes exactly this, and advises against disposing.)
/// One client held for the process's lifetime keeps the daemon running
/// and the link healthy; the machines' ports come and go beneath it.
fn shared_client() -> Result<MidiClientRef> {
    static CLIENT: OnceLock<std::result::Result<MidiClientRef, OSStatus>> = OnceLock::new();
    let made = CLIENT.get_or_init(|| {
        let name = cfstring("Copperline");
        let mut client: MidiClientRef = 0;
        let status = unsafe { MIDIClientCreate(name, None, std::ptr::null_mut(), &mut client) };
        unsafe { CFRelease(name) };
        if status == 0 {
            Ok(client)
        } else {
            Err(status)
        }
    });
    match made {
        Ok(client) => Ok(*client),
        // A failed first create lost a race against the daemon shutting
        // down, and the loss is permanent for this process: retrying
        // returns -2 for the rest of its life, so the cached failure is
        // the truth and the only cure is named.
        Err(status) => bail!(
            "MIDIClientCreate failed (OSStatus {status}); CoreMIDI cannot              recover in this process -- quit and relaunch Copperline to              restore MIDI"
        ),
    }
}

pub fn enumerate() -> MidiEndpoints {
    // Enumerating without a client would answer, but leaves the process
    // holding a server link nothing keeps alive -- the exact state that
    // strands CoreMIDI when the daemon idles out. The client comes
    // first, and holds the daemon up from here on.
    if let Err(e) = shared_client() {
        log::warn!("midi: {e:#}");
        return MidiEndpoints::default();
    }
    MidiEndpoints {
        inputs: sources().map(|(_, name)| MidiEndpoint { name }).collect(),
        outputs: destinations()
            .map(|(_, name)| MidiEndpoint { name })
            .collect(),
    }
}

pub fn open(
    midi_out: Option<&str>,
    midi_in: Option<&str>,
    input: InputProducer,
) -> Result<Box<dyn MidiBackend>> {
    let client = shared_client()?;

    let immediate = crate::envcfg::flag("COPPERLINE_MIDI_IMMEDIATE");
    if immediate {
        log::info!("midi: immediate send mode (COPPERLINE_MIDI_IMMEDIATE), scheduling bypassed");
    }

    // Create both ports now so the runtime menu can retarget devices without
    // creating ports mid-session. The input state (and its ring producer) is
    // kept even when no source is connected yet.
    let out_port = create_port(client, "Copperline out", |c, n, p| unsafe {
        MIDIOutputPortCreate(c, n, p)
    })?;
    let state = Box::new(InputState { producer: input });
    let ref_con = &*state as *const InputState as *mut c_void;
    let in_port = create_port(client, "Copperline in", |c, n, p| unsafe {
        MIDIInputPortCreate(c, n, read_proc, ref_con, p)
    })?;

    let mut backend = CoreMidiBackend {
        out_port,
        dest: 0,
        in_port,
        source: 0,
        immediate,
        _input_state: state,
    };

    if let Some(want) = midi_out {
        backend.dest = find_endpoint(destinations(), want)
            .ok_or_else(|| anyhow!("no MIDI output endpoint matches {want:?}"))?;
        log::info!("midi: output connected to {:?}", backend.output_name());
    }
    if let Some(want) = midi_in {
        let src = find_endpoint(sources(), want)
            .ok_or_else(|| anyhow!("no MIDI input endpoint matches {want:?}"))?;
        if unsafe { MIDIPortConnectSource(in_port, src, std::ptr::null_mut()) } != 0 {
            bail!("MIDIPortConnectSource failed");
        }
        backend.source = src;
        log::info!("midi: input connected to {:?}", backend.input_name());
    }

    Ok(Box::new(backend))
}

impl CoreMidiBackend {
    fn output_name(&self) -> String {
        endpoint_name(self.dest).unwrap_or_default()
    }
    fn input_name(&self) -> String {
        endpoint_name(self.source).unwrap_or_default()
    }
}

/// Create a CoreMIDI port with a temporary CFString name.
fn create_port(
    client: MidiClientRef,
    name: &str,
    make: impl Fn(MidiClientRef, CFStringRef, *mut MidiPortRef) -> OSStatus,
) -> Result<MidiPortRef> {
    let cf = cfstring(name);
    let mut port: MidiPortRef = 0;
    let status = make(client, cf, &mut port);
    unsafe { CFRelease(cf) };
    if status != 0 {
        bail!("MIDI port create failed for {name:?} (OSStatus {status})");
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::{Consumer, Split};
    use ringbuf::HeapRb;
    use std::time::Duration;

    /// The new lifecycle's teardown holds up: a drop disposes only the
    /// machine's ports, and the next open builds fresh ones on the
    /// process's one client. (This passes immediately by design; the
    /// test that discriminates the old lifecycle from the new is the
    /// enumerate-linger-open one below, which must span the daemon's
    /// idle exit and is ignored for its sleep.)
    #[test]
    fn backends_reopen_after_drop() {
        let (p1, _c1) = HeapRb::<u8>::new(8).split();
        let b1 = open(None, None, p1).expect("first open");
        drop(b1);
        let (p2, _c2) = HeapRb::<u8>::new(8).split();
        let b2 = open(None, None, p2).expect("open again after drop");
        drop(b2);
    }

    /// The launcher's exact failure: enumerate the endpoints, linger past
    /// the MIDIServer's idle-exit window, then open. Without the shared
    /// client the enumerate held a doomed server link and the open was the
    /// process's first create over it -- OSStatus -2, unrecoverable.
    /// Ignored for its 12-second sleep; run it solo:
    /// `cargo test --release backends_open_after_an_enumerate_and_a_wait -- --ignored`
    #[test]
    #[ignore]
    fn backends_open_after_an_enumerate_and_a_wait() {
        let ends = enumerate();
        let _ = ends;
        std::thread::sleep(Duration::from_secs(12));
        let (producer, _consumer) = HeapRb::<u8>::new(8).split();
        let backend = open(None, None, producer).expect("open after enumerate and wait");
        drop(backend);
    }

    /// Round-trip a scheduled message through a loopback port (an IAC Driver bus,
    /// or whatever `COPPERLINE_MIDI_TEST_PORT` names). Proves the whole live path
    /// at once -- the packet timestamp, output to the destination, the input
    /// callback, and the ring push -- and that the schedule is honoured rather
    /// than collapsed to "now". Ignored by default: it needs a loopback port with
    /// the same name visible as both a source and a destination (enable an IAC bus
    /// in Audio MIDI Setup), so run it explicitly with
    /// `cargo test --features midi -- --ignored`.
    #[test]
    #[ignore]
    fn midi_through_loopback() {
        let port = std::env::var("COPPERLINE_MIDI_TEST_PORT")
            .unwrap_or_else(|_| "IAC Driver Bus 1".into());
        let (producer, mut consumer) = HeapRb::<u8>::new(64).split();
        let mut backend = match open(Some(&port), Some(&port), producer) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skipping midi_through_loopback: {e}");
                return;
            }
        };

        // Schedule far enough ahead that a broken schedule (delivered now) is
        // distinguishable from an honoured one purely by arrival time.
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
