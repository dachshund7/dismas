// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// One playlist is selected at all times: its tracks are in the queue of
// the player, which plays them, and tracks are added to it. It's either
// the queue, saved in the cache, or one of the saved playlists

use adw::prelude::*;

use super::*;

impl Window {
    pub(super) fn setup_playlists(&self) {
        // The file of the selected playlist could have been deleted since
        let selected = self.selected_playlist();
        if !selected.is_empty() && !playlists::exists(&selected) {
            self.set_selected_playlist("");
        }
        self.update_playlist_labels();
    }

    // The name of the selected playlist, empty for the queue
    pub(super) fn selected_playlist(&self) -> String {
        self.imp().settings.string("selected-playlist").into()
    }

    // Only changes which playlist the tracks of the queue are saved to
    pub(super) fn set_selected_playlist(&self, name: &str) {
        self.imp()
            .settings
            .set_string("selected-playlist", name)
            .expect("Unable to store setting");
        self.update_playlist_labels();
        if self.playlists_visible() {
            self.update_playlists_page();
        }
    }

    pub(super) fn update_playlists_page(&self) {
        self.imp()
            .playlists_page
            .set_playlists(&playlists::names(), &self.selected_playlist());
    }

    // The label of the buttons that add tracks to the selected playlist
    pub(super) fn add_to_playlist_label(&self) -> String {
        let name = self.selected_playlist();
        if name.is_empty() {
            i18n("Add to Queue")
        } else {
            // Translators: `{playlist}` must be left unmodified; it is
            // expanded to the name of a playlist
            i18n_k("Add to {playlist}", &[("playlist", &name)])
        }
    }

    pub(super) fn remove_from_playlist_label(&self) -> String {
        let name = self.selected_playlist();
        if name.is_empty() {
            i18n("Remove from Queue")
        } else {
            // Translators: `{playlist}` must be left unmodified; it is
            // expanded to the name of a playlist
            i18n_k("Remove from {playlist}", &[("playlist", &name)])
        }
    }

    fn update_playlist_labels(&self) {
        let imp = self.imp();
        let name = self.selected_playlist();
        let title = if name.is_empty() { i18n("Queue") } else { name };
        imp.playlist_view.set_title(&title);

        let add_label = self.add_to_playlist_label();
        imp.album_tracks.set_add_label(&add_label);
        imp.library_view.set_add_label(&add_label);
    }

    // Saves the tracks of the queue to the selected playlist
    pub(super) fn store_queue(&self) {
        let Some(player) = self.player() else {
            return;
        };
        // A deleted playlist could still be playing, while the queue loads
        let name = self.selected_playlist();
        if !name.is_empty() && !playlists::exists(&name) {
            return;
        }
        // Shuffling only changes the order they're played in
        playlists::save(&name, &player.queue().files_in_order());
    }

    // Playing tracks right away replaces those of the queue, rather than
    // those of a saved playlist
    pub(super) fn select_queue_to_play(&self) {
        if !self.selected_playlist().is_empty() {
            self.set_selected_playlist("");
        }
    }

    // Puts the tracks of the playlist in the queue; the tracks of the
    // previous one were saved with each change. They keep playing while
    // the new ones load, which can take a while, rather than leaving
    // nothing to show in the meantime
    pub(super) fn select_playlist(&self, name: &str) {
        if name == self.selected_playlist() {
            return;
        }

        let Some(files) = playlists::load(name) else {
            self.add_toast(i18n("Unable to open the playlist"));
            return;
        };

        // The tracks of an album could be showing in its place
        self.show_playlist_view();

        if files.is_empty() {
            self.replace_selected_playlist(name);
        } else {
            self.imp().pending_playlist.replace(Some(name.to_string()));
            self.queue_tracks(files, QueueMode::Restore);
        }
    }

    // Stops playing the selected playlist, and empties the queue for the
    // tracks of the new one; with none, the album grid takes the place of
    // the player, to add tracks
    pub(super) fn replace_selected_playlist(&self, name: &str) {
        if let Some(player) = self.player() {
            player.empty_queue();
        }
        self.set_selected_playlist(name);
    }

    pub(super) fn show_new_playlist_dialog(&self) {
        self.show_playlist_name_dialog(
            &i18n("New Playlist"),
            &i18n("_Create"),
            "",
            Self::create_playlist,
        );
    }

    pub(super) fn show_rename_playlist_dialog(&self, name: &str) {
        let old_name = name.to_string();
        self.show_playlist_name_dialog(
            &i18n("Rename Playlist"),
            &i18n("_Rename"),
            name,
            move |win, new_name| win.rename_playlist(&old_name, new_name),
        );
    }

    // Asks for the name of a playlist, starting from `name`, and passes it
    // to `done` without the surrounding spaces
    fn show_playlist_name_dialog(
        &self,
        heading: &str,
        response_label: &str,
        name: &str,
        done: impl Fn(&Self, &str) + 'static,
    ) {
        let entry = gtk::Entry::builder()
            .activates_default(true)
            .placeholder_text(i18n("Name"))
            .text(name)
            .build();
        let error_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["caption", "error"])
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.append(&entry);
        content.append(&error_label);

        let dialog = adw::AlertDialog::builder()
            .heading(heading)
            .extra_child(&content)
            .close_response("cancel")
            .default_response("ok")
            .build();
        dialog.add_responses(&[("cancel", &i18n("_Cancel")), ("ok", response_label)]);
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        dialog.set_response_enabled("ok", false);

        let name = name.to_string();
        entry.connect_changed(clone!(
            #[weak]
            dialog,
            #[weak]
            error_label,
            move |entry| {
                let text = entry.text();
                // Keeping the name is nothing to do, rather than an error
                let unchanged = !name.is_empty() && text.trim() == name;
                let result = playlists::check_name(&text);
                dialog.set_response_enabled("ok", !unchanged && result.is_ok());

                let message = match result {
                    _ if unchanged => String::new(),
                    Ok(()) | Err(NameError::Empty) => String::new(),
                    Err(NameError::Invalid) => {
                        i18n("The name can't start with a dot, or have slashes or line breaks")
                    }
                    Err(NameError::Exists) => i18n("A playlist with this name already exists"),
                };
                error_label.set_label(&message);
                error_label.set_visible(!message.is_empty());
            }
        ));

        dialog.connect_response(
            Some("ok"),
            clone!(
                #[weak(rename_to = win)]
                self,
                #[weak]
                entry,
                move |_, _| done(&win, entry.text().trim())
            ),
        );

        dialog.present(Some(self));
        dialog.set_focus(Some(&entry));
        entry.select_region(0, -1);
    }

    // Creates an empty playlist, and selects it to add tracks to it
    fn create_playlist(&self, name: &str) {
        match playlists::create(name) {
            Ok(()) => {
                self.set_playlists_visible(false);
                self.select_playlist(name);
            }
            Err(e) => {
                log::warn!("Unable to create the playlist '{name}': {e}");
                self.add_toast(i18n("Unable to create the playlist"));
            }
        }
    }

    // The tracks in the queue stay as they are, even when the playlist is
    // the selected one, which keeps playing
    fn rename_playlist(&self, name: &str, new_name: &str) {
        if let Err(e) = playlists::rename(name, new_name) {
            log::warn!("Unable to rename the playlist '{name}': {e}");
            self.add_toast(i18n("Unable to rename the playlist"));
            return;
        }

        if self.selected_playlist() == name {
            self.set_selected_playlist(new_name);
        } else {
            self.update_playlists_page();
        }
    }

    pub(super) fn show_delete_playlist_dialog(&self, name: &str) {
        let dialog = adw::AlertDialog::builder()
            // Translators: `{playlist}` must be left unmodified; it is
            // expanded to the name of a playlist
            .heading(i18n_k("Delete “{playlist}”?", &[("playlist", name)]))
            .body(i18n(
                "The playlist is moved to the trash. Its tracks stay where they are.",
            ))
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_responses(&[("cancel", &i18n("_Cancel")), ("delete", &i18n("_Delete"))]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);

        let name = name.to_string();
        dialog.connect_response(
            Some("delete"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| win.delete_playlist(&name)
            ),
        );

        dialog.present(Some(self));
    }

    // The queue takes the place of the selected playlist, when it's the
    // one deleted
    fn delete_playlist(&self, name: &str) {
        if let Err(e) = playlists::trash(name) {
            log::warn!("Unable to move the playlist '{name}' to the trash: {e}");
            self.add_toast(i18n("Unable to delete the playlist"));
            return;
        }

        if self.selected_playlist() == name {
            self.select_playlist("");
        } else {
            self.update_playlists_page();
        }
    }
}
