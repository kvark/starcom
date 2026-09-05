//! File drops on Wayland. winit 0.30 never binds `wl_data_device`, so the
//! compositor shows a forbidden cursor and never delivers `DroppedFile`.
//!
//! We attach a guest connection to the same `wl_display` winit already owns,
//! bind the data-device globals onto our own event queue, and only
//! `dispatch_pending` after winit has read the socket.
//!
//! This is a 0.30 workaround, not a fork of winit. Master (the 0.31 line)
//! already merged a new `DataTransfer` API that binds the device on Wayland
//! ([rust-windowing/winit#4571](https://github.com/rust-windowing/winit/pull/4571),
//! closing #1881). egui-winit 0.34 still pins winit ^0.30.13, so we keep the
//! guest bind until that bump. Do not extract this into navigato-rs.

use std::{io, os::fd::AsFd, path};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_backend::sys::client::Backend;
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, event_created_child,
    protocol::{
        wl_data_device::{self, WlDataDevice},
        wl_data_device_manager::{self, DndAction, WlDataDeviceManager},
        wl_data_offer::{self, WlDataOffer},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_surface::WlSurface,
    },
};

const URI_LIST: &str = "text/uri-list";

pub struct Pump {
    pub hovering: bool,
    pub dropped: Vec<path::PathBuf>,
}

pub struct WaylandDrop {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
}

struct State {
    /// Protocol object id of our window's `wl_surface`, if we could read it.
    /// `None` accepts every surface rather than rejecting all drops.
    surface: Option<u32>,
    manager: Option<WlDataDeviceManager>,
    _registry: Option<WlRegistry>,
    _seats: Vec<WlSeat>,
    devices: Vec<WlDataDevice>,
    offer: Option<WlDataOffer>,
    serial: u32,
    uri_list: bool,
    hovering: bool,
    dropped: Vec<path::PathBuf>,
}

impl WaylandDrop {
    pub fn attach(window: &winit::window::Window) -> Option<Self> {
        let display = window.display_handle().ok()?;
        let RawDisplayHandle::Wayland(display) = display.as_raw() else {
            return None;
        };
        let window = window.window_handle().ok()?;
        let RawWindowHandle::Wayland(window) = window.as_raw() else {
            return None;
        };
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        let mut queue = conn.new_event_queue::<State>();
        let qh = queue.handle();
        let registry = conn.display().get_registry(&qh, ());
        let surface = unsafe {
            wayland_backend::sys::client::ObjectId::from_ptr(
                WlSurface::interface(),
                window.surface.as_ptr().cast(),
            )
        }
        .ok()
        .map(|id| id.protocol_id());
        let mut state = State {
            surface,
            manager: None,
            _registry: Some(registry),
            _seats: Vec::new(),
            devices: Vec::new(),
            offer: None,
            serial: 0,
            uri_list: false,
            hovering: false,
            dropped: Vec::new(),
        };
        if let Err(error) = queue.roundtrip(&mut state) {
            log::warn!("Wayland file-drop registry: {error}");
            return None;
        }
        if state.devices.is_empty() {
            log::warn!("Wayland file drops unavailable (no data device)");
            return None;
        }
        log::info!("Wayland file drops enabled");
        Some(Self { conn, queue, state })
    }

    pub fn pump(&mut self) -> Pump {
        let _ = self.conn.flush();
        let _ = self.queue.dispatch_pending(&mut self.state);
        Pump {
            hovering: self.state.hovering,
            dropped: std::mem::take(&mut self.state.dropped),
        }
    }
}

impl State {
    fn bind_seat(&mut self, seat: WlSeat, qh: &QueueHandle<Self>) {
        if let Some(ref manager) = self.manager {
            self.devices.push(manager.get_data_device(&seat, qh, ()));
        }
        self._seats.push(seat);
    }

    fn accept_if_ours(&mut self, surface: &WlSurface, serial: u32, offer: Option<WlDataOffer>) {
        let ours = self
            .surface
            .is_none_or(|id| surface.id().protocol_id() == id);
        self.serial = serial;
        self.hovering = ours && self.uri_list;
        if let Some(offer) = offer {
            if ours && self.uri_list {
                offer.accept(serial, Some(URI_LIST.to_owned()));
                if offer.version() >= 3 {
                    offer.set_actions(DndAction::Copy, DndAction::Copy);
                }
            } else {
                offer.accept(serial, None);
            }
            self.offer = Some(offer);
        }
        if !ours {
            self.hovering = false;
        }
    }

    fn take_uri_list(&mut self, conn: &Connection) {
        let Some(offer) = self.offer.take() else {
            return;
        };
        let Ok((mut reader, writer)) = io::pipe() else {
            return;
        };
        offer.receive(URI_LIST.to_owned(), writer.as_fd());
        drop(writer);
        if offer.version() >= 3 {
            offer.finish();
        }
        let _ = conn.flush();
        let mut text = String::new();
        if io::Read::read_to_string(&mut reader, &mut text).is_err() {
            return;
        }
        self.dropped = parse_uri_list(&text);
        self.hovering = false;
        self.uri_list = false;
    }
}

fn parse_uri_list(text: &str) -> Vec<path::PathBuf> {
    let mut files = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(path) = decode_file_uri(line) {
            files.push(path);
        }
    }
    files
}

fn decode_file_uri(uri: &str) -> Option<path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if let Some(tail) = rest.strip_prefix("localhost") {
        tail
    } else if rest.starts_with('/') {
        rest
    } else {
        rest.find('/').map(|index| &rest[index..])?
    };
    let mut bytes = Vec::with_capacity(path.len());
    let raw = path.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' && index + 2 < raw.len() {
            let hex = std::str::from_utf8(&raw[index + 1..index + 3]).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            bytes.push(raw[index]);
            index += 1;
        }
    }
    String::from_utf8(bytes)
        .ok()
        .filter(|path| path.starts_with('/'))
        .map(path::PathBuf::from)
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_data_device_manager" if state.manager.is_none() => {
                let manager =
                    registry.bind::<WlDataDeviceManager, _, _>(name, version.min(3), qh, ());
                for seat in &state._seats {
                    state.devices.push(manager.get_data_device(seat, qh, ()));
                }
                state.manager = Some(manager);
            }
            "wl_seat" => {
                let seat = registry.bind::<WlSeat, _, _>(name, version.min(7), qh, ());
                state.bind_seat(seat, qh);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: wl_data_device_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ())
    ]);

    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        conn: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => {
                state.uri_list = false;
                state.offer = Some(id);
            }
            wl_data_device::Event::Enter {
                serial,
                surface,
                id,
                ..
            } => state.accept_if_ours(&surface, serial, id),
            wl_data_device::Event::Leave => {
                state.hovering = false;
                state.offer = None;
                state.uri_list = false;
            }
            wl_data_device::Event::Drop => state.take_uri_list(conn),
            wl_data_device::Event::Motion { .. } | wl_data_device::Event::Selection { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(
        state: &mut Self,
        offer: &WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_offer::Event::Offer { mime_type } if mime_type == URI_LIST => {
                state.uri_list = true;
                if state.hovering || state.offer.is_some() {
                    offer.accept(state.serial, Some(URI_LIST.to_owned()));
                    if offer.version() >= 3 {
                        offer.set_actions(DndAction::Copy, DndAction::Copy);
                    }
                    state.hovering = true;
                }
            }
            wl_data_offer::Event::Offer { .. }
            | wl_data_offer::Event::SourceActions {
                source_actions: WEnum::Value(_),
            }
            | wl_data_offer::Event::Action { .. } => {}
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_decode_paths() {
        assert_eq!(
            decode_file_uri("file:///tmp/notes.txt").unwrap(),
            path::PathBuf::from("/tmp/notes.txt")
        );
        assert_eq!(
            decode_file_uri("file://localhost/tmp/a%20b.png").unwrap(),
            path::PathBuf::from("/tmp/a b.png")
        );
        assert!(decode_file_uri("https://example.test/x").is_none());
        assert_eq!(
            parse_uri_list("# comment\nfile:///tmp/a\n\nfile:///tmp/b\n").len(),
            2
        );
    }
}
