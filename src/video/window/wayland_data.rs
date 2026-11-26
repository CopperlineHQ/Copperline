//! Copperline's own Wayland data device: file drops onto the main window,
//! and the clipboard egui copies to and pastes from, under native Wayland.
//!
//! winit 0.30 binds no `wl_data_device` on Wayland, so a file dragged onto
//! the window from a file manager produces no `HoveredFile`/`DroppedFile`
//! events there, while X11 and XWayland deliver them (issue #936). A worker
//! thread fills the gap: it opens a second event queue on the window's own
//! `wl_display` and creates a data device for every seat.
//!
//! That device must be the process's only one. Some compositors (Hyprland)
//! send every drag and clipboard-selection event to the first data device a
//! client creates and none to the rest, and egui-winit would otherwise start
//! smithay-clipboard, with a data device of its own, for every egui state.
//! So on native Wayland the egui states are created without the Wayland
//! display handle ([`EguiDisplay`]), which keeps smithay-clipboard off, and
//! [`EguiClipboard`] serves their copy and paste through this worker
//! instead. X11, XWayland, macOS and Windows keep egui-winit's own
//! clipboard; the rest of Copperline's clipboard (guest sharing, Paste as
//! Keystrokes) uses arboard, whose Wayland data-control protocol is a
//! separate one.
//!
//! A drag over the main window's surface that offers `text/uri-list` is
//! accepted with the copy action and nothing else: a file manager deletes
//! the source of a completed move. On drop the list is read from the offer's
//! pipe and its `file:` URIs are decoded to paths; each drop is handed to
//! the main loop as one batch, like a set of winit `DroppedFile` events.
//!
//! TODO: remove this module once winit 0.31's drag-and-drop events reach
//! Copperline through egui-winit, and give the egui states their own
//! clipboard back with it.

use std::path::PathBuf;

use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, RawDisplayHandle};
use winit::window::Window;

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
pub(super) use native::{Clipboard, WaylandData};
#[cfg(not(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
pub(super) use stub::{Clipboard, WaylandData};
#[cfg(unix)]
mod uri_list;

/// Whether this build can run the worker: the platforms where winit has a
/// Wayland backend.
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

/// Stand-ins on platforms without Wayland, where winit reports every drop
/// and egui-winit keeps the clipboard.
#[cfg(not(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
mod stub {
    use super::DropEvent;

    pub(in crate::video::window) enum WaylandData {}

    impl WaylandData {
        pub(in crate::video::window) fn start(
            _window: &winit::window::Window,
            _wake: Box<dyn Fn() + Send>,
        ) -> Option<Self> {
            None
        }

        pub(in crate::video::window) fn take_events(&self) -> Vec<DropEvent> {
            match *self {}
        }

        pub(in crate::video::window) fn clipboard(&self) -> Clipboard {
            match *self {}
        }
    }

    #[derive(Clone)]
    pub(in crate::video::window) enum Clipboard {}

    impl Clipboard {
        pub(in crate::video::window) fn load(&self) -> Option<String> {
            match *self {}
        }

        pub(in crate::video::window) fn store(&self, _text: String) {
            match *self {}
        }
    }
}

/// The display handle an egui state is created with. With the worker's
/// clipboard to serve it, the Wayland handle is withheld so egui-winit
/// starts no smithay-clipboard (and no second data device); egui-winit
/// uses the handle for nothing else.
pub(super) struct EguiDisplay<'a> {
    window: &'a Window,
    hide_wayland: bool,
}

impl<'a> EguiDisplay<'a> {
    pub(super) fn new(window: &'a Window, clipboard: Option<&Clipboard>) -> Self {
        Self {
            window,
            hide_wayland: clipboard.is_some(),
        }
    }
}

impl HasDisplayHandle for EguiDisplay<'_> {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        let handle = self.window.display_handle()?;
        if self.hide_wayland && matches!(handle.as_raw(), RawDisplayHandle::Wayland(_)) {
            return Err(HandleError::Unavailable);
        }
        Ok(handle)
    }
}

/// Serves one egui state's copy and paste from the worker's clipboard. With
/// no worker clipboard it does nothing and egui-winit's own clipboard runs.
#[derive(Default)]
pub(super) struct EguiClipboard {
    clipboard: Option<Clipboard>,
    modifiers: ModifiersState,
}

impl EguiClipboard {
    pub(super) fn new(clipboard: Option<Clipboard>) -> Self {
        Self {
            clipboard,
            modifiers: ModifiersState::empty(),
        }
    }

    /// Hand `event` to `input`. On native Wayland a paste takes its text
    /// from the worker; egui-winit's own paste, which there reads its
    /// in-process clipboard and not the system's, is dropped.
    pub(super) fn on_window_event(
        &mut self,
        input: &mut egui_winit::State,
        window: &Window,
        event: &WindowEvent,
    ) -> egui_winit::EventResponse {
        if let WindowEvent::ModifiersChanged(modifiers) = event {
            self.modifiers = modifiers.state();
        }
        let Some(clipboard) = &self.clipboard else {
            return input.on_window_event(window, event);
        };
        let queued = input.egui_input().events.len();
        let response = input.on_window_event(window, event);
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            let pasting = is_paste_shortcut(
                key.state,
                &key.logical_key,
                key.physical_key,
                self.modifiers,
            );
            let text = if pasting { clipboard.load() } else { None };
            replace_pastes(&mut input.egui_input_mut().events, queued, text);
        }
        response
    }

    /// Take the text egui copied (or cut) out of `output` and offer it as
    /// the system clipboard, before egui-winit would put it in its own.
    pub(super) fn take_copies(&self, output: &mut egui::PlatformOutput) {
        let Some(clipboard) = &self.clipboard else {
            return;
        };
        for text in take_copy_commands(&mut output.commands) {
            clipboard.store(text);
        }
    }
}

/// Whether a key event is egui's paste shortcut: the Paste key, or V with
/// the command modifier (Control; Command on macOS) and no other, as
/// egui-winit 0.37 has it. The key is the layout's character where it is an
/// ASCII one, and the physical key otherwise, as egui-winit maps it.
fn is_paste_shortcut(
    state: ElementState,
    logical: &Key,
    physical: PhysicalKey,
    modifiers: ModifiersState,
) -> bool {
    if state != ElementState::Pressed {
        return false;
    }
    if *logical == Key::Named(NamedKey::Paste) {
        return true;
    }
    let command = if cfg!(target_os = "macos") {
        ModifiersState::SUPER
    } else {
        ModifiersState::CONTROL
    };
    let is_v = match logical {
        Key::Character(text) if text.is_ascii() => text.eq_ignore_ascii_case("v"),
        _ => physical == PhysicalKey::Code(KeyCode::KeyV),
    };
    is_v && modifiers == command
}

/// Drop the paste events egui-winit queued from `from` on, and queue
/// `text` (with CRLF line ends folded, as egui-winit does) in their place.
fn replace_pastes(events: &mut Vec<egui::Event>, from: usize, text: Option<String>) {
    let from = from.min(events.len());
    let mut i = from;
    while i < events.len() {
        if matches!(events[i], egui::Event::Paste(_)) {
            events.remove(i);
        } else {
            i += 1;
        }
    }
    if let Some(text) = text.map(|text| text.replace("\r\n", "\n")) {
        if !text.is_empty() {
            events.push(egui::Event::Paste(text));
        }
    }
}

/// Remove egui's copy-text commands, returning their text in order.
fn take_copy_commands(commands: &mut Vec<egui::OutputCommand>) -> Vec<String> {
    let mut copied = Vec::new();
    commands.retain(|command| match command {
        egui::OutputCommand::CopyText(text) => {
            copied.push(text.clone());
            false
        }
        _ => true,
    });
    copied
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMAND: ModifiersState = if cfg!(target_os = "macos") {
        ModifiersState::SUPER
    } else {
        ModifiersState::CONTROL
    };

    fn v_key() -> (Key, PhysicalKey) {
        (Key::Character("v".into()), PhysicalKey::Code(KeyCode::KeyV))
    }

    #[test]
    fn paste_shortcut_is_command_v_or_the_paste_key() {
        let (logical, physical) = v_key();
        assert!(is_paste_shortcut(
            ElementState::Pressed,
            &logical,
            physical,
            COMMAND
        ));
        assert!(!is_paste_shortcut(
            ElementState::Released,
            &logical,
            physical,
            COMMAND
        ));
        assert!(!is_paste_shortcut(
            ElementState::Pressed,
            &logical,
            physical,
            ModifiersState::empty()
        ));
        // Copperline's Paste as Keystrokes chord adds Shift; it is not
        // egui's paste.
        assert!(!is_paste_shortcut(
            ElementState::Pressed,
            &logical,
            physical,
            COMMAND | ModifiersState::SHIFT
        ));
        assert!(is_paste_shortcut(
            ElementState::Pressed,
            &Key::Named(NamedKey::Paste),
            PhysicalKey::Code(KeyCode::Paste),
            ModifiersState::empty()
        ));
    }

    #[test]
    fn paste_shortcut_follows_the_layout_and_falls_back_to_the_key_position() {
        // A layout that puts V elsewhere: the character decides.
        assert!(!is_paste_shortcut(
            ElementState::Pressed,
            &Key::Character("q".into()),
            PhysicalKey::Code(KeyCode::KeyV),
            COMMAND
        ));
        assert!(is_paste_shortcut(
            ElementState::Pressed,
            &Key::Character("v".into()),
            PhysicalKey::Code(KeyCode::KeyQ),
            COMMAND
        ));
        // A non-Latin layout: the V position decides.
        assert!(is_paste_shortcut(
            ElementState::Pressed,
            &Key::Character("\u{043c}".into()),
            PhysicalKey::Code(KeyCode::KeyV),
            COMMAND
        ));
    }

    #[test]
    fn pastes_queued_by_egui_winit_are_replaced_by_the_system_text() {
        let mut events = vec![
            egui::Event::Text("before".into()),
            egui::Event::Paste("stale".into()),
        ];
        // Only events queued for this key (from index 1) are touched.
        replace_pastes(&mut events, 1, Some("line 1\r\nline 2".into()));
        assert_eq!(
            events,
            vec![
                egui::Event::Text("before".into()),
                egui::Event::Paste("line 1\nline 2".into()),
            ]
        );
        let mut events = vec![egui::Event::Paste("stale".into())];
        replace_pastes(&mut events, 0, None);
        assert!(
            events.is_empty(),
            "no system text: egui-winit's paste still goes"
        );
        let mut events = vec![egui::Event::Paste("earlier".into())];
        replace_pastes(&mut events, 1, Some(String::new()));
        assert_eq!(events, vec![egui::Event::Paste("earlier".into())]);
    }

    #[test]
    fn copy_commands_are_taken_and_others_kept() {
        let mut commands = vec![
            egui::OutputCommand::CopyText("one".into()),
            egui::OutputCommand::OpenUrl(egui::OpenUrl::same_tab("https://example.com")),
            egui::OutputCommand::CopyText("two".into()),
        ];
        assert_eq!(take_copy_commands(&mut commands), ["one", "two"]);
        assert_eq!(commands.len(), 1);
        assert!(matches!(commands[0], egui::OutputCommand::OpenUrl(_)));
    }
}
