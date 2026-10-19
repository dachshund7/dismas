// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::utils::ATTRIBUTE_HOST_PATH;

impl Window {
    // The menu of a track, at the given point of its row
    pub(super) fn show_track_menu(&self, row: &QueueRow, x: f64, y: f64) {
        let Some(track) = row.track() else {
            return;
        };

        // The album is looked up while the menu opens, and the item stays
        // off for the tracks that are not in the album grid
        let album_id: Rc<RefCell<Option<String>>> = Rc::default();
        let actions = gio::SimpleActionGroup::new();
        let go_to_album = gio::SimpleAction::new("go-to-album", None);
        go_to_album.set_enabled(false);
        go_to_album.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            #[strong]
            album_id,
            move |_, _| {
                let album_id = album_id.borrow().clone();
                if let Some(album_id) = album_id {
                    win.go_to_album(&album_id, &track.file());
                }
            }
        ));
        actions.add_action(&go_to_album);
        glib::spawn_future_local(clone!(
            #[weak]
            go_to_album,
            #[strong]
            track,
            async move {
                if let Some(id) = track_album(&track.file()).await {
                    album_id.replace(Some(id));
                    go_to_album.set_enabled(true);
                }
            }
        ));

        let info = gio::SimpleAction::new("info", None);
        info.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_, _| win.show_track_info_for(&track)
        ));
        actions.add_action(&info);

        let menu = gio::Menu::new();
        menu.append(Some(&i18n("Go to Album")), Some("track.go-to-album"));
        menu.append(Some(&i18n("Track Information")), Some("track.info"));

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

    async fn playing_album(&self) -> Option<(String, gio::File)> {
        let file = self.player()?.state().track()?.file();
        let album_id = track_album(&file).await?;
        Some((album_id, file))
    }

    pub(super) fn go_to_playing_album(&self) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                if let Some((album_id, file)) = win.playing_album().await {
                    win.go_to_album(&album_id, &file);
                }
            }
        ));
    }

    // Going to the album of the playing track needs it in the album grid,
    // which is looked up when the menu opens; the item is off until then
    pub(super) fn setup_more_button(&self) {
        self.imp()
            .playback_control
            .more_button()
            .set_create_popup_func(clone!(
                #[weak(rename_to = win)]
                self,
                move |_| {
                    win.action_set_enabled("win.go-to-album", false);
                    glib::spawn_future_local(clone!(
                        #[weak]
                        win,
                        async move {
                            let found = win.playing_album().await.is_some();
                            win.action_set_enabled("win.go-to-album", found);
                        }
                    ));
                }
            ));
    }

    pub(super) fn show_track_info(&self) {
        if let Some(player) = self.player()
            && let Some(track) = player.state().track()
        {
            self.show_track_info_for(&track);
        }
    }

    // The dialog is modal, so the button that opened it can't take the
    // click that closes it; the click is caught on its way to the dialog,
    // which covers the window. While the pointer hasn't moved since the
    // dialog opened, GTK still sends clicks towards the button, through the
    // window and not the dialog, so the window catches those
    pub(super) fn setup_track_info_button(&self) {
        self.add_controller(self.track_info_close_gesture(self));
    }

    fn track_info_close_gesture(&self, widget: &impl IsA<gtk::Widget>) -> gtk::GestureClick {
        let gesture = gtk::GestureClick::new();
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
        gesture.connect_pressed(clone!(
            #[weak(rename_to = win)]
            self,
            #[weak(rename_to = widget)]
            widget.upcast_ref::<gtk::Widget>(),
            move |gesture, _, x, y| {
                let Some(dialog) = adw::prelude::AdwApplicationWindowExt::visible_dialog(&win)
                    .and_downcast::<TrackInfoDialog>()
                else {
                    return;
                };
                let button = win.imp().playback_control.track_info_button();
                let on_button = button.is_mapped()
                    && widget
                        .compute_point(&button, &graphene::Point::new(x as f32, y as f32))
                        .is_some_and(|p| button.contains(f64::from(p.x()), f64::from(p.y())));
                if on_button {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    adw::prelude::AdwDialogExt::close(&dialog);
                }
            }
        ));
        gesture
    }

    fn show_track_info_for(&self, track: &Track) {
        // Replace the information of another track, instead of stacking
        // dialogs on top of each other; the same track keeps its dialog
        if let Some(open) = adw::prelude::AdwApplicationWindowExt::visible_dialog(self)
            .and_downcast::<TrackInfoDialog>()
        {
            if open.shows(track) {
                return;
            }
            adw::prelude::AdwDialogExt::force_close(&open);
        }

        let dialog = TrackInfoDialog::new(track);

        // Follow the colors of the cover, like the main window
        if self.imp().toast_overlay.has_css_class("main-window") {
            dialog.add_css_class("recolored");
        }

        dialog.add_controller(self.track_info_close_gesture(&dialog));

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
                                self.add_toast(i18n("Unable to open folder"));
                            }
                        }
                    }
                }
            }
        }
    }
}

// The album of a track in the album grid; the database is read in a
// separate thread
async fn track_album(file: &gio::File) -> Option<String> {
    let path = file.path()?;
    gio::spawn_blocking(move || library::track_album(&path))
        .await
        .ok()
        .flatten()
}
