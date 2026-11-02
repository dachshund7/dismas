// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

mod fs;
mod threads;
mod ui;

use core::cmp::Ordering;
use std::cell::{Cell, RefCell};

pub(crate) use fs::*;
use gtk::{gdk, gio, glib, prelude::*};
use log::{debug, warn};
pub(crate) use threads::*;
pub(crate) use ui::*;

use crate::{audio::Track, config::APPLICATION_ID, zoom_row::ZoomRow};

// The path of a file outside the sandbox, for the files that came through
// the document portal
pub(crate) const ATTRIBUTE_HOST_PATH: &str = "xattr::document-portal.host-path";

// The part of a list that changed, past the items it has in common at its
// start and at its end, which `same(old, new)` compares: where it starts,
// the number of old items removed there, and the number of new ones put in
// their place. A list keeps its place by its items, so replacing only
// those keeps it where it was
pub(crate) fn changed_range(
    old_len: usize,
    new_len: usize,
    same: impl Fn(usize, usize) -> bool,
) -> (usize, usize, usize) {
    let shorter = old_len.min(new_len);
    let start = (0..shorter).take_while(|&i| same(i, i)).count();
    let end = (0..shorter - start)
        .take_while(|&i| same(old_len - 1 - i, new_len - 1 - i))
        .count();
    (start, old_len - start - end, new_len - start - end)
}

// A playlist name to put in a sentence, cut short with "…" if it's long,
// e.g. in a title, where it would make several lines
pub(crate) fn short_playlist_name(name: &str) -> String {
    const MAX_CHARS: usize = 30;
    if name.chars().count() <= MAX_CHARS {
        return name.to_string();
    }
    let short: String = name.chars().take(MAX_CHARS - 1).collect();
    format!("{}…", short.trim_end())
}

// A playlist name in a menu, shortened, as a menu is as wide as its
// longest item; underscores in names would be taken for mnemonics
pub(crate) fn playlist_menu_label(name: &str) -> String {
    short_playlist_name(name).replace('_', "__")
}

pub(crate) fn settings_manager() -> gio::Settings {
    // We ship a single schema for both default and development profiles
    let app_id = APPLICATION_ID.trim_end_matches(".Devel");
    gio::Settings::new(app_id)
}

// Formats seconds as m:ss, or h:mm:ss from an hour
pub(crate) fn format_duration(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

// The time left, with a minus sign. An explicit LRM character keeps the
// minus sign in front of the time in RTL locales, instead of getting flipped
// at the end of the text
pub(crate) fn format_remaining_time(seconds: u64) -> String {
    format!("\u{200e}\u{2212}{}", format_duration(seconds))
}

// A hash in lowercase hex, as it names caches and albums, and has to stay
// the same across versions of the hashing crates
pub(crate) fn hex(hash: &[u8]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

// A path to show, with the home folder as "~"
pub(crate) fn home_path(path: &std::path::Path) -> String {
    match path.strip_prefix(glib::home_dir()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

// Whether Dismas runs in the Flatpak sandbox, which Flatpak marks with
// this file
pub(crate) fn in_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").is_file()
}

// Makes a named pipe, which opening as usual would wait on
#[cfg(test)]
pub(crate) fn make_test_pipe(path: &std::path::Path) {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is a C string that lives through the call
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
}

// Runs `f` in another thread, and fails if it hasn't returned within a
// few seconds, so that an open that waits, or work that a crafted file
// makes endless, fails the test rather than stopping the whole run
#[cfg(test)]
pub(crate) fn within_seconds<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || sender.send(f()));
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("it didn't return within 5 s, as when it waits on a named pipe")
}

// A WAV file of silence, 8000 bytes a second
#[cfg(test)]
pub(crate) fn write_test_wav(path: &std::path::Path, seconds: u32) {
    let data_size = 8000 * seconds;
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + data_size).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    // PCM, mono, 8000 Hz, 8000 bytes a second, 1 byte a frame, 8 bits
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(8000u32.to_le_bytes());
    bytes.extend(8000u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(8u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(data_size.to_le_bytes());
    bytes.resize(bytes.len() + data_size as usize, 128);
    std::fs::write(path, bytes).unwrap();
}

// Writes an ID3v2 tag to a test file, as a tagger would, in ID3v2.4 or
// ID3v2.3. It's the only
// code that writes tags: Dismas never writes to music files, which
// clippy.toml enforces
#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "tests need files with tags to read"
)]
pub(crate) fn write_test_id3v2(
    path: &std::path::Path,
    tag: &lofty::id3::v2::Id3v2Tag,
    id3v23: bool,
) {
    use lofty::{config::WriteOptions, tag::TagExt};
    let options = WriteOptions::default().use_id3v23(id3v23);
    tag.save_to_path(path, options).unwrap();
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_item_is_worked_on_once_in_threads() {
        let items: Vec<u32> = (0..100).collect();
        let mut seen = vec![0; items.len()];
        super::in_threads(
            &items,
            4,
            |n| n * 2,
            |i, doubled| {
                assert_eq!(doubled, items[i] * 2);
                seen[i] += 1;
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert!(seen.iter().all(|&n| n == 1));

        // More threads than items, and no items
        let mut count = 0;
        super::in_threads(
            &items[..2],
            8,
            |n| *n,
            |_, _| {
                count += 1;
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert_eq!(count, 2);
        let none: [u32; 0] = [];
        super::in_threads(&none, 4, |n| *n, |_, _| Err("called")).unwrap();
    }

    #[test]
    fn an_error_stops_the_threads() {
        // Each item takes long enough that the threads can't get through
        // them all before the first result is handed over
        let items: Vec<u32> = (0..200).collect();
        let worked = std::sync::atomic::AtomicUsize::new(0);
        let result = super::in_threads(
            &items,
            4,
            |_| {
                std::thread::sleep(std::time::Duration::from_millis(1));
                worked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            },
            |_, ()| Err("stop"),
        );
        assert_eq!(result, Err("stop"));
        assert!(worked.into_inner() < items.len());
    }

    use super::*;

    #[test]
    fn folders_are_listed_like_a_file_manager_shows_them() {
        let root = std::env::temp_dir().join(format!("dismas-utils-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // The files right in the folder sort by its own name, "M"
        let base = root.join("M");
        for path in [
            "b.flac",
            "a.flac",
            ".hidden.flac",
            "Disc 10/x.flac",
            "Disc 2/x.flac",
            "Album/track10.flac",
            "Album/track2.flac",
            ".git/x.flac",
        ] {
            let path = base.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }

        let files = load_files_from_folder(&gio::File::for_path(&base), true);
        let names: Vec<String> = files
            .iter()
            .map(|file| {
                let path = file.path().unwrap();
                path.strip_prefix(&base).unwrap().display().to_string()
            })
            .collect();
        assert_eq!(
            names,
            [
                "Album/track2.flac",
                "Album/track10.flac",
                "Disc 2/x.flac",
                "Disc 10/x.flac",
                "a.flac",
                "b.flac",
                ".hidden.flac",
                ".git/x.flac",
            ]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn only_folders_that_are_still_there_can_have_files_left() {
        let error = |code| glib::Error::new(code, "");
        assert!(!may_have_files(&error(gio::IOErrorEnum::NotFound)));
        assert!(!may_have_files(&error(gio::IOErrorEnum::NotDirectory)));
        assert!(may_have_files(&error(gio::IOErrorEnum::PermissionDenied)));
        assert!(may_have_files(&error(gio::IOErrorEnum::TimedOut)));
        assert!(may_have_files(&error(gio::IOErrorEnum::Failed)));
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_read_is_noted() {
        use std::os::unix::fs::PermissionsExt;

        // Root reads any folder, as in the containers of CI
        // SAFETY: geteuid has no preconditions
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let root = std::env::temp_dir().join(format!("dismas-unread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let locked = root.join("Locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(root.join("a.flac"), "").unwrap();
        std::fs::write(locked.join("b.flac"), "").unwrap();

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let tree = read_folder_tree(&gio::File::for_path(&root), true, &|_| true);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let files: Vec<_> = tree.files.iter().filter_map(FileExt::path).collect();
        assert_eq!(files, [root.join("a.flac")]);
        assert_eq!(tree.unread, [locked]);

        // A folder that isn't there has nothing left in it
        let gone = read_folder_tree(&gio::File::for_path(root.join("Gone")), true, &|_| true);
        assert!(gone.unread.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn playlists_are_not_audio_but_webm_files_can_be() {
        use std::path::Path;
        for name in ["a.flac", "b.MP3", "c.opus", "d.m4a", "e.mka", "f.webm"] {
            assert!(is_audio_path(Path::new(name)), "{name}");
        }
        for name in [
            "a.m3u", "b.m3u8", "c.pls", "d.asx", "e.wax", "f.wvx", "g.mp4", "h.txt",
        ] {
            assert!(!is_audio_path(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn keeps_short_playlist_names() {
        assert_eq!(short_playlist_name("Mix"), "Mix");
        let name = "a".repeat(30);
        assert_eq!(short_playlist_name(&name), name);
    }

    #[test]
    fn cuts_long_playlist_names() {
        let short = short_playlist_name(&"é".repeat(40));
        assert_eq!(short, format!("{}…", "é".repeat(29)));
        assert_eq!(short.chars().count(), 30);
        // No space before the ellipsis
        let name = format!("{} {}", "a".repeat(28), "b".repeat(10));
        assert_eq!(short_playlist_name(&name), format!("{}…", "a".repeat(28)));
    }

    #[test]
    fn old_files_of_a_cache_are_removed() {
        use std::time::{Duration, SystemTime};

        let dir = std::env::temp_dir().join(format!("dismas-cache-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("folder")).unwrap();
        let old = dir.join("old.peaks");
        let used = dir.join("used.peaks");
        std::fs::write(&old, "").unwrap();
        std::fs::write(&used, "").unwrap();
        let long_ago = SystemTime::now() - Duration::from_secs(100 * 24 * 60 * 60);
        for path in [&old, &used] {
            std::fs::File::options()
                .append(true)
                .open(path)
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        }
        touch(&used);

        remove_old_files(&dir, Duration::from_secs(90 * 24 * 60 * 60));
        assert!(!old.exists());
        assert!(used.exists());
        assert!(dir.join("folder").is_dir());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn formats_duration() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(59), "0:59");
        assert_eq!(format_duration(15 * 60), "15:00");
        assert_eq!(format_duration(3600), "1:00:00");
        assert_eq!(format_duration(2 * 3600 - 1), "1:59:59");
        assert_eq!(format_remaining_time(75), "\u{200e}\u{2212}1:15");
    }

    #[test]
    fn only_the_part_of_a_list_that_changed_is_replaced() {
        let range =
            |old: &[u32], new: &[u32]| changed_range(old.len(), new.len(), |o, n| old[o] == new[n]);
        assert_eq!(range(&[1, 2, 3], &[1, 2, 3]), (3, 0, 0));
        assert_eq!(range(&[1, 3], &[1, 2, 3]), (1, 0, 1));
        assert_eq!(range(&[1, 2, 3], &[1, 3]), (1, 1, 0));
        assert_eq!(range(&[1, 2, 3], &[1, 4, 3]), (1, 1, 1));
        assert_eq!(range(&[], &[1, 2]), (0, 0, 2));
        assert_eq!(range(&[1, 2], &[]), (0, 2, 0));
        assert_eq!(range(&[1, 2, 3], &[4, 5]), (0, 3, 2));
        // The items in common at the start and the end don't overlap
        assert_eq!(range(&[1, 1], &[1, 1, 1]), (2, 0, 1));
    }

    #[test]
    fn guarded_falls_back_after_a_panic() {
        assert_eq!(guarded("a file", || 1, || 0), 1);
        assert_eq!(guarded("a file", || panic!("malformed"), || 0), 0);
    }

    #[test]
    fn a_named_pipe_is_turned_down_without_waiting() {
        use std::{io::Read, os::fd::AsRawFd};

        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-test-XXXXXX")).unwrap();
        let pipe = dir.join("cover.jpg");
        make_test_pipe(&pipe);
        let path = pipe.clone();
        let opened = within_seconds(move || open_regular_file(&path).map(drop));
        assert_eq!(opened.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
        // As when the path was swapped for the pipe once it was checked
        let opened = within_seconds(move || open_if_regular(&pipe).map(drop));
        assert_eq!(opened.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);

        // A regular file is read as usual
        let path = dir.join("folder.jpg");
        std::fs::write(&path, "cover").unwrap();
        let mut file = open_regular_file(&path).unwrap();
        // SAFETY: the descriptor is open for the call
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(flags & libc::O_NONBLOCK, 0);
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        assert_eq!(text, "cover");

        // Neither a folder nor a file that isn't there is opened
        assert!(open_regular_file(&dir).is_err());
        let gone = open_regular_file(&dir.join("front.jpg")).unwrap_err();
        assert_eq!(gone.kind(), std::io::ErrorKind::NotFound);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writes_hashes_as_before() {
        use sha2::{Digest, Sha256};

        assert_eq!(
            hex(&Sha256::digest("abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
