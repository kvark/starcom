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

use std::{io, os::fd::AsFd, path, sync, thread, time};

use anyhow::Context;
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
const MAX_URI_LIST_BYTES: usize = 64 * 1024;
const MAX_DROPPED_FILES: usize = 8;
const DROP_READ_TIMEOUT: time::Duration = time::Duration::from_secs(5);
type Wake = sync::Arc<dyn Fn() + Send + Sync>;

pub struct Pump {
    pub hovering: bool,
    pub dropped: Vec<path::PathBuf>,
    pub error: Option<String>,
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
    pending: Option<sync::mpsc::Receiver<Result<Vec<path::PathBuf>, String>>>,
    error: Option<String>,
    wake: Wake,
}

impl WaylandDrop {
    pub fn attach(window: &winit::window::Window, wake: Wake) -> Option<Self> {
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
            pending: None,
            error: None,
            wake,
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
        self.state.finish_pending();
        Pump {
            hovering: self.state.hovering,
            dropped: std::mem::take(&mut self.state.dropped),
            error: self.state.error.take(),
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
            self.error = Some("could not create the file-drop pipe".to_owned());
            return;
        };
        offer.receive(URI_LIST.to_owned(), writer.as_fd());
        drop(writer);
        if offer.version() >= 3 {
            offer.finish();
        }
        let _ = conn.flush();
        self.hovering = false;
        self.uri_list = false;
        if self.pending.is_some() {
            self.error = Some("another Wayland file drop is still being read".to_owned());
            return;
        }
        let (tx, rx) = sync::mpsc::sync_channel(1);
        let wake = sync::Arc::clone(&self.wake);
        match thread::Builder::new()
            .name("starcom-wayland-drop".to_owned())
            .spawn(move || {
                let result = read_uri_list(&mut reader).map_err(|error| error.to_string());
                let _ = tx.send(result);
                wake();
            }) {
            Ok(_) => self.pending = Some(rx),
            Err(error) => self.error = Some(format!("could not read the file drop: {error}")),
        }
    }

    fn finish_pending(&mut self) {
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        match pending.try_recv() {
            Ok(Ok(files)) => {
                self.dropped = files;
                self.pending = None;
            }
            Ok(Err(error)) => {
                self.error = Some(error.chars().take(512).collect());
                self.pending = None;
            }
            Err(sync::mpsc::TryRecvError::Empty) => {}
            Err(sync::mpsc::TryRecvError::Disconnected) => {
                self.error = Some("Wayland file drop ended without a result".to_owned());
                self.pending = None;
            }
        }
    }
}

fn read_uri_list(reader: &mut io::PipeReader) -> anyhow::Result<Vec<path::PathBuf>> {
    let poller = polling::Poller::new()?;
    // SAFETY: this function owns the reader and deregisters it below before
    // either the poller or reader can be dropped.
    unsafe { poller.add(&*reader, polling::Event::readable(0)) }?;
    let result = (|| {
        let deadline = time::Instant::now() + DROP_READ_TIMEOUT;
        let mut events = polling::Events::new();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let remaining = deadline
                .checked_duration_since(time::Instant::now())
                .filter(|duration| !duration.is_zero())
                .ok_or_else(|| anyhow::anyhow!("Wayland file drop timed out"))?;
            poller.modify(&*reader, polling::Event::readable(0))?;
            events.clear();
            match poller.wait(&mut events, Some(remaining)) {
                Ok(0) => anyhow::bail!("Wayland file drop timed out"),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
            match io::Read::read(reader, &mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    anyhow::ensure!(
                        bytes.len() + count <= MAX_URI_LIST_BYTES,
                        "Wayland file drop exceeds {MAX_URI_LIST_BYTES} bytes"
                    );
                    bytes.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let text = String::from_utf8(bytes).context("Wayland file drop is not UTF-8")?;
        parse_uri_list(&text)
    })();
    let _ = poller.delete(&*reader);
    result
}

fn parse_uri_list(text: &str) -> anyhow::Result<Vec<path::PathBuf>> {
    let mut files = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(path) = decode_file_uri(line) {
            anyhow::ensure!(
                files.len() < MAX_DROPPED_FILES,
                "drop at most {MAX_DROPPED_FILES} files at a time"
            );
            files.push(path);
        }
    }
    anyhow::ensure!(!files.is_empty(), "file drop contains no local files");
    Ok(files)
}

fn decode_file_uri(uri: &str) -> Option<path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if rest.starts_with('/') {
        rest
    } else {
        let slash = rest.find('/')?;
        let authority = &rest[..slash];
        if !authority.eq_ignore_ascii_case("localhost") {
            return None;
        }
        &rest[slash..]
    };
    let mut bytes = Vec::with_capacity(path.len());
    let raw = path.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' {
            if index + 2 >= raw.len() {
                return None;
            }
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
        .filter(|path| path.starts_with('/') && !path.contains('\0'))
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
        assert!(decode_file_uri("file://other-host/tmp/private").is_none());
        assert!(decode_file_uri("file:///tmp/bad%").is_none());
        assert!(decode_file_uri("https://example.test/x").is_none());
        assert!(parse_uri_list("https://example.test/x\n").is_err());
        assert_eq!(
            parse_uri_list("# comment\nfile:///tmp/a\n\nfile:///tmp/b\n")
                .unwrap()
                .len(),
            2
        );
        let too_many = "file:///tmp/a\n".repeat(MAX_DROPPED_FILES + 1);
        assert!(parse_uri_list(&too_many).is_err());
    }

    #[test]
    fn uri_list_pipe_is_read_to_eof() {
        let (mut reader, mut writer) = io::pipe().unwrap();
        let sender = thread::spawn(move || {
            io::Write::write_all(&mut writer, b"file:///tmp/a%20b\n").unwrap();
        });
        assert_eq!(
            read_uri_list(&mut reader).unwrap(),
            vec![path::PathBuf::from("/tmp/a b")]
        );
        sender.join().unwrap();
    }
}
