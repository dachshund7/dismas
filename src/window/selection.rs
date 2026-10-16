// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Window {
    pub(super) fn update_selected_count(&self) {
        if let Some(player) = self.player() {
            let queue = player.queue();
            let n_selected = queue.n_selected_tracks();

            let selected_str = if n_selected == 0 {
                i18n("No track selected")
            } else {
                ni18n_f(
                    // Translators: The '{}' must be left unmodified, and
                    // it is expanded to the number of tracks selected
                    "{} track selected",
                    "{} tracks selected",
                    n_selected,
                    &[&n_selected.to_string()],
                )
            };

            self.imp()
                .playlist_view
                .queue_selected_label()
                .set_label(&selected_str);
        }
    }

    // Selects all the tracks in the queue, or the ones matching the
    // search
    pub(super) fn select_all_queue_tracks(&self) {
        self.set_queue_selection(true);
        if self.queue_search() {
            if let Some(ref model) = *self.imp().queue_filtermodel.borrow() {
                for idx in 0..model.n_items() {
                    if let Some(track) = model.item(idx).and_downcast::<Track>() {
                        track.set_selected(true);
                    }
                }
            }
        } else if let Some(player) = self.player() {
            let queue = player.queue();
            for idx in 0..queue.n_tracks() {
                queue.track_at(idx).unwrap().set_selected(true);
            }
        }
        self.update_selected_count();
    }

    // Selects or unselects a track of the queue, entering selection
    // mode if needed
    pub(super) fn toggle_queue_track(&self, track: &Track) {
        if self.queue_selection() {
            track.set_selected(!track.selected());
        } else {
            self.set_queue_selection(true);
            track.set_selected(true);
        }
        self.imp().queue_anchor.set(Some(track));
        self.update_selected_count();
    }

    // Selects the tracks from the last one clicked to this one, in the
    // order of the queue or the search results; without a last one,
    // it toggles the track like a click
    pub(super) fn select_queue_range(&self, track: &Track) {
        let imp = self.imp();
        let anchor = imp.queue_anchor.upgrade();
        let model = imp.playlist_view.queue_view().model();
        let selected = match (anchor, model) {
            (Some(anchor), Some(model)) => {
                utils::select_track_range(model.upcast_ref(), &anchor, track)
            }
            _ => false,
        };

        if selected {
            self.update_selected_count();
        } else {
            self.toggle_queue_track(track);
        }
    }

    pub(super) fn remove_selected_tracks(&self) {
        if let Some(player) = self.player() {
            let queue = player.queue();
            // Collect all tracks to be removed first, since we can't
            // remove objects from the model while we're iterating it
            let remove_tracks: Vec<Track> = (0..queue.n_tracks())
                .filter_map(|idx| queue.track_at(idx))
                .filter(Track::selected)
                .collect();

            for track in remove_tracks {
                self.remove_track(&track);
            }

            // Store the current state of the queue
            utils::store_queue(queue);
        }
    }

    // Leaves selection mode in the grid, the tracks of an album and the
    // queue; returns whether any of them was in it
    pub(super) fn exit_selection_mode(&self) -> bool {
        let imp = self.imp();
        let mut exited = false;
        if imp.library_view.selection_mode() {
            imp.library_view.set_selection_mode(false);
            exited = true;
        }
        if imp.album_tracks.selection_mode() {
            imp.album_tracks.set_selection_mode(false);
            exited = true;
        }
        if self.queue_selection() {
            self.set_queue_selection(false);
            exited = true;
        }
        exited
    }

    // Ctrl+A selects all the items of the list with the focus: the album
    // grid, the tracks of an album, or the queue
    pub(super) fn select_all(&self) {
        let imp = self.imp();
        let focus = gtk::prelude::GtkWindowExt::focus(self);
        let focus_in = |widget: &gtk::Widget| focus.as_ref().is_some_and(|f| f.is_ancestor(widget));
        let sidebar_page = imp.sidebar_stack.visible_child_name();

        if self.library_visible() && focus_in(imp.library_view.upcast_ref()) {
            imp.library_view.select_all();
        } else if focus_in(imp.album_tracks.upcast_ref()) {
            imp.album_tracks.select_all();
        } else if self.playlist_visible() && sidebar_page.as_deref() == Some("playlist") {
            self.select_all_queue_tracks();
        } else if self.playlist_visible() && sidebar_page.as_deref() == Some("album") {
            imp.album_tracks.select_all();
        } else if self.library_visible() {
            imp.library_view.select_all();
        }
    }

    // Ctrl+A selects the items of the pane with the focus, even from its
    // search entry, so it's handled before the focused widget gets it; the
    // grid and the lists would otherwise take it for their own selection,
    // which does nothing, and the search entries for selecting their text
    pub(super) fn setup_select_all_shortcut(&self) {
        let controller = gtk::ShortcutController::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        controller.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>a"),
            Some(gtk::NamedAction::new("queue.select-all")),
        ));
        self.add_controller(controller);
    }
}
