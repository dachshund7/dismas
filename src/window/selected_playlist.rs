// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// One playlist is selected at all times: its tracks are in the queue of
// the player, which plays them, and tracks are added to it. It's either
// the queue, saved in the cache, or one of the saved playlists

use adw::prelude::*;

use super::*;
use crate::{cue_sheet, play_stats};

impl Window {
    pub(super) fn setup_playlists(&self) {
        // The file of the selected playlist could have been deleted since
        let selected = self.selected_playlist();
        if !selected.is_empty() && !playlists::exists(&selected) {
            self.set_selected_playlist("");
        }
        // Or any other, e.g. outside the app, leaving its point and its
        // order from before shuffling behind
        let names = playlists::names();
        ResumePoints::update(&self.imp().settings, |points| {
            points.retain_playlists(&names);
        });
        playlists::retain_unshuffled(&names);
        self.update_playlist_labels();
        self.update_playlists_menu();
    }

    // The name of the selected playlist, empty for the queue
    pub(super) fn selected_playlist(&self) -> String {
        self.imp().settings.string("selected-playlist").into()
    }

    // Only changes which playlist the tracks of the queue are saved to
    pub(super) fn set_selected_playlist(&self, name: &str) {
        // The key can be locked down by the system administrator
        if let Err(e) = self.imp().settings.set_string("selected-playlist", name) {
            log::warn!("Unable to store the selected playlist: {e}");
        }
        self.update_playlist_labels();
        self.update_playlists_menu();
        if self.playlists_visible() {
            self.update_playlists_page();
        }
        self.sync_queue_shuffled();
    }

    pub(super) fn update_playlists_page(&self) {
        self.imp()
            .playlists_page
            .set_playlists(&playlists::names(), &self.selected_playlist());
    }

    // The menu of the playlist pane's title, to switch playlists without
    // going through the Playlists page; it's rebuilt whenever they change,
    // even while the page is hidden
    pub(super) fn update_playlists_menu(&self) {
        let selected = self.selected_playlist();
        if let Some(action) = self
            .lookup_action("selected-playlist")
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_state(&selected.to_variant());
        }

        let playlists = gio::Menu::new();
        let queue = gio::MenuItem::new(Some(&i18n("Queue")), None);
        queue.set_action_and_target_value(Some("win.selected-playlist"), Some(&"".to_variant()));
        playlists.append_item(&queue);
        for name in playlists::names() {
            // Shortened like where tracks are dropped, as a menu is as
            // wide as its longest item; underscores in names would be taken
            // for mnemonics
            let label = utils::short_playlist_name(&name).replace('_', "__");
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(
                Some("win.selected-playlist"),
                Some(&name.to_variant()),
            );
            playlists.append_item(&item);
        }

        let manage = gio::Menu::new();
        let item = gio::MenuItem::new(Some(&i18n("_Manage Playlists…")), None);
        item.set_action_and_target_value(Some("win.page"), Some(&"playlists".to_variant()));
        manage.append_item(&item);

        let menu = gio::Menu::new();
        menu.append_section(None, &playlists);
        menu.append_section(None, &manage);
        self.imp()
            .playlist_view
            .set_playlists_menu(menu.upcast_ref());
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

    fn update_playlist_labels(&self) {
        let imp = self.imp();
        let name = self.selected_playlist();
        let title = if name.is_empty() {
            i18n("Queue")
        } else {
            name.clone()
        };
        imp.playlist_view.set_title(&title);

        let add_label = self.add_to_playlist_label();
        imp.album_tracks.set_add_label(&add_label);
        imp.library_view.set_add_label(&add_label);

        // The tooltips and the drop target name the list they're about
        let (close, back, drop) = if name.is_empty() {
            (
                i18n("Close the Queue"),
                i18n("Back to the Queue"),
                i18n("Drop tracks to add them to the queue"),
            )
        } else {
            let args = [("playlist", name.as_str())];
            // The tooltips show the whole name, like those of the buttons
            // that add tracks, but it would fill the window when dropping
            let short_name = utils::short_playlist_name(&name);
            (
                // Translators: `{playlist}` must be left unmodified; it is
                // expanded to the name of a playlist
                i18n_k("Close {playlist}", &args),
                // Translators: `{playlist}` must be left unmodified; it is
                // expanded to the name of a playlist
                i18n_k("Back to {playlist}", &args),
                // Translators: `{playlist}` must be left unmodified; it is
                // expanded to the name of a playlist
                i18n_k(
                    "Drop tracks to add them to {playlist}",
                    &[("playlist", short_name.as_str())],
                ),
            )
        };
        imp.playlist_view
            .back_button()
            .set_tooltip_text(Some(&close));
        imp.album_tracks.set_back_tooltip(&back);
        imp.drag_overlay.set_title(drop);
    }

    // Saves the tracks of the queue to the selected playlist
    pub(super) fn store_queue(&self) {
        let player = self.player();
        // A deleted playlist could still be playing, while the queue loads
        let name = self.selected_playlist();
        if !name.is_empty() && !playlists::exists(&name) {
            return;
        }
        playlists::save(&name, &player.queue().files());
    }

    // The shuffle button shows whether the selected playlist was shuffled,
    // and keeps the order it had before; it's on, to put it back, even
    // when there's a single track left
    pub(super) fn sync_queue_shuffled(&self) {
        let shuffled = playlists::is_shuffled(&self.selected_playlist());
        if shuffled != self.imp().queue_shuffled.replace(shuffled) {
            self.notify_queue_shuffled();
        }
        self.update_queue_actions();
    }

    // Shuffles the tracks of the selected playlist, keeping the order they
    // had to put it back. The order goes by the play stats, which are read
    // right away: it's a quick read, and the order is saved here as well.
    // The track playing, or paused partway, e.g. where the last session
    // stopped, comes first, to go on from there; otherwise the first of the
    // shuffled tracks is ready to play
    pub(super) fn shuffle_queue(&self) {
        let player = self.player();
        let queue = player.queue();
        // While tracks are read, the queue doesn't have them all yet
        let name = self.selected_playlist();
        if queue.is_empty()
            || self.imp().loads.get() > 0
            || (!name.is_empty() && !playlists::exists(&name))
        {
            return;
        }

        // Kept before the shuffled order is saved, so that a crash in
        // between can't lose it
        if let Err(e) = playlists::save_unshuffled(&name, &queue.files()) {
            log::warn!("Unable to keep the order of the playlist '{name}': {e}");
            return;
        }

        let state = player.state();
        let keep = queue
            .current()
            .filter(|_| state.playing() || state.position() > 0);
        let uris: Vec<String> = queue.files().iter().map(|f| f.uri().to_string()).collect();
        let stats = play_stats::track_stats(&uris);
        queue.shuffle(keep.as_ref(), &stats, play_stats::now(), &mut rand::rng());
        if keep.is_none() {
            player.skip_to_first();
        }
        self.store_queue();
    }

    // Puts the tracks of the selected playlist back in the order they had
    // before they were shuffled; the tracks added since go at the end. The
    // current track stays, and playback goes on from it
    pub(super) fn unshuffle_queue(&self) {
        let player = self.player();
        let name = self.selected_playlist();
        if self.imp().loads.get() > 0 {
            return;
        }

        if let Some(files) = playlists::load_unshuffled(&name) {
            let uris: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
            player.queue().restore_order(&uris);
            self.store_queue();
        }
        // Only once the order is saved, so that a crash in between can't
        // lose it
        playlists::drop_unshuffled(&name);
    }

    // Playing tracks right away replaces those of the queue, rather than
    // those of a saved playlist
    pub(super) fn select_queue_to_play(&self) {
        if !self.selected_playlist().is_empty() {
            // Where the saved one was, to go back to it: the save at idle
            // would come after the switch
            self.save_resume_point();
            self.set_selected_playlist("");
        }
    }

    // What playing dropped tracks is about to replace: the queue, and the
    // playlist that was selected, to put them back. The queue is taken
    // from its file, which also has the tracks that couldn't be read, or
    // from the player if it's unreadable
    pub(super) fn replaced_queue(&self) -> ReplacedQueue {
        self.save_resume_point();
        let selected = self.selected_playlist();
        let queue = playlists::load("")
            .or_else(|| selected.is_empty().then(|| self.player().queue().files()))
            .unwrap_or_default();
        ReplacedQueue {
            selected,
            queue,
            queue_unshuffled: playlists::load_unshuffled(""),
            queue_point: ResumePoint::load(&self.imp().settings, ""),
        }
    }

    // Puts back the queue and the playlist that dropped tracks replaced,
    // paused where it was
    pub(super) fn undo_replace(&self, replaced: &ReplacedQueue) {
        let imp = self.imp();
        // Like the actions on playlists, while tracks are read
        if imp.loads.get() > 0 {
            return;
        }

        // It could have been deleted while the toast was up
        let selected = if playlists::exists(&replaced.selected) {
            replaced.selected.as_str()
        } else {
            ""
        };

        // The dropped tracks stop first: points are saved from the current
        // track, at idle and when switching, and theirs would take the
        // place of the one put back. The pane stays meanwhile
        self.save_resume_point();
        imp.pending_playlist.replace(Some(selected.to_string()));
        self.player().clear_queue();

        // The order from before shuffling is kept first, so that a crash in
        // between can't lose it
        match &replaced.queue_unshuffled {
            Some(files) => {
                if let Err(e) = playlists::save_unshuffled("", files) {
                    log::warn!("Unable to keep the order of the queue: {e}");
                }
            }
            None => playlists::drop_unshuffled(""),
        }
        playlists::save("", &replaced.queue);
        ResumePoints::update(&imp.settings, |points| match &replaced.queue_point {
            Some(point) => points.set("", point.clone()),
            None => points.remove(""),
        });
        self.load_playlist(selected);
    }

    // Puts the tracks of the playlist in the queue; the tracks of the
    // previous one were saved with each change. They keep playing while
    // the new ones load, which can take a while, rather than leaving
    // nothing to show in the meantime
    pub(super) fn select_playlist(&self, name: &str) {
        if name == self.selected_playlist() {
            return;
        }
        self.load_playlist(name);
    }

    // Even the selected one, to read its tracks again
    fn load_playlist(&self, name: &str) {
        let Some(files) = playlists::load(name) else {
            self.end_pending_playlist();
            self.add_toast(i18n("Unable to open the playlist"));
            return;
        };

        // The tracks of an album could be showing in its place
        self.show_playlist_view();

        if files.is_empty() {
            self.replace_selected_playlist(name);
            self.end_pending_playlist();
        } else {
            self.imp().pending_playlist.replace(Some(name.to_string()));
            self.queue_playlist_tracks(files, name);
        }
    }

    // After undoing a drop, the pane stayed while the queue was emptied,
    // waiting for the tracks
    fn end_pending_playlist(&self) {
        if self.imp().pending_playlist.take().is_some() {
            self.update_library_for_queue();
        }
    }

    // Stops playing the selected playlist, and empties the queue for the
    // tracks of the new one; with none, the album grid takes the place of
    // the player, to add tracks
    pub(super) fn replace_selected_playlist(&self, name: &str) {
        // Where the previous one was, to go back to it: the save at idle
        // would come after the switch. Its tracks played until now, while
        // those of the new one were read
        self.save_resume_point();
        self.player().clear_queue();
        self.set_selected_playlist(name);
        // The tracks of the new one aren't what was searched for
        self.close_queue_search();
    }

    pub(super) fn show_new_playlist_dialog(&self) {
        self.show_playlist_name_dialog(
            &i18n("New Playlist"),
            &i18n("_Create"),
            "",
            Self::create_playlist,
        );
    }

    pub(super) fn show_save_queue_dialog(&self) {
        self.show_playlist_name_dialog(
            &i18n("Save the Queue as a Playlist"),
            &i18n("_Save"),
            "",
            Self::save_queue_as_playlist,
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
                    Err(NameError::TooLong) => i18n("The name is too long"),
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

    // Makes a playlist with the tracks of the queue, in their order; it
    // isn't selected, so that what's playing goes on, and the
    // queue can be cleared without emptying it. The toast can open it
    fn save_queue_as_playlist(&self, name: &str) {
        // The queue in the player, while it's selected, or as last saved
        let files = if self.selected_playlist().is_empty() {
            Some(self.player().queue().files())
        } else {
            playlists::load("")
        };
        let result = files
            .ok_or_else(|| glib::Error::new(gio::IOErrorEnum::Failed, "Unable to read the queue"))
            .and_then(|files| playlists::create_with_tracks(name, &files));
        match result {
            Ok(()) => {
                // Shuffled like the queue, to put back the same order
                playlists::copy_unshuffled("", name);
                self.update_playlists_page();
                self.update_playlists_menu();
                // Translators: `{playlist}` must be left unmodified; it is
                // expanded to the name of a playlist
                let message = i18n_k("Saved as “{playlist}”", &[("playlist", name)]);
                let name = name.to_string();
                self.add_toast_with_button(message, i18n("Open"), move |win| {
                    win.open_saved_queue(&name);
                });
            }
            Err(e) => {
                log::warn!("Unable to save the queue as '{name}': {e}");
                self.add_toast(i18n("Unable to save the playlist"));
            }
        }
    }

    // Selects the playlist the queue was saved as. While the queue is
    // selected with the same tracks, they're already in the player, so
    // they keep playing; otherwise they're loaded from the playlist
    fn open_saved_queue(&self, name: &str) {
        // It could have been renamed or deleted while the toast was up
        if !playlists::exists(name) {
            self.add_toast(i18n("Unable to open the playlist"));
            return;
        }

        let unchanged = self.selected_playlist().is_empty() && {
            let queue = self.player().queue().files();
            playlists::load(name).is_some_and(|files| {
                files.len() == queue.len() && files.iter().zip(&queue).all(|(a, b)| a.equal(b))
            })
        };
        if unchanged {
            // The queue still has the same tracks, and keeps its point
            ResumePoints::update(&self.imp().settings, |points| points.copy("", name));
            self.set_selected_playlist(name);
        } else {
            self.select_playlist(name);
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
        ResumePoints::update(&self.imp().settings, |points| {
            points.rename(name, new_name);
        });

        if self.selected_playlist() == name {
            self.set_selected_playlist(new_name);
        } else {
            self.update_playlists_page();
            self.update_playlists_menu();
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
        ResumePoints::update(&self.imp().settings, |points| points.remove(name));

        if self.selected_playlist() == name {
            self.select_playlist("");
        } else {
            self.update_playlists_page();
        }
        // Even while the queue loads in its place
        self.update_playlists_menu();
    }

    // Makes a saved playlist of an M3U playlist, like one from another
    // player or device, keeping the tracks that are here; it isn't
    // selected, so that what's playing goes on
    pub(super) async fn import_playlist(&self) {
        let dialog = playlist_file_dialog(&i18n("Import a Playlist")).await;
        let Ok(file) = dialog.open_future(Some(self)).await else {
            // Cancelled
            return;
        };
        let Some(path) = file.path() else {
            self.add_toast(i18n("Unable to open the playlist"));
            return;
        };

        // Looking for the tracks blocks
        let read = gio::spawn_blocking(move || {
            let files = playlists::read_m3u(&path)?;
            let n_entries = files.len();
            let found: Vec<gio::File> = files
                .into_iter()
                .filter(|f| f.path().is_some_and(|p| cue_sheet::location_exists(&p)))
                .collect();
            let stem = path.file_stem()?.to_string_lossy().into_owned();
            Some((stem, n_entries, found))
        })
        .await;
        let Ok(Some((stem, n_entries, found))) = read else {
            self.add_toast(i18n("Unable to open the playlist"));
            return;
        };
        if found.is_empty() {
            self.add_toast(i18n("None of the tracks of the playlist were found"));
            return;
        }

        let name = playlists::free_name(&stem);
        if let Err(e) = playlists::create_with_tracks(&name, &found) {
            log::warn!("Unable to import the playlist '{name}': {e}");
            self.add_toast(i18n("Unable to import the playlist"));
            return;
        }
        self.update_playlists_page();
        self.update_playlists_menu();

        let n_missing = (n_entries - found.len()) as u32;
        let message = if n_missing == 0 {
            // Translators: `{playlist}` must be left unmodified; it is
            // expanded to the name of a playlist
            i18n_k("Imported “{playlist}”", &[("playlist", &name)])
        } else {
            ni18n_k(
                // Translators: `{playlist}` and `{n}` must be left
                // unmodified; they are expanded to the name of a playlist,
                // and to the number of its tracks that weren't found
                "Imported “{playlist}”, without one track that wasn't found",
                "Imported “{playlist}”, without {n} tracks that weren't found",
                n_missing,
                &[("playlist", &name), ("n", &n_missing.to_string())],
            )
        };
        self.add_toast(message);
    }

    // Saves a playlist where the user picks, e.g. to copy it with the
    // music to a phone; the tracks in that folder have relative paths
    pub(super) async fn export_playlist(&self, name: &str) {
        let title = if name.is_empty() {
            i18n("Queue")
        } else {
            name.to_string()
        };
        let dialog = playlist_file_dialog(&i18n("Export the Playlist")).await;
        dialog.set_initial_name(Some(&format!("{title}.m3u8")));
        let Ok(file) = dialog.save_future(Some(self)).await else {
            // Cancelled
            return;
        };
        let Some(path) = file.path() else {
            self.add_toast(i18n("Unable to export the playlist"));
            return;
        };

        // Written in another thread, as it's often to a USB stick or a phone
        let export = {
            let name = name.to_string();
            let path = path.clone();
            gio::spawn_blocking(move || playlists::export(&name, &path)).await
        };
        let Ok(export) = export else {
            return;
        };
        let message = match export {
            // Translators: `{playlist}` must be left unmodified; it is
            // expanded to the name of a playlist
            Ok(0) => i18n_k("Exported “{playlist}”", &[("playlist", &title)]),
            Ok(n_outside) => ni18n_k(
                // Translators: `{playlist}` and `{n}` must be left
                // unmodified; they are expanded to the name of a playlist,
                // and to the number of its tracks outside the folder it was
                // exported to
                "Exported “{playlist}”; one track isn't in its folder, so other devices won't find it",
                "Exported “{playlist}”; {n} tracks aren't in its folder, so other devices won't find them",
                n_outside as u32,
                &[("playlist", &title), ("n", &n_outside.to_string())],
            ),
            Err(e) => {
                log::warn!("Unable to export the playlist '{name}' to {path:?}: {e}");
                i18n("Unable to export the playlist")
            }
        };
        self.add_toast(message);
    }
}

// A dialog for M3U playlists, starting in the first music folder that's
// there. Which ones are there is found in another thread, as a network
// drive that's gone can keep it waiting
async fn playlist_file_dialog(title: &str) -> gtk::FileDialog {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(&i18n("Playlists")));
    filter.add_mime_type("audio/x-mpegurl");
    filter.add_mime_type("application/vnd.apple.mpegurl");
    filter.add_suffix("m3u");
    filter.add_suffix("m3u8");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);

    let dialog = gtk::FileDialog::builder()
        .title(title)
        .modal(true)
        .filters(&filters)
        .default_filter(&filter)
        .build();
    let folders = library::music_folders(&utils::settings_manager());
    let folder = gio::spawn_blocking(move || {
        folders
            .into_iter()
            .find(|folder| folder.path.is_dir())
            .map(|folder| folder.path)
    })
    .await
    .ok()
    .flatten();
    if let Some(folder) = folder {
        dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
    }
    dialog
}

// The queue and the playlist that playing dropped tracks replaced
pub(super) struct ReplacedQueue {
    // The name of the playlist that was selected, empty for the queue
    selected: String,
    queue: Vec<gio::File>,
    // The order the queue had before it was shuffled, if it was
    queue_unshuffled: Option<Vec<gio::File>>,
    queue_point: Option<ResumePoint>,
}
