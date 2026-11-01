//! File drops on Wayland. winit only implements drag and drop on X11, Windows and macOS, so
//! this listens to the seat's data device itself, on its own queue over the window's display
//! connection, which is how Wayland clipboard crates work too.

use eframe::egui;
use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use std::{
    ffi::OsStr,
    io::Read,
    os::{fd::AsFd, unix::ffi::OsStrExt},
    path::PathBuf,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
use wayland_backend::client::Backend;
use wayland_client::{
    Connection, Dispatch, QueueHandle, event_created_child,
    globals::{GlobalList, GlobalListContents, registry_queue_init},
    protocol::{
        wl_data_device::{self, WlDataDevice},
        wl_data_device_manager::{DndAction, WlDataDeviceManager},
        wl_data_offer::{self, WlDataOffer},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
    },
};

const URI_LIST: &str = "text/uri-list";

/// Where drops land. It outlives a window, because the listener stays with the connection.
#[derive(Default)]
pub struct DropInbox {
    paths: Mutex<Vec<PathBuf>>,
    hovering: AtomicBool,
    listening: AtomicBool,
    waker: Mutex<Option<egui::Context>>,
}

impl DropInbox {
    pub fn set_waker(&self, ctx: &egui::Context) {
        *self.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(ctx.clone());
    }

    pub fn take(&self) -> Vec<PathBuf> {
        std::mem::take(&mut self.paths.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Whether a drag is over the window right now.
    pub fn hovering(&self) -> bool {
        self.hovering.load(Ordering::SeqCst)
    }

    fn wake(&self) {
        if let Some(ctx) = self.waker.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            ctx.request_repaint();
        }
    }

    fn deliver(&self, paths: Vec<PathBuf>) {
        self.paths
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(paths);
        self.wake();
    }

    fn set_hovering(&self, hovering: bool) {
        self.hovering.store(hovering, Ordering::SeqCst);
        self.wake();
    }
}

struct Listener {
    inbox: &'static DropInbox,
    offer: Option<WlDataOffer>,
}

impl Dispatch<WlRegistry, GlobalListContents> for Listener {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for Listener {
    fn event(_: &mut Self, _: &WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlDataDeviceManager, ()> for Listener {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataOffer, ()> for Listener {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        _: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Listener {
    fn enter(&mut self, serial: u32, offer: Option<WlDataOffer>) {
        if let Some(offer) = &offer {
            offer.accept(serial, Some(URI_LIST.into()));
            offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Copy);
        }
        self.offer = offer;
        self.inbox.set_hovering(self.offer.is_some());
    }

    fn leave(&mut self) {
        if let Some(offer) = self.offer.take() {
            offer.destroy();
        }
        self.inbox.set_hovering(false);
    }

    fn receive(&mut self, connection: &Connection) {
        self.inbox.set_hovering(false);
        let Some(offer) = self.offer.take() else {
            return;
        };
        if let Some(text) = read_offer(&offer, connection) {
            self.inbox.deliver(parse_uri_list(&text));
        }
        offer.finish();
        offer.destroy();
    }
}

fn read_offer(offer: &WlDataOffer, connection: &Connection) -> Option<String> {
    let (mut reader, writer) = std::io::pipe().ok()?;
    offer.receive(URI_LIST.into(), writer.as_fd());
    drop(writer);
    connection.flush().ok()?;
    let mut text = String::new();
    reader.read_to_string(&mut text).ok()?;
    Some(text)
}

impl Dispatch<WlDataDevice, ()> for Listener {
    fn event(
        listener: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        connection: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::Enter { serial, id, .. } => listener.enter(serial, id),
            wl_data_device::Event::Leave => listener.leave(),
            wl_data_device::Event::Drop => listener.receive(connection),
            wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
            _ => {}
        }
    }

    event_created_child!(Listener, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

fn bind_devices(globals: &GlobalList, queue: &QueueHandle<Listener>) -> Option<WlDataDeviceManager> {
    let manager: WlDataDeviceManager = globals.bind(queue, 1..=3, ()).ok()?;
    let seats = globals.contents().clone_list();
    for seat in seats.iter().filter(|global| global.interface == "wl_seat") {
        let seat: WlSeat = globals.registry().bind(seat.name, seat.version.min(1), queue, ());
        manager.get_data_device(&seat, queue, ());
    }
    Some(manager)
}

/// Starts listening, once per process. Does nothing off Wayland, where winit handles drops.
pub fn listen(inbox: &'static DropInbox, window: &impl HasDisplayHandle) {
    let Ok(handle) = window.display_handle() else {
        return;
    };
    let RawDisplayHandle::Wayland(display) = handle.as_raw() else {
        return;
    };
    if inbox.listening.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: the display comes from the live event loop, which eframe keeps for the whole
    // process, and libwayland allows further queues on one connection from other threads.
    let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
    let connection = Connection::from_backend(backend);
    let Ok((globals, mut queue)) = registry_queue_init::<Listener>(&connection) else {
        return;
    };
    let Some(manager) = bind_devices(&globals, &queue.handle()) else {
        return;
    };
    let mut listener = Listener { inbox, offer: None };
    let spawned = thread::Builder::new()
        .name("unlight-drops".into())
        .spawn(move || {
            let _manager = manager;
            while queue.blocking_dispatch(&mut listener).is_ok() {}
        });
    if let Err(error) = spawned {
        eprintln!("couldn't listen for dropped files: {error}");
    }
}

fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape = (bytes[index] == b'%')
            .then(|| text.get(index + 1..index + 3))
            .flatten();
        match escape.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
            Some(byte) => {
                decoded.push(byte);
                index += 3;
            }
            None => {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
    }
    decoded
}

/// The local paths in a `text/uri-list`.
pub fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.strip_prefix("file://"))
        .filter_map(|rest| rest.find('/').map(|start| &rest[start..]))
        .map(|path| PathBuf::from(OsStr::from_bytes(&percent_decode(path))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_lists_become_paths() {
        let list = "# comment\r\nfile:///home/me/one.arw\r\nfile:///a%20b/two%C3%A9.nef\r\nhttps://example.com/x\r\n";
        let paths = parse_uri_list(list);
        assert_eq!(
            paths,
            [PathBuf::from("/home/me/one.arw"), PathBuf::from("/a b/twoé.nef")]
        );
    }

    #[test]
    fn a_hostname_in_the_uri_is_skipped() {
        assert_eq!(
            parse_uri_list("file://laptop/p/a.dng"),
            [PathBuf::from("/p/a.dng")]
        );
    }

    #[test]
    fn a_stray_percent_stays_literal() {
        assert_eq!(
            parse_uri_list("file:///100%/a.raf"),
            [PathBuf::from("/100%/a.raf")]
        );
    }
}
