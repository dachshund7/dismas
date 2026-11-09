// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::{
    adding::Tracks,
    albums::{LookedUpAction, MenuLookups, look_up_album_and_work, new_menu_request, popup_menu},
    *,
};

// Which menu of a track the items are for, see `append_track_items()`
#[derive(Clone, Copy, PartialEq, Eq)]
enum TrackMenu {
    // A row of the queue, or of the preview of a playlist or a smart
    // playlist in the pane of an album's tracks, whose tracks come from
    // many albums
    Row,
    // A row of an album's tracks
    AlbumTracks,
    // The More menu of the playing track
    Playing,
}

impl Window {
    // The menu of a track, at the given point of its row; a row of the
    // queue, rather than of an album's tracks, can be removed. Both can be
    // played next, added to a playlist and marked as favorites, besides
    // the items of every menu of a track, see `append_track_items()`
    pub(super) fn show_track_menu(&self, row: &QueueRow, x: f64, y: f64, in_queue: bool) {
        let Some(track) = row.track() else {
            return;
        };

        let actions = gio::SimpleActionGroup::new();
        let menu = gio::Menu::new();

        // On a selected track, they take all the selected ones, like
        // removing it; on another, only that one
        let album_tracks = self.imp().album_tracks.get();
        let player = self.player();
        let queue = player.queue();
        let selection = track.selected()
            && if in_queue {
                self.queue_selection()
            } else {
                album_tracks.selection_mode()
            };
        let tracks = match (in_queue, selection) {
            (true, true) => Tracks::InQueue(queue.selected_tracks()),
            (true, false) => Tracks::InQueue(vec![track.clone()]),
            (false, true) => Tracks::Shown(album_tracks.files()),
            (false, false) => Tracks::Shown(vec![track.file()]),
        };
        let enabled = self.imp().loads.get() == 0;
        self.add_adding_actions(&actions, enabled, move |_| Some(tracks.clone()));

        let section = gio::Menu::new();
        // The playing track is where Play Next would put it, and the tracks
        // of the queue are in the selected playlist already, as those of
        // a playlist on the tracks page are in it
        let playing = !selection && in_queue && queue.current().as_ref() == Some(&track);
        let already_in = if in_queue {
            Some(self.selected_playlist())
        } else {
            self.shown_playlist()
        };
        self.append_adding_items(&section, "track", !playing, already_in.as_deref());
        menu.append_section(None, &section);

        let section = gio::Menu::new();

        // Only the track clicked, even when it's selected, as the selected
        // tracks can be some favorites and some not
        let favorite = play_stats::is_favorite(&track.uri());
        let mark_favorite = gio::SimpleAction::new("favorite", None);
        mark_favorite.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_, _| win.mark_favorite(&track, !favorite)
        ));
        actions.add_action(&mark_favorite);
        let label = if favorite {
            i18n("Remove from Favorites")
        } else {
            i18n("Add to Favorites")
        };
        section.append(Some(&label), Some("track.favorite"));

        // While a search hides some of the tracks of the queue, the track
        // can be shown among all of them, to add or move tracks around it
        if in_queue && self.queue_filtering() {
            let show = gio::SimpleAction::new("show-in-playlist", None);
            show.connect_activate(clone!(
                #[weak(rename_to = win)]
                self,
                #[strong]
                track,
                move |_, _| win.show_in_playlist(&track)
            ));
            actions.add_action(&show);
            section.append(
                Some(&i18n("Show in Playlist")),
                Some("track.show-in-playlist"),
            );
        }

        // A playlist's preview, and a smart playlist's, which has no
        // `shown_playlist()`, show tracks without an album, as the queue
        // does
        let kind = if !in_queue && album_tracks.album_id().is_some() {
            TrackMenu::AlbumTracks
        } else {
            TrackMenu::Row
        };
        let mut lookups = MenuLookups::new(&menu);
        self.append_track_items(&section, &actions, &track, kind, &mut lookups);
        menu.append_section(None, &section);

        // On a selected track, it removes all the selected ones, like the
        // button of selection mode; on another, only that one. It comes
        // last, away from the pointer, as the menu opens under it
        if in_queue {
            let remove = gio::SimpleAction::new("remove", None);
            remove.connect_activate(clone!(
                #[weak(rename_to = win)]
                self,
                #[strong]
                track,
                move |_, _| {
                    if selection {
                        win.remove_selected_tracks();
                    } else {
                        win.remove_tracks(std::slice::from_ref(&track));
                    }
                }
            ));
            actions.add_action(&remove);

            let several = selection && queue.n_selected_tracks() > 1;
            let label = if several {
                i18n("Remove Selected Tracks")
            } else {
                i18n("Remove")
            };
            let section = gio::Menu::new();
            section.append(Some(&label), Some("track.remove"));
            menu.append_section(None, &section);
        }

        // Unless the row shows another track by then, as the list scrolled
        lookups.open_when_done(
            self,
            clone!(
                #[weak]
                row,
                move |win: &Window| {
                    if row.is_mapped() && row.track().as_ref() == Some(&track) {
                        popup_menu(win, &menu, "track", &actions, row.upcast_ref(), x, y);
                    }
                }
            ),
        );
    }

    // Appends to a section of a menu the items that every menu of a track
    // has, in the same order: those of the track's rows and the More menu
    // of the playing track. The album and the work are looked up in one
    // go (see `MenuLookups`), and their items stay off for the tracks that
    // aren't in the library. In fullscreen, where only the More menu
    // opens, the items that would leave the player are left out: going to
    // a page would last until leaving fullscreen, and a folder or booklet
    // opens in another app, over the player. The actions hold the track
    // until the menu goes, so that they still act on it after the rows
    // were given new tracks, as a reload of the album shown does
    fn append_track_items(
        &self,
        section: &gio::Menu,
        actions: &gio::SimpleActionGroup,
        track: &Track,
        kind: TrackMenu,
        lookups: &mut MenuLookups,
    ) {
        let in_fullscreen = self.is_fullscreen();
        // The tracks of an album show all of it already, and go to the
        // track instead
        let go_to_album = (kind != TrackMenu::AlbumTracks && !in_fullscreen).then(|| {
            let go_to_album = LookedUpAction::new(
                actions,
                "go-to-album",
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    #[strong]
                    track,
                    move |album_id: &String| win.go_to_album(album_id, Some(&track.file()))
                ),
            );
            section.append(Some(&i18n("Go to Album")), Some("track.go-to-album"));
            go_to_album
        });
        let path = track.file().path();
        let go_to_work = (track.has_work() && !in_fullscreen)
            .then(|| self.add_go_to_work_item(section, actions, "track", path.clone()));
        if (go_to_album.is_some() || go_to_work.is_some())
            && let Some(path) = path
        {
            let hidden = self.imp().library_view.hidden_folders();
            let (album_wanted, work_wanted) = (go_to_album.is_some(), go_to_work.is_some());
            lookups.spawn(async move {
                let looked_up = gio::spawn_blocking(move || {
                    look_up_album_and_work(path, &hidden, album_wanted, work_wanted)
                })
                .await;
                let Ok((album, work)) = looked_up else {
                    return;
                };
                if let Some(go_to_work) = go_to_work
                    && let Some(key) = work
                {
                    go_to_work.set((key, album.clone()));
                }
                if let Some(go_to_album) = go_to_album
                    && let Some(album) = album
                {
                    go_to_album.set(album);
                }
            });
        }

        // Ctrl+C does the same on the track's row, or else on the playing
        // track, but isn't an accel (see `setup_edit_shortcuts()`)
        let copy = gio::SimpleAction::new("copy", None);
        copy.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_, _| win.copy_track_details(&track)
        ));
        actions.add_action(&copy);
        let item = gio::MenuItem::new(Some(&i18n("Copy")), Some("track.copy"));
        item.set_attribute_value("accel", Some(&"<primary>c".to_variant()));
        section.append_item(&item);

        if !in_fullscreen {
            let open_directory = gio::SimpleAction::new("open-directory", None);
            open_directory.connect_activate(clone!(
                #[weak(rename_to = win)]
                self,
                #[strong]
                track,
                move |_, _| {
                    glib::spawn_future_local(clone!(
                        #[weak]
                        win,
                        #[strong]
                        track,
                        async move { win.open_track_directory(&track).await }
                    ));
                }
            ));
            actions.add_action(&open_directory);
            section.append(Some(&i18n("Open Folder")), Some("track.open-directory"));

            if let Some(path) = track.file().path() {
                self.add_booklet_item(section, lookups, move || vec![path]);
            }
        }

        let info = gio::SimpleAction::new("info", None);
        info.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            track,
            move |_, _| win.show_track_info_for(&track)
        ));
        actions.add_action(&info);
        let item = gio::MenuItem::new(Some(&i18n("Track Information")), Some("track.info"));
        // Ctrl+I shows the playing track's, so only its menu shows the key
        if kind == TrackMenu::Playing {
            item.set_attribute_value("accel", Some(&"<primary>i".to_variant()));
        }
        section.append_item(&item);
    }

    // Ctrl+C: the track of the row with the focus, in the playlist pane or
    // an album's tracks, as the key goes by the focus, or else the playing
    // track
    pub(super) fn copy_focused_track(&self) {
        let track = utils::focused_child::<QueueRow>(self)
            .and_then(|row| row.track())
            .or_else(|| self.player().state().track());
        if let Some(track) = track {
            self.copy_track_details(&track);
        }
    }

    fn copy_track_details(&self, track: &Track) {
        self.clipboard().set_text(&track.copied_details());
    }

    // The More menu of the playing track, with the items of a track's menu
    // and then the lyrics. It's made again when another track plays, the
    // window goes in or out of fullscreen, a scan ends or the playing
    // track's tags are read again (`rebuild_more_menu()`), so that its
    // items are for the playing track and the window as it is, and so that
    // it opens at once with Go to Album and Go to Work on and Open Booklet
    // in place, as they were looked up before. The button tells the menu's
    // lookups whether it's open, so that an item found while it is doesn't
    // push the items below it down (see `MenuAdder`), and stops a row's
    // menu that still waits for its lookups from opening over it. A menu
    // that missed a change or an item while open is made again once it
    // closes, when idle, as it closes before the action of the item
    // chosen runs, and making it again then would take the action away
    pub(super) fn setup_more_button(&self) {
        self.imp()
            .playback_control
            .more_button()
            .connect_active_notify(clone!(
                #[weak(rename_to = win)]
                self,
                move |button| {
                    let open = button.is_active();
                    if open {
                        new_menu_request(&win);
                    }
                    let mut held_back = false;
                    if let Some(adder) = win.imp().more_menu_adder.borrow().as_ref() {
                        adder.set_open(open);
                        held_back = adder.held_back();
                    }
                    if !open && (win.imp().more_menu_stale.take() || held_back) {
                        glib::idle_add_local_once(clone!(
                            #[weak]
                            win,
                            move || win.refresh_more_menu()
                        ));
                    }
                }
            ));
    }

    // Makes the More menu again (see `setup_more_button()`), and closes it
    // if it's open. The actions are new each time too, so a lookup that
    // ends after the menu was made again only reaches the menu it was for,
    // which is gone; and so is the popover, as one kept from the last menu
    // kept its size. It must not run from an action of the menu, which it
    // would take away while the item is activated
    pub(super) fn rebuild_more_menu(&self) {
        let imp = self.imp();
        let button = imp.playback_control.more_button();
        // First, so that closing the menu doesn't make it again, see
        // `setup_more_button()`
        imp.more_menu_adder.take();
        imp.more_menu_stale.set(false);
        button.popdown();
        let Some(track) = self.player().state().track() else {
            button.set_menu_model(None::<&gio::MenuModel>);
            // Its actions hold the last track, and so its cover
            button.insert_action_group("track", None::<&gio::ActionGroup>);
            // Its items would have nothing to act on
            button.set_sensitive(false);
            return;
        };
        imp.more_menu_with_work.set(track.has_work());
        // Not inside a row, so the rows' groups of the same name don't
        // reach it
        let actions = gio::SimpleActionGroup::new();
        button.insert_action_group("track", Some(&actions));
        let section = gio::Menu::new();
        let menu = gio::Menu::new();
        let mut lookups = MenuLookups::new(&menu);
        self.append_track_items(&section, &actions, &track, TrackMenu::Playing, &mut lookups);
        // Hidden in fullscreen, where the pane doesn't show, see
        // `update_lyrics_action()`
        let lyrics = gio::MenuItem::new(Some(&i18n("Lyrics")), Some("win.lyrics"));
        lyrics.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
        section.append_item(&lyrics);
        menu.append_section(None, &section);
        button.set_menu_model(Some(&menu));
        button.set_sensitive(true);
        // Last, as setting the model notifies `active`, whose handler
        // reads it
        imp.more_menu_adder.replace(Some(lookups.adder()));
    }

    // Makes the More menu again, or once it closes if it's open, for a
    // change that the menu open can do without, as a scan's
    pub(super) fn refresh_more_menu(&self) {
        if self.imp().playback_control.more_button().is_active() {
            self.imp().more_menu_stale.set(true);
        } else {
            self.rebuild_more_menu();
        }
    }

    pub(super) fn show_track_info(&self) {
        if let Some(track) = self.player().state().track() {
            self.show_track_info_for(&track);
        }
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

        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    async fn open_track_directory(&self, track: &Track) {
        self.open_containing_folder(&track.audio_file()).await;
    }

    // Opens the folder `file` is in, with `file` selected. It takes the
    // file's own path, even for one that came through the document portal:
    // in the Flatpak, the launcher hands the open file to the portal, which
    // works out its folder outside the sandbox, while the path outside the
    // sandbox can't be opened inside it
    pub(super) async fn open_containing_folder(&self, file: &gio::File) {
        if file.path().is_none() {
            return;
        }

        let launcher = gtk::FileLauncher::new(Some(file));
        if let Err(err) = launcher
            .open_containing_folder_future(Some(self.upcast_ref::<gtk::Window>()))
            .await
        {
            debug!("Unable to open the folder of {}: {err}", file.uri());
            self.add_toast(i18n("Unable to open folder"));
        }
    }
}
