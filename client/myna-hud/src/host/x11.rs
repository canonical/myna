//! The X11 status surface host (`myna-hud --host x11`): the HUD types,
//! protects and places its own window, as `myna-shell` does on GNOME.
//!
//! GTK4 has no API for any of it, so this works on the window's XID over an
//! x11rb connection of its own:
//!
//! - `_NET_WM_WINDOW_TYPE_NOTIFICATION`: xfwm4 never focuses it on map (not
//!   `WINDOW_REGULAR_FOCUSABLE`) and stacks it in its notification layer,
//!   above panels and normal windows. DOCK would match mutter but xfwm4
//!   draws a dock shadow around the transparent window; UTILITY is focused
//!   on map.
//! - `_NET_WM_USER_TIME` 0 and no `WM_TAKE_FOCUS`, so focus-stealing
//!   prevention and focus fallback skip it too.
//! - Position chosen before every map (`WM_NORMAL_HINTS` `PPosition`), so
//!   the window manager maps it in place instead of placing it. GDK rewrites
//!   those hints right after mapping, racing the manager's read, so the
//!   position is checked again once the window is mapped.
//! - After every map, input refused in `WM_HINTS` and sticky requested:
//!   GDK rewrites `WM_HINTS` and `_NET_WM_STATE` each time it maps, so
//!   neither can be set ahead. Skip-taskbar/pager are GDK's own hints
//!   ([`crate::window`]), click-through is the window's empty input region,
//!   set again once the window manager has framed the window.
//!
//! Placement is `myna_platform::status_surface`'s: bottom-centre of the work
//! area, the monitor pinned at map (focus, then pointer, then primary) and
//! re-chosen when monitors change, the work area the monitor less panel
//! struts and clear of strut-less bottom docks, followed on `_NET_WORKAREA`
//! changes and the window's own resizes.

use std::cell::Cell;
use std::error::Error;
use std::fmt;
use std::os::fd::AsRawFd;
use std::rc::{Rc, Weak};

use gdk4_x11 as gdkx11;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use x11rb::connection::Connection;
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ClipOrdering, ConfigureWindowAux,
    ConnectionExt as _, EventMask, GetPropertyReply, PropMode, Window,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use myna_platform::status_surface::{
    choose_monitor, placement, work_area, Point, Rect, Size, Strut, BOTTOM_MARGIN,
};

use super::{ewmh, Host};
use crate::window::HudWindow;

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        WM_PROTOCOLS,
        WM_TAKE_FOCUS,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_NOTIFICATION,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_STATE,
        _NET_WM_STATE_STICKY,
        _NET_WORKAREA,
        _NET_CLIENT_LIST,
        _NET_ACTIVE_WINDOW,
        _NET_WM_STRUT,
        _NET_WM_STRUT_PARTIAL,
    }
}

type XResult<T> = Result<T, Box<dyn Error>>;

/// `_NET_WM_STATE` client message action.
const STATE_ADD: u32 = 1;
/// EWMH source indication: a normal application.
const SOURCE_APPLICATION: u32 = 1;

/// Why the host cannot run.
#[derive(Debug)]
pub enum HostError {
    /// GDK is not on an X11 display.
    NotX11,
    /// The X server refused a connection or a request.
    X(Box<dyn Error>),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotX11 => f.write_str("the display is not X11"),
            Self::X(e) => write!(f, "X11: {e}"),
        }
    }
}

impl<E: Error + 'static> From<E> for HostError {
    fn from(e: E) -> Self {
        Self::X(Box::new(e))
    }
}

/// The host. Lives as long as the HUD window, which owns it.
pub struct X11Host {
    conn: RustConnection,
    atoms: Atoms,
    root: Window,
    xid: Window,
    window: gtk::ApplicationWindow,
    display: gdk::Display,
    /// The monitor the pill is pinned to while mapped.
    monitor: Cell<Option<usize>>,
}

impl X11Host {
    /// Realize `hud`'s window and take it over.
    pub fn install(hud: &Rc<HudWindow>) -> Result<Rc<Self>, HostError> {
        let window = hud.window().clone();
        let display = WidgetExt::display(&window);
        if !display.is::<gdkx11::X11Display>() {
            return Err(HostError::NotX11);
        }
        WidgetExt::realize(&window);
        let surface = window.surface().ok_or(HostError::NotX11)?;
        let x11 = surface
            .downcast_ref::<gdkx11::X11Surface>()
            .ok_or(HostError::NotX11)?;
        // Before the first map, and GDK keeps it: it only ever raises it from
        // input events, which an input-less window never gets.
        x11.set_user_time(0);
        let xid = x11.xid() as Window;

        let (conn, screen) = x11rb::connect(Some(display.name().as_str()))?;
        let root = conn.setup().roots[screen].root;
        let atoms = Atoms::new(&conn)?.reply()?;
        let host = Rc::new(Self {
            conn,
            atoms,
            root,
            xid,
            window,
            display,
            monitor: Cell::new(None),
        });
        host.claim().map_err(HostError::X)?;
        host.watch(&surface);
        hud.set_host(host.clone());
        Ok(host)
    }

    /// The properties GDK sets once and never again.
    fn claim(&self) -> XResult<()> {
        let a = &self.atoms;
        self.conn.change_property32(
            PropMode::REPLACE,
            self.xid,
            a._NET_WM_WINDOW_TYPE,
            AtomEnum::ATOM,
            &[a._NET_WM_WINDOW_TYPE_NOTIFICATION],
        )?;
        let protocols = self.cardinals(self.xid, a.WM_PROTOCOLS, AtomEnum::ATOM.into())?;
        self.conn.change_property32(
            PropMode::REPLACE,
            self.xid,
            a.WM_PROTOCOLS,
            AtomEnum::ATOM,
            &ewmh::without_take_focus(&protocols, a.WM_TAKE_FOCUS),
        )?;
        self.conn.change_window_attributes(
            self.root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        self.conn.change_window_attributes(
            self.xid,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
        )?;
        self.conn.sync()?;
        Ok(())
    }

    fn watch(self: &Rc<Self>, surface: &gdk::Surface) {
        let weak = Rc::downgrade(self);
        self.window.connect_map(move |_| {
            if let Some(host) = weak.upgrade() {
                host.report(host.after_map());
            }
        });

        let weak = Rc::downgrade(self);
        surface.connect_layout(move |_, _, _| {
            if let Some(host) = weak.upgrade() {
                if host.window.is_mapped() {
                    host.report(host.place_mapped());
                }
            }
        });

        let weak = Rc::downgrade(self);
        self.display
            .monitors()
            .connect_items_changed(move |_, _, _, _| {
                if let Some(host) = weak.upgrade() {
                    host.monitor.set(None);
                    if host.window.is_mapped() {
                        host.report(host.place_mapped());
                    }
                }
            });

        let weak: Weak<Self> = Rc::downgrade(self);
        let fd = self.conn.stream().as_raw_fd();
        glib::source::unix_fd_add_local(
            fd,
            glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR,
            move |_, condition| {
                let Some(host) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if condition.intersects(glib::IOCondition::HUP | glib::IOCondition::ERR) {
                    eprintln!("myna-hud: x11 host: lost the X connection");
                    return glib::ControlFlow::Break;
                }
                match host.pump() {
                    Ok(()) => glib::ControlFlow::Continue,
                    Err(e) => {
                        eprintln!("myna-hud: x11 host: {e}");
                        glib::ControlFlow::Break
                    }
                }
            },
        );
    }

    /// Log a failed step; the HUD keeps rendering wherever the window is.
    fn report(&self, result: XResult<()>) {
        if let Err(e) = result.and_then(|()| self.pump()) {
            eprintln!("myna-hud: x11 host: {e}");
        }
    }

    /// Handle every queued event. Replies can carry events in with them,
    /// past the fd watch, so every step ends here.
    fn pump(&self) -> XResult<()> {
        loop {
            let mut moved = false;
            while let Some(event) = self.conn.poll_for_event()? {
                match event {
                    Event::PropertyNotify(e) => {
                        moved |= e.window == self.root && e.atom == self.atoms._NET_WORKAREA;
                    }
                    // Managed now: the manager may have placed it after all.
                    Event::MapNotify(e) if e.window == self.xid => {
                        self.refuse_pointer()?;
                        moved = true;
                    }
                    _ => {}
                }
            }
            if !moved || !self.window.is_mapped() {
                return Ok(());
            }
            self.place_mapped()?;
        }
    }

    /// Set the empty input region again once the window manager has framed
    /// the window: xfwm4 copies a client's input shape to its frame only on
    /// a ShapeNotify, so the frame would otherwise take every click.
    fn refuse_pointer(&self) -> XResult<()> {
        self.conn.shape_rectangles(
            SO::SET,
            SK::INPUT,
            ClipOrdering::UNSORTED,
            self.xid,
            0,
            0,
            &[],
        )?;
        self.conn.flush()?;
        Ok(())
    }

    /// Pin the monitor and put the unmapped window where it will show.
    fn prepare_map(&self) -> XResult<()> {
        // GDK's own pending hints first, or they land on top of ours.
        self.display.sync();
        self.monitor.set(None);
        let size = self.expected_size();
        let Some(at) = self.target(size)? else {
            return Ok(());
        };
        self.conn
            .configure_window(self.xid, &ConfigureWindowAux::new().x(at.x).y(at.y))?;
        self.conn.change_property32(
            PropMode::REPLACE,
            self.xid,
            AtomEnum::WM_NORMAL_HINTS,
            AtomEnum::WM_SIZE_HINTS,
            &ewmh::position_hints(at),
        )?;
        // Processed before GDK's map request, which is still to be sent.
        self.conn.sync()?;
        Ok(())
    }

    fn after_map(&self) -> XResult<()> {
        // GDK's map and its rewritten hints reach the server before ours.
        self.display.sync();
        let hints = self.cardinals(
            self.xid,
            AtomEnum::WM_HINTS.into(),
            AtomEnum::WM_HINTS.into(),
        )?;
        self.conn.change_property32(
            PropMode::REPLACE,
            self.xid,
            AtomEnum::WM_HINTS,
            AtomEnum::WM_HINTS,
            &ewmh::refuse_input(&hints),
        )?;
        let sticky = ClientMessageEvent::new(
            32,
            self.xid,
            self.atoms._NET_WM_STATE,
            [
                STATE_ADD,
                self.atoms._NET_WM_STATE_STICKY,
                0,
                SOURCE_APPLICATION,
                0,
            ],
        );
        self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            sticky,
        )?;
        self.place_mapped()
    }

    /// Move the mapped window to its target if it is not there.
    fn place_mapped(&self) -> XResult<()> {
        let geometry = self.conn.get_geometry(self.xid)?.reply()?;
        let size = Size {
            width: geometry.width.into(),
            height: geometry.height.into(),
        };
        let Some(at) = self.target(size)? else {
            return Ok(());
        };
        let now = self
            .conn
            .translate_coordinates(self.xid, self.root, 0, 0)?
            .reply()?;
        if (i32::from(now.dst_x), i32::from(now.dst_y)) != (at.x, at.y) {
            self.conn
                .configure_window(self.xid, &ConfigureWindowAux::new().x(at.x).y(at.y))?;
        }
        self.conn.flush()?;
        Ok(())
    }

    /// Where a window of `size` goes, or `None` with no monitor at all.
    fn target(&self, size: Size) -> XResult<Option<Point>> {
        let (monitors, primary) = self.monitors();
        let index = match self.monitor.get() {
            Some(index) if index < monitors.len() => index,
            _ => {
                let focus = self
                    .focus_point()?
                    .and_then(|p| ewmh::monitor_at(p, &monitors));
                let pointer = ewmh::monitor_at(self.pointer()?, &monitors);
                let Some(index) = choose_monitor(focus, pointer, primary, monitors.len()) else {
                    return Ok(None);
                };
                self.monitor.set(Some(index));
                index
            }
        };
        let root = self.conn.get_geometry(self.root)?.reply()?;
        let screen = Size {
            width: root.width.into(),
            height: root.height.into(),
        };
        let (struts, docks) = self.reserved(screen)?;
        let monitor = monitors[index];
        let area =
            ewmh::clear_of_bottom_docks(work_area(monitor, screen, &struts), monitor, &docks);
        let margin = BOTTOM_MARGIN * self.window.scale_factor();
        Ok(Some(placement(area, size, margin)))
    }

    /// The window's size once mapped, in device pixels, as GTK will compute
    /// it: its default size, no smaller than its content.
    fn expected_size(&self) -> Size {
        let scale = self.window.scale_factor();
        let (default_width, default_height) = self.window.default_size();
        let (min_width, ..) = self.window.measure(gtk::Orientation::Horizontal, -1);
        let width = default_width.max(min_width);
        let (min_height, ..) = self.window.measure(gtk::Orientation::Vertical, width);
        Size {
            width: width * scale,
            height: default_height.max(min_height) * scale,
        }
    }

    /// Monitor rectangles in device pixels, and the primary's index.
    fn monitors(&self) -> (Vec<Rect>, Option<usize>) {
        let primary = self
            .display
            .downcast_ref::<gdkx11::X11Display>()
            .map(|display| display.primary_monitor());
        let list = self.display.monitors();
        let monitors: Vec<gdk::Monitor> = (0..list.n_items())
            .filter_map(|i| list.item(i).and_downcast::<gdk::Monitor>())
            .collect();
        let rects = monitors
            .iter()
            .map(|monitor| {
                let g = monitor.geometry();
                let s = monitor.scale_factor();
                Rect {
                    x: g.x() * s,
                    y: g.y() * s,
                    width: g.width() * s,
                    height: g.height() * s,
                }
            })
            .collect();
        let primary = primary.and_then(|p| monitors.iter().position(|m| *m == p));
        (rects, primary)
    }

    /// The focused window's centre, unless nothing (or the HUD) has focus.
    fn focus_point(&self) -> XResult<Option<Point>> {
        let active = self.cardinals(
            self.root,
            self.atoms._NET_ACTIVE_WINDOW,
            AtomEnum::WINDOW.into(),
        )?;
        let Some(&active) = active.first().filter(|&&w| w != 0 && w != self.xid) else {
            return Ok(None);
        };
        Ok(self.window_rect(active)?.map(|r| {
            ewmh::centre(
                Point { x: r.x, y: r.y },
                Size {
                    width: r.width,
                    height: r.height,
                },
            )
        }))
    }

    fn pointer(&self) -> XResult<Point> {
        let pointer = self.conn.query_pointer(self.root)?.reply()?;
        Ok(Point {
            x: pointer.root_x.into(),
            y: pointer.root_y.into(),
        })
    }

    /// Every managed window's struts, and the rectangles of its docks.
    fn reserved(&self, screen: Size) -> XResult<(Vec<Strut>, Vec<Rect>)> {
        let clients = self.cardinals(
            self.root,
            self.atoms._NET_CLIENT_LIST,
            AtomEnum::WINDOW.into(),
        )?;
        let a = &self.atoms;
        let property =
            |w, atom, kind: AtomEnum, len| self.conn.get_property(false, w, atom, kind, 0, len);
        let cookies = clients
            .iter()
            .map(|&w| {
                Ok((
                    w,
                    property(w, a._NET_WM_STRUT_PARTIAL, AtomEnum::CARDINAL, 12)?,
                    property(w, a._NET_WM_STRUT, AtomEnum::CARDINAL, 4)?,
                    property(w, a._NET_WM_WINDOW_TYPE, AtomEnum::ATOM, 16)?,
                ))
            })
            .collect::<XResult<Vec<_>>>()?;
        // A client can be gone by now: its replies are errors, and skipped.
        let read = |reply: Result<GetPropertyReply, _>| {
            reply
                .ok()
                .and_then(|r| r.value32().map(Iterator::collect::<Vec<u32>>))
                .unwrap_or_default()
        };
        let (mut struts, mut docks) = (Vec::new(), Vec::new());
        for (w, partial, legacy, kind) in cookies {
            struts.extend(ewmh::strut(
                &read(partial.reply()),
                &read(legacy.reply()),
                screen,
            ));
            if read(kind.reply()).contains(&a._NET_WM_WINDOW_TYPE_DOCK) {
                docks.extend(self.window_rect(w)?);
            }
        }
        Ok((struts, docks))
    }

    /// A window's rectangle on the root, or `None` once it is gone.
    fn window_rect(&self, window: Window) -> XResult<Option<Rect>> {
        let (Ok(origin), Ok(geometry)) = (
            self.conn
                .translate_coordinates(window, self.root, 0, 0)?
                .reply(),
            self.conn.get_geometry(window)?.reply(),
        ) else {
            return Ok(None);
        };
        Ok(Some(Rect {
            x: origin.dst_x.into(),
            y: origin.dst_y.into(),
            width: geometry.width.into(),
            height: geometry.height.into(),
        }))
    }

    /// A 32-bit property's values; empty when unset or of another type.
    fn cardinals(&self, window: Window, property: u32, kind: u32) -> XResult<Vec<u32>> {
        let reply = self
            .conn
            .get_property(false, window, property, kind, 0, u32::MAX / 4)?
            .reply()?;
        Ok(reply.value32().map(Iterator::collect).unwrap_or_default())
    }
}

impl Host for X11Host {
    fn before_map(&self) {
        self.report(self.prepare_map());
    }
}
