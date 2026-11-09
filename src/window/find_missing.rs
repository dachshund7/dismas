// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// Find Missing Tracks: looks for the tracks of a playlist whose files are
// gone where they moved in the music folders, as an imported playlist's
// are looked for, and among the library's tracks, see
// `playlists::find_moved()`, and puts them back in their places. The play
// history isn't moved: it keeps a track's listens, its totals and whether
// it's a favorite by its URI, and the totals and the favorites of the new
// URI would have to be merged with those of the old

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use super::*;
use crate::{cue_sheet, playlists, resume_point::ResumePoints};

// How many of a playlist's missing tracks were found
struct Found {
    found: usize,
    missing: usize,
}

impl Window {
    // Runs once no tracks are being read, and counts as a load itself, so
    // that a switch to the playlist, or tracks added to it, wait for it:
    // they would go into the queue or the file meanwhile, and be lost
    // when it's saved
    pub(super) fn find_missing_tracks(&self, name: &str) {
        let name = name.to_string();
        self.after_loads(move |win| {
            if !name.is_empty() && !playlists::exists(&name) {
                return;
            }
            win.set_loading(true);
            glib::spawn_future_local(clone!(
                #[weak]
                win,
                async move {
                    let found = if name == win.selected_playlist() {
                        Some(win.find_missing_in_queue(&name).await)
                    } else {
                        win.find_missing_in_file(&name).await
                    };
                    win.set_loading(false);
                    win.run_after_loads();
                    match found {
                        Some(found) => win.add_toast(found_message(&found)),
                        None => win.add_toast(i18n("Unable to open the playlist")),
                    }
                }
            ));
        });
    }

    // The selected playlist, whose missing tracks are in the queue, where
    // they take the tags of their files in place. Only those whose file
    // wasn't there are looked for; one that can't be read is there. A file
    // can be back since the queue was read, as when its drive is mounted
    // again: it's read where it is, as the Finder would only find it there
    async fn find_missing_in_queue(&self, name: &str) -> Found {
        let queue = self.player().queue();
        let missing: Vec<Track> = queue
            .model()
            .iter::<Track>()
            .flatten()
            .filter(|track| track.missing_reason() == Some(Missing::NotFound))
            .collect();
        if missing.is_empty() {
            return Found {
                found: 0,
                missing: 0,
            };
        }
        let n_missing = missing.len();
        let paths: Vec<PathBuf> = missing
            .iter()
            .filter_map(|track| track.file().path())
            .collect();
        let (back, gone) = gio::spawn_blocking(move || {
            paths
                .into_iter()
                .partition::<Vec<PathBuf>, _>(|path| file_exists(path))
        })
        .await
        .unwrap_or_default();
        let back: HashSet<PathBuf> = back.into_iter().collect();
        let moved = find_moved(gone).await;
        // Still in the queue, as it is now
        let queued: HashSet<Track> = queue.model().iter::<Track>().flatten().collect();
        let found_tracks: Vec<(Track, gio::File)> = missing
            .into_iter()
            .filter(|track| queued.contains(track))
            .filter_map(|track| {
                let path = track.file().path()?;
                let file = if back.contains(&path) {
                    track.file()
                } else {
                    gio::File::for_path(moved.get(&path)?)
                };
                Some((track, file))
            })
            .collect();
        let mut uris = self.update_moved_tracks(found_tracks).await;
        let found = uris.len();
        // Those back where they were keep their places in the files
        uris.retain(|uri, file| file.uri().as_str() != uri.as_str());
        if !uris.is_empty() {
            self.store_queue();
            if let Err(e) = playlists::relocate_unshuffled(name, &uris) {
                log::warn!("Unable to save the order of '{name}' from before it was shuffled: {e}");
            }
            ResumePoints::update(&self.imp().settings, |points| points.relocate(name, &uris));
        }
        Found {
            found,
            missing: n_missing,
        }
    }

    // A playlist that isn't selected, whose tracks are only in its file;
    // the tracks of its preview take the tags of their files in place
    async fn find_missing_in_file(&self, name: &str) -> Option<Found> {
        let missing = {
            let name = name.to_string();
            gio::spawn_blocking(move || {
                let missing: Vec<PathBuf> = playlists::load(&name)?
                    .iter()
                    .filter_map(gio::File::path)
                    .filter(|path| !file_exists(path))
                    .collect();
                Some(missing)
            })
            .await
            .ok()
            .flatten()?
        };
        let n_missing = missing.len();
        let moved = find_moved(missing).await;
        let uris: HashMap<String, gio::File> = moved
            .into_iter()
            .map(|(old, new)| {
                (
                    gio::File::for_path(old).uri().to_string(),
                    gio::File::for_path(new),
                )
            })
            .collect();
        if !uris.is_empty() {
            // The load this counts as kept appends and switches out since
            // the file was read, so it's read again as it is now
            if let Err(e) = playlists::relocate(name, &uris) {
                log::warn!("Unable to save the playlist '{name}': {e}");
                return None;
            }
            ResumePoints::update(&self.imp().settings, |points| points.relocate(name, &uris));
            self.shown_playlist_changed(name, &uris).await;
        }
        Some(Found {
            found: uris.len(),
            missing: n_missing,
        })
    }

    // The tracks of the preview of a playlist whose files moved, if it's
    // shown, take the tags of their new files. A read of it going on
    // starts over, as it could have read the old file
    async fn shown_playlist_changed(&self, name: &str, uris: &HashMap<String, gio::File>) {
        if self.reading_preview_of(name) {
            self.show_playlist_preview(name);
            return;
        }
        if self.shown_playlist().as_deref() != Some(name) {
            return;
        }
        let moved: Vec<(Track, gio::File)> = self
            .imp()
            .album_tracks
            .tracks()
            .into_iter()
            .filter_map(|track| {
                let file = uris.get(&track.uri())?.clone();
                Some((track, file))
            })
            .collect();
        self.update_moved_tracks(moved).await;
    }

    // Reads the new files of tracks that moved, and gives each track the
    // tags of its file, if the file gives one track, as it did before:
    // a file that a CUE sheet splits now stays missing. Returns the URIs
    // the tracks had, with their new files
    async fn update_moved_tracks(
        &self,
        moved: Vec<(Track, gio::File)>,
    ) -> HashMap<String, gio::File> {
        let receiver = load_tracks(moved.iter().map(|(_, file)| file.clone()).collect(), true);
        let mut uris = HashMap::new();
        for (track, file) in moved {
            let Ok(data) = receiver.recv().await else {
                break;
            };
            let Ok([data]) = <[_; 1]>::try_from(data) else {
                continue;
            };
            if data.is_missing() {
                continue;
            }
            uris.insert(track.uri(), file);
            track.update(data);
            self.show_updated_track(&track);
        }
        uris
    }
}

// Whether a track's file is there, or its CUE sheet's for a track of one.
// This blocks
fn file_exists(path: &Path) -> bool {
    cue_sheet::location_exists(path) || path.exists()
}

// Where the files moved, by their old paths, looked for in another thread,
// as the music folders can be on a slow drive
async fn find_moved(paths: Vec<PathBuf>) -> HashMap<PathBuf, PathBuf> {
    if paths.is_empty() {
        return HashMap::new();
    }
    let music_folders = library::music_folder_paths();
    gio::spawn_blocking(move || {
        playlists::find_moved(&paths, &music_folders, || {
            playlists::LibraryIndex::new(library::track_paths())
        })
    })
    .await
    .unwrap_or_default()
    .into_iter()
    .collect()
}

fn found_message(found: &Found) -> String {
    let Found { found, missing } = *found;
    if missing == 0 {
        return i18n("No tracks are missing");
    }
    let n = missing.to_string();
    let found_text = found.to_string();
    if found == missing {
        ni18n_k(
            // Translators: `{n}` must be left unmodified; it is expanded to
            // the number of tracks of a playlist whose files were missing,
            // and that were all found
            "Found the missing track",
            "Found all {n} missing tracks",
            missing as u32,
            &[("n", &n)],
        )
    } else if found == 0 {
        ni18n_k(
            // Translators: `{n}` must be left unmodified; it is expanded to
            // the number of tracks of a playlist whose files are missing
            "The missing track wasn't found in the music folders",
            "None of the {n} missing tracks were found in the music folders",
            missing as u32,
            &[("n", &n)],
        )
    } else {
        ni18n_k(
            // Translators: `{found}` and `{n}` must be left unmodified;
            // they are expanded to the number of tracks found, and to the
            // number of tracks of a playlist whose files were missing
            "Found {found} of {n} missing track",
            "Found {found} of {n} missing tracks",
            missing as u32,
            &[("found", &found_text), ("n", &n)],
        )
    }
}
