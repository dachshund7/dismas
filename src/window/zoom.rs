// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::zoom::Zoom;

// What the zoom keys make larger or smaller
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ZoomTarget {
    Covers,
    Lyrics,
}

impl Window {
    // Ctrl+plus, Ctrl+minus and Ctrl+0 zoom what the focus is on, as
    // Ctrl+F searches there; at the end it goes to, or with nothing to
    // zoom, the bell rings
    pub(super) fn zoom(&self, zoom: Zoom) {
        let imp = self.imp();
        let in_pane =
            self.focus_in_pane() || (imp.split_view.is_collapsed() && self.playlist_visible());
        let target = zoom_target(
            in_pane,
            LibraryPage::named(&self.page()).is_some(),
            self.lyrics_shown(),
        );
        let zoomed = match target {
            Some(ZoomTarget::Covers) => imp.library_view.zoom_covers(zoom),
            Some(ZoomTarget::Lyrics) => imp.lyrics_view.zoom(zoom),
            None => false,
        };
        if !zoomed {
            self.error_bell();
        }
    }
}

// What the zoom keys zoom: what the playlist pane shows when the focus is
// in it, or when it covers the page in a narrow window, which is only the
// lyrics; the covers on the album pages; and the lyrics beside the player
// and the playlists, which have nothing to zoom. Fullscreen hides the pane
fn zoom_target(in_pane: bool, album_page: bool, lyrics_shown: bool) -> Option<ZoomTarget> {
    if album_page && !in_pane {
        Some(ZoomTarget::Covers)
    } else {
        lyrics_shown.then_some(ZoomTarget::Lyrics)
    }
}

#[cfg(test)]
mod tests {
    use super::{ZoomTarget, zoom_target};

    #[test]
    fn the_keys_zoom_what_the_focus_is_on() {
        // The album pages
        assert_eq!(zoom_target(false, true, false), Some(ZoomTarget::Covers));
        assert_eq!(zoom_target(false, true, true), Some(ZoomTarget::Covers));
        // The pane, which only zooms the lyrics
        assert_eq!(zoom_target(true, true, true), Some(ZoomTarget::Lyrics));
        assert_eq!(zoom_target(true, true, false), None);
        // The player and the playlists, beside the lyrics or not
        assert_eq!(zoom_target(false, false, true), Some(ZoomTarget::Lyrics));
        assert_eq!(zoom_target(false, false, false), None);
    }
}
