// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

// How long after a scan the music folders that can't be watched are read
// again when the albums are shown. Reading a network share or a folder
// from the document portal takes a while, and going back and forth
// between the pages would read it each time
const UNWATCHED_RESCAN_INTERVAL: Duration = Duration::from_secs(5 * 60);

impl LibraryView {
    fn set_scanning(&self, scanning: bool) {
        if self.imp().scanning.replace(scanning) != scanning {
            self.notify_scanning();
        }
    }

    // Reads the albums from the database, in a separate thread; the albums
    // of the last reload are shown, e.g. the one after a scan rather than
    // the one before it, if it takes longer. Those of the music folders
    // that aren't there are hidden, as they can't play, and so are those of
    // the folders left out, until the scan forgets them
    pub(crate) fn reload(&self) {
        let imp = self.imp();
        let generation = imp.reload_generation.get().wrapping_add(1);
        imp.reload_generation.set(generation);
        let folders = library::music_folders(self.app_settings());

        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let (
                    hidden,
                    albums,
                    albums_unavailable,
                    has_composers,
                    has_performers,
                    has_works,
                    favorites,
                ) = gio::spawn_blocking(move || {
                    let roots = library::roots(&folders);
                    let hidden = [roots.unavailable.as_slice(), &roots.excluded].concat();
                    let albums = library::albums(&hidden);
                    // Only asked while there's no album to show: with
                    // only the folders left out hidden, the albums found
                    // are in the folders that are missing or empty
                    let albums_unavailable = albums.is_empty()
                        && !roots.unavailable.is_empty()
                        && library::has_albums(&roots.excluded);
                    let has_composers = library::has_composers(&hidden);
                    let has_performers = library::has_performers(&hidden);
                    let has_works = library::has_works(&hidden);
                    let shown = albums.iter().map(|album| album.id.as_str()).collect();
                    let favorites = favorite_album_updates(
                        &play_stats::favorite_albums(),
                        &shown,
                        album_of_uri,
                        first_track_uri,
                    );
                    (
                        hidden,
                        albums,
                        albums_unavailable,
                        has_composers,
                        has_performers,
                        has_works,
                        favorites,
                    )
                })
                .await
                .unwrap_or_default();
                if this.imp().reload_generation.get() == generation {
                    play_stats::update_favorite_albums(&favorites);
                    // Read again here, rather than kept from the other
                    // thread, which would leave out an album marked while
                    // the albums were read
                    this.imp()
                        .favorite_albums
                        .replace(play_stats::favorite_album_ids());
                    this.imp().hidden_folders.replace(hidden);
                    // Before the albums, which can switch to the page and
                    // give its button the focus, and also when there were
                    // no albums before either, as the grid then stays as it
                    // is, but the folders can have changed
                    this.imp().albums_unavailable.set(albums_unavailable);
                    this.update_empty_page();
                    this.show_albums(albums);
                    // Also when the albums are the same, and the grid keeps
                    // them
                    this.apply_favorite_albums();
                    this.set_pickers_available(has_composers, has_performers);
                    // The works are read again, as the tracks or the hidden
                    // folders can have changed
                    let works_generation = &this.imp().works_generation;
                    works_generation.set(works_generation.get().wrapping_add(1));
                    this.set_works_available(has_works);
                }
            }
        ));
    }

    fn show_albums(&self, albums: Vec<library::AlbumInfo>) {
        let imp = self.imp();

        // After a scan that changed nothing, as at most starts, the albums
        // read again are the same, and the grid stays as it is, rather
        // than read the covers it shows again. So does it after tags were
        // changed in a way the grid doesn't show, as when a tagger saves
        // an album, though a search can find other tracks now
        let covers_changed = imp.covers_changed.take();
        let tracks_changed = imp.tracks_changed.take();
        if !covers_changed && *imp.shown_albums.borrow() == albums {
            if tracks_changed && imp.search_matches.borrow().is_some() {
                self.update_search();
            }
            return;
        }
        imp.shown_albums.replace(albums.clone());

        let albums: Vec<Album> = albums.into_iter().map(Album::new).collect();
        debug!("Library: {} albums", albums.len());

        // Keep the selection, minus the albums that are gone; the covers of
        // an open work's recordings that are gone are left out once they're
        // read again
        {
            let mut selected = imp.selected.borrow_mut();
            let ids: HashSet<&str> = albums.iter().map(Album::id_str).collect();
            selected.retain(|key| ids.contains(key.album.as_str()));
            for album in &albums {
                album.set_selected(selected.contains(&album.selection_key()));
            }
        }
        self.update_selection();

        // A cover made again can keep its path, so the covers in the cache
        // go before the albums do: GTK binds the cells on screen to them as
        // they go in, and they'd show the old cover. Otherwise, the covers
        // in the cache are still the ones of these albums
        if covers_changed {
            self.forget_covers();
        }
        if let Some(model) = imp.model.borrow().as_ref() {
            model.splice(0, model.n_items(), &albums);
        }
        self.update_stack();

        // The tracks might have changed since the last search
        if imp.search_matches.borrow().is_some() {
            self.update_search();
        }
    }

    // Files that come, go or change in the music folders are read once
    // they stop changing, as at startup
    pub(super) fn setup_watch(&self) {
        let watch = library::FolderWatch::new(clone!(
            #[weak(rename_to = this)]
            self,
            move || this.start_scan(false)
        ));
        self.imp().watch.set(watch).unwrap();
    }

    // A music folder that can't be watched is read again when the albums
    // are shown, unless it's being read already, or was read in the last
    // few minutes
    pub(crate) fn scan_unwatched(&self) {
        let imp = self.imp();
        let recent = imp
            .last_scan_finished
            .get()
            .is_some_and(|finished| finished.elapsed() < UNWATCHED_RESCAN_INTERVAL);
        if imp.unwatched.get() && !imp.scanning.get() && !recent {
            self.start_scan(false);
        }
    }

    // Shows the albums we know about, then looks for changes in the music
    // folders in a separate thread; `requested` is whether the user asked
    // for it, to tell them what it found. During a scan, another one follows
    // it, as the folders might have changed since it started; the user can't
    // ask for one then, as the action is off
    pub(crate) fn start_scan(&self, requested: bool) {
        let imp = self.imp();
        if imp.scanning.get() {
            imp.rescan_pending.set(true);
            return;
        }
        self.set_scanning(true);
        self.run_scan(requested);
    }

    fn run_scan(&self, requested: bool) {
        let imp = self.imp();
        let folders = library::music_folders(self.app_settings());

        self.reload();
        imp.scan_progress.set_fraction(0.0);
        imp.scan_progress.set_text(None);

        // Looking for the files that changed takes a while in a large
        // folder, before there is anything to count
        let counting = Rc::new(Cell::new(false));
        glib::timeout_add_local(
            Duration::from_millis(100),
            clone!(
                #[weak(rename_to = this)]
                self,
                #[strong]
                counting,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    let imp = this.imp();
                    if counting.get() || !imp.scanning.get() {
                        return glib::ControlFlow::Break;
                    }
                    imp.scan_progress.pulse();
                    imp.update_progress.pulse();
                    glib::ControlFlow::Continue
                }
            ),
        );

        let (sender, receiver) = async_channel::unbounded();
        let thumbnail_sizes = thumbnail_sizes(imp.cover_zoom.get());
        imp.thumbnail_size.set(thumbnail_sizes.wanted);
        std::thread::spawn(move || library::scan(&folders, thumbnail_sizes, &sender));

        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let mut result = ScanResult::default();
                let mut finished = false;
                while let Ok(event) = receiver.recv().await {
                    let imp = this.imp();
                    match event {
                        ScanEvent::Progress(done, total) | ScanEvent::Covers(done, total) => {
                            counting.set(true);
                            let fraction = done as f64 / total.max(1) as f64;
                            let (done, total) = (done.to_string(), total.to_string());
                            let numbers = [("done", done.as_str()), ("total", total.as_str())];
                            let text = if matches!(event, ScanEvent::Covers(..)) {
                                i18n_k(
                                    // Translators: the `{done}` and `{total}` must be left
                                    // unmodified; they are the number of album covers made
                                    // so far and the number of covers to make
                                    "{done} of {total} covers",
                                    &numbers,
                                )
                            } else {
                                i18n_k(
                                    // Translators: the `{done}` and `{total}` must be left
                                    // unmodified; they are the number of tracks read so far
                                    // and the number of tracks to read
                                    "{done} of {total} tracks",
                                    &numbers,
                                )
                            };
                            imp.scan_progress.set_fraction(fraction);
                            imp.scan_progress.set_text(Some(&text));
                            imp.update_progress.set_fraction(fraction);
                        }
                        ScanEvent::Finished(scanned) => {
                            result = scanned;
                            finished = true;
                            break;
                        }
                    }
                }

                // The progress bars stop pulsing for this scan, even when
                // another one follows. The channel also closes if the scan
                // panicked
                counting.set(true);
                this.imp().last_scan_finished.set(Some(Instant::now()));
                if result.covers_changed {
                    this.imp().covers_changed.set(true);
                }
                if result.n_changed > 0 {
                    this.imp().tracks_changed.set(true);
                }
                // The tracks to read again: those that changed, and those
                // whose cover file did
                let uris: Vec<String> = result
                    .updated
                    .iter()
                    .chain(&result.tracks_with_new_cover)
                    .map(|path| gio::File::for_path(path).uri().to_string())
                    .collect();
                let n_changed = u32::try_from(result.n_changed).unwrap_or(u32::MAX);

                // The folders the scan read are watched from now on. Files
                // that came into a new one before it was watched are read
                // by another scan. A scan that panicked doesn't say
                if finished {
                    let imp = this.imp();
                    imp.unwatched.set(result.watch.unwatched);
                    if imp
                        .watch
                        .get()
                        .is_some_and(|watch| watch.set_folders(result.watch))
                    {
                        imp.rescan_pending.set(true);
                    }
                }

                // The next scan goes on without stopping the spinner
                if this.imp().rescan_pending.take() {
                    this.emit_by_name::<()>("scan-finished", &[&uris, &n_changed, &requested]);
                    this.run_scan(false);
                    return;
                }
                this.set_scanning(false);
                this.reload();
                this.emit_by_name::<()>("scan-finished", &[&uris, &n_changed, &requested]);
            }
        ));
    }
}

// What follows the favorite albums through changes to the library: the
// albums whose ids changed, and the first tracks of the albums marked since
// they were last looked up. An album whose id isn't among the `shown` ones
// has another id if its first track, by `album_of_uri`, does; if that's the
// same, the album is only hidden, as on a drive that isn't mounted.
// `first_track_uri` gives the URI of an album's first track, by its id
pub(super) fn favorite_album_updates(
    favorites: &[play_stats::FavoriteAlbum],
    shown: &HashSet<&str>,
    album_of_uri: impl Fn(&str) -> Option<String>,
    first_track_uri: impl Fn(&str) -> Option<String>,
) -> play_stats::FavoriteAlbumUpdates {
    let mut updates = play_stats::FavoriteAlbumUpdates::default();
    for favorite in favorites {
        match &favorite.uri {
            None => {
                if let Some(uri) = first_track_uri(&favorite.id) {
                    updates.uris.push((favorite.id.clone(), uri));
                }
            }
            Some(uri) if !shown.contains(favorite.id.as_str()) => {
                if let Some(id) = album_of_uri(uri).filter(|id| *id != favorite.id) {
                    updates.moves.push((favorite.id.clone(), id));
                }
            }
            Some(_) => {}
        }
    }
    updates
}

// The album of the track with a URI, in the library, whether its folder is
// hidden or not; this blocks
fn album_of_uri(uri: &str) -> Option<String> {
    let path = gio::File::for_uri(uri).path()?;
    library::track_album(&path, &[])
}

// The URI of the first track of an album, whether its folder is hidden or
// not; this blocks
pub(super) fn first_track_uri(album_id: &str) -> Option<String> {
    let path = library::album_tracks(album_id, &TrackFilter::default(), &[])
        .into_iter()
        .next()?;
    Some(gio::File::for_path(path).uri().to_string())
}
