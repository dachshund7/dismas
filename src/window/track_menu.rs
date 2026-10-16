// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::utils::ATTRIBUTE_HOST_PATH;

impl Window {
    // The menu of a track, at the given point of its row
    pub(super) fn show_track_menu(&self, row: &QueueRow, x: f64, y: f64, kind: TrackMenu) {
        let Some(track) = row.track() else {
            return;
        };
        let album_id = track.file().path().and_then(|p| library::track_album(&p));

        let actions = gio::SimpleActionGroup::new();
        let go_to_album = gio::SimpleAction::new("go-to-album", None);
        go_to_album.set_enabled(album_id.is_some());
        go_to_album.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(album_id) = &album_id {
                    win.go_to_album(album_id);
                }
            }
        ));
        actions.add_action(&go_to_album);

        let info = gio::SimpleAction::new("info", None);
        info.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_, _| win.show_track_info_for(&track)
        ));
        actions.add_action(&info);

        let remove = gio::SimpleAction::new("remove", None);
        remove.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_, _| {
                win.remove_track(&track);
                if let Some(player) = win.player() {
                    utils::store_playlist(player.queue());
                }
            }
        ));
        actions.add_action(&remove);

        let add = gio::SimpleAction::new("add", None);
        // Like adding the whole album, not while tracks are being added
        add.set_enabled(self.imp().album_tracks.adding_enabled());
        add.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_, _| win.queue_tracks(vec![track.file()], QueueMode::Add)
        ));
        actions.add_action(&add);

        let select = gio::SimpleAction::new("select", None);
        select.set_enabled(match kind {
            TrackMenu::Playlist => !self.playlist_selection(),
            TrackMenu::Album => !self.imp().album_tracks.selection_mode(),
        });
        select.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_, _| match kind {
                TrackMenu::Playlist => win.toggle_playlist_track(&track),
                TrackMenu::Album => win.imp().album_tracks.toggle_track(&track),
            }
        ));
        actions.add_action(&select);

        let menu = gio::Menu::new();
        menu.append(Some(&i18n("Go to Album")), Some("track.go-to-album"));
        menu.append(Some(&i18n("Track Information")), Some("track.info"));
        menu.append(Some(&i18n("Select")), Some("track.select"));
        let section = gio::Menu::new();
        match kind {
            TrackMenu::Playlist => {
                section.append(Some(&i18n("Remove from Playlist")), Some("track.remove"))
            }
            TrackMenu::Album => section.append(Some(&i18n("Add to Playlist")), Some("track.add")),
        }
        menu.append_section(None, &section);

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.insert_action_group("track", Some(&actions));
        popover.set_parent(row);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        // The menu item is activated after the popover closes, so it can
        // only go once the action ran
        popover.connect_closed(|popover| {
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
    }

    pub(super) fn copy_track(&self) {
        if let Some(player) = self.player() {
            let state = player.state();
            if let Some(track) = state.track() {
                let s = i18n_k(
                    // Translators: `{title}` and `{artist}` must be left
                    // untranslated; they will expand to the title and
                    // artist of the currently playing track, respectively
                    "Currently playing “{title}” by “{artist}”",
                    &[("title", &track.title()), ("artist", &track.artist())],
                );
                self.clipboard().set_text(&s);
            }
        }
    }

    pub(super) fn show_track_info(&self) {
        if let Some(player) = self.player()
            && let Some(track) = player.state().track()
        {
            self.show_track_info_for(&track);
        }
    }

    fn show_track_info_for(&self, track: &Track) {
        let dialog = TrackInfoDialog::new(track);

        // Follow the colors of the cover, like the main window
        if self.imp().toast_overlay.has_css_class("main-window") {
            dialog.add_css_class("recolored");
        }

        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    pub(super) async fn open_track_directory(&self) {
        if let Some(player) = self.player() {
            let state = player.state();

            if let Some(track) = state.track() {
                let file = track.file();

                match file
                    .query_info_future(
                        ATTRIBUTE_HOST_PATH,
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await
                {
                    Err(err) => debug!("Unable to get host-path attribute: {err}"),
                    Ok(info) => {
                        let host_path = info
                            .attribute_as_string(ATTRIBUTE_HOST_PATH)
                            .and_then(|x| PathBuf::from_str(x.as_str()).ok());

                        if let Some(path) = host_path.or_else(|| file.path()) {
                            let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
                            if let Err(_e) = launcher
                                .open_containing_folder_future(Some(
                                    self.upcast_ref::<gtk::Window>(),
                                ))
                                .await
                            {
                                self.add_toast(i18n("Unable to open folder"))
                            }
                        }
                    }
                }
            }
        }
    }
}
