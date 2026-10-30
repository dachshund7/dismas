// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The pages of the playlist pane: the queue, the tracks of an album or the
// recordings of a work, and the lyrics of the playing track. An album and
// the lyrics open over the page that shows, and going back shows that page
// again, so that an album opened while the lyrics show goes back to them

use super::*;

// A page of the pane that opens over another
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanePage {
    Album,
    Lyrics,
}

impl PanePage {
    // Its name in the stack of the pane
    fn name(self) -> &'static str {
        match self {
            PanePage::Album => "album",
            PanePage::Lyrics => "lyrics",
        }
    }

    fn named(name: &str) -> Option<Self> {
        match name {
            "album" => Some(PanePage::Album),
            "lyrics" => Some(PanePage::Lyrics),
            _ => None,
        }
    }
}

// Where going back from a page of the pane goes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Back {
    // The queue, and whether the pane was showing before the page opened,
    // as the pane closes again if it wasn't
    Queue { pane_visible: bool },
    Page(PanePage),
}

// The pages under the one that shows, the queue first; empty while the
// queue shows
#[derive(Debug, Default)]
pub struct PaneHistory(Vec<Back>);

impl PaneHistory {
    // `page` opens over `from`, the page that shows, which is the queue if
    // it's `None`, while the pane shows or not. A page that's under it
    // already leaves its place, so that each is there once
    fn open(&mut self, page: PanePage, from: Option<PanePage>, pane_visible: bool) {
        match from {
            Some(from) if from == page => {}
            Some(from) => {
                self.0.retain(|back| *back != Back::Page(page));
                self.0.push(Back::Page(from));
            }
            None => self.0 = vec![Back::Queue { pane_visible }],
        }
    }

    fn back(&mut self) -> Back {
        self.0.pop().unwrap_or(Back::Queue { pane_visible: true })
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

impl Window {
    // The page of the pane over the queue that shows, if any
    fn shown_pane_page(&self) -> Option<PanePage> {
        PanePage::named(self.imp().sidebar_stack.visible_child_name()?.as_str())
    }

    // Shows a page of the pane over the one that shows, opening the pane
    // if it's closed
    pub(super) fn open_pane_page(&self, page: PanePage) {
        let imp = self.imp();
        imp.pane_history
            .borrow_mut()
            .open(page, self.shown_pane_page(), self.playlist_visible());
        imp.sidebar_stack.set_visible_child_name(page.name());
        self.set_playlist_visible(true);
    }

    // Goes back from the tracks of an album or from the lyrics to the page
    // under them, closing the pane if it was opened for them
    pub(super) fn show_pane_back(&self) {
        let imp = self.imp();
        let back = imp.pane_history.borrow_mut().back();
        match back {
            Back::Page(page) => imp.sidebar_stack.set_visible_child_name(page.name()),
            Back::Queue { pane_visible } => {
                imp.sidebar_stack.set_visible_child_name("playlist");
                if !pane_visible {
                    self.set_playlist_visible(false);
                }
            }
        }
    }

    // Shows the queue in the pane, under every other page, e.g. once tracks
    // are added to it
    pub(super) fn show_playlist_view(&self) {
        let imp = self.imp();
        imp.pane_history.borrow_mut().clear();
        imp.sidebar_stack.set_visible_child_name("playlist");
    }

    pub(super) fn lyrics_on_pane(&self) -> bool {
        self.shown_pane_page() == Some(PanePage::Lyrics)
    }

    // Lyrics, in the More menu, and Ctrl+Shift+L: the lyrics of the playing
    // track, in the pane, as long as there's a track and the window isn't
    // fullscreen, where the player shows by itself
    pub(super) fn setup_lyrics(&self) {
        let imp = self.imp();
        imp.lyrics_view.set_state(self.player().state());
        imp.lyrics_view.connect_back(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.show_pane_back()
        ));
        imp.sidebar_stack.connect_visible_child_name_notify(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.notify_lyrics_shown()
        ));
        self.update_lyrics_action();
    }

    pub(super) fn update_lyrics_action(&self) {
        let available = self.player().state().track().is_some() && !self.is_fullscreen();
        self.action_set_enabled("win.lyrics", available);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn going_back_shows_the_page_from_before() {
        let mut history = PaneHistory::default();
        // From the queue, with the pane closed, to the lyrics and an album
        history.open(PanePage::Lyrics, None, false);
        history.open(PanePage::Album, Some(PanePage::Lyrics), true);
        // Another album takes the place of the first
        history.open(PanePage::Album, Some(PanePage::Album), true);
        assert_eq!(history.back(), Back::Page(PanePage::Lyrics));
        assert_eq!(
            history.back(),
            Back::Queue {
                pane_visible: false
            }
        );
        // With nothing under it, the queue shows in the pane
        assert_eq!(history.back(), Back::Queue { pane_visible: true });
    }

    #[test]
    fn a_page_is_under_another_once() {
        let mut history = PaneHistory::default();
        history.open(PanePage::Album, None, true);
        history.open(PanePage::Lyrics, Some(PanePage::Album), true);
        // Back to the album from the lyrics' item, and to the lyrics
        history.open(PanePage::Album, Some(PanePage::Lyrics), true);
        assert_eq!(history.back(), Back::Page(PanePage::Lyrics));
        assert_eq!(history.back(), Back::Queue { pane_visible: true });
    }

    #[test]
    fn the_queue_starts_again() {
        let mut history = PaneHistory::default();
        history.open(PanePage::Album, None, false);
        history.open(PanePage::Lyrics, Some(PanePage::Album), true);
        history.clear();
        history.open(PanePage::Lyrics, None, true);
        assert_eq!(history.back(), Back::Queue { pane_visible: true });
    }
}
