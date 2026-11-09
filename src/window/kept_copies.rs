// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The copies of playlists kept up to date in the export folder, see
// `crate::kept_copies`: which playlists have one, when they're written,
// and what the user is told. A playlist is written a few seconds after it
// last changed, as a drag or a removal saves it at once, and when the
// window closes or the app quits

use std::{
    cell::{Cell, OnceCell, RefCell},
    collections::{BTreeSet, HashSet},
    time::Duration,
};

use adw::prelude::*;

use super::*;
use crate::kept_copies::{self as copies, CopyConfig, Job, Outcome, Report, Writer};

// How long after a playlist's last change its copy is written
const SETTLE_TIME: Duration = Duration::from_secs(3);

// How long after startup the copies of playlists that changed since they
// were written are written, out of the way of the startup's own reads
const STARTUP_DELAY: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub(crate) struct KeptCopies {
    writer: OnceCell<Writer>,
    // The playlists that changed since their copies were last written
    dirty: RefCell<BTreeSet<String>>,
    timer: RefCell<Option<glib::SourceId>>,
    // The playlists whose copies the user was told were changed by another
    // program, once until Dismas writes them again, as each write of a
    // playlist that changed would tell it again; and whether the folder
    // wasn't there, once a session
    told_changed: RefCell<HashSet<String>>,
    told_missing: Cell<bool>,
}

impl Window {
    pub(super) fn setup_kept_copies(&self) {
        let (writer, reports) = Writer::start();
        let _ = self.imp().kept_copies.writer.set(writer);
        let win = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(report) = reports.recv().await {
                let Some(win) = win.upgrade() else {
                    break;
                };
                win.copy_written(report);
            }
        });

        let settings = &self.imp().settings;
        // The copies follow the folder, the music folders, which decide the
        // tracks written relative to the copy and the paths they're given
        // elsewhere, and the way paths are written
        for key in [
            "export-folder",
            "music-folders",
            "export-absolute-paths",
            "export-path-mappings",
        ] {
            settings.connect_changed(
                Some(key),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| win.write_all_copies()
                ),
            );
        }
        settings.connect_changed(
            Some("kept-playlists"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |settings, _| {
                    win.imp()
                        .playlists_page
                        .set_kept_playlists(&copies::kept_playlists(settings));
                }
            ),
        );
        glib::timeout_add_local_once(
            STARTUP_DELAY,
            clone!(
                #[weak(rename_to = win)]
                self,
                move || win.write_changed_copies()
            ),
        );
    }

    // Whether there was an export folder to send it with
    fn send_copy_job(&self, job: impl FnOnce(CopyConfig) -> Job) -> bool {
        let Some(config) = CopyConfig::from_settings(&self.imp().settings) else {
            return false;
        };
        if let Some(writer) = self.imp().kept_copies.writer.get() {
            writer.send(job(config));
        }
        true
    }

    fn is_kept(&self, name: &str) -> bool {
        !name.is_empty()
            && copies::kept_playlists(&self.imp().settings)
                .iter()
                .any(|kept| kept == name)
    }

    // A playlist's file was written: its copy is, once it stops changing
    pub(super) fn playlist_saved(&self, name: &str) {
        if !self.is_kept(name) {
            return;
        }
        let kept = &self.imp().kept_copies;
        kept.dirty.borrow_mut().insert(name.to_string());
        if let Some(source) = kept.timer.take() {
            source.remove();
        }
        let source = glib::timeout_add_local_once(
            SETTLE_TIME,
            clone!(
                #[weak(rename_to = win)]
                self,
                move || {
                    // It ran, so it's gone
                    win.imp().kept_copies.timer.take();
                    win.write_dirty_copies();
                }
            ),
        );
        kept.timer.replace(Some(source));
    }

    fn write_dirty_copies(&self) {
        let names = self.imp().kept_copies.dirty.take();
        for name in names {
            self.send_write(&name, false, false);
        }
    }

    // `force` writes over a copy that another program changed; `first`
    // says that keeping the copy was just turned on, to say so once it's
    // written
    fn send_write(&self, name: &str, force: bool, first: bool) {
        self.send_copy_job(|config| Job::Write {
            name: name.to_string(),
            config,
            force,
            first,
        });
    }

    // Turns keeping a playlist's copy off; the copy stays as it is
    fn stop_keeping(&self, name: &str) {
        copies::update_kept_playlists(&self.imp().settings, |kept| {
            kept.retain(|kept| kept != name);
        });
        self.imp().kept_copies.dirty.borrow_mut().remove(name);
    }

    fn write_all_copies(&self) {
        let names = copies::kept_playlists(&self.imp().settings);
        self.imp().kept_copies.dirty.borrow_mut().extend(names);
        self.write_dirty_copies();
    }

    // At startup, only the copies that are behind their playlists, so that
    // a folder on a drive that isn't plugged in isn't looked for, and those
    // of playlists deleted while it couldn't be reached, which the thread
    // knows of. That's sent even when no playlist is kept
    fn write_changed_copies(&self) {
        let names: Vec<String> = copies::kept_playlists(&self.imp().settings)
            .into_iter()
            .filter(|name| playlists::exists(name))
            .collect();
        self.send_copy_job(|config| Job::Startup { names, config });
    }

    // Writes what changed, and waits for it, for a while at most, as the
    // window closes or the app quits
    pub(crate) fn flush_kept_copies(&self) {
        let kept = &self.imp().kept_copies;
        if let Some(source) = kept.timer.take() {
            source.remove();
        }
        self.write_dirty_copies();
        if let Some(writer) = kept.writer.get() {
            writer.flush();
        }
    }

    // The playlists removed outside the app have no copies kept
    pub(super) fn retain_kept_playlists(&self, names: &[String]) {
        copies::update_kept_playlists(&self.imp().settings, |kept| {
            kept.retain(|name| names.contains(name));
        });
    }

    pub(super) fn kept_playlist_renamed(&self, name: &str, new_name: &str) {
        if !self.is_kept(name) {
            return;
        }
        let settings = &self.imp().settings;
        // The copy can't follow the playlist, so it goes, as with any
        // rename the old name does, rather than stay behind, never written
        // again; the user is told once the thread says whether it went
        if let Some(other) =
            copies::colliding_copy(&copies::kept_playlists(settings), name, new_name)
        {
            self.stop_keeping(name);
            let sent = self.send_copy_job(|config| Job::Remove {
                name: name.to_string(),
                config,
                collided: Some(other.clone()),
            });
            if !sent {
                self.tell_collided(name, &other, false);
            }
            return;
        }
        copies::update_kept_playlists(settings, |kept| {
            if let Some(kept) = kept.iter_mut().find(|kept| *kept == name) {
                *kept = new_name.to_string();
            }
        });
        self.imp().kept_copies.dirty.borrow_mut().remove(name);
        self.send_copy_job(|config| Job::Rename {
            name: name.to_string(),
            new_name: new_name.to_string(),
            config,
        });
        self.send_write(new_name, false, false);
    }

    pub(super) fn kept_playlist_deleted(&self, name: &str) {
        if !self.is_kept(name) {
            return;
        }
        self.stop_keeping(name);
        self.send_copy_job(|config| Job::Remove {
            name: name.to_string(),
            config,
            collided: None,
        });
    }

    // A renamed playlist's copy would have had the name of the copy of
    // `other`. `name` is the old name, which the copy has
    fn tell_collided(&self, name: &str, other: &str, removed: bool) {
        let names = [("playlist", name), ("other", other)];
        self.add_toast(if removed {
            i18n_k(
                // Translators: `{playlist}` and `{other}` must be left
                // unmodified; they are expanded to the old name of a
                // playlist that was renamed, and to the name of another
                // playlist
                "The copy of “{playlist}” was removed from the export folder, as under its new name it would replace the copy of “{other}”",
                &names,
            )
        } else {
            i18n_k(
                // Translators: `{playlist}` and `{other}` must be left
                // unmodified; they are expanded to the old name of a
                // playlist that was renamed, and to the name of another
                // playlist
                "The copy of “{playlist}” is no longer kept up to date, as under its new name it would replace the copy of “{other}”",
                &names,
            )
        });
    }

    // The check item of a playlist's menu. Without an export folder, one
    // is asked for first
    pub(super) async fn set_keep_copy(&self, name: &str, keep: bool) {
        if name.is_empty() || !playlists::exists(name) {
            return;
        }
        let settings = &self.imp().settings;
        if !keep {
            self.stop_keeping(name);
            return;
        }
        if let Some(other) = copies::colliding_copy(&copies::kept_playlists(settings), name, name) {
            self.add_toast(i18n_k(
                // Translators: `{playlist}` must be left unmodified; it is
                // expanded to the name of a playlist
                "The copy of “{playlist}” would have the same name in the export folder",
                &[("playlist", &other)],
            ));
            return;
        }
        if copies::export_folder(settings).is_none() {
            match copies::choose_export_folder(Some(self.upcast_ref()), settings).await {
                Ok(true) => (),
                Ok(false) => return,
                Err(message) => {
                    self.add_toast(message);
                    return;
                }
            }
        }
        copies::update_kept_playlists(settings, |kept| {
            if !kept.iter().any(|kept| kept == name) {
                kept.push(name.to_string());
            }
        });
        self.send_write(name, false, true);
    }

    fn copy_written(&self, report: Report) {
        let Report {
            name,
            outcome,
            asked,
            first,
        } = report;
        // A playlist that's kept no longer, by then
        let outcome = match outcome {
            Outcome::Collided { other, removed } => {
                self.tell_collided(&name, &other, removed);
                return;
            }
            outcome => outcome,
        };
        // Turned off or deleted meanwhile
        if !self.is_kept(&name) {
            return;
        }
        let kept = &self.imp().kept_copies;
        if outcome == Outcome::Written {
            kept.told_changed.borrow_mut().remove(&name);
        }
        match outcome {
            Outcome::Written if first => {
                let folder = copies::export_folder(&self.imp().settings)
                    .map(|folder| utils::home_path(folder.shown_path()))
                    .unwrap_or_default();
                self.add_toast(i18n_k(
                    // Translators: `{playlist}` and `{folder}` must be left
                    // unmodified; they are expanded to the name of a
                    // playlist and to the path of a folder
                    "A copy of “{playlist}” is kept in “{folder}”",
                    &[("playlist", &name), ("folder", &folder)],
                ));
            }
            Outcome::Written => (),
            Outcome::Conflict if first => self.ask_to_replace_copy(&name),
            Outcome::Conflict => {
                if !kept.told_changed.borrow_mut().insert(name.clone()) {
                    return;
                }
                self.add_toast_with_button(
                    i18n_k(
                        // Translators: `{playlist}` must be left unmodified;
                        // it is expanded to the name of a playlist
                        "The copy of “{playlist}” was changed by another program, so it's no longer updated",
                        &[("playlist", &name)],
                    ),
                    i18n("_Replace"),
                    move |win| win.send_write(&name, true, false),
                );
            }
            Outcome::FolderMissing => {
                if asked && !kept.told_missing.replace(true) {
                    self.add_toast(i18n(
                        "The export folder can't be reached, so the copies of playlists aren't kept up to date",
                    ));
                }
            }
            // Told above
            Outcome::Collided { .. } => (),
            Outcome::Failed => {
                if asked {
                    self.add_toast(i18n_k(
                        // Translators: `{playlist}` must be left unmodified;
                        // it is expanded to the name of a playlist
                        "Unable to write the copy of “{playlist}”",
                        &[("playlist", &name)],
                    ));
                }
            }
        }
    }

    // When keeping a copy was turned on, and a file of its name that Dismas
    // didn't write is in the folder
    fn ask_to_replace_copy(&self, name: &str) {
        let dialog = adw::AlertDialog::builder()
            // Translators: `{file}` must be left unmodified; it is expanded
            // to the name of a file
            .heading(i18n_k(
                "Replace “{file}”?",
                &[("file", &copies::copy_file_name(name))],
            ))
            .body(i18n(
                "A file of this name is already in the export folder, and Dismas didn't write it.",
            ))
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_responses(&[("cancel", &i18n("_Cancel")), ("replace", &i18n("_Replace"))]);
        dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        let name = name.to_string();
        dialog.connect_response(
            None,
            clone!(
                #[weak(rename_to = win)]
                self,
                move |_, response| {
                    if response == "replace" {
                        win.send_write(&name, true, true);
                    } else {
                        win.stop_keeping(&name);
                    }
                }
            ),
        );
        dialog.present(Some(self));
    }
}
