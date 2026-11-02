// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl LibraryView {
    fn set_album_selected(&self, album: &Album, selected: bool) {
        album.set_selected(selected);
        let mut keys = self.imp().selected.borrow_mut();
        if selected {
            keys.insert(album.selection_key());
        } else {
            keys.remove(&album.selection_key());
        }
        drop(keys);
        self.update_selection();
    }

    // Selects or unselects an album, entering selection mode if needed
    pub(super) fn toggle_album(&self, album: &Album) {
        if self.selection_mode() {
            self.set_album_selected(album, !album.selected());
        } else {
            self.set_selection_mode(true);
            self.set_album_selected(album, true);
        }
        self.imp().selection_anchor.set(Some(album));
    }

    // The albums that can be selected, in their order: those the grid
    // shows, or on the Works page those of the covers of the open work
    fn selectable_albums(&self) -> Vec<Album> {
        let imp = self.imp();
        if self.shows_works() {
            return self.open_work_albums();
        }
        let Some(model) = imp.grid_view.model() else {
            return Vec::new();
        };
        // The items of the selection model are typed as GObject, so
        // `iter::<Album>()` would panic
        (0..model.n_items())
            .filter_map(|i| model.item(i).and_downcast::<Album>())
            .collect()
    }

    // Selects the albums from the last one clicked to this one, in the
    // order of the grid; without a last one, it toggles the album like a
    // click
    pub(super) fn select_range(&self, album: &Album) {
        let anchor = self.imp().selection_anchor.upgrade();
        let albums = self.selectable_albums();
        let position = |album: &Album| albums.iter().position(|a| a == album);

        let Some((from, to)) = anchor.as_ref().and_then(position).zip(position(album)) else {
            self.toggle_album(album);
            return;
        };
        for album in &albums[from.min(to)..=from.max(to)] {
            if !album.selected() {
                self.set_album_selected(album, true);
            }
        }
    }

    // Selects all the albums shown by the search, or the covers of the open
    // work; on the Works page, there's nothing to select until one is open
    pub(crate) fn select_all(&self) {
        let albums = self.selectable_albums();
        if self.shows_works() && albums.is_empty() {
            return;
        }
        self.set_selection_mode(true);
        for album in albums {
            if !album.selected() {
                self.set_album_selected(&album, true);
            }
        }
    }

    pub(super) fn unselect_all(&self) {
        if let Some(model) = self.imp().model.borrow().as_ref() {
            for album in model.iter::<Album>().flatten() {
                album.set_selected(false);
            }
        }
        for album in self.open_work_albums() {
            album.set_selected(false);
        }
        self.imp().selected.borrow_mut().clear();
        self.update_selection();
    }

    // The selected albums, in the order of the grid or of the covers, with
    // what their tracks have to match to be taken; the covers of the open
    // work each take their recording's
    pub(crate) fn selected_albums(&self) -> Vec<(String, TrackFilter)> {
        if self.shows_works() {
            return self.selected_recordings();
        }
        let filter = self.taken().0;
        self.selectable_albums()
            .into_iter()
            .filter(Album::selected)
            .map(|album| (album.id(), filter.clone()))
            .collect()
    }

    // The selected albums, leaving selection mode, as the buttons of its
    // bar do once they've taken them
    pub(crate) fn take_selected_albums(&self) -> Vec<(String, TrackFilter)> {
        let albums = self.selected_albums();
        self.set_selection_mode(false);
        albums
    }

    // The menu next to the button that adds the albums, with the other ways
    // to add them
    pub(crate) fn set_add_menu(&self, menu: &gio::MenuModel) {
        self.imp().add_albums_button.set_menu_model(Some(menu));
    }

    // The label of the button that adds the albums to the selected playlist
    pub(crate) fn set_add_label(&self, label: &str) {
        crate::utils::set_playlist_button_label(&self.imp().add_albums_button, label);
    }

    // Albums can't be added while tracks are being added to the queue
    pub(crate) fn set_adding_enabled(&self, enabled: bool) {
        self.imp().loading.set(!enabled);
        self.update_selection();
    }

    // On the Works page, the covers of the open work are selected, so
    // there's nothing to select until one is open
    pub(super) fn update_select_button(&self) {
        let imp = self.imp();
        let selectable = !self.shows_works() || imp.expanded_work.borrow().is_some();
        imp.select_button.set_sensitive(selectable);
    }

    pub(super) fn update_selection(&self) {
        let imp = self.imp();
        let selected = imp.selected.borrow();
        let n_selected = selected.len() as u32;
        // During a search on the Tracks page, and on the Works page, the
        // number of tracks the buttons take, as they take only some of each
        // album
        let matching = match imp.page.get() {
            LibraryPage::Albums => None,
            LibraryPage::Tracks => imp.search_matches.borrow().as_ref().map(|matches| {
                selected
                    .iter()
                    .filter_map(|key| matches.get(&key.album))
                    .map(|(matching, _)| matching)
                    .sum()
            }),
            LibraryPage::Works => self.open_work_tracks(&selected),
        };
        drop(selected);
        let recordings = imp.page.get() == LibraryPage::Works;
        let label = if n_selected == 0 && recordings {
            i18n("No recording selected")
        } else if n_selected == 0 {
            i18n("No album selected")
        } else {
            let albums = if recordings {
                ni18n_f(
                    // Translators: The '{}' must be left unmodified, and it
                    // is expanded to the number of recordings of a work
                    // selected, whose covers are those of their albums
                    "{} recording selected",
                    "{} recordings selected",
                    n_selected,
                    &[&n_selected.to_string()],
                )
            } else {
                ni18n_f(
                    // Translators: The '{}' must be left unmodified, and
                    // it is expanded to the number of albums selected
                    "{} album selected",
                    "{} albums selected",
                    n_selected,
                    &[&n_selected.to_string()],
                )
            };
            match matching {
                Some(n) => {
                    let tracks = ni18n_f(
                        // Translators: The '{}' must be left unmodified, and
                        // it is expanded to the number of tracks of the
                        // albums selected that the search matches, or of the
                        // recordings of a work selected
                        "{} track",
                        "{} tracks",
                        n,
                        &[&n.to_string()],
                    );
                    format!("{albums} · {tracks}")
                }
                None => albums,
            }
        };
        imp.selected_label.set_label(&label);

        let can_add = n_selected > 0 && !imp.loading.get();
        imp.play_albums_button.set_sensitive(can_add);
        imp.add_albums_button.set_sensitive(can_add);
    }
}
