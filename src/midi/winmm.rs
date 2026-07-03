// SPDX-License-Identifier: GPL-3.0-or-later

//! Windows WinMM backend, linked straight against `winmm.dll` with no wrapper
//! crate. WinMM's short/long message calls carry no timestamp -- they play a
//! message the instant they are called -- so the scheduled-send contract is
//! honoured by a dedicated host scheduler thread: `send` pushes each message
//! onto a priority queue keyed by its host `Instant` and wakes the thread, which
//! sleeps until the next message is due and only then calls `midiOutShortMsg` /
//! `midiOutLongMsg`. That thread is the direct analogue of a CoreMIDI packet
//! timestamp or an ALSA real-time queue event, and it keeps `send` non-blocking
//! on the emulation thread. `timeBeginPeriod(1)` raises the system timer to ~1 ms
//! so the thread's timed wait is fine-grained rather than the default ~15 ms.
//!
//! Input runs the CoreMIDI model, not the ALSA one: `midiInOpen` with
//! `CALLBACK_FUNCTION` delivers messages on a system-owned thread, and that
//! callback pushes decoded bytes straight into the lock-free SPSC ring Paula's
//! receiver drains. Short messages arrive as `MIM_DATA` (the message packed into
//! a dword); SysEx as `MIM_LONGDATA` into pre-posted buffers that are recycled
//! with `midiInAddBuffer` on each callback.
//!
//! The raw FFI is layout-sensitive: `MIDIHDR`/`MIDIOUTCAPSW`/`MIDIINCAPSW` are
//! pinned to the Win32 headers with compile-time size/offset assertions, the way
//! CoreMIDI's packet list and ALSA's `snd_seq_event_t` are. WinMM is stdcall
//! (`WINAPI`), so every import and the input callback are `extern "system"`, not
//! `extern "C"` -- the wrong convention corrupts the stack.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use ringbuf::traits::Producer;

use super::{InputProducer, MidiBackend, MidiEndpoint, MidiEndpoints};

// --- WinMM FFI ----------------------------------------------------------

type Mmresult = u32;
type Hmidiout = *mut c_void;
type Hmidiin = *mut c_void;
// DWORD_PTR: pointer-sized, carries handles/instances/packed callback params.
type DwordPtr = usize;

const MMSYSERR_NOERROR: Mmresult = 0;

const CALLBACK_NULL: u32 = 0x0000_0000;
const CALLBACK_FUNCTION: u32 = 0x0003_0000;

// midiInProc `wMsg` values (mmsystem.h). Only the two data messages matter here.
const MIM_DATA: u32 = 0x03C3;
const MIM_LONGDATA: u32 = 0x03C4;

// MIDIHDR.dwFlags: set by the driver when it has finished with a buffer.
const MHDR_DONE: u32 = 0x0000_0001;

// MAXPNAMELEN: length of the szPname device-name field in the caps structs.
const MAXPNAMELEN: usize = 32;

// The input callback: `void CALLBACK MidiInProc(HMIDIIN, UINT wMsg,
// DWORD_PTR dwInstance, DWORD_PTR dwParam1, DWORD_PTR dwParam2)`.
type MidiInProc = extern "system" fn(Hmidiin, u32, DwordPtr, DwordPtr, DwordPtr);

// MIDIHDR (mmeapi.h). The multimedia headers pack this struct, so on 64-bit it is
// 112 bytes with lp_next at offset 28, not the 120/32 that natural alignment gives
// -- hence packed(4) and the offset asserts below. Only lp_data, the two length
// fields, and dw_flags are ever read or written; the rest is the driver's.
#[repr(C, packed(4))]
#[allow(dead_code)] // FFI layout mirror: several fields exist only for the driver.
struct MidiHdr {
    lp_data: *mut u8,
    dw_buffer_length: u32,
    dw_bytes_recorded: u32,
    dw_user: DwordPtr,
    dw_flags: u32,
    lp_next: *mut MidiHdr,
    reserved: DwordPtr,
    dw_offset: u32,
    dw_reserved: [DwordPtr; 8],
}

// MIDIOUTCAPSW (mmeapi.h): device caps in the wide (UTF-16) form.
#[repr(C)]
#[allow(dead_code)] // FFI layout mirror: only szPname is read.
struct MidiOutCapsW {
    w_mid: u16,
    w_pid: u16,
    v_driver_version: u32,
    sz_pname: [u16; MAXPNAMELEN],
    w_technology: u16,
    w_voices: u16,
    w_notes: u16,
    w_channel_mask: u16,
    dw_support: u32,
}

// MIDIINCAPSW (mmeapi.h).
#[repr(C)]
#[allow(dead_code)] // FFI layout mirror: only szPname is read.
struct MidiInCapsW {
    w_mid: u16,
    w_pid: u16,
    v_driver_version: u32,
    sz_pname: [u16; MAXPNAMELEN],
    dw_support: u32,
}

// Pin the mirrors to the SDK layout. The caps structs have no pointers, so their
// size is width-independent; MIDIHDR does, so it is checked per pointer width.
const _: () = {
    assert!(std::mem::size_of::<MidiOutCapsW>() == 84);
    assert!(std::mem::offset_of!(MidiOutCapsW, sz_pname) == 8);
    assert!(std::mem::size_of::<MidiInCapsW>() == 76);
    assert!(std::mem::offset_of!(MidiInCapsW, sz_pname) == 8);
};

#[cfg(target_pointer_width = "64")]
const _: () = {
    use std::mem::offset_of;
    // From <mmeapi.h> (64-bit).
    assert!(std::mem::size_of::<MidiHdr>() == 112);
    assert!(offset_of!(MidiHdr, lp_data) == 0);
    assert!(offset_of!(MidiHdr, dw_buffer_length) == 8);
    assert!(offset_of!(MidiHdr, dw_bytes_recorded) == 12);
    assert!(offset_of!(MidiHdr, dw_user) == 16);
    assert!(offset_of!(MidiHdr, dw_flags) == 24);
    assert!(offset_of!(MidiHdr, lp_next) == 28);
    assert!(offset_of!(MidiHdr, reserved) == 36);
    assert!(offset_of!(MidiHdr, dw_offset) == 44);
    assert!(offset_of!(MidiHdr, dw_reserved) == 48);
};

#[cfg(target_pointer_width = "32")]
const _: () = {
    use std::mem::offset_of;
    assert!(std::mem::size_of::<MidiHdr>() == 64);
    assert!(offset_of!(MidiHdr, lp_data) == 0);
    assert!(offset_of!(MidiHdr, dw_flags) == 16);
    assert!(offset_of!(MidiHdr, lp_next) == 20);
};

#[link(name = "winmm")]
extern "system" {
    fn midiOutGetNumDevs() -> u32;
    fn midiOutGetDevCapsW(device_id: DwordPtr, caps: *mut MidiOutCapsW, size: u32) -> Mmresult;
    fn midiOutOpen(
        out: *mut Hmidiout,
        device_id: u32,
        callback: DwordPtr,
        instance: DwordPtr,
        flags: u32,
    ) -> Mmresult;
    fn midiOutClose(midi_out: Hmidiout) -> Mmresult;
    fn midiOutReset(midi_out: Hmidiout) -> Mmresult;
    fn midiOutShortMsg(midi_out: Hmidiout, msg: u32) -> Mmresult;
    fn midiOutLongMsg(midi_out: Hmidiout, hdr: *mut MidiHdr, size: u32) -> Mmresult;
    fn midiOutPrepareHeader(midi_out: Hmidiout, hdr: *mut MidiHdr, size: u32) -> Mmresult;
    fn midiOutUnprepareHeader(midi_out: Hmidiout, hdr: *mut MidiHdr, size: u32) -> Mmresult;

    fn midiInGetNumDevs() -> u32;
    fn midiInGetDevCapsW(device_id: DwordPtr, caps: *mut MidiInCapsW, size: u32) -> Mmresult;
    fn midiInOpen(
        out: *mut Hmidiin,
        device_id: u32,
        callback: DwordPtr,
        instance: DwordPtr,
        flags: u32,
    ) -> Mmresult;
    fn midiInClose(midi_in: Hmidiin) -> Mmresult;
    fn midiInStart(midi_in: Hmidiin) -> Mmresult;
    fn midiInStop(midi_in: Hmidiin) -> Mmresult;
    fn midiInReset(midi_in: Hmidiin) -> Mmresult;
    fn midiInPrepareHeader(midi_in: Hmidiin, hdr: *mut MidiHdr, size: u32) -> Mmresult;
    fn midiInUnprepareHeader(midi_in: Hmidiin, hdr: *mut MidiHdr, size: u32) -> Mmresult;
    fn midiInAddBuffer(midi_in: Hmidiin, hdr: *mut MidiHdr, size: u32) -> Mmresult;

    // Timer resolution, also in winmm: raise the scheduler clock to ~1 ms.
    fn timeBeginPeriod(period: u32) -> Mmresult;
    fn timeEndPeriod(period: u32) -> Mmresult;
}

// Ceiling on a single received SysEx message, and how many receive buffers to
// keep posted so a burst is not dropped between callbacks. Matches the ALSA
// backend's decode-buffer size.
const SYSEX_BUF: usize = 4096;
const NUM_SYSEX_BUFFERS: usize = 4;

// --- helpers ------------------------------------------------------------

/// Turn a fixed `szPname` field into a `String`, stopping at the NUL.
fn caps_name(sz: &[u16]) -> String {
    let end = sz.iter().position(|&c| c == 0).unwrap_or(sz.len());
    String::from_utf16_lossy(&sz[..end])
}

/// Display name of output device `id`, if it can be queried.
fn output_name(id: u32) -> Option<String> {
    let mut caps: MidiOutCapsW = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        midiOutGetDevCapsW(
            id as DwordPtr,
            &mut caps,
            std::mem::size_of::<MidiOutCapsW>() as u32,
        )
    };
    (rc == MMSYSERR_NOERROR).then(|| caps_name(&caps.sz_pname))
}

/// Display name of input device `id`, if it can be queried.
fn input_name(id: u32) -> Option<String> {
    let mut caps: MidiInCapsW = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        midiInGetDevCapsW(
            id as DwordPtr,
            &mut caps,
            std::mem::size_of::<MidiInCapsW>() as u32,
        )
    };
    (rc == MMSYSERR_NOERROR).then(|| caps_name(&caps.sz_pname))
}

/// First output device whose name contains `want` (case-insensitive), the same
/// match the CoreMIDI and ALSA backends use. Device ids are `0..numDevs`.
fn find_output(want: &str) -> Option<(u32, String)> {
    let needle = want.to_lowercase();
    (0..unsafe { midiOutGetNumDevs() })
        .filter_map(|id| output_name(id).map(|n| (id, n)))
        .find(|(_, name)| name.to_lowercase().contains(&needle))
}

/// First input device whose name contains `want` (case-insensitive).
fn find_input(want: &str) -> Option<(u32, String)> {
    let needle = want.to_lowercase();
    (0..unsafe { midiInGetNumDevs() })
        .filter_map(|id| input_name(id).map(|n| (id, n)))
        .find(|(_, name)| name.to_lowercase().contains(&needle))
}

/// Total bytes of a short (non-SysEx) message from its status byte, so the input
/// callback pushes exactly the valid bytes MIM_DATA packed into its dword and no
/// trailing zeros. Mirrors the shared bridge's `message_len`.
fn short_len(status: u8) -> usize {
    match status & 0xF0 {
        0x80 | 0x90 | 0xA0 | 0xB0 | 0xE0 => 3,
        0xC0 | 0xD0 => 2,
        0xF0 => match status {
            0xF2 => 3,
            0xF1 | 0xF3 => 2,
            _ => 1,
        },
        _ => 1,
    }
}

/// Hand decoded input bytes to the guest, honouring the Active Sensing opt-out
/// exactly like the CoreMIDI and ALSA backends.
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

/// Log the first input callback so a silent MIDI-in path (nothing arriving from
/// the host) is distinguishable from a consumption problem (arriving but the
/// guest's serial receiver never runs).
fn log_first_input(n: usize) {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        log::info!("midi: input callback received {n} byte(s) from host");
    }
}

/// Log a send failure once (errors typically repeat every message).
fn log_send_error(msg: &str) {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        log::warn!("midi: {msg}");
    }
}

// --- output delivery ----------------------------------------------------

/// The current output device. Shared (behind a `Mutex`) between the emulation
/// thread (which retargets it) and the scheduler thread (which sends through it).
/// The handle is stored as a `usize` so the shared state is plainly `Send`; it is
/// only ever a WinMM `HMIDIOUT`, cast back at the call sites below.
struct OutputState {
    handle: usize,
    name: Option<String>,
}

/// Play one message on the current output *now*. Called by the scheduler thread
/// when a message comes due, and directly by `send` under
/// `COPPERLINE_MIDI_IMMEDIATE`. Holding the lock across the call serialises with
/// a device switch, so the handle can never be closed mid-send.
fn deliver(out: &Mutex<OutputState>, data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let state = out.lock().unwrap();
    if state.handle == 0 {
        return;
    }
    let handle = state.handle as Hmidiout;
    if data[0] == 0xF0 {
        deliver_long(handle, data);
    } else {
        // Status in the low byte, then data1, data2, packed little-endian.
        let mut msg = 0u32;
        for (i, &b) in data.iter().take(3).enumerate() {
            msg |= (b as u32) << (8 * i as u32);
        }
        let rc = unsafe { midiOutShortMsg(handle, msg) };
        if rc != MMSYSERR_NOERROR {
            log_send_error(&format!("midiOutShortMsg failed ({rc})"));
        }
    }
}

/// Send a SysEx message via `midiOutLongMsg`. The send is asynchronous and the
/// buffer must outlive it, so the buffer and header sit on this stack frame and we
/// wait for the driver to raise `MHDR_DONE` before unpreparing. SysEx is rare, so
/// the brief block does not disturb note timing.
fn deliver_long(handle: Hmidiout, data: &[u8]) {
    let mut buf = data.to_vec();
    let mut hdr: MidiHdr = unsafe { std::mem::zeroed() };
    hdr.lp_data = buf.as_mut_ptr();
    hdr.dw_buffer_length = buf.len() as u32;
    hdr.dw_bytes_recorded = buf.len() as u32;
    let size = std::mem::size_of::<MidiHdr>() as u32;
    unsafe {
        if midiOutPrepareHeader(handle, &mut hdr, size) != MMSYSERR_NOERROR {
            log_send_error("midiOutPrepareHeader failed");
            return;
        }
        let rc = midiOutLongMsg(handle, &mut hdr, size);
        if rc != MMSYSERR_NOERROR {
            log_send_error(&format!("midiOutLongMsg failed ({rc})"));
        } else {
            // Wait for the driver to finish with the buffer (bounded, ~1s).
            let mut spins = 0;
            while hdr.dw_flags & MHDR_DONE == 0 && spins < 1000 {
                std::thread::sleep(Duration::from_millis(1));
                spins += 1;
            }
        }
        midiOutUnprepareHeader(handle, &mut hdr, size);
    }
}

// --- scheduler thread ---------------------------------------------------

/// A message due for delivery at `at`. `seq` breaks ties so equal instants keep
/// arrival order (FIFO) and the heap never has to compare payloads.
struct Scheduled {
    at: Instant,
    seq: u64,
    data: Vec<u8>,
}

impl PartialEq for Scheduled {
    fn eq(&self, other: &Self) -> bool {
        self.at == other.at && self.seq == other.seq
    }
}
impl Eq for Scheduled {}
impl PartialOrd for Scheduled {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Scheduled {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.at.cmp(&other.at).then(self.seq.cmp(&other.seq))
    }
}

/// The scheduler's queue plus its shutdown flag and tie-break counter.
struct SchedQueue {
    // A min-heap by (at, seq): `Reverse` flips the max-heap so the soonest
    // message is at the top.
    heap: BinaryHeap<Reverse<Scheduled>>,
    seq: u64,
    stop: bool,
}

/// Shared between the emulation thread (`send` pushes here) and the scheduler
/// thread (which pops due messages and delivers them through `out`).
struct SchedulerShared {
    queue: Mutex<SchedQueue>,
    wake: Condvar,
    out: Arc<Mutex<OutputState>>,
}

/// The scheduler loop: sleep until the soonest message is due, deliver it, repeat.
/// A message already past its instant fires immediately (as soon as possible),
/// matching the CoreMIDI and ALSA backends. The timed wait relies on
/// `timeBeginPeriod(1)` for ~1 ms granularity rather than busy-spinning.
fn scheduler_loop(shared: Arc<SchedulerShared>) {
    let mut queue = shared.queue.lock().unwrap();
    loop {
        if queue.stop {
            return;
        }
        match queue.heap.peek() {
            None => {
                // Nothing scheduled: park until `send` or shutdown wakes us.
                queue = shared.wake.wait(queue).unwrap();
            }
            Some(Reverse(next)) => {
                let now = Instant::now();
                if next.at <= now {
                    let due = queue.heap.pop().unwrap().0;
                    // Release the queue while delivering so `send` never blocks
                    // on a (possibly slow SysEx) send.
                    drop(queue);
                    deliver(&shared.out, &due.data);
                    queue = shared.queue.lock().unwrap();
                } else {
                    let wait = next.at - now;
                    let (g, _) = shared.wake.wait_timeout(queue, wait).unwrap();
                    queue = g;
                }
            }
        }
    }
}

// --- input --------------------------------------------------------------

/// Kept alive (boxed) for the input port's lifetime; its address is the
/// callback's `dwInstance`. Only the WinMM callback thread pushes to `producer`,
/// so the single-writer invariant of the SPSC ring holds. `closing` fences the
/// callback during teardown so a buffer returned by `midiInReset` is neither
/// pushed nor re-added to a port being closed.
struct InputState {
    producer: InputProducer,
    handle: Hmidiin,
    closing: AtomicBool,
    // Pre-posted SysEx receive buffers, kept alive while registered with the
    // driver; their headers are recycled in the callback via `midiInAddBuffer`.
    sysex: Vec<Box<SysexBuf>>,
}

/// A receive buffer and its header, boxed so the header's `lp_data` pointer and
/// the address handed to the driver stay put.
#[repr(C)]
struct SysexBuf {
    hdr: MidiHdr,
    data: [u8; SYSEX_BUF],
}

/// The input callback. `extern "system"` (stdcall) to match WinMM's `CALLBACK`.
extern "system" fn midi_in_proc(
    _midi_in: Hmidiin,
    msg: u32,
    instance: DwordPtr,
    param1: DwordPtr,
    _param2: DwordPtr,
) {
    if instance == 0 {
        return;
    }
    // Sole writer: WinMM serialises a port's callback. The instance pointer is
    // the boxed InputState, live until midiInClose returns (see close_input).
    let state = unsafe { &mut *(instance as *mut InputState) };
    if state.closing.load(Ordering::Acquire) {
        return;
    }
    match msg {
        MIM_DATA => {
            // dwParam1 packs the short message: status, data1, data2 (low bytes).
            let packed = param1 as u32;
            let status = (packed & 0xFF) as u8;
            let bytes = [status, (packed >> 8) as u8, (packed >> 16) as u8];
            let len = short_len(status);
            log_first_input(len);
            push_input(&mut state.producer, &bytes[..len]);
        }
        MIM_LONGDATA => {
            // dwParam1 is the header the driver just filled with SysEx bytes.
            let hdr = param1 as *mut MidiHdr;
            if hdr.is_null() {
                return;
            }
            let recorded = unsafe { (*hdr).dw_bytes_recorded } as usize;
            // A zero-length return means the buffer is being handed back on
            // reset/close, not fresh data: do not push it or re-post it.
            if recorded == 0 {
                return;
            }
            let data = unsafe { std::slice::from_raw_parts((*hdr).lp_data, recorded) };
            log_first_input(recorded);
            push_input(&mut state.producer, data);
            // Recycle the buffer so the next SysEx is not dropped.
            unsafe {
                (*hdr).dw_bytes_recorded = 0;
                midiInAddBuffer(state.handle, hdr, std::mem::size_of::<MidiHdr>() as u32);
            }
        }
        _ => {}
    }
}

// --- backend ------------------------------------------------------------

pub struct WinmmBackend {
    out: Arc<Mutex<OutputState>>,
    sched: Arc<SchedulerShared>,
    sched_thread: Option<JoinHandle<()>>,
    // `COPPERLINE_MIDI_IMMEDIATE=1`: deliver on the calling thread instead of via
    // the scheduler queue, to isolate a scheduling problem from a routing one.
    immediate: bool,
    // Input port. The handle is null when no input is open; the ring producer is
    // parked here while closed and moved into the InputState while open.
    in_handle: Hmidiin,
    in_state: Option<Box<InputState>>,
    in_name: Option<String>,
    in_producer: Option<InputProducer>,
}

// The WinMM handles are only touched by the thread that owns the backend (the
// input handle also by the driver's own callback thread, which the backend
// outlives). The scheduler thread shares only the plainly-Send OutputState.
unsafe impl Send for WinmmBackend {}

impl WinmmBackend {
    fn open_output(&mut self, id: u32, name: String) -> Mmresult {
        let mut handle: Hmidiout = std::ptr::null_mut();
        let rc = unsafe { midiOutOpen(&mut handle, id, 0, 0, CALLBACK_NULL) };
        if rc == MMSYSERR_NOERROR {
            let mut state = self.out.lock().unwrap();
            state.handle = handle as usize;
            state.name = Some(name);
        }
        rc
    }

    fn close_output(&mut self) {
        let mut state = self.out.lock().unwrap();
        if state.handle != 0 {
            let handle = state.handle as Hmidiout;
            unsafe {
                midiOutReset(handle);
                midiOutClose(handle);
            }
            state.handle = 0;
            state.name = None;
        }
    }

    fn open_input(&mut self, id: u32, name: String) -> Result<()> {
        let producer = self
            .in_producer
            .take()
            .ok_or_else(|| anyhow!("MIDI input already open"))?;
        let mut state = Box::new(InputState {
            producer,
            handle: std::ptr::null_mut(),
            closing: AtomicBool::new(false),
            sysex: Vec::with_capacity(NUM_SYSEX_BUFFERS),
        });
        let instance = state.as_ref() as *const InputState as DwordPtr;
        let mut handle: Hmidiin = std::ptr::null_mut();
        let rc = unsafe {
            midiInOpen(
                &mut handle,
                id,
                midi_in_proc as MidiInProc as DwordPtr,
                instance,
                CALLBACK_FUNCTION,
            )
        };
        if rc != MMSYSERR_NOERROR {
            // Reclaim the producer so a later open can retry.
            self.in_producer = Some(state.producer);
            bail!("midiInOpen failed ({rc}) for {name:?}");
        }
        state.handle = handle;

        // Post the SysEx receive buffers before starting, so a dump arriving
        // immediately has somewhere to land.
        let size = std::mem::size_of::<MidiHdr>() as u32;
        for _ in 0..NUM_SYSEX_BUFFERS {
            let mut buf = Box::new(SysexBuf {
                hdr: unsafe { std::mem::zeroed() },
                data: [0u8; SYSEX_BUF],
            });
            buf.hdr.lp_data = buf.data.as_mut_ptr();
            buf.hdr.dw_buffer_length = SYSEX_BUF as u32;
            let hptr = &mut buf.hdr as *mut MidiHdr;
            unsafe {
                if midiInPrepareHeader(handle, hptr, size) == MMSYSERR_NOERROR
                    && midiInAddBuffer(handle, hptr, size) == MMSYSERR_NOERROR
                {
                    state.sysex.push(buf);
                }
            }
        }

        unsafe { midiInStart(handle) };
        self.in_handle = handle;
        self.in_name = Some(name);
        self.in_state = Some(state);
        Ok(())
    }

    fn close_input(&mut self) {
        if self.in_handle.is_null() {
            return;
        }
        let handle = self.in_handle;
        // Fence the callback before resetting: midiInReset hands the posted
        // buffers back through the callback, which must not push or re-post them.
        if let Some(state) = self.in_state.as_ref() {
            state.closing.store(true, Ordering::Release);
        }
        unsafe {
            midiInStop(handle);
            midiInReset(handle);
            if let Some(state) = self.in_state.as_mut() {
                let size = std::mem::size_of::<MidiHdr>() as u32;
                for buf in &mut state.sysex {
                    midiInUnprepareHeader(handle, &mut buf.hdr, size);
                }
            }
            // After close returns the driver raises no further callbacks, so the
            // boxed InputState is safe to drop and its producer to reclaim.
            midiInClose(handle);
        }
        self.in_handle = std::ptr::null_mut();
        self.in_name = None;
        if let Some(state) = self.in_state.take() {
            self.in_producer = Some(state.producer);
        }
    }
}

impl MidiBackend for WinmmBackend {
    fn send(&mut self, data: &[u8], at: Instant) {
        if data.is_empty() {
            return;
        }
        if self.immediate {
            deliver(&self.out, data);
            return;
        }
        // Push and wake: non-blocking on the emulation thread. If no output is
        // selected the scheduler still pops the message at its due instant and
        // drops it, so the queue stays bounded by the in-flight window.
        let mut queue = self.sched.queue.lock().unwrap();
        queue.seq += 1;
        let seq = queue.seq;
        queue.heap.push(Reverse(Scheduled {
            at,
            seq,
            data: data.to_vec(),
        }));
        drop(queue);
        self.sched.wake.notify_one();
    }

    fn set_output(&mut self, endpoint: Option<&str>) {
        self.close_output();
        if let Some(want) = endpoint {
            match find_output(want) {
                Some((id, name)) => {
                    let rc = self.open_output(id, name.clone());
                    if rc != MMSYSERR_NOERROR {
                        log::warn!("midi: could not open output {name:?} ({rc})");
                    }
                }
                None => log::warn!("midi: no MIDI output endpoint matches {want:?}"),
            }
        }
    }

    fn set_input(&mut self, endpoint: Option<&str>) {
        self.close_input();
        if let Some(want) = endpoint {
            match find_input(want) {
                Some((id, name)) => {
                    if let Err(e) = self.open_input(id, name) {
                        log::warn!("midi: could not open input: {e}");
                    }
                }
                None => log::warn!("midi: no MIDI input endpoint matches {want:?}"),
            }
        }
    }

    fn current_output(&self) -> Option<String> {
        self.out.lock().unwrap().name.clone()
    }

    fn current_input(&self) -> Option<String> {
        self.in_name.clone()
    }
}

impl Drop for WinmmBackend {
    fn drop(&mut self) {
        // Stop the scheduler and join it before the output handle goes, so no
        // delivery is in flight when the device is closed.
        {
            let mut queue = self.sched.queue.lock().unwrap();
            queue.stop = true;
        }
        self.sched.wake.notify_all();
        if let Some(t) = self.sched_thread.take() {
            let _ = t.join();
        }
        self.close_input();
        self.close_output();
        unsafe { timeEndPeriod(1) };
    }
}

pub fn enumerate() -> MidiEndpoints {
    MidiEndpoints {
        inputs: (0..unsafe { midiInGetNumDevs() })
            .filter_map(input_name)
            .map(|name| MidiEndpoint { name })
            .collect(),
        outputs: (0..unsafe { midiOutGetNumDevs() })
            .filter_map(output_name)
            .map(|name| MidiEndpoint { name })
            .collect(),
    }
}

pub fn open(
    midi_out: Option<&str>,
    midi_in: Option<&str>,
    input: InputProducer,
) -> Result<Box<dyn MidiBackend>> {
    let immediate = crate::envcfg::flag("COPPERLINE_MIDI_IMMEDIATE");
    if immediate {
        log::info!("midi: immediate send mode (COPPERLINE_MIDI_IMMEDIATE), scheduling bypassed");
    }

    // Raise the timer resolution so the scheduler's timed wait is ~1 ms, not the
    // default ~15 ms. Paired with timeEndPeriod in Drop.
    unsafe { timeBeginPeriod(1) };

    let out = Arc::new(Mutex::new(OutputState {
        handle: 0,
        name: None,
    }));
    let sched = Arc::new(SchedulerShared {
        queue: Mutex::new(SchedQueue {
            heap: BinaryHeap::new(),
            seq: 0,
            stop: false,
        }),
        wake: Condvar::new(),
        out: Arc::clone(&out),
    });
    let sched_thread = {
        let shared = Arc::clone(&sched);
        std::thread::Builder::new()
            .name("copperline-midi-sched".into())
            .spawn(move || scheduler_loop(shared))
            .map_err(|e| anyhow!("could not spawn MIDI scheduler thread: {e}"))?
    };

    let mut backend = WinmmBackend {
        out,
        sched,
        sched_thread: Some(sched_thread),
        immediate,
        in_handle: std::ptr::null_mut(),
        in_state: None,
        in_name: None,
        in_producer: Some(input),
    };

    if let Some(want) = midi_out {
        let (id, name) =
            find_output(want).ok_or_else(|| anyhow!("no MIDI output endpoint matches {want:?}"))?;
        let rc = backend.open_output(id, name.clone());
        if rc != MMSYSERR_NOERROR {
            bail!("could not open MIDI output {name:?} ({rc})");
        }
        log::info!("midi: output connected to {name:?}");
    }
    if let Some(want) = midi_in {
        let (id, name) =
            find_input(want).ok_or_else(|| anyhow!("no MIDI input endpoint matches {want:?}"))?;
        backend.open_input(id, name.clone())?;
        log::info!("midi: input connected to {name:?}");
    }
    if backend.out.lock().unwrap().handle == 0 && backend.in_handle.is_null() {
        log::warn!("[serial] mode = midi but no midi_out/midi_in endpoint selected; MIDI is inert");
    }

    Ok(Box::new(backend))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::{Consumer, Split};
    use ringbuf::HeapRb;

    /// Round-trip a scheduled message through a virtual loopback port (loopMIDI,
    /// or whatever `COPPERLINE_MIDI_TEST_PORT` names). Proves the whole live path
    /// at once -- scheduler thread, output open, the input callback, and the ring
    /// push -- and that the schedule is honoured rather than collapsed to "now".
    /// Ignored by default: it needs a loopback port with the same name visible as
    /// both an output and an input, so run it explicitly with
    /// `cargo test --features midi -- --ignored`.
    #[test]
    #[ignore]
    fn midi_through_loopback() {
        let port = std::env::var("COPPERLINE_MIDI_TEST_PORT").unwrap_or_else(|_| "loopMIDI".into());
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
