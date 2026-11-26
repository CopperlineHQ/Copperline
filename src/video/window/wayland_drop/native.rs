//! The Wayland side of the file-drop receiver (see the parent module): a
//! worker thread with its own event queue on winit's `wl_display`, one data
//! device per seat, and the drag-and-drop handshake for the main window.

use std::io::{ErrorKind, Read};
use std::os::fd::AsRawFd;
use std::sync::mpsc;
use std::thread::JoinHandle;

use log::{debug, info, warn};
use smithay_client_toolkit as sctk;
use wayland_backend::client::{Backend, ObjectId};
use winit::raw_window_handle::{
    HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
};
use winit::window::Window;

use sctk::data_device_manager::data_device::{DataDevice, DataDeviceData, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::DataSourceHandler;
use sctk::data_device_manager::{DataDeviceManagerState, WritePipe};
use sctk::reexports::calloop::{channel, EventLoop, LoopHandle, PostAction};
use sctk::reexports::calloop_wayland_source::WaylandSource;
use sctk::reexports::client::globals::registry_queue_init;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::{delegate_data_device, delegate_registry, delegate_seat, registry_handlers};

use super::uri_list::parse_uri_list;
use super::DropEvent;

/// The only drag type accepted: a list of URIs, one per line (RFC 2483).
const URI_LIST: &str = "text/uri-list";

/// Upper bound on a dropped URI list. Thousands of paths fit; a source that
/// keeps writing past this is abandoned rather than buffered without end.
const MAX_URI_LIST_BYTES: usize = 4 << 20;

/// Owns the worker thread and its connection to the main loop. Dropping it
/// stops the worker and waits for it, so it must be dropped while winit's
/// `wl_display` is still connected (`App::exiting`).
pub(in crate::video::window) struct WaylandDrop {
    events: mpsc::Receiver<DropEvent>,
    exit: channel::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl WaylandDrop {
    /// Start receiving drops for `window`. None when the window is not a
    /// native Wayland one (X11 and XWayland report drops through winit) or
    /// the worker could not start. `wake` kicks the main loop out of
    /// `ControlFlow::Wait` once an event is queued.
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
        // loop, and this receiver is dropped (its worker joined) before the
        // loop ends. The wl_display is thread-safe, and every object made on
        // this connection lives on the worker's own event queue.
        let backend = unsafe { Backend::from_foreign_display(display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        // SAFETY: the surface pointer is winit's live wl_surface proxy, and
        // only its protocol id is read here.
        let surface_id =
            match unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.as_ptr().cast()) } {
                Ok(id) => id.protocol_id(),
                Err(_) => {
                    warn!("file drops: the window's Wayland surface handle is not a wl_surface");
                    return None;
                }
            };
        let (events_tx, events) = mpsc::channel();
        let (exit, exit_rx) = channel::channel();
        let notify = Notify {
            tx: events_tx,
            wake,
        };
        let worker = std::thread::Builder::new()
            .name("wayland-drop".into())
            .spawn(move || run_worker(connection, surface_id, notify, exit_rx))
            .map_err(|e| warn!("file drops: could not start the Wayland worker: {e}"))
            .ok()?;
        info!("file drops: receiving native Wayland drags");
        Some(Self {
            events,
            exit,
            worker: Some(worker),
        })
    }

    /// The drag events queued since the last call, oldest first.
    pub(in crate::video::window) fn take_events(&self) -> Vec<DropEvent> {
        self.events.try_iter().collect()
    }
}

impl Drop for WaylandDrop {
    fn drop(&mut self) {
        let _ = self.exit.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The worker's half of the channel to the main loop.
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

fn run_worker(connection: Connection, surface_id: u32, notify: Notify, exit: channel::Channel<()>) {
    let (globals, queue) = match registry_queue_init::<State>(&connection) {
        Ok(init) => init,
        Err(e) => {
            warn!("file drops: Wayland registry unavailable: {e}");
            return;
        }
    };
    let qh = queue.handle();
    let data_device_manager = match DataDeviceManagerState::bind(&globals, &qh) {
        Ok(manager) => manager,
        Err(e) => {
            warn!("file drops: the compositor offers no wl_data_device_manager: {e}");
            return;
        }
    };
    let mut event_loop = match EventLoop::<State>::try_new() {
        Ok(event_loop) => event_loop,
        Err(e) => {
            warn!("file drops: could not create the worker's event loop: {e}");
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
        handle: handle.clone(),
        notify,
        running: true,
    };
    // SeatHandler::new_seat reports only seats announced later; the ones
    // present now were bound by SeatState::new.
    for seat in state.seats.seats() {
        state.add_seat(&qh, seat);
    }
    if let Err(e) = WaylandSource::new(connection, queue).insert(handle.clone()) {
        warn!("file drops: could not watch the Wayland connection: {e}");
        return;
    }
    if handle
        .insert_source(exit, |_, _, state: &mut State| state.running = false)
        .is_err()
    {
        warn!("file drops: could not install the worker's exit channel");
        return;
    }
    while state.running {
        if let Err(e) = event_loop.dispatch(None, &mut state) {
            warn!("file drops: Wayland worker stopped: {e}");
            break;
        }
    }
}

struct State {
    registry: RegistryState,
    seats: SeatState,
    data_device_manager: DataDeviceManagerState,
    /// One data device per seat, released with the seat.
    devices: Vec<(WlSeat, DataDevice)>,
    /// Protocol id of the main window's wl_surface; drags over any other
    /// surface (tool windows, client-side decorations) are refused.
    surface_id: u32,
    /// The data device whose drag is over the main window and accepted.
    active: Option<WlDataDevice>,
    handle: LoopHandle<'static, State>,
    notify: Notify,
    running: bool,
}

impl State {
    fn add_seat(&mut self, qh: &QueueHandle<Self>, seat: WlSeat) {
        if !self.devices.iter().any(|(known, _)| *known == seat) {
            let device = self.data_device_manager.get_data_device(qh, &seat);
            self.devices.push((seat, device));
        }
    }

    /// Read the dropped URI list without blocking the worker, then hand the
    /// paths to the main loop and finish the offer.
    fn receive_drop(&mut self, connection: &Connection, offer: DragOffer) {
        let pipe = match offer.receive(URI_LIST.to_owned()) {
            Ok(pipe) => pipe,
            Err(e) => {
                warn!("file drops: could not receive the dropped URI list: {e}");
                offer.destroy();
                self.notify.send(DropEvent::Cancelled);
                return;
            }
        };
        // The source writes only once it sees the receive request.
        let _ = connection.flush();
        if let Err(e) = set_nonblocking(pipe.as_raw_fd()) {
            warn!("file drops: could not read the dropped URI list: {e}");
            offer.destroy();
            self.notify.send(DropEvent::Cancelled);
            return;
        }
        let mut content = Vec::new();
        let mut buffer = [0u8; 4096];
        let inserted = self.handle.insert_source(pipe, move |_, file, state| {
            // SAFETY: the file is only read here, never closed or replaced.
            let file = unsafe { file.get_mut() };
            loop {
                match file.read(&mut buffer) {
                    Ok(0) => {
                        let paths = parse_uri_list(&content);
                        debug!("file drops: {} path(s) dropped", paths.len());
                        offer.finish();
                        offer.destroy();
                        state.notify.send(if paths.is_empty() {
                            DropEvent::Cancelled
                        } else {
                            DropEvent::Dropped(paths)
                        });
                        return PostAction::Remove;
                    }
                    Ok(n) if content.len() + n > MAX_URI_LIST_BYTES => {
                        warn!("file drops: the dropped URI list is larger than {MAX_URI_LIST_BYTES} bytes; ignored");
                        offer.destroy();
                        state.notify.send(DropEvent::Cancelled);
                        return PostAction::Remove;
                    }
                    Ok(n) => content.extend_from_slice(&buffer[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => return PostAction::Continue,
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(e) => {
                        warn!("file drops: reading the dropped URI list failed: {e}");
                        offer.destroy();
                        state.notify.send(DropEvent::Cancelled);
                        return PostAction::Remove;
                    }
                }
            }
        });
        if let Err(e) = inserted {
            warn!("file drops: could not watch the dropped URI list: {e}");
            self.notify.send(DropEvent::Cancelled);
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

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(
        &mut self,
        connection: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
    ) {
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
        self.receive_drop(connection, offer);
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

// This side never offers data, but the data device delegate requires a
// source handler.
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
        _: &WlDataSource,
        _: String,
        _: WritePipe,
    ) {
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat) {
        self.add_seat(qh, seat);
    }

    fn new_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        _: Capability,
    ) {
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        if let Some(i) = self.devices.iter().position(|(s, _)| *s == seat) {
            let (_, device) = self.devices.remove(i);
            if self.active.as_ref() == Some(device.inner()) {
                self.active = None;
                self.notify.send(DropEvent::Cancelled);
            }
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
