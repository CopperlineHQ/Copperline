// SPDX-License-Identifier: GPL-3.0-or-later

//! Running a native file picker without wedging the window's message queue.
//!
//! rfd's blocking pickers run the platform's own modal loop on the calling
//! thread. Copperline opens them from inside the winit event callback, and on
//! Windows that combination livelocks the queue:
//!
//! * winit's `WM_PAINT` arm sets `redraw_requested = should_buffer()` and, once
//!   `DefWindowProcW` has validated the window, calls `RedrawWindow` with
//!   `RDW_INTERNALPAINT` if that flag is set -- re-arming a paint so a redraw
//!   asked for during a buffered period is not lost.
//! * `should_buffer()` is "the runner is not holding an event handler", which
//!   is true for as long as one of our callbacks is on the stack. It is
//!   therefore true for the whole life of a dialog opened from one.
//!
//! So every `WM_PAINT` the dialog's modal loop dispatches immediately arms the
//! next one, `GetMessage` always has a message to hand back, and the thread's
//! queue never goes idle. The shell's file dialog fills its item view on idle,
//! so the list sits on "Working on it..." for as long as the dialog is open, in
//! every folder, while the dialog itself stays responsive. Whether it bites at
//! all depends on an internal paint being pending when the click is handled --
//! which the per-frame `request_redraw` of a running machine arms constantly,
//! and a machine sitting in the launcher does not. That is the whole of the
//! "sometimes the first dialog, sometimes the second" behaviour.
//!
//! The fix is to give the picker a thread of its own, whose queue is quiet.
//! This thread then pumps its own messages while it waits, or the window is
//! ghosted as "Not Responding" after about five seconds; `WM_PAINT` is
//! validated rather than dispatched, since dispatching it is exactly the
//! re-arm above and would spin this thread for the life of the dialog. Nothing
//! is lost by not painting: the emulator is stopped for the duration, so the
//! frame on screen is the one it stopped on, and the windows are invalidated
//! again on the way out.
//!
//! Every other platform calls the picker directly on this thread, which is
//! what AppKit and GTK require of it.

/// Show a native file picker, returning what it picked.
///
/// The bounds are the same on every platform even though only Windows moves
/// the closure to another thread, so that a capture which could not make that
/// move fails to build everywhere rather than on Windows alone.
#[cfg(not(windows))]
pub(super) fn pick<T, F>(picker: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    picker()
}

#[cfg(windows)]
pub(super) fn pick<T, F>(picker: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    use std::sync::mpsc;
    use std::time::Duration;

    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            // A picker that panics drops the sender; the scope re-raises the
            // panic when it joins, so the wait below only has to end.
            let _ = tx.send(picker());
        });
        let mut pump = MessagePump::default();
        loop {
            pump.drain();
            match rx.recv_timeout(Duration::from_millis(16)) {
                Ok(picked) => {
                    pump.invalidate();
                    return picked;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    pump.invalidate();
                    panic!("native file picker ended without a result");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    /// On Windows this drives the whole hand-off -- scoped thread, message
    /// pump, channel -- and on every platform it pins the contract the call
    /// sites rely on: what the picker chose is what comes back.
    #[test]
    fn pick_returns_what_the_picker_chose() {
        let chosen = std::path::PathBuf::from("df0.adf");
        let expected = chosen.clone();
        assert_eq!(super::pick(move || Some(chosen)), Some(expected));
    }

    /// A cancelled picker is a None, not a hang: the wait has to end on the
    /// empty answer too.
    #[test]
    fn pick_returns_a_cancelled_picker() {
        assert_eq!(super::pick(|| Option::<std::path::PathBuf>::None), None);
    }
}

/// Keeps this thread's windows serviced while a picker runs on another one,
/// remembering which of them it validated so they can be repainted after.
#[cfg(windows)]
#[derive(Default)]
struct MessagePump {
    /// Windows whose paints were validated rather than dispatched. Held as
    /// `isize` because `HWND` is a raw pointer, and these are only ever used
    /// on the thread that collected them.
    validated: Vec<isize>,
}

#[cfg(windows)]
impl MessagePump {
    /// Dispatch everything waiting for this thread, short of a paint.
    fn drain(&mut self) {
        use windows_sys::Win32::Graphics::Gdi::{RedrawWindow, RDW_NOINTERNALPAINT, RDW_VALIDATE};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_PAINT,
        };

        // Bounded so that a message which re-posts itself cannot hold this
        // pass and stop the picker's result from being collected.
        const MAX_MESSAGES: usize = 512;

        let mut msg = MSG::default();
        for _ in 0..MAX_MESSAGES {
            // SAFETY: a plain thread-wide peek; `msg` is owned here and the
            // window handles come from the messages themselves.
            unsafe {
                if PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) == 0 {
                    break;
                }
                if msg.message == WM_PAINT {
                    // Validating clears the update region and the internal
                    // paint flag both, which dispatching would only re-arm
                    // (see the module comment).
                    RedrawWindow(
                        msg.hwnd,
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        RDW_VALIDATE | RDW_NOINTERNALPAINT,
                    );
                    let hwnd = msg.hwnd as isize;
                    if !self.validated.contains(&hwnd) {
                        self.validated.push(hwnd);
                    }
                    continue;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// Ask for the paints back that `drain` swallowed. They are served once
    /// this callback returns and winit's loop is dispatching again, so they
    /// arrive as ordinary `RedrawRequested` events.
    fn invalidate(&self) {
        use windows_sys::Win32::Graphics::Gdi::{
            RedrawWindow, RDW_ERASE, RDW_INTERNALPAINT, RDW_INVALIDATE,
        };

        for &hwnd in &self.validated {
            // SAFETY: handles this thread collected from its own messages.
            unsafe {
                RedrawWindow(
                    hwnd as _,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    RDW_INVALIDATE | RDW_ERASE | RDW_INTERNALPAINT,
                );
            }
        }
    }
}
