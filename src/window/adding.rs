// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

// Tracks to play next or add to a playlist, from wherever they're chosen
#[derive(Clone)]
pub(super) enum Tracks {
    // Tracks of the queue, which are moved rather than added again
    InQueue(Vec<Track>),
    // Tracks of the album shown in the pane, which shows the queue again
    // when they go to it, as with their Add button
    ShownAlbum(Vec<gio::File>),
    // Albums of the grid, each with what its tracks have to match
    Albums(Vec<(String, TrackFilter)>),
}

impl Tracks {
    // The files of the tracks; those of albums leave out the tracks in the
    // folders the grid hides
    async fn files(self, win: &Window) -> Option<Vec<gio::File>> {
        match self {
            Tracks::InQueue(tracks) => Some(tracks.iter().map(Track::file).collect()),
            Tracks::ShownAlbum(files) => Some(files),
            Tracks::Albums(albums) => win.album_files(albums).await,
        }
    }
}

impl Window {
    // Puts the tracks after the playing one, or after the movements of its
    // work, and after those put there before while they're still to come;
    // see `Queue::play_next_before()`
    pub(super) fn play_next(&self, tracks: Tracks) {
        // Like adding tracks, while others are read
        if self.imp().loads.get() > 0 {
            return;
        }

        match tracks {
            Tracks::InQueue(tracks) => {
                self.move_to_play_next(tracks);
                return;
            }
            Tracks::ShownAlbum(_) => self.show_playlist_view(),
            Tracks::Albums(_) => {}
        }

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let Some(files) = tracks.files(&win).await else {
                    return;
                };
                win.load_into_queue(
                    files,
                    QueueMode::Add,
                    LoadOptions {
                        place: Place::Next,
                        ..Default::default()
                    },
                );
            }
        ));
    }

    // Tracks of the queue are moved, without the playing one, which
    // stays where it is
    fn move_to_play_next(&self, mut moved: Vec<Track>) {
        let player = self.player();
        let queue = player.queue();
        let current = queue.current();
        moved.retain(|t| Some(t) != current.as_ref());
        if moved.is_empty() {
            return;
        }

        let before = queue.play_next_before(&moved);
        player.move_tracks(&moved, before.as_ref());
        queue.mark_played_next(&moved);
        self.store_queue();
        let moved: HashSet<&Track> = moved.iter().collect();
        let first = queue
            .model()
            .iter::<Track>()
            .flatten()
            .find(|t| moved.contains(t));
        if let Some(first) = first {
            self.reveal_queue_track(&first);
        }
    }

    // Adds the tracks at the end of a playlist, the queue for an empty
    // name. The selected one gets them through the queue, which saves it;
    // another one in its file, so that what plays goes on. The toast can
    // open it
    pub(super) fn add_to_playlist(&self, tracks: Tracks, name: &str) {
        if self.imp().loads.get() > 0 {
            return;
        }
        // It could have been renamed or deleted while the menu was open
        if !name.is_empty() && !playlists::exists(name) {
            self.add_toast(i18n("Unable to add to the playlist"));
            return;
        }
        let to_queue = name == self.selected_playlist();
        if to_queue && matches!(tracks, Tracks::ShownAlbum(_)) {
            self.show_playlist_view();
        }

        let name = name.to_string();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let Some(files) = tracks.files(&win).await else {
                    return;
                };
                if to_queue {
                    win.queue_tracks(files, QueueMode::Add);
                } else {
                    win.append_to_playlist(&name, &files);
                }
            }
        ));
    }

    fn append_to_playlist(&self, name: &str, files: &[gio::File]) {
        // A playlist being switched to was read already, and would be
        // saved from the queue without them
        if self.imp().loads.get() > 0 || name == self.selected_playlist() {
            self.add_toast(i18n("Unable to add to the playlist"));
            return;
        }
        match playlists::append(name, files) {
            Ok(()) => self.add_added_to_playlist_toast(name, files.len()),
            Err(e) => {
                log::warn!("Unable to add tracks to the playlist '{name}': {e}");
                self.add_toast(i18n("Unable to add to the playlist"));
            }
        }
    }

    // Makes a playlist with the tracks `source` gives, without selecting
    // it, so that what plays goes on. They're taken once the playlist is
    // named, as taking the selected albums leaves selection mode, and the
    // selection would be lost if the dialog was cancelled
    pub(super) fn add_to_new_playlist(&self, source: impl Fn(&Self) -> Option<Tracks> + 'static) {
        if self.imp().loads.get() > 0 {
            return;
        }
        self.show_playlist_name_dialog(
            &i18n("New Playlist"),
            &i18n("_Create"),
            "",
            move |win, name| {
                let Some(tracks) = source(win) else {
                    return;
                };
                let name = name.to_string();
                glib::spawn_future_local(clone!(
                    #[weak]
                    win,
                    async move {
                        let Some(files) = tracks.files(&win).await else {
                            return;
                        };
                        win.create_playlist_with(&name, &files);
                    }
                ));
            },
        );
    }

    fn create_playlist_with(&self, name: &str, files: &[gio::File]) {
        match playlists::create_with_tracks(name, files) {
            Ok(()) => {
                self.update_playlists_page();
                self.update_playlists_menu();
                self.add_added_to_playlist_toast(name, files.len());
            }
            Err(e) => {
                log::warn!("Unable to create the playlist '{name}': {e}");
                self.add_toast(i18n("Unable to create the playlist"));
            }
        }
    }

    fn add_added_to_playlist_toast(&self, name: &str, n_tracks: usize) {
        let n = n_tracks.to_string();
        let message = if name.is_empty() {
            ni18n_k(
                // Translators: `{n}` must be left unmodified; it is
                // expanded to the number of tracks added
                "Added one track to the queue",
                "Added {n} tracks to the queue",
                n_tracks as u32,
                &[("n", &n)],
            )
        } else {
            ni18n_k(
                // Translators: `{playlist}` and `{n}` must be left
                // unmodified; they are expanded to the name of a playlist,
                // and to the number of tracks added to it
                "Added one track to “{playlist}”",
                "Added {n} tracks to “{playlist}”",
                n_tracks as u32,
                &[("playlist", name), ("n", &n)],
            )
        };
        let name = name.to_string();
        self.add_toast_with_button(message, i18n("Open"), move |win| {
            win.open_playlist(&name);
        });
    }

    // Switches to the playlist tracks were added to, which stops what
    // plays, like any switch
    fn open_playlist(&self, name: &str) {
        // It could have been renamed or deleted while the toast was up
        if !name.is_empty() && !playlists::exists(name) {
            self.add_toast(i18n("Unable to open the playlist"));
            return;
        }
        // Like the other ways to switch, while tracks are read
        if self.imp().loads.get() == 0 {
            self.select_playlist(name);
        }
    }

    // The items of a menu that add tracks, for the actions of
    // `add_adding_actions()`: Play Next, the item that adds them to the
    // selected playlist, with its name shortened like the other playlists of
    // a menu, and the other playlists
    pub(super) fn append_adding_items(
        &self,
        section: &gio::Menu,
        prefix: &str,
        play_next: bool,
        to_selected: bool,
    ) {
        if play_next {
            section.append(
                Some(&i18n("Play Next")),
                Some(&format!("{prefix}.play-next")),
            );
        }
        if to_selected {
            let selected = self.selected_playlist();
            let label = add_to_label(&utils::playlist_menu_label(&selected));
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(
                Some(&format!("{prefix}.add-to")),
                Some(&selected.to_variant()),
            );
            section.append_item(&item);
        }
        section.append_submenu(Some(&i18n("Add to Playlist")), &self.add_to_menu(prefix));
    }

    // The playlists that tracks can be added to besides the selected one,
    // which has an item or a button of its own: the queue first, as in the
    // menu of the pane's title, the saved ones, and a new one. Their items
    // activate `{prefix}.add-to` with the name, and `{prefix}.add-to-new`
    pub(super) fn add_to_menu(&self, prefix: &str) -> gio::Menu {
        let selected = self.selected_playlist();
        let names = std::iter::once(String::new()).chain(playlists::names());
        let others = gio::Menu::new();
        for name in names.filter(|name| *name != selected) {
            let label = if name.is_empty() {
                i18n("Queue")
            } else {
                utils::playlist_menu_label(&name)
            };
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(
                Some(&format!("{prefix}.add-to")),
                Some(&name.to_variant()),
            );
            others.append_item(&item);
        }

        let new = gio::Menu::new();
        new.append(
            Some(&i18n("New Playlist…")),
            Some(&format!("{prefix}.add-to-new")),
        );

        let menu = gio::Menu::new();
        menu.append_section(None, &others);
        menu.append_section(None, &new);
        menu
    }

    // The menu of an Add button, with the other ways to add the tracks
    pub(super) fn add_button_menu(&self, prefix: &str) -> gio::Menu {
        let play_next = gio::Menu::new();
        play_next.append(
            Some(&i18n("Play Next")),
            Some(&format!("{prefix}.play-next")),
        );
        let menu = gio::Menu::new();
        menu.append_section(None, &play_next);
        menu.append_section(Some(&i18n("Add to Playlist")), &self.add_to_menu(prefix));
        menu
    }

    // The actions of Play Next and of the items of `add_to_menu()`, for the
    // tracks `source` gives when one is activated. The actions of a menu
    // made while tracks are read are off; they do nothing then anyway
    pub(super) fn add_adding_actions(
        &self,
        group: &gio::SimpleActionGroup,
        enabled: bool,
        source: impl Fn(&Self) -> Option<Tracks> + 'static,
    ) {
        let source = Rc::new(source);

        let play_next = gio::SimpleAction::new("play-next", None);
        play_next.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            source,
            move |_, _| {
                if let Some(tracks) = source(&win) {
                    win.play_next(tracks);
                }
            }
        ));

        let add_to = gio::SimpleAction::new("add-to", Some(glib::VariantTy::STRING));
        add_to.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            source,
            move |_, target| {
                let Some(name) = target.and_then(String::from_variant) else {
                    return;
                };
                if let Some(tracks) = source(&win) {
                    win.add_to_playlist(tracks, &name);
                }
            }
        ));

        let add_to_new = gio::SimpleAction::new("add-to-new", None);
        add_to_new.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            source,
            move |_, _| {
                let source = source.clone();
                win.add_to_new_playlist(move |win| source(win));
            }
        ));

        for action in [play_next, add_to, add_to_new] {
            action.set_enabled(enabled);
            group.add_action(&action);
        }
    }
}

// The label of what adds tracks to a playlist, by its name, the queue for
// an empty one
pub(super) fn add_to_label(name: &str) -> String {
    if name.is_empty() {
        i18n("Add to Queue")
    } else {
        // Translators: `{playlist}` must be left unmodified; it is
        // expanded to the name of a playlist
        i18n_k("Add to {playlist}", &[("playlist", name)])
    }
}
