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
//! macOS cannot run the blocking picker from inside the callback either, for
//! a reason of its own. While one of our callbacks is on the stack winit does
//! not deliver a further event -- its handler is not re-entrant -- but queues
//! it as a block on the main run loop in `kCFRunLoopDefaultMode`, reasoning
//! that a modal panel runs in `NSModalPanelRunLoopMode` and resizing one in
//! `NSEventTrackingRunLoopMode`, so the block waits until the callback has
//! returned. From macOS 27 that no longer holds: a mouse-down on the panel
//! (its resize edge, most simply) is routed through the gesture environment,
//! and `_NSGestureRecognizerSortAndSendDelayedEvents` spins
//! `-[NSRunLoop runMode:beforeDate:]` in the default mode from inside
//! `-[NSSavePanel runModal]`. The queued block runs there, under our callback,
//! and winit panics with "tried to handle event while another event is
//! currently being handled" -- in a frame that cannot unwind, so the process
//! aborts. Whatever was queued will do; the window losing key status to the
//! panel is enough.
//!
//! Nothing can be done about that from under the callback, so on macOS the
//! picker is not run from there at all. It is opened as a sheet on the window
//! (rfd's async dialog, which returns at once), the callback returns, and the
//! window loop collects the answer on a later pass and only then runs what
//! the caller wanted done with it. With no callback on the stack, events that
//! arrive while the sheet is up are delivered as they come.
//!
//! That is why a picker is asked for with a [`PickRequest`] and a
//! continuation rather than called for its return value: the one shape serves
//! both the platforms that answer before [`App::pick_paths`] returns and the
//! one that answers later. The machine is held while a sheet is up, as it is
//! for the life of a blocking picker, so neither sees emulated time pass.
//!
//! GTK and the XDG portal are called directly on this thread, which is what
//! they require.

use std::path::PathBuf;

use super::App;

/// What kind of answer a picker gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PickKind {
    File,
    Files,
    /// A file or a directory. Only AppKit has the combined panel; elsewhere
    /// this is a file picker.
    FileOrFolder,
    Folder,
    Save,
}

/// A native file picker, described rather than run, so that the same
/// description opens a blocking dialog or a sheet as the platform requires
/// (see the module comment).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PickRequest {
    kind: PickKind,
    title: String,
    filters: Vec<(String, Vec<String>)>,
    directory: Option<PathBuf>,
    file_name: Option<String>,
}

impl PickRequest {
    fn new(kind: PickKind, title: impl Into<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            filters: Vec::new(),
            directory: None,
            file_name: None,
        }
    }

    /// Pick one existing file.
    pub(super) fn file(title: impl Into<String>) -> Self {
        Self::new(PickKind::File, title)
    }

    /// Pick one or more existing files.
    pub(super) fn files(title: impl Into<String>) -> Self {
        Self::new(PickKind::Files, title)
    }

    /// Pick one existing file or directory where the host has a panel for
    /// both (macOS), one existing file elsewhere.
    pub(super) fn file_or_folder(title: impl Into<String>) -> Self {
        Self::new(PickKind::FileOrFolder, title)
    }

    /// Pick one existing directory.
    pub(super) fn folder(title: impl Into<String>) -> Self {
        Self::new(PickKind::Folder, title)
    }

    /// Name a file to create or overwrite.
    pub(super) fn save(title: impl Into<String>) -> Self {
        Self::new(PickKind::Save, title)
    }

    /// Whether the host's picker for this request takes directories as well
    /// as files, for a caller whose title should say so.
    pub(super) const FILE_OR_FOLDER_TAKES_FOLDERS: bool = cfg!(target_os = "macos");

    pub(super) fn filter(mut self, name: &str, extensions: &[&str]) -> Self {
        self.filters.push((
            name.to_string(),
            extensions.iter().map(|ext| ext.to_string()).collect(),
        ));
        self
    }

    /// Where the picker opens; `None` leaves it to the host.
    pub(super) fn directory(mut self, directory: Option<PathBuf>) -> Self {
        self.directory = directory;
        self
    }

    /// The name a save picker suggests.
    pub(super) fn file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = Some(file_name.into());
        self
    }
}

/// Carry a request's fields over to one of rfd's two builders, which share
/// their method names but no trait.
macro_rules! rfd_dialog {
    ($builder:ty, $request:expr) => {{
        let request = $request;
        let mut dialog = <$builder>::new().set_title(request.title.as_str());
        for (name, extensions) in &request.filters {
            dialog = dialog.add_filter(name.as_str(), extensions);
        }
        if let Some(directory) = &request.directory {
            dialog = dialog.set_directory(directory);
        }
        if let Some(file_name) = &request.file_name {
            dialog = dialog.set_file_name(file_name.as_str());
        }
        dialog
    }};
}

#[cfg(not(target_os = "macos"))]
impl PickRequest {
    /// Run the picker to its answer on the calling thread.
    fn run_blocking(&self) -> Option<Vec<PathBuf>> {
        let dialog = rfd_dialog!(rfd::FileDialog, self);
        match self.kind {
            PickKind::File | PickKind::FileOrFolder => dialog.pick_file().map(|path| vec![path]),
            PickKind::Files => dialog.pick_files(),
            PickKind::Folder => dialog.pick_folder().map(|path| vec![path]),
            PickKind::Save => dialog.save_file().map(|path| vec![path]),
        }
    }
}

#[cfg(target_os = "macos")]
impl PickRequest {
    /// Open the picker as a sheet and return at once; the future resolves
    /// when the sheet is dismissed.
    fn open_deferred(&self) -> PickFuture {
        fn one(handle: Option<rfd::FileHandle>) -> Option<Vec<PathBuf>> {
            handle.map(|handle| vec![handle.path().to_path_buf()])
        }
        let dialog = rfd_dialog!(rfd::AsyncFileDialog, self);
        match self.kind {
            PickKind::File => {
                let picked = dialog.pick_file();
                Box::pin(async move { one(picked.await) })
            }
            PickKind::FileOrFolder => {
                let picked = dialog.pick_file_or_folder();
                Box::pin(async move { one(picked.await) })
            }
            PickKind::Files => {
                let picked = dialog.pick_files();
                Box::pin(async move {
                    picked.await.map(|handles| {
                        handles
                            .iter()
                            .map(|handle| handle.path().to_path_buf())
                            .collect()
                    })
                })
            }
            PickKind::Folder => {
                let picked = dialog.pick_folder();
                Box::pin(async move { one(picked.await) })
            }
            PickKind::Save => {
                let picked = dialog.save_file();
                Box::pin(async move { one(picked.await) })
            }
        }
    }
}

/// A picker that has been opened and not yet answered.
#[cfg(any(target_os = "macos", test))]
pub(super) type PickFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<PathBuf>>>>>;

/// What a caller wants done with a picker's answer; `None` is a cancel.
pub(super) type PickThen = Box<dyn FnOnce(&mut App, Option<Vec<PathBuf>>)>;

/// A sheet that is up, and what is waiting on it.
///
/// Only ever built where pickers are sheets (and by the tests, which stand
/// in for one); the field in `App` is simply `None` for good elsewhere.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(super) struct PendingPick {
    #[cfg(any(target_os = "macos", test))]
    picked: PickFuture,
    then: PickThen,
}

impl App {
    /// Ask for one path and do `then` with it; `None` is a cancel.
    pub(super) fn pick_path(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut App, Option<PathBuf>) + 'static,
    ) {
        self.pick_paths(request, move |app, picked| {
            then(app, picked.and_then(|paths| paths.into_iter().next()))
        });
    }

    /// Ask for the paths `request` describes and do `then` with them.
    ///
    /// `then` has run by the time this returns on the platforms whose picker
    /// blocks, and runs from a later pass of the window loop on macOS (see
    /// the module comment), so nothing after the call may depend on it.
    /// Live audio is suspended and the realtime clock re-anchored around the
    /// picker either way; callers do neither.
    pub(super) fn pick_paths(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut App, Option<Vec<PathBuf>>) + 'static,
    ) {
        if self.pending_pick.is_some() {
            // One sheet at a time. The window under a sheet takes no
            // pointer or key input, but a pad still walks the panels.
            log::debug!(
                "native picker: \"{}\" ignored, one is already open",
                request.title
            );
            return;
        }
        #[cfg(target_os = "macos")]
        {
            self.open_deferred_pick(request.open_deferred(), Box::new(then));
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Nothing else runs on this thread until the picker answers, so
            // the mute needs no state behind it.
            self.suspend_live_audio_for_host_io();
            let picked = pick(move || request.run_blocking());
            self.finish_pick(Box::new(then), picked);
        }
    }

    /// Put a sheet up. The loop runs on under it, so its mute is not a
    /// one-off call that the next `sync_live_audio_suspension` -- a control
    /// client or a debugger changing the run state -- would undo: the sheet
    /// is part of what that predicate reads (`machine_advances`), and stays
    /// so until `finish_pick`.
    #[cfg(any(target_os = "macos", test))]
    pub(super) fn open_deferred_pick(&mut self, picked: PickFuture, then: PickThen) {
        self.pending_pick = Some(PendingPick { picked, then });
        self.sync_live_audio_suspension();
    }

    /// Whether a picker sheet is up. The machine is held for as long as one
    /// is, as it is for the life of a blocking picker.
    pub(super) fn native_pick_pending(&self) -> bool {
        self.pending_pick.is_some()
    }

    /// Whether this pass of the window loop steps the machine: powered on,
    /// not halted, not paused, and not standing behind a picker sheet. A
    /// netplay session runs on behind one, since the peer has to be serviced
    /// whatever this side is looking at.
    pub(super) fn machine_advances(&self) -> bool {
        self.powered_on
            && !self.cpu_halted
            && !self.paused
            && (!self.native_pick_pending() || self.netplay.is_some())
    }

    /// Collect a dismissed sheet's answer and run what was waiting on it.
    /// Called once per pass of the window loop.
    pub(super) fn poll_native_pick(&mut self) {
        #[cfg(any(target_os = "macos", test))]
        {
            let Some(pending) = &mut self.pending_pick else {
                return;
            };
            // rfd completes the future from the sheet's completion handler;
            // the loop is kept awake while one is up, so nothing needs waking.
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            let std::task::Poll::Ready(picked) = pending.picked.as_mut().poll(&mut context) else {
                return;
            };
            let pending = self.pending_pick.take().expect("checked above");
            self.finish_pick(pending.then, picked);
            self.request_redraw();
        }
    }

    fn finish_pick(&mut self, then: PickThen, picked: Option<Vec<PathBuf>>) {
        then(self, picked);
        // A continuation may have asked for a second picker (the extended
        // ROM after the Kickstart); the pause then runs on until that one
        // is answered.
        if self.pending_pick.is_none() {
            self.finish_host_io_pause();
        }
    }
}

/// Show a native file picker, returning what it picked.
///
/// The bounds are the same on every platform even though only Windows moves
/// the closure to another thread, so that a capture which could not make that
/// move fails to build everywhere rather than on Windows alone.
#[cfg(all(not(windows), not(target_os = "macos")))]
fn pick<T, F>(picker: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    picker()
}

#[cfg(windows)]
fn pick<T, F>(picker: F) -> T
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

#[cfg(all(test, not(target_os = "macos")))]
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

#[cfg(test)]
mod deferred_tests {
    use super::super::tests::test_app;
    use super::{PickFuture, PickRequest};
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::task::Poll;

    type Answer = Option<Vec<PathBuf>>;

    /// A sheet the test dismisses by hand: pending until `answer` is filled.
    fn sheet() -> (Rc<RefCell<Option<Answer>>>, PickFuture) {
        let answer = Rc::new(RefCell::new(None));
        let slot = Rc::clone(&answer);
        let picked = std::future::poll_fn(move |_| match slot.borrow_mut().take() {
            Some(answer) => Poll::Ready(answer),
            None => Poll::Pending,
        });
        (answer, Box::pin(picked))
    }

    #[test]
    fn a_sheet_that_is_still_up_holds_the_machine_and_its_continuation() {
        let mut app = test_app();
        app.powered_on = true;
        assert!(app.machine_advances());

        let (_answer, picked) = sheet();
        let ran = Rc::new(RefCell::new(false));
        let flag = Rc::clone(&ran);
        app.open_deferred_pick(picked, Box::new(move |_, _| *flag.borrow_mut() = true));

        app.poll_native_pick();
        assert!(app.native_pick_pending());
        assert!(!app.machine_advances());
        assert!(!*ran.borrow());
    }

    #[test]
    fn a_dismissed_sheet_hands_its_answer_over_once_and_lets_the_machine_go() {
        let mut app = test_app();
        app.powered_on = true;
        let (answer, picked) = sheet();
        let got = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&got);
        app.open_deferred_pick(
            picked,
            Box::new(move |_, picked| sink.borrow_mut().push(picked)),
        );

        *answer.borrow_mut() = Some(Some(vec![PathBuf::from("df0.adf")]));
        app.poll_native_pick();
        app.poll_native_pick();

        assert_eq!(*got.borrow(), vec![Some(vec![PathBuf::from("df0.adf")])]);
        assert!(!app.native_pick_pending());
        assert!(app.machine_advances());
    }

    #[test]
    fn a_cancelled_sheet_reaches_the_continuation_as_none() {
        let mut app = test_app();
        let (answer, picked) = sheet();
        let got = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&got);
        app.open_deferred_pick(
            picked,
            Box::new(move |_, picked| sink.borrow_mut().push(picked)),
        );

        *answer.borrow_mut() = Some(None);
        app.poll_native_pick();

        assert_eq!(*got.borrow(), vec![None]);
        assert!(!app.native_pick_pending());
    }

    /// The Kickstart picker asks for the extended ROM from its continuation;
    /// the second sheet has to be the one waiting afterwards, not cleared
    /// away with the first.
    #[test]
    fn a_continuation_may_open_the_next_sheet() {
        let mut app = test_app();
        let (first, first_picked) = sheet();
        let (second, second_picked) = sheet();
        let got = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&got);
        app.open_deferred_pick(
            first_picked,
            Box::new(move |app, _| {
                app.open_deferred_pick(
                    second_picked,
                    Box::new(move |_, picked| sink.borrow_mut().push(picked)),
                );
            }),
        );

        *first.borrow_mut() = Some(Some(vec![PathBuf::from("kick.rom")]));
        app.poll_native_pick();
        assert!(app.native_pick_pending());
        assert!(got.borrow().is_empty());

        *second.borrow_mut() = Some(Some(vec![PathBuf::from("ext.rom")]));
        app.poll_native_pick();
        assert_eq!(*got.borrow(), vec![Some(vec![PathBuf::from("ext.rom")])]);
        assert!(!app.native_pick_pending());
    }

    /// Refused before any picker is opened, so this opens nothing on any
    /// host; the sheet already up keeps its own continuation.
    #[test]
    fn a_second_picker_is_refused_while_a_sheet_is_up() {
        let mut app = test_app();
        let (answer, picked) = sheet();
        let got = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&got);
        app.open_deferred_pick(
            picked,
            Box::new(move |_, _| sink.borrow_mut().push("first")),
        );

        let sink = Rc::clone(&got);
        app.pick_path(PickRequest::file("Load CD image"), move |_, _| {
            sink.borrow_mut().push("second")
        });
        assert!(got.borrow().is_empty());

        *answer.borrow_mut() = Some(None);
        app.poll_native_pick();
        assert_eq!(*got.borrow(), vec!["first"]);
    }

    #[test]
    fn a_request_carries_what_it_was_given() {
        let request = PickRequest::save("Create disk image")
            .filter("Amiga hard disk image", &["hdf", "img"])
            .directory(Some(PathBuf::from("/tmp")))
            .file_name("new.hdf");
        assert_eq!(request.title, "Create disk image");
        assert_eq!(
            request.filters,
            vec![(
                "Amiga hard disk image".to_string(),
                vec!["hdf".to_string(), "img".to_string()]
            )]
        );
        assert_eq!(request.directory, Some(PathBuf::from("/tmp")));
        assert_eq!(request.file_name.as_deref(), Some("new.hdf"));
    }
}
