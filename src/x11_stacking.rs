// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// Whether other windows cover a window, from what the window manager
// publishes on X11; GTK only knows about its own windows. Errors come
// back as values: a window can close while it's looked at, and Xlib would
// end the process for that

use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ConnectionExt, MapState, Window},
    rust_connection::RustConnection,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Visibility {
    // Unmapped by the window manager: minimized, or on a desktop that
    // isn't shown
    Hidden,
    Covered,
    Uncovered,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl Rect {
    fn overlaps(&self, other: &Rect) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

struct Atoms {
    stacking: u32,
    window_type: u32,
    type_normal: u32,
    type_dialog: u32,
    state: u32,
    state_hidden: u32,
    frame_extents: u32,
}

impl Atoms {
    fn new(conn: &RustConnection) -> Option<Self> {
        let names: [&[u8]; 7] = [
            b"_NET_CLIENT_LIST_STACKING",
            b"_NET_WM_WINDOW_TYPE",
            b"_NET_WM_WINDOW_TYPE_NORMAL",
            b"_NET_WM_WINDOW_TYPE_DIALOG",
            b"_NET_WM_STATE",
            b"_NET_WM_STATE_HIDDEN",
            b"_GTK_FRAME_EXTENTS",
        ];
        // All asked at once, rather than waiting for each in turn
        let cookies = names
            .map(|name| conn.intern_atom(false, name).ok())
            .map(|cookie| Some(cookie?.reply().ok()?.atom));
        let [
            Some(stacking),
            Some(window_type),
            Some(type_normal),
            Some(type_dialog),
            Some(state),
            Some(state_hidden),
            Some(frame_extents),
        ] = cookies
        else {
            return None;
        };
        Some(Self {
            stacking,
            window_type,
            type_normal,
            type_dialog,
            state,
            state_hidden,
            frame_extents,
        })
    }
}

// None when the X server can't be reached, or the window manager doesn't
// publish the order of its windows
pub fn visibility(window: Window) -> Option<Visibility> {
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let atoms = Atoms::new(&conn)?;

    if !is_shown(&conn, &atoms, window) {
        return Some(Visibility::Hidden);
    }
    let area = rect(&conn, &atoms, root, window)?;

    // From the bottom to the top
    let stacking = property32(&conn, root, atoms.stacking)?;
    let position = stacking.iter().position(|&w| w == window)?;
    let covered = stacking[position + 1..].iter().any(|&above| {
        is_shown(&conn, &atoms, above)
            && is_normal(&conn, &atoms, above)
            && rect(&conn, &atoms, root, above).is_some_and(|r| r.overlaps(&area))
    });

    Some(if covered {
        Visibility::Covered
    } else {
        Visibility::Uncovered
    })
}

fn property32(conn: &RustConnection, window: Window, property: u32) -> Option<Vec<u32>> {
    let reply = conn
        .get_property(false, window, property, AtomEnum::ANY, 0, u32::MAX)
        .ok()?
        .reply()
        .ok()?;
    Some(reply.value32()?.collect())
}

// Window managers unmap the windows of hidden desktops, or their frames,
// which leaves the windows unviewable
fn is_shown(conn: &RustConnection, atoms: &Atoms, window: Window) -> bool {
    let viewable = conn
        .get_window_attributes(window)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|attributes| attributes.map_state == MapState::VIEWABLE);
    viewable
        && !property32(conn, window, atoms.state)
            .is_some_and(|state| state.contains(&atoms.state_hidden))
}

// Panels and docks stay above the windows, which don't count as covered
// where they reach under them
fn is_normal(conn: &RustConnection, atoms: &Atoms, window: Window) -> bool {
    property32(conn, window, atoms.window_type).is_none_or(|types| {
        types.is_empty()
            || types
                .iter()
                .any(|&t| t == atoms.type_normal || t == atoms.type_dialog)
    })
}

// Where the window is on the screen, without the shadows GTK draws around
// its windows
fn rect(conn: &RustConnection, atoms: &Atoms, root: Window, window: Window) -> Option<Rect> {
    let geometry = conn.get_geometry(window).ok()?.reply().ok()?;
    let origin = conn
        .translate_coordinates(window, root, 0, 0)
        .ok()?
        .reply()
        .ok()?;
    // Any client can set the extents; as large as a window at most, the
    // sums below stay well within an i32
    let [left, right, top, bottom] = property32(conn, window, atoms.frame_extents)
        .and_then(|extents| <[u32; 4]>::try_from(extents).ok())
        .unwrap_or_default()
        .map(|extent| i32::from(u16::try_from(extent).unwrap_or(u16::MAX)));
    Some(Rect {
        x: i32::from(origin.dst_x) + left,
        y: i32::from(origin.dst_y) + top,
        width: i32::from(geometry.width) - left - right,
        height: i32::from(geometry.height) - top - bottom,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn overlapping_rects() {
        let window = rect(0, 0, 100, 100);
        assert!(window.overlaps(&rect(50, 50, 100, 100)));
        assert!(window.overlaps(&rect(10, 10, 10, 10)));
        assert!(rect(10, 10, 10, 10).overlaps(&window));
    }

    // Tiled windows touch without overlapping
    #[test]
    fn touching_rects() {
        let window = rect(0, 0, 100, 100);
        assert!(!window.overlaps(&rect(100, 0, 100, 100)));
        assert!(!window.overlaps(&rect(0, 100, 100, 100)));
        assert!(!window.overlaps(&rect(200, 200, 10, 10)));
    }
}
