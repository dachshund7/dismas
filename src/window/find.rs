// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Window {
    // Whether the focus is in the playlist pane, where Ctrl+F, typing and
    // zooming go to what the pane shows, rather than to the page beside it
    pub(super) fn focus_in_pane(&self) -> bool {
        gtk::prelude::GtkWindowExt::focus(self)
            .is_some_and(|focus| focus.is_ancestor(&*self.imp().playlist_pane))
    }

    // The search that Ctrl+F and typing go to: the album search on the
    // albums, the tracks and the works, unless the focus is in the playlist
    // pane, and the queue's otherwise, as the player and the playlists have
    // no search of their own
    fn focused_search(&self) -> gtk::SearchEntry {
        let imp = self.imp();
        if !self.focus_in_pane() && LibraryPage::named(&self.page()).is_some() {
            imp.library_view.search_entry()
        } else {
            imp.playlist_view.queue_searchentry()
        }
    }

    // Ctrl+F puts the cursor in the search of what has the focus. The
    // queue's opens if it's closed, and so does the pane, which can hide a
    // search left open in it, and the pane goes back to the queue from an
    // album's tracks or the lyrics, which would hide it too. Its text is
    // selected, as in a browser's find bar, so that typing starts a new
    // search, and End keeps it; a search entry takes the focus without
    // selecting, and typing elsewhere goes on with the search
    pub(super) fn find(&self) {
        let entry = self.focused_search();
        if entry == self.imp().playlist_view.queue_searchentry() {
            self.show_playlist_view();
            self.set_playlist_visible(true);
            self.set_queue_search(true);
        }
        entry.grab_focus();
        entry.select_region(0, -1);
    }

    // Typing outside a text entry types into the search of what has the
    // focus, which then takes it, so that the rest goes there too; the
    // queue's opens, while the pane shows the queue. GTK's own key capture
    // would leave the focus where it was, and the queue's only started a
    // search, so typing in the queue went nowhere once it was open. GTK
    // runs the controllers added last first, so this comes before the
    // window's bindings, and only takes the keys that type (`types_text`)
    pub(super) fn setup_type_to_search(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |keys, key, _, state| win.type_into_search(keys, key, state)
        ));
        self.add_controller(keys);
    }

    fn type_into_search(
        &self,
        keys: &gtk::EventControllerKey,
        key: gdk::Key,
        state: gdk::ModifierType,
    ) -> glib::Propagation {
        let imp = self.imp();
        if !types_text(key, state) {
            return glib::Propagation::Proceed;
        }
        // Not from a text entry, which had the key and left it, nor from a
        // popover or a dialog, as the search is behind them
        let elsewhere = gtk::prelude::GtkWindowExt::focus(self).is_some_and(|focus| {
            !focus.is_ancestor(&*imp.split_view)
                || focus.ancestor(gtk::Popover::static_type()).is_some()
                || focus.ancestor(gtk::Editable::static_type()).is_some()
        });
        if elsewhere {
            return glib::Propagation::Proceed;
        }

        let entry = self.focused_search();
        let queue = entry == imp.playlist_view.queue_searchentry();
        // The pane shows an album's tracks or the lyrics, or is hidden
        if queue && !imp.playlist_view.queue_searchbar().is_mapped() {
            return glib::Propagation::Proceed;
        }
        let Some(text) = entry.delegate().and_downcast::<gtk::Text>() else {
            return glib::Propagation::Proceed;
        };
        let before = entry.text();
        if !keys.forward(&text) {
            return glib::Propagation::Proceed;
        }
        // A dead key only starts a letter, which the next key finishes, and
        // moving the focus would drop it
        if entry.text() != before {
            if queue {
                self.set_queue_search(true);
            }
            entry.grab_focus();
        }
        glib::Propagation::Stop
    }
}

// Whether a key types into a search: a character, Backspace, or a dead key
// or Compose, which start one. Not Delete, Esc, Enter or Tab, which GDK
// gives control characters, nor the keys without one, as the window and
// the focused widget have them, nor Space, which plays and pauses, nor a
// key with Ctrl, Alt or Super
fn types_text(key: gdk::Key, state: gdk::ModifierType) -> bool {
    let modifiers = gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SUPER_MASK
        | gdk::ModifierType::META_MASK;
    !state.intersects(modifiers)
        && (key
            .to_unicode()
            .is_some_and(|c| !c.is_control() && !c.is_whitespace())
            || key == gdk::Key::BackSpace
            || key == gdk::Key::Multi_key
            || key.name().is_some_and(|name| name.starts_with("dead_")))
}
