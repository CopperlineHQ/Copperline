//! The Wayland side of the data-device worker (see the parent module): a
//! thread with its own event queue on winit's `wl_display` and one
//! `wl_data_device` per seat, which serves both file drops onto the main
//! window and the clipboard egui copies to and pastes from.

use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use log::{debug, info, warn};
use smithay_client_toolkit as sctk;
use wayland_backend::client::{Backend, ObjectId};
use winit::raw_window_handle::{
    HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
};
use winit::window::Window;

use sctk::data_device_manager::data_device::{DataDevice, DataDeviceData, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::{CopyPasteSource, DataSourceHandler};
use sctk::data_device_manager::{DataDeviceManagerState, ReadPipe, WritePipe};
use sctk::reexports::calloop::{channel, EventLoop, LoopHandle, PostAction};
use sctk::reexports::calloop_wayland_source::WaylandSource;
use sctk::reexports::client::globals::registry_queue_init;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::wl_keyboard::{self, WlKeyboard};
use sctk::reexports::client::protocol::wl_pointer::{self, WlPointer};
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::{delegate_data_device, delegate_registry, delegate_seat, registry_handlers};

use super::uri_list::parse_uri_list;
use super::DropEvent;

/// The only drag type accepted: a list of URIs, one per line (RFC 2483).
const URI_LIST: &str = "text/uri-list";

/// Text clipboard types, in order of preference, as other toolkits offer
/// them; the same three are offered for text copied from Copperline.
const TEXT_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];

/// Upper bound on a dropped URI list. Thousands of paths fit; a source that
/// keeps writing past this is abandoned rather than buffered without end.
const MAX_URI_LIST_BYTES: usize = 4 << 20;

/// Upper bound on pasted clipboard text, for the same reason.
const MAX_CLIPBOARD_BYTES: usize = 16 << 20;

/// How long a paste waits for the application that owns the clipboard to
/// hand its text over before giving up.
const CLIPBOARD_LOAD_TIMEOUT: Duration = Duration::from_secs(2);

/// Requests from the main thread.
enum Command {
    /// Read the clipboard as text; the answer goes back on the sender.
    Load(mpsc::Sender<Option<String>>),
    /// Offer this text as the clipboard selection.
    Store(String),
    Exit,
}

/// Owns the worker thread and its connection to the main loop. Dropping it
/// stops the worker and waits for it, so it must be dropped while winit's
/// `wl_display` is still connected (`App::exiting`).
pub(in crate::video::window) struct WaylandData {
    events: mpsc::Receiver<DropEvent>,
    commands: channel::Sender<Command>,
    worker: Option<JoinHandle<()>>,
}

/// The main thread's handle on the worker's clipboard, for the egui states.
#[derive(Clone)]
pub(in crate::video::window) struct Clipboard {
    commands: channel::Sender<Command>,
}

impl Clipboard {
    /// The clipboard's text, or None when it holds none or its owner did
    /// not hand it over in time. Blocks while the owner writes it.
    pub(in crate::video::window) fn load(&self) -> Option<String> {
        let (reply, answer) = mpsc::channel();
        self.commands.send(Command::Load(reply)).ok()?;
        match answer.recv_timeout(CLIPBOARD_LOAD_TIMEOUT) {
            Ok(text) => text,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                warn!("clipboard: the clipboard's owner did not send its text in time");
                None
            }
            // The worker could not start the transfer (it logged why).
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Make `text` the clipboard selection.
    pub(in crate::video::window) fn store(&self, text: String) {
        let _ = self.commands.send(Command::Store(text));
    }
}

impl WaylandData {
    /// Start the worker for `window`. None when the window is not a native
    /// Wayland one (X11 and XWayland report drops through winit, and
    /// egui-winit keeps their clipboard) or the worker could not start.
    /// `wake` kicks the main loop out of `ControlFlow::Wait` once a drop
    /// event is queued.
    pub(in crate::video::window) fn start(
        window: &Window,
        wake: Box<dyn Fn() + Send>,
    ) -> Option<Self> {
        let display = match window.display_handle().ok()?.as_raw() {
            RawDisplayHandle::Wayland(handle) => handle.display,
            _ => return None,
        };
        let surface = match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Wayland(handle) => handle.surface,
            _ => return None,
        };
        // SAFETY: winit's display stays connected for the life of its event
        // loop, and this worker is dropped (joined) before the loop ends.
        // The wl_display is thread-safe, and every object made on this
        // connection lives on the worker's own event queue.
        let backend = unsafe { Backend::from_foreign_display(display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        // SAFETY: the surface pointer is winit's live wl_surface proxy, and
        // only its protocol id is read here.
        let surface_id =
            match unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.as_ptr().cast()) } {
                Ok(id) => id.protocol_id(),
                Err(_) => {
                    warn!("wayland data: the window's surface handle is not a wl_surface");
                    return None;
                }
            };
        let (events_tx, events) = mpsc::channel();
        let (commands, commands_rx) = channel::channel();
        let notify = Notify {
            tx: events_tx,
            wake,
        };
        let worker = std::thread::Builder::new()
            .name("wayland-data".into())
            .spawn(move || run_worker(connection, surface_id, notify, commands_rx))
            .map_err(|e| warn!("wayland data: could not start the worker: {e}"))
            .ok()?;
        info!("wayland data: serving file drops and the clipboard on Copperline's own data device");
        Some(Self {
            events,
            commands,
            worker: Some(worker),
        })
    }

    /// The drag events queued since the last call, oldest first.
    pub(in crate::video::window) fn take_events(&self) -> Vec<DropEvent> {
        self.events.try_iter().collect()
    }

    /// A handle on the clipboard this worker serves.
    pub(in crate::video::window) fn clipboard(&self) -> Clipboard {
        Clipboard {
            commands: self.commands.clone(),
        }
    }
}

impl Drop for WaylandData {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Exit);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The worker's half of the drop-event channel to the main loop.
struct Notify {
    tx: mpsc::Sender<DropEvent>,
    wake: Box<dyn Fn() + Send>,
}

impl Notify {
    fn send(&self, event: DropEvent) {
        if self.tx.send(event).is_ok() {
            (self.wake)();
        }
    }
}

fn run_worker(
    connection: Connection,
    surface_id: u32,
    notify: Notify,
    commands: channel::Channel<Command>,
) {
    let (globals, queue) = match registry_queue_init::<State>(&connection) {
        Ok(init) => init,
        Err(e) => {
            warn!("wayland data: registry unavailable: {e}");
            return;
        }
    };
    let qh = queue.handle();
    let data_device_manager = match DataDeviceManagerState::bind(&globals, &qh) {
        Ok(manager) => manager,
        Err(e) => {
            warn!("wayland data: the compositor offers no wl_data_device_manager: {e}");
            return;
        }
    };
    let mut event_loop = match EventLoop::<State>::try_new() {
        Ok(event_loop) => event_loop,
        Err(e) => {
            warn!("wayland data: could not create the worker's event loop: {e}");
            return;
        }
    };
    let handle = event_loop.handle();
    let mut state = State {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        data_device_manager,
        devices: Vec::new(),
        surface_id,
        active: None,
        last_input: None,
        copy: None,
        connection: connection.clone(),
        qh: qh.clone(),
        handle: handle.clone(),
        notify,
        running: true,
    };
    // SeatHandler::new_seat reports only seats announced later; the ones
    // present now were bound by SeatState::new.
    for seat in state.seats.seats() {
        state.add_seat(seat);
    }
    if let Err(e) = WaylandSource::new(connection, queue).insert(handle.clone()) {
        warn!("wayland data: could not watch the Wayland connection: {e}");
        return;
    }
    let inserted = handle.insert_source(commands, |event, _, state: &mut State| match event {
        channel::Event::Msg(Command::Load(reply)) => state.load_clipboard(reply),
        channel::Event::Msg(Command::Store(text)) => state.store_clipboard(text),
        channel::Event::Msg(Command::Exit) | channel::Event::Closed => state.running = false,
    });
    if inserted.is_err() {
        warn!("wayland data: could not install the worker's command channel");
        return;
    }
    while state.running {
        if let Err(e) = event_loop.dispatch(None, &mut state) {
            warn!("wayland data: worker stopped: {e}");
            break;
        }
    }
}

/// A seat's data device, and the input objects that give this connection
/// the event serials a clipboard selection is set with.
struct SeatDevice {
    seat: WlSeat,
    device: DataDevice,
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
}

/// Text this process offers as the clipboard selection, and the source
/// offering it. The source is destroyed with it.
struct OwnedSelection {
    source: CopyPasteSource,
    text: Arc<str>,
}

struct State {
    registry: RegistryState,
    seats: SeatState,
    data_device_manager: DataDeviceManagerState,
    /// One data device per seat. This is the process's only data device:
    /// egui-winit's clipboard is kept off native Wayland (see the parent
    /// module), as some compositors (Hyprland) send all drag and selection
    /// events to a client's first data device and none to the rest.
    devices: Vec<SeatDevice>,
    /// Protocol id of the main window's wl_surface; drags over any other
    /// surface (client-side decorations) are refused.
    surface_id: u32,
    /// The data device whose drag is over the main window and accepted.
    active: Option<WlDataDevice>,
    /// The seat and serial of the latest keyboard or pointer event this
    /// connection saw: what a selection is set with.
    last_input: Option<(WlSeat, u32)>,
    copy: Option<OwnedSelection>,
    connection: Connection,
    qh: QueueHandle<State>,
    handle: LoopHandle<'static, State>,
    notify: Notify,
    running: bool,
}

impl State {
    fn add_seat(&mut self, seat: WlSeat) {
        if !self.devices.iter().any(|known| known.seat == seat) {
            let device = self.data_device_manager.get_data_device(&self.qh, &seat);
            self.devices.push(SeatDevice {
                seat,
                device,
                keyboard: None,
                pointer: None,
            });
        }
    }

    /// The data device a clipboard request goes to: the seat with the most
    /// recent input, else the first one.
    fn clipboard_device(&self) -> Option<&SeatDevice> {
        let latest = self.last_input.as_ref().map(|(seat, _)| seat);
        self.devices
            .iter()
            .find(|known| Some(&known.seat) == latest)
            .or_else(|| self.devices.first())
    }

    fn load_clipboard(&mut self, reply: mpsc::Sender<Option<String>>) {
        if let Some(copy) = &self.copy {
            let _ = reply.send(Some(copy.text.to_string()));
            return;
        }
        let offer = self
            .clipboard_device()
            .and_then(|known| known.device.data().selection_offer());
        let Some(offer) = offer else {
            let _ = reply.send(None);
            return;
        };
        let mime = offer.with_mime_types(|types| {
            TEXT_TYPES
                .iter()
                .find(|wanted| types.iter().any(|t| t == *wanted))
                .map(|wanted| (*wanted).to_owned())
        });
        let Some(mime) = mime else {
            debug!("clipboard: the selection offers no text");
            let _ = reply.send(None);
            return;
        };
        let pipe = match offer.receive(mime.clone()) {
            Ok(pipe) => pipe,
            Err(e) => {
                warn!("clipboard: could not receive the selection: {e}");
                let _ = reply.send(None);
                return;
            }
        };
        self.read_pipe(pipe, MAX_CLIPBOARD_BYTES, move |content, _| {
            let text = content.map(|bytes| {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                if mime.starts_with("text/plain") {
                    text.replace("\r\n", "\n")
                } else {
                    text
                }
            });
            if let Some(text) = &text {
                debug!(
                    "clipboard: read {} bytes of {mime} from the selection",
                    text.len()
                );
            }
            let _ = reply.send(text);
        });
    }

    fn store_clipboard(&mut self, text: String) {
        let Some((seat, serial)) = self.last_input.clone() else {
            debug!("clipboard: no input has reached this window yet; copy not offered");
            return;
        };
        let Some(known) = self.devices.iter().find(|known| known.seat == seat) else {
            return;
        };
        let source = self
            .data_device_manager
            .create_copy_paste_source(&self.qh, TEXT_TYPES);
        source.set_selection(&known.device, serial);
        debug!("clipboard: offering {} bytes as the selection", text.len());
        // Replacing the previous selection drops (destroys) its source.
        self.copy = Some(OwnedSelection {
            source,
            text: text.into(),
        });
        let _ = self.connection.flush();
    }

    /// Read `pipe` to its end without blocking the worker, then call
    /// `done` with the bytes (None on an error or past `limit`). False when
    /// the read could not start; `done` is then dropped uncalled.
    fn read_pipe<F>(&mut self, pipe: ReadPipe, limit: usize, done: F) -> bool
    where
        F: FnOnce(Option<Vec<u8>>, &mut State) + 'static,
    {
        // The writing side starts once it sees the receive request.
        let _ = self.connection.flush();
        if let Err(e) = set_nonblocking(pipe.as_raw_fd()) {
            warn!("wayland data: could not read a transfer: {e}");
            return false;
        }
        let mut done = Some(done);
        let mut content = Vec::new();
        let mut buffer = [0u8; 4096];
        let inserted = self.handle.insert_source(pipe, move |_, file, state| {
            // SAFETY: the file is only read here, never closed or replaced.
            let file = unsafe { file.get_mut() };
            loop {
                let result = match file.read(&mut buffer) {
                    Ok(0) => Some(Some(std::mem::take(&mut content))),
                    Ok(n) if content.len() + n > limit => {
                        warn!("wayland data: a transfer is larger than {limit} bytes; ignored");
                        Some(None)
                    }
                    Ok(n) => {
                        content.extend_from_slice(&buffer[..n]);
                        None
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => return PostAction::Continue,
                    Err(e) if e.kind() == ErrorKind::Interrupted => None,
                    Err(e) => {
                        warn!("wayland data: reading a transfer failed: {e}");
                        Some(None)
                    }
                };
                if let Some(result) = result {
                    if let Some(done) = done.take() {
                        done(result, state);
                    }
                    return PostAction::Remove;
                }
            }
        });
        match inserted {
            Ok(_) => true,
            Err(e) => {
                warn!("wayland data: could not watch a transfer: {e}");
                false
            }
        }
    }

    /// Read the dropped URI list, then hand the paths to the main loop and
    /// finish the offer.
    fn receive_drop(&mut self, offer: DragOffer) {
        let pipe = match offer.receive(URI_LIST.to_owned()) {
            Ok(pipe) => pipe,
            Err(e) => {
                warn!("file drops: could not receive the dropped URI list: {e}");
                offer.destroy();
                self.notify.send(DropEvent::Cancelled);
                return;
            }
        };
        let finished = offer.clone();
        let reading = self.read_pipe(pipe, MAX_URI_LIST_BYTES, move |content, state| {
            let paths = content
                .map(|bytes| parse_uri_list(&bytes))
                .unwrap_or_default();
            debug!("file drops: {} path(s) dropped", paths.len());
            finished.finish();
            finished.destroy();
            state.notify.send(if paths.is_empty() {
                DropEvent::Cancelled
            } else {
                DropEvent::Dropped(paths)
            });
        });
        if !reading {
            offer.destroy();
            self.notify.send(DropEvent::Cancelled);
        }
    }

    /// Write the owned selection's text into a paste's pipe without
    /// blocking the worker.
    fn serve_selection(&mut self, text: Arc<str>, pipe: WritePipe) {
        if let Err(e) = set_nonblocking(pipe.as_raw_fd()) {
            warn!("clipboard: could not serve a paste: {e}");
            return;
        }
        let mut written = 0;
        let inserted = self.handle.insert_source(pipe, move |_, file, _| {
            // SAFETY: the file is only written here, never closed or replaced.
            let file = unsafe { file.get_mut() };
            loop {
                match file.write(&text.as_bytes()[written..]) {
                    Ok(n) => {
                        written += n;
                        if written >= text.len() || n == 0 {
                            debug!("clipboard: sent {written} bytes to a paste");
                            return PostAction::Remove;
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => return PostAction::Continue,
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(e) => {
                        debug!("clipboard: a paste stopped reading: {e}");
                        return PostAction::Remove;
                    }
                }
            }
        });
        if let Err(e) = inserted {
            warn!("clipboard: could not serve a paste: {e}");
        }
    }
}

/// The drag currently offered to `device`, if any.
fn drag_offer(device: &WlDataDevice) -> Option<DragOffer> {
    device
        .data::<DataDeviceData>()
        .and_then(|data| data.drag_offer())
}

impl DataDeviceHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        _x: f64,
        _y: f64,
        surface: &WlSurface,
    ) {
        let Some(offer) = drag_offer(device) else {
            return;
        };
        let offers_files = offer.with_mime_types(|types| types.iter().any(|t| t == URI_LIST));
        let on_window = surface.id().protocol_id() == self.surface_id;
        debug!(
            "file drops: drag entered surface {} (main window {}), offers {:?}",
            surface.id().protocol_id(),
            self.surface_id,
            offer.with_mime_types(<[String]>::to_vec)
        );
        if on_window && offers_files {
            offer.accept_mime_type(offer.serial, Some(URI_LIST.to_owned()));
            offer.set_actions(DndAction::Copy, DndAction::Copy);
            self.active = Some(device.clone());
            self.notify.send(DropEvent::Hovered);
        } else {
            offer.accept_mime_type(offer.serial, None);
            offer.set_actions(DndAction::empty(), DndAction::empty());
            if self.active.as_ref() == Some(device) {
                self.active = None;
                self.notify.send(DropEvent::Cancelled);
            }
        }
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        debug!("file drops: drag left");
        // A completed drop is followed by a leave too; receive_drop has
        // already taken that drag over, so only an abandoned one clears
        // the hint here.
        if self.active.as_ref() == Some(device) {
            self.active = None;
            self.notify.send(DropEvent::Cancelled);
        }
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        // Read only when something pastes (Clipboard::load).
    }

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        let Some(offer) = drag_offer(device) else {
            return;
        };
        debug!(
            "file drops: drop (accepted: {}, action {:?})",
            self.active.as_ref() == Some(device),
            offer.selected_action
        );
        if self.active.as_ref() != Some(device) {
            // Refused drags normally end without a drop; a compositor that
            // drops one anyway leaves the offer to the client to destroy.
            offer.destroy();
            return;
        }
        self.active = None;
        // Only copy was offered, so a version 3 compositor selects copy. A
        // drop with no action reported is still a drop the compositor let
        // through; any other action is not one this side performs.
        let action = offer.selected_action;
        if offer.inner().version() >= 3 && !action.is_empty() && action != DndAction::Copy {
            offer.destroy();
            self.notify.send(DropEvent::Cancelled);
            return;
        }
        self.receive_drop(offer);
    }
}

impl DataOfferHandler for State {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl DataSourceHandler for State {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        _mime: String,
        pipe: WritePipe,
    ) {
        // Every type offered is the same text.
        let text = match &self.copy {
            Some(copy) if copy.source.inner() == source => copy.text.clone(),
            _ => return,
        };
        self.serve_selection(text, pipe);
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        // Another client took the selection over.
        if self
            .copy
            .as_ref()
            .is_some_and(|copy| copy.source.inner() == source)
        {
            self.copy = None;
        }
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        self.add_seat(seat);
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        self.add_seat(seat.clone());
        let Some(known) = self.devices.iter_mut().find(|known| known.seat == seat) else {
            return;
        };
        match capability {
            Capability::Keyboard if known.keyboard.is_none() => {
                known.keyboard = Some(seat.get_keyboard(qh, seat.clone()));
            }
            Capability::Pointer if known.pointer.is_none() => {
                known.pointer = Some(seat.get_pointer(qh, seat.clone()));
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        let Some(known) = self.devices.iter_mut().find(|known| known.seat == seat) else {
            return;
        };
        match capability {
            Capability::Keyboard => {
                if let Some(keyboard) = known.keyboard.take() {
                    if keyboard.version() >= 3 {
                        keyboard.release();
                    }
                }
            }
            Capability::Pointer => {
                if let Some(pointer) = known.pointer.take() {
                    if pointer.version() >= 3 {
                        pointer.release();
                    }
                }
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        if let Some(i) = self.devices.iter().position(|known| known.seat == seat) {
            let known = self.devices.remove(i);
            if self.active.as_ref() == Some(known.device.inner()) {
                self.active = None;
                self.notify.send(DropEvent::Cancelled);
            }
            if self
                .last_input
                .as_ref()
                .is_some_and(|(latest, _)| *latest == seat)
            {
                self.last_input = None;
            }
        }
    }
}

// The keyboard and pointer are bound only for their event serials:
// wl_data_device.set_selection must name the serial of recent input that
// this connection received. Key and pointer contents are winit's to handle.
impl Dispatch<WlKeyboard, WlSeat> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        seat: &WlSeat,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { serial, .. } | wl_keyboard::Event::Key { serial, .. } => {
                state.last_input = Some((seat.clone(), serial));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, WlSeat> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        seat: &WlSeat,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { serial, .. } | wl_pointer::Event::Button { serial, .. } => {
                state.last_input = Some((seat.clone(), serial));
            }
            _ => {}
        }
    }
}

impl ProvidesRegistryState for State {
    registry_handlers![SeatState];

    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
}

delegate_registry!(State);
delegate_seat!(State);
delegate_data_device!(State);

fn set_nonblocking(fd: i32) -> std::io::Result<()> {
    // SAFETY: fcntl on a descriptor this thread owns, with no pointers.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
