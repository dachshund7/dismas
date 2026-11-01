// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::zoom::Zoom;

// What the zoom keys make larger or smaller
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ZoomTarget {
    Covers,
}

impl Window {
    // Ctrl+plus, Ctrl+minus and Ctrl+0 zoom what the focus is on, as
    // Ctrl+F searches there; at the end it goes to, or with nothing to
    // zoom, the bell rings
    pub(super) fn zoom(&self, zoom: Zoom) {
        let imp = self.imp();
        let in_pane = gtk::prelude::GtkWindowExt::focus(self)
            .is_some_and(|focus| focus.is_ancestor(&*imp.playlist_pane))
            || (imp.split_view.is_collapsed() && self.playlist_visible());
        let zoomed = match zoom_target(in_pane, LibraryPage::named(&self.page()).is_some()) {
            Some(ZoomTarget::Covers) => imp.library_view.zoom_covers(zoom),
            None => false,
        };
        if !zoomed {
            self.error_bell();
        }
    }
}

// What the zoom keys zoom: the covers on the album pages, unless the focus
// is in the playlist pane, or the pane covers the page in a narrow window
fn zoom_target(in_pane: bool, album_page: bool) -> Option<ZoomTarget> {
    (album_page && !in_pane).then_some(ZoomTarget::Covers)
}

#[cfg(test)]
mod tests {
    use super::{ZoomTarget, zoom_target};

    #[test]
    fn the_keys_zoom_the_covers_on_the_album_pages() {
        assert_eq!(zoom_target(false, true), Some(ZoomTarget::Covers));
        // The focus is in the pane, which has no covers
        assert_eq!(zoom_target(true, true), None);
        // The player and the playlists have none
        assert_eq!(zoom_target(false, false), None);
    }
}
