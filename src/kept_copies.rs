// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// Copies of playlists kept up to date in an export folder, for phones,
// servers and other players, which read playlists from a folder, written
// as exports are (`playlists::export_text()`). Dismas is the only writer
// of a copy: it never reads one back, and it doesn't write over a copy
// that another program changed since Dismas wrote it, such as a phone's
// app through Syncthing, unless the user says to; every program that
// let others write to its playlist files too lost tracks or kept stale
// ones. Whether a copy is still as Dismas wrote it is told by its time and
// size, as Dismas recorded them, or else by its contents' hash, as FAT
// drives round times to 2 seconds and move them by an hour when daylight
// saving time starts or ends. The records are a file of their own, as they
// change at each write, which the settings would spread to every program
// watching them.
//
// The copies are written by one thread, in the order asked, as an export
// folder can be on a slow drive, and a rename must come before the write
// of the playlist under its new name. It's a thread of its own, rather
// than tasks on the main loop, so that the copies can be written as the
// app quits, after the main loop has stopped

use std::{
    collections::HashMap,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use gtk::{gio, glib, prelude::*};
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    i18n::i18n,
    library::{self, MusicFolder},
    playlists::{self, ExportOptions},
    utils,
};

const RECORDS_FILE: &str = "kept-copies.json";

// The longest it waits for the copies at quit, as the folder can be on a
// network drive that's gone
const FLUSH_TIME: Duration = Duration::from_secs(5);

// The export folder in the settings, kept as a music folder is, see
// `library::folder_from_entry()`: where the copies are written, as the app
// reaches it, and its path outside the sandbox, if it came through the
// document portal, which the paths of the tracks are made relative to
pub(crate) fn export_folder(settings: &gio::Settings) -> Option<MusicFolder> {
    let entry = settings.get::<(String, String)>("export-folder");
    if entry.0.is_empty() {
        return None;
    }
    library::folder_from_entry(entry)
}

pub(crate) fn store_export_folder(settings: &gio::Settings, folder: Option<&MusicFolder>) {
    let value = folder.map_or_else(|| (String::new(), String::new()), library::folder_entry);
    if let Err(e) = settings.set("export-folder", value.to_variant()) {
        warn!("Unable to store the export folder: {e}");
    }
}

// Asks for the export folder, and stores it; false if that was cancelled,
// or what to tell the user. It isn't resolved as a music folder is
// (`library::resolve_folder()`), which would take the Music folder's own
// path for the portal's, which the sandbox can only read
pub(crate) async fn choose_export_folder(
    parent: Option<&gtk::Window>,
    settings: &gio::Settings,
) -> Result<bool, String> {
    let dialog = gtk::FileDialog::builder()
        .title(i18n("Choose an Export Folder"))
        .modal(true)
        .build();
    if let Some(folder) = library::music_folders(settings).first() {
        dialog.set_initial_folder(Some(&gio::File::for_path(&folder.path)));
    }
    let Ok(file) = dialog.select_folder_future(parent).await else {
        return Ok(false);
    };
    let Some(path) = file.path() else {
        return Err(i18n("Only local folders can be chosen"));
    };
    // This blocks, as the files of the document portal can be slow
    let checked = path.clone();
    let (in_data, host) = gio::spawn_blocking(move || {
        (
            playlists::is_in_data_dir(&checked),
            library::host_path(&checked),
        )
    })
    .await
    .map_err(|_| i18n("Unable to use the folder"))?;
    if in_data {
        return Err(i18n("Choose a folder outside Dismas's own data"));
    }
    let folder = MusicFolder {
        host,
        ..MusicFolder::new(path)
    };
    store_export_folder(settings, Some(&folder));
    Ok(true)
}

// The playlists whose copies are kept
pub(crate) fn kept_playlists(settings: &gio::Settings) -> Vec<String> {
    settings.get::<Vec<String>>("kept-playlists")
}

// Changes which playlists' copies are kept, and stores them if they
// changed
pub(crate) fn update_kept_playlists(
    settings: &gio::Settings,
    change: impl FnOnce(&mut Vec<String>),
) {
    let old = kept_playlists(settings);
    let mut names = old.clone();
    change(&mut names);
    if names != old
        && let Err(e) = settings.set("kept-playlists", names.to_variant())
    {
        warn!("Unable to store the playlists whose copies are kept: {e}");
    }
}

// The name of a playlist's copy. FAT and exFAT drives, which phones and
// USB sticks have, and Windows, don't take some characters that a
// playlist's name can have, so they're replaced; a name can't have "/",
// see `playlists::check_name()`
pub(crate) fn copy_file_name(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| if "\"*:<>?|\\".contains(c) { '_' } else { c })
        .collect();
    format!("{name}.m3u8")
}

// Whether two playlists would have copies of the same name: on the drives
// that ignore case, as FAT drives and Windows do, too
pub(crate) fn copy_names_collide(a: &str, b: &str) -> bool {
    copy_file_name(a).to_lowercase() == copy_file_name(b).to_lowercase()
}

// The kept playlist other than `name` whose copy would have the name of
// the copy of `candidate`
pub(crate) fn colliding_copy(kept: &[String], name: &str, candidate: &str) -> Option<String> {
    kept.iter()
        .find(|other| *other != name && copy_names_collide(other, candidate))
        .cloned()
}

// What's asked of the thread that writes the copies. Each job has what it
// needs from the settings, which are only read on the main thread
#[derive(Debug)]
pub(crate) struct CopyConfig {
    pub(crate) folder: MusicFolder,
    pub(crate) options: ExportOptions,
}

impl CopyConfig {
    pub(crate) fn from_settings(settings: &gio::Settings) -> Option<Self> {
        Some(Self {
            folder: export_folder(settings)?,
            options: ExportOptions::from_settings(settings),
        })
    }

    fn uri(&self) -> String {
        gio::File::for_path(&self.folder.path).uri().to_string()
    }

    fn copy_path(&self, name: &str) -> PathBuf {
        self.folder.path.join(copy_file_name(name))
    }
}

#[derive(Debug)]
pub(crate) enum Job {
    // A playlist's copy is written, if Dismas wrote what's there, or if
    // it's forced. `first` is whether keeping the copy was just turned on
    Write {
        name: String,
        config: CopyConfig,
        force: bool,
        first: bool,
    },
    // The playlists whose files changed since their copies were written,
    // as when the app quit before it wrote them; like each job, it first
    // removes the copies that couldn't be when they were to go
    Startup {
        names: Vec<String>,
        config: CopyConfig,
    },
    Rename {
        name: String,
        new_name: String,
        config: CopyConfig,
    },
    // `collided` is the kept playlist whose copy has the name the removed
    // one's would take, for a playlist renamed so, which is told whether
    // its copy went
    Remove {
        name: String,
        config: CopyConfig,
        collided: Option<String>,
    },
    Flush(mpsc::Sender<()>),
}

impl Job {
    fn config(&self) -> Option<&CopyConfig> {
        match self {
            Self::Write { config, .. }
            | Self::Startup { config, .. }
            | Self::Rename { config, .. }
            | Self::Remove { config, .. } => Some(config),
            Self::Flush(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Written,
    // Another program changed the copy, or a file of its name that Dismas
    // didn't write is there
    Conflict,
    FolderMissing,
    Failed,
    // A renamed playlist's copy would have had the name of the copy of
    // `other`, so it's no longer kept; `removed` is whether it went
    Collided { other: String, removed: bool },
}

// `asked` is whether the user did something that wrote the copy, rather
// than it being written when the app started, so that a folder that isn't
// there is said only then
#[derive(Debug)]
pub(crate) struct Report {
    pub(crate) name: String,
    pub(crate) outcome: Outcome,
    pub(crate) asked: bool,
    pub(crate) first: bool,
}

// The thread that writes the copies; it ends once this is dropped
#[derive(Debug)]
pub(crate) struct Writer {
    jobs: mpsc::Sender<Job>,
}

impl Writer {
    pub(crate) fn start() -> (Self, async_channel::Receiver<Report>) {
        let (jobs, receiver) = mpsc::channel();
        let (reports, reports_receiver) = async_channel::unbounded();
        let dir = glib::user_data_dir().join("dismas");
        let started = std::thread::Builder::new()
            .name("kept-copies".to_string())
            .spawn(move || run(&receiver, &reports, &dir));
        if let Err(e) = started {
            warn!("Unable to start writing the copies of playlists: {e}");
        }
        (Self { jobs }, reports_receiver)
    }

    pub(crate) fn send(&self, job: Job) {
        if self.jobs.send(job).is_err() {
            warn!("The copies of playlists are no longer written");
        }
    }

    // Waits for the jobs sent so far, for a while at most
    pub(crate) fn flush(&self) {
        let (done, wait) = mpsc::channel();
        self.send(Job::Flush(done));
        if wait.recv_timeout(FLUSH_TIME).is_err() {
            warn!("Not waiting longer for the copies of playlists to be written");
        }
    }
}

fn run(jobs: &mpsc::Receiver<Job>, reports: &async_channel::Sender<Report>, dir: &Path) {
    let report = |name: &str, outcome: Outcome, asked: bool, first: bool| {
        let _ = reports.send_blocking(Report {
            name: name.to_string(),
            outcome,
            asked,
            first,
        });
    };
    let mut records = Records::load(dir);
    while let Ok(job) = jobs.recv() {
        // Before anything is written in the folder, as a new playlist can
        // have the name of one deleted while the folder was away
        if let Some(config) = job.config() {
            remove_pending(config, &mut records);
        }
        match job {
            Job::Write {
                name,
                config,
                force,
                first,
            } => {
                let outcome = guarded(
                    &name,
                    || write_copy(&name, &config, &mut records, force),
                    Outcome::Failed,
                );
                report(&name, outcome, true, first);
            }
            Job::Startup { names, config } => {
                for name in names {
                    if records.is_current(&name, &config, source_stamp(&name)) {
                        continue;
                    }
                    let outcome = guarded(
                        &name,
                        || write_copy(&name, &config, &mut records, false),
                        Outcome::Failed,
                    );
                    report(&name, outcome, false, false);
                }
            }
            Job::Rename {
                name,
                new_name,
                config,
            } => guarded(
                &name,
                || rename_copy(&name, &new_name, &config, &mut records),
                (),
            ),
            Job::Remove {
                name,
                config,
                collided,
            } => {
                let removed = guarded(&name, || remove_copy(&name, &config, &mut records), false);
                if let Some(other) = collided {
                    report(&name, Outcome::Collided { other, removed }, true, false);
                }
            }
            Job::Flush(done) => {
                let _ = done.send(());
                continue;
            }
        }
        records.store(dir);
    }
}

fn guarded<T>(name: &str, job: impl FnOnce() -> T, fallback: T) -> T {
    utils::guarded(format!("the copy of {name:?}"), job, || fallback)
}

// What Dismas recorded of a copy it wrote: the folder it's in, by its URI,
// its time and size, and its hash, and the time and size of the playlist's
// own file it was written from
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    folder: String,
    mtime: (i64, i64),
    size: u64,
    sha256: String,
    source: Stamp,
}

// `removals` are the copies of playlists deleted or renamed while their
// folder couldn't be reached, as when its drive isn't plugged in, which
// are removed once it can be, see `remove_pending()`: forgotten, they'd
// stay there for good, as copies of playlists that are gone
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Records {
    copies: HashMap<String, Record>,
    #[serde(default)]
    removals: Vec<(String, Record)>,
    #[serde(skip)]
    changed: bool,
}

impl Records {
    // A file that can't be read is as none: then each copy that's there is
    // asked about once, or taken as Dismas's if it's what Dismas would
    // write
    fn load(dir: &Path) -> Self {
        let path = dir.join(RECORDS_FILE);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            warn!("Unable to read the records of the copies of playlists: {e}");
            Self::default()
        })
    }

    fn store(&mut self, dir: &Path) {
        if !self.changed {
            return;
        }
        let stored = serde_json::to_string(self)
            .map_err(|e| e.to_string())
            .and_then(|text| {
                glib::mkdir_with_parents(dir, 0o700);
                glib::file_set_contents(dir.join(RECORDS_FILE), text.as_bytes())
                    .map_err(|e| e.to_string())
            });
        match stored {
            Ok(()) => self.changed = false,
            Err(e) => warn!("Unable to store the records of the copies of playlists: {e}"),
        }
    }

    // The record of a copy in this folder
    fn get(&self, name: &str, config: &CopyConfig) -> Option<&Record> {
        self.copies
            .get(name)
            .filter(|record| record.folder == config.uri())
    }

    fn insert(&mut self, name: &str, record: Record) {
        self.copies.insert(name.to_string(), record);
        self.changed = true;
    }

    fn remove_later(&mut self, name: &str, record: Record) {
        self.removals
            .retain(|(removed, old)| removed != name || old.folder != record.folder);
        self.removals.push((name.to_string(), record));
        self.changed = true;
    }

    fn remove(&mut self, name: &str) {
        self.changed |= self.copies.remove(name).is_some();
    }

    // The record of a copy in this folder, which the records then forget
    fn take(&mut self, name: &str, config: &CopyConfig) -> Option<Record> {
        self.get(name, config)?;
        self.changed = true;
        self.copies.remove(name)
    }

    // Whether the copy in this folder was written from the playlist's
    // file as it is now, with the time and size it has, `source`
    fn is_current(&self, name: &str, config: &CopyConfig, source: Option<Stamp>) -> bool {
        self.get(name, config)
            .is_some_and(|record| source == Some(record.source))
    }
}

// The time, in seconds and nanoseconds, and the size of a file
type Stamp = (i64, i64, u64);

// The time and size of a playlist's own file
fn source_stamp(name: &str) -> Option<Stamp> {
    let metadata = std::fs::metadata(playlists::playlist_path(name)?).ok()?;
    Some((metadata.mtime(), metadata.mtime_nsec(), metadata.len()))
}

fn sha256(bytes: &[u8]) -> String {
    utils::hex(&Sha256::digest(bytes))
}

// Whose the file at a copy's place is
#[derive(Debug, PartialEq, Eq)]
enum Owner {
    // There's none
    Nobody,
    // It's as Dismas wrote it
    Dismas,
    // It has what Dismas would write now, whoever wrote it
    Same,
    Other,
}

// Who wrote the file at `path`, from Dismas's record of the copy it wrote
// there, and with `text`, what Dismas would write now. The file is only
// read, never parsed, when its time or size changed: it's a file in a
// folder that other programs write to
fn owner(path: &Path, record: Option<&Record>, text: Option<&[u8]>) -> Owner {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Owner::Nobody,
        Err(e) => {
            warn!("Unable to look at {path:?}: {e}");
            return Owner::Other;
        }
    };
    if !metadata.is_file() {
        return Owner::Other;
    }
    let stamp = ((metadata.mtime(), metadata.mtime_nsec()), metadata.len());
    if let Some(record) = record
        && stamp == (record.mtime, record.size)
    {
        return match text {
            Some(text) if sha256(text) == record.sha256 => Owner::Same,
            _ => Owner::Dismas,
        };
    }
    let mut contents = Vec::new();
    let read = utils::open_regular_file(path).and_then(|file| {
        file.take(playlists::MAX_PLAYLIST_SIZE + 1)
            .read_to_end(&mut contents)
    });
    if let Err(e) = read {
        warn!("Unable to read {path:?}: {e}");
        return Owner::Other;
    }
    if text == Some(contents.as_slice()) {
        Owner::Same
    } else if record.is_some_and(|record| sha256(&contents) == record.sha256) {
        Owner::Dismas
    } else {
        Owner::Other
    }
}

// The record of a copy just written, or found to be what Dismas writes,
// from the playlist's file with the time and size `source`
fn record_of(
    path: &Path,
    text: &[u8],
    config: &CopyConfig,
    source: Option<Stamp>,
) -> Option<Record> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(Record {
        folder: config.uri(),
        mtime: (metadata.mtime(), metadata.mtime_nsec()),
        size: metadata.len(),
        sha256: sha256(text),
        source: source.unwrap_or_default(),
    })
}

fn write_copy(name: &str, config: &CopyConfig, records: &mut Records, force: bool) -> Outcome {
    // A drive that isn't plugged in, or a grant of the portal that was
    // taken back, whose folder isn't made again, in its place
    if !config.folder.path.is_dir() {
        debug!(
            "Not writing the copy of {name:?}, as {:?} isn't there",
            config.folder.path
        );
        return Outcome::FolderMissing;
    }
    // Taken before the playlist is read, so that a save in between makes
    // the copy look behind it, not as new as it
    let source = source_stamp(name);
    let Some(files) = playlists::load(name) else {
        return Outcome::Failed;
    };
    let dir = library::folder_key(&config.folder);
    let (text, _) = playlists::export_text(&files, Some(dir), &config.options);
    let path = config.copy_path(name);
    match owner(&path, records.get(name, config), Some(text.as_bytes())) {
        // What's there is taken for Dismas's
        Owner::Same => (),
        Owner::Other if !force => return Outcome::Conflict,
        Owner::Nobody | Owner::Dismas | Owner::Other => {
            if let Err(e) = glib::file_set_contents(&path, text.as_bytes()) {
                warn!("Unable to write the copy of {name:?} to {path:?}: {e}");
                return Outcome::Failed;
            }
            debug!("Wrote the copy of {name:?} to {path:?}");
        }
    }
    match record_of(&path, text.as_bytes(), config, source) {
        Some(record) => records.insert(name, record),
        None => records.remove(name),
    }
    Outcome::Written
}

// A copy that's as Dismas wrote it takes the playlist's new name, and one
// that was changed stays, the user's now. The playlist is written under
// its new name after this. If a file of the new name is there already,
// Dismas's copy is removed instead, as a rename leaves nothing under the
// old name, and the write leaves that file alone if Dismas didn't write
// it. On a drive that ignores case, a new name that only differs in case
// finds the copy itself, which is then written again under the new name.
// If the folder can't be reached, the copy is removed once it can be, and
// the playlist is written under its new name then
fn rename_copy(name: &str, new_name: &str, config: &CopyConfig, records: &mut Records) {
    let Some(record) = records.take(name, config) else {
        return;
    };
    if !config.folder.path.is_dir() {
        records.remove_later(name, record);
        return;
    }
    let path = config.copy_path(name);
    let new_path = config.copy_path(new_name);
    if new_path.exists() {
        remove_file_of(name, &record, config);
        return;
    }
    if owner(&path, Some(&record), None) != Owner::Dismas {
        return;
    }
    match std::fs::rename(&path, &new_path) {
        // Renaming keeps the time and the size
        Ok(()) => records.insert(new_name, record),
        Err(e) => warn!("Unable to rename the copy {path:?}: {e}"),
    }
}

// A copy that's as Dismas wrote it goes with its playlist, which is in
// the trash: the copy isn't put there too, as the document portal's
// folders and USB sticks often have no trash, or one that phones then sync.
// Returns whether it went; it goes later if the folder can't be reached
fn remove_copy(name: &str, config: &CopyConfig, records: &mut Records) -> bool {
    let Some(record) = records.take(name, config) else {
        return false;
    };
    if !config.folder.path.is_dir() {
        records.remove_later(name, record);
        return false;
    }
    remove_file_of(name, &record, config)
}

fn remove_file_of(name: &str, record: &Record, config: &CopyConfig) -> bool {
    let path = config.copy_path(name);
    if owner(&path, Some(record), None) != Owner::Dismas {
        return false;
    }
    if let Err(e) = std::fs::remove_file(&path) {
        warn!("Unable to remove the copy {path:?}: {e}");
        return false;
    }
    true
}

// Removes the copies of this folder that were to go while it couldn't be
// reached, if it can be now. The drive isn't looked at when none are
fn remove_pending(config: &CopyConfig, records: &mut Records) {
    utils::guarded(
        "the copies of deleted playlists",
        || remove_pending_now(config, records),
        || (),
    );
}

fn remove_pending_now(config: &CopyConfig, records: &mut Records) {
    let uri = config.uri();
    if !records
        .removals
        .iter()
        .any(|(_, record)| record.folder == uri)
        || !config.folder.path.is_dir()
    {
        return;
    }
    let (here, elsewhere) = std::mem::take(&mut records.removals)
        .into_iter()
        .partition::<Vec<_>, _>(|(_, record)| record.folder == uri);
    records.removals = elsewhere;
    records.changed = true;
    for (name, record) in here {
        if remove_file_of(&name, &record, config) {
            debug!("Removed the copy of {name:?}, which was deleted or renamed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A test folder: the export folder, and the copy of "Mix" in it
    fn folder() -> (PathBuf, CopyConfig, PathBuf) {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let folder = dir.join("Playlists");
        std::fs::create_dir(&folder).unwrap();
        let config = CopyConfig {
            folder: MusicFolder::new(folder.clone()),
            options: ExportOptions {
                music_folders: Vec::new(),
                mappings: HashMap::new(),
                absolute: false,
            },
        };
        let copy = folder.join("Mix.m3u8");
        (dir, config, copy)
    }

    fn written(path: &Path, text: &str, config: &CopyConfig, records: &mut Records) {
        std::fs::write(path, text).unwrap();
        let record = record_of(path, text.as_bytes(), config, Some((1, 2, 3))).unwrap();
        records.insert("Mix", record);
    }

    #[test]
    fn copy_names_are_safe_on_fat_drives() {
        assert_eq!(copy_file_name("Road trip"), "Road trip.m3u8");
        assert_eq!(copy_file_name("Live: \"Best\"?"), "Live_ _Best__.m3u8");
        assert_eq!(copy_file_name("a<b>c|d*e\\f"), "a_b_c_d_e_f.m3u8");
    }

    #[test]
    fn names_that_collide_on_fat_drives_are_refused() {
        assert!(copy_names_collide("Mix", "MIX"));
        assert!(copy_names_collide("a:b", "a_b"));
        assert!(!copy_names_collide("Mix", "Mix 2"));
    }

    #[test]
    fn a_copy_dismas_wrote_is_replaced() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        let record = records.get("Mix", &config);
        assert_eq!(
            owner(&copy, record, Some(b"#EXTM3U\nb.flac\n")),
            Owner::Dismas
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_copy_changed_elsewhere_is_left_alone() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        std::fs::write(&copy, "#EXTM3U\na.flac\nphone.flac\n").unwrap();
        let record = records.get("Mix", &config);
        assert_eq!(
            owner(&copy, record, Some(b"#EXTM3U\nb.flac\n")),
            Owner::Other
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_copy_whose_time_alone_changed_is_replaced() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        // As FAT drives move times by an hour
        records.copies.get_mut("Mix").unwrap().mtime.0 -= 3600;
        let record = records.get("Mix", &config);
        assert_eq!(
            owner(&copy, record, Some(b"#EXTM3U\nb.flac\n")),
            Owner::Dismas
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_dismas_did_not_write_is_not_replaced() {
        let (dir, _, copy) = folder();
        std::fs::write(&copy, "#EXTM3U\nmine.flac\n").unwrap();
        assert_eq!(owner(&copy, None, Some(b"#EXTM3U\n")), Owner::Other);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn startup_writes_only_playlists_changed_since_their_copy() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\n", &config, &mut records);
        assert!(records.is_current("Mix", &config, Some((1, 2, 3))));
        assert!(!records.is_current("Mix", &config, Some((1, 2, 4))));
        assert!(!records.is_current("Mix", &config, None));
        assert!(!records.is_current("Trip", &config, Some((1, 2, 3))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_copy_written_without_its_record_is_ours() {
        let (dir, _, copy) = folder();
        std::fs::write(&copy, "#EXTM3U\na.flac\n").unwrap();
        assert_eq!(owner(&copy, None, Some(b"#EXTM3U\na.flac\n")), Owner::Same);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unchanged_copy_is_not_written_again() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        let record = records.get("Mix", &config);
        assert_eq!(
            owner(&copy, record, Some(b"#EXTM3U\na.flac\n")),
            Owner::Same
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_export_folder_is_not_made() {
        let (dir, mut config, _) = folder();
        config.folder.path = dir.join("Unplugged");
        let mut records = Records::default();
        assert_eq!(
            write_copy("Mix", &config, &mut records, true),
            Outcome::FolderMissing
        );
        assert!(!config.folder.path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_symbolic_link_in_place_of_the_copy_is_a_conflict() {
        let (dir, config, copy) = folder();
        let target = dir.join("elsewhere.m3u8");
        std::fs::write(&target, "#EXTM3U\n").unwrap();
        std::os::unix::fs::symlink(&target, &copy).unwrap();
        let mut records = Records::default();
        written(&target, "#EXTM3U\n", &config, &mut records);
        let record = records.get("Mix", &config);
        assert_eq!(owner(&copy, record, Some(b"#EXTM3U\n")), Owner::Other);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn renaming_moves_a_copy_dismas_wrote() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        rename_copy("Mix", "Trip", &config, &mut records);
        assert!(!copy.exists());
        assert!(config.copy_path("Trip").is_file());
        assert!(records.get("Trip", &config).is_some());
        assert!(records.get("Mix", &config).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn renaming_leaves_a_changed_copy() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        std::fs::write(&copy, "#EXTM3U\nphone.flac\n").unwrap();
        rename_copy("Mix", "Trip", &config, &mut records);
        assert!(copy.is_file());
        assert!(!config.copy_path("Trip").exists());
        assert!(records.copies.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn renaming_does_not_overwrite_a_file_of_the_new_name() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        let other = config.copy_path("Trip");
        std::fs::write(&other, "#EXTM3U\nmine.flac\n").unwrap();
        rename_copy("Mix", "Trip", &config, &mut records);
        assert_eq!(
            std::fs::read_to_string(&other).unwrap(),
            "#EXTM3U\nmine.flac\n"
        );
        // Nothing is left under the old name
        assert!(!copy.exists());
        assert!(records.copies.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn deleting_removes_only_a_copy_dismas_wrote() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        assert!(remove_copy("Mix", &config, &mut records));
        assert!(!copy.exists());

        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        std::fs::write(&copy, "#EXTM3U\nphone.flac\n").unwrap();
        assert!(!remove_copy("Mix", &config, &mut records));
        assert!(copy.is_file());
        // Without a record, nothing is Dismas's
        std::fs::write(&copy, "#EXTM3U\n").unwrap();
        assert!(!remove_copy("Mix", &config, &mut records));
        assert!(copy.is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // As a drive is unplugged, and plugged in again
    fn unplugged(config: &CopyConfig, then: impl FnOnce()) {
        let away = config.folder.path.with_file_name("Unplugged");
        std::fs::rename(&config.folder.path, &away).unwrap();
        then();
        std::fs::rename(&away, &config.folder.path).unwrap();
    }

    #[test]
    fn a_copy_deleted_while_its_folder_is_away_goes_once_it_is_back() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        unplugged(&config, || {
            assert!(!remove_copy("Mix", &config, &mut records));
            // Until the folder is back
            remove_pending(&config, &mut records);
        });
        assert!(copy.is_file());
        assert!(records.copies.is_empty());
        assert_eq!(records.removals.len(), 1);
        remove_pending(&config, &mut records);
        assert!(!copy.exists());
        assert!(records.removals.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_copy_renamed_while_its_folder_is_away_goes_once_it_is_back() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        unplugged(&config, || {
            rename_copy("Mix", "Trip", &config, &mut records);
        });
        assert!(records.get("Trip", &config).is_none());
        remove_pending(&config, &mut records);
        assert!(!copy.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_copy_changed_while_its_folder_was_away_stays() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        unplugged(&config, || {
            remove_copy("Mix", &config, &mut records);
        });
        std::fs::write(&copy, "#EXTM3U\nphone.flac\n").unwrap();
        remove_pending(&config, &mut records);
        assert!(copy.is_file());
        assert!(records.removals.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_playlist_renamed_as_one_deleted_while_away_takes_its_place() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        let trip = config.copy_path("Trip");
        std::fs::write(&trip, "#EXTM3U\nb.flac\n").unwrap();
        let record = record_of(&trip, b"#EXTM3U\nb.flac\n", &config, None).unwrap();
        records.insert("Trip", record);
        unplugged(&config, || {
            remove_copy("Trip", &config, &mut records);
        });
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        // As the thread does before each job
        remove_pending(&config, &mut records);
        rename_copy("Mix", "Trip", &config, &mut records);
        assert!(!copy.exists());
        assert_eq!(std::fs::read_to_string(&trip).unwrap(), "#EXTM3U\na.flac\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removals_from_another_folder_wait_for_it() {
        let (dir, mut config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        unplugged(&config, || {
            remove_copy("Mix", &config, &mut records);
        });
        let first = config.folder.path.clone();
        config.folder.path = dir.join("Other");
        std::fs::create_dir(&config.folder.path).unwrap();
        remove_pending(&config, &mut records);
        assert_eq!(records.removals.len(), 1);
        config.folder.path = first;
        remove_pending(&config, &mut records);
        assert!(!copy.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn records_of_another_folder_do_not_count() {
        let (dir, mut config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        config.folder.path = dir.join("Other");
        assert!(records.get("Mix", &config).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn records_are_kept_between_runs() {
        let (dir, config, copy) = folder();
        let mut records = Records::default();
        written(&copy, "#EXTM3U\na.flac\n", &config, &mut records);
        records.store(&dir);
        let loaded = Records::load(&dir);
        assert_eq!(loaded.copies, records.copies);
        // A damaged file is as none
        std::fs::write(dir.join(RECORDS_FILE), "{").unwrap();
        assert!(Records::load(&dir).copies.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
