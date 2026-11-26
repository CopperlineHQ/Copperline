//! File drops onto the main window under native Wayland.
//!
//! winit 0.30 binds no `wl_data_device` on Wayland, so a file dragged onto
//! the window from a file manager produces no `HoveredFile`/`DroppedFile`
//! events there, while X11 and XWayland deliver them (issue #936). This
//! receiver fills the gap the way egui-winit's Wayland clipboard
//! (smithay-clipboard) already reaches the compositor: a worker thread opens
//! a second event queue on the window's own `wl_display`, binds
//! `wl_data_device_manager`, and creates a data device for every seat. The
//! compositor sends drag-and-drop events to each data device the client
//! holds, so this one sees every drag that crosses the window.
//!
//! A drag over the main window's surface that offers `text/uri-list` is
//! accepted with the copy action and nothing else: a file manager deletes
//! the source of a completed move. On drop the list is read from the offer's
//! pipe and its `file:` URIs are decoded to paths, which the main loop
//! handles exactly as it handles winit's `DroppedFile` events (one coalesced
//! batch per drop, see `about_to_wait`).
//!
//! TODO: remove this module once winit 0.31's drag-and-drop events reach
//! Copperline through egui-winit; they report the same drops natively, with a
//! cursor position on every platform.

use std::path::PathBuf;

#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
mod native;
#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub(super) use native::WaylandDrop;
#[cfg(unix)]
mod uri_list;

/// Whether this build can receive native Wayland drops: the platforms where
/// winit has a Wayland backend.
pub(super) const SUPPORTED: bool = cfg!(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
));

/// What a drag over the main window did, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )),
    allow(dead_code)
)]
pub(super) enum DropEvent {
    /// An acceptable drag is over the window: show the drop hint.
    Hovered,
    /// The drag left, was refused or failed: clear the hint.
    Cancelled,
    /// The drag was dropped and delivered these local files.
    Dropped(Vec<PathBuf>),
}

/// Stand-in on platforms without Wayland, where winit reports every drop.
#[cfg(not(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
pub(super) enum WaylandDrop {}

#[cfg(not(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
impl WaylandDrop {
    pub(super) fn start(
        _window: &winit::window::Window,
        _wake: Box<dyn Fn() + Send>,
    ) -> Option<Self> {
        None
    }

    pub(super) fn take_events(&self) -> Vec<DropEvent> {
        match *self {}
    }
}
