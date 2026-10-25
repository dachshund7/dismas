// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use core::cmp::Ordering;
use std::cell::RefCell;

use gtk::{gdk, gio, glib, prelude::*};
use log::{debug, warn};

use crate::{audio::Track, config::APPLICATION_ID};

// The path of a file outside the sandbox, for the files that came through
// the document portal
pub const ATTRIBUTE_HOST_PATH: &str = "xattr::document-portal.host-path";

// The drag that Esc cancelled. The windowing system can go on with it until
// the button is released (on X11, it does), so its drop is refused
thread_local! {
    static CANCELLED_DRAG: RefCell<glib::WeakRef<gdk::Drag>> = RefCell::default();
}

// Cancels the drag of a source, if it's dragging, and says if it was
pub fn cancel_drag(source: &gtk::DragSource) -> bool {
    let Some(drag) = source.drag() else {
        return false;
    };
    CANCELLED_DRAG.with_borrow(|cancelled| cancelled.set(Some(&drag)));
    source.drag_cancel();
    true
}

// Whether a drop is the one of a drag that was cancelled
pub fn is_drag_cancelled(drop: Option<&gdk::Drop>) -> bool {
    let Some(drag) = drop.and_then(gdk::Drop::drag) else {
        return false;
    };
    CANCELLED_DRAG.with_borrow(|cancelled| cancelled.upgrade().as_ref() == Some(&drag))
}

// The main menu, for the menu buttons of each page
pub fn primary_menu() -> gio::MenuModel {
    gtk::Builder::from_resource("/io/github/dachshund7/Dismas/primary-menu.ui")
        .object("primary_menu")
        .expect("The main menu is in the resources")
}

// Gives a page's menu button the main menu. GTK opens the primary menu
// of the visible page with F10, but doesn't close it with F10 again, as
// the menu has the keyboard; its popover takes care of that
pub fn setup_primary_menu_button(button: &gtk::MenuButton) {
    button.set_menu_model(Some(&primary_menu()));
    button.set_primary(true);

    if let Some(popover) = button.popover() {
        let controller = gtk::ShortcutController::new();
        controller.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("F10"),
            Some(gtk::CallbackAction::new(|widget, _| {
                if let Some(popover) = widget.downcast_ref::<gtk::Popover>() {
                    popover.popdown();
                }
                glib::Propagation::Stop
            })),
        ));
        popover.add_controller(controller);
    }
}

// Sets the label of a button that names the selected playlist, which can
// have a long name
pub fn set_playlist_button_label(button: &gtk::Button, label: &str) {
    button.set_label(label);
    // The full label, when it doesn't fit
    button.set_tooltip_text(Some(label));
    if let Some(label) = button.child().and_downcast::<gtk::Label>() {
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(16);
    }
}

// Shows the tooltip of the widget on several lines when it's long, e.g.
// with the name of a long playlist, which GTK shows on a single line, as
// wide as the screen. Connected once; it shows whatever tooltip the widget
// has at the time
pub fn wrap_tooltip(widget: &impl IsA<gtk::Widget>) {
    widget.connect_query_tooltip(|widget, _, _, _, tooltip| {
        let Some(text) = widget.tooltip_text() else {
            return false;
        };
        let label = gtk::Label::builder()
            .label(text.as_str())
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(50)
            .xalign(0.0)
            .build();
        tooltip.set_custom(Some(&label));
        true
    });
}

// Puts the cursor in a search entry when its clear icon is clicked. GTK
// clears the text but leaves the focus where it was, e.g. on the player
pub fn focus_on_clear(entry: &gtk::SearchEntry) {
    let click = gtk::GestureClick::new();
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.connect_pressed(|gesture, _, x, y| {
        let Some(entry) = gesture.widget() else {
            return;
        };
        let on_icon = entry
            .pick(x, y, gtk::PickFlags::DEFAULT)
            .is_some_and(|w| w.is::<gtk::Image>());
        if on_icon && !entry.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN) {
            entry.grab_focus();
        }
    });
    entry.add_controller(click);
}

// A playlist name to put in a sentence, cut short with "…" if it's long,
// e.g. in a title, where it would make several lines
pub fn short_playlist_name(name: &str) -> String {
    const MAX_CHARS: usize = 30;
    if name.chars().count() <= MAX_CHARS {
        return name.to_string();
    }
    let short: String = name.chars().take(MAX_CHARS - 1).collect();
    format!("{}…", short.trim_end())
}

// Selects the tracks of a list from `from` to `to`, both included, in the
// order of the list; returns false if either is not in it
pub fn select_track_range(model: &gio::ListModel, from: &Track, to: &Track) -> bool {
    let position = |track: &Track| {
        (0..model.n_items()).find(|&i| model.item(i).and_downcast_ref::<Track>() == Some(track))
    };
    let (Some(from), Some(to)) = (position(from), position(to)) else {
        return false;
    };

    for i in from.min(to)..=from.max(to) {
        if let Some(track) = model.item(i).and_downcast::<Track>() {
            track.set_selected(true);
        }
    }
    true
}

// Selects the tracks of a list from position `start` up to `end`, or
// unselects them if they are all selected already; returns the last one
pub fn toggle_track_section(model: &gio::ListModel, start: u32, end: u32) -> Option<Track> {
    let tracks: Vec<Track> = (start..end.min(model.n_items()))
        .filter_map(|i| model.item(i).and_downcast::<Track>())
        .collect();
    let selected = !tracks.iter().all(Track::selected);
    for track in &tracks {
        track.set_selected(selected);
    }
    tracks.last().cloned()
}

pub fn settings_manager() -> gio::Settings {
    // We ship a single schema for both default and development profiles
    let app_id = APPLICATION_ID.trim_end_matches(".Devel");
    gio::Settings::new(app_id)
}

// Formats seconds as m:ss, or h:mm:ss from an hour
pub fn format_duration(seconds: u64) -> String {
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
pub fn format_remaining_time(seconds: u64) -> String {
    format!("\u{200e}\u{2212}{}", format_duration(seconds))
}

// A toast whose title is plain text: the titles of toasts are markup by
// default, and they can have the names of playlists and folders, which can
// have "&"
pub fn plain_toast(title: &str) -> adw::Toast {
    adw::Toast::builder().title(title).use_markup(false).build()
}

// A hash in lowercase hex, as it names caches and albums, and has to stay
// the same across versions of the hashing crates
pub fn hex(hash: &[u8]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

// Marks a file of a cache as used, so that `remove_old_files()` keeps it
pub fn touch(path: &std::path::Path) {
    let result = std::fs::File::options()
        .append(true)
        .open(path)
        .and_then(|file| file.set_modified(std::time::SystemTime::now()));
    if let Err(e) = result {
        debug!("Unable to mark {path:?} as used: {e}");
    }
}

// Removes the files of a cache folder that weren't changed or used for
// `age`. The caches of waveforms and covers would otherwise only grow, with
// a file for every track and album ever played; one that's needed again is
// made again
pub fn remove_old_files(dir: &std::path::Path, age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let old = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|since| since > age);
        if metadata.is_file()
            && old
            && let Err(e) = std::fs::remove_file(entry.path())
        {
            warn!("Unable to remove {:?}: {e}", entry.path());
        }
    }
}

// Runs `f`, which reads a file that anyone could have crafted, and returns
// `fallback()` if it panics. The tag reader can panic on a malformed file,
// and without this one file would stop a whole load or scan, every time
pub fn guarded<T>(
    what: impl std::fmt::Display,
    f: impl FnOnce() -> T,
    fallback: impl FnOnce() -> T,
) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        warn!("Skipping {what}, which couldn't be read");
        fallback()
    })
}

// Whether a file is an audio track, guessing from its name only, which is
// much faster than looking at its contents when scanning a whole library
pub fn is_audio_path(path: &std::path::Path) -> bool {
    let (mime_type, _) = gio::content_type_guess(Some(path), None);
    mime_type.starts_with("audio") && !mime_type.ends_with("x-mpegurl")
}

// The files of a folder and its subfolders, in no order
fn load_files_from_folder_internal(
    folder: &gio::File,
    recursive: bool,
    keep: &dyn Fn(&std::path::Path) -> bool,
) -> Vec<gio::File> {
    let Ok(mut enumerator) = folder.enumerate_children(
        "standard::name,standard::type",
        gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
        None::<&gio::Cancellable>,
    ) else {
        warn!("Unable to enumerate {}", folder.uri());
        return Vec::new();
    };

    let mut files = Vec::new();
    while let Some(info) = enumerator.next().and_then(Result::ok) {
        let child = enumerator.child(&info);
        if child.path().is_some_and(|path| !keep(&path)) {
            continue;
        }
        if recursive && info.file_type() == gio::FileType::Directory {
            let mut res = load_files_from_folder_internal(&child, recursive, keep);
            files.append(&mut res);
        } else if info.file_type() == gio::FileType::Regular {
            files.push(child.clone());
        }
    }

    files
}

// What files are sorted by, in the order a file manager shows them: the
// folder they're in, relative to the base folder, then their name
fn file_sort_key(
    base: Option<&gio::File>,
    file: &gio::File,
) -> (
    (bool, glib::FilenameCollationKey),
    (bool, glib::FilenameCollationKey),
) {
    (
        file_name_key(&folder_sort_name(base, file)),
        file_name_key(&file_sort_name(file)),
    )
}

// The folder of the file, relative to the base folder if it's inside it,
// or else by its own name
fn folder_sort_name(base: Option<&gio::File>, file: &gio::File) -> String {
    let Some(parent) = file.parent() else {
        return String::new();
    };
    base.and_then(|base| base.relative_path(&parent))
        .or_else(|| parent.basename())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// Files that aren't paths, like some GVFS locations, have no basename
fn file_sort_name(file: &gio::File) -> String {
    file.basename().map_or_else(
        || file.uri().to_string(),
        |path| path.to_string_lossy().into_owned(),
    )
}

pub fn cmp_file_names(filename_a: &str, filename_b: &str) -> Ordering {
    file_name_key(filename_a).cmp(&file_name_key(filename_b))
}

// The key a name sorts by: the names starting with "." or "#" go last, and
// the others in the order of file names in the locale, with numbers in
// their order, e.g. "Disc 2" before "Disc 10"
fn file_name_key(name: &str) -> (bool, glib::FilenameCollationKey) {
    (
        name.starts_with(['.', '#']),
        glib::FilenameCollationKey::from(name),
    )
}

pub fn load_files_from_folder(folder: &gio::File, recursive: bool) -> Vec<gio::File> {
    load_files_from_folder_filtered(folder, recursive, &|_| true)
}

// The files of a folder, without the files and folders whose paths `keep`
// turns down
pub fn load_files_from_folder_filtered(
    folder: &gio::File,
    recursive: bool,
    keep: &dyn Fn(&std::path::Path) -> bool,
) -> Vec<gio::File> {
    use std::time::Instant;

    let now = Instant::now();
    let mut res = load_files_from_folder_internal(folder, recursive, keep);
    // gio::FileEnumerator has no guaranteed order. The files are sorted
    // once, with the keys made once for each file: a library of thousands
    // of files was sorted again at each level of its folders, with the keys
    // made again at each comparison
    res.sort_by_cached_key(|file| file_sort_key(Some(folder), file));
    debug!(
        "Folder enumeration: {} us (recursive: {}), total files: {}",
        now.elapsed().as_micros(),
        recursive,
        res.len()
    );

    res
}

// Header bars have a window handle inside, which moves the window when
// dragged; turning off its drag gesture keeps its clicks, like the window
// menu. Turning it on puts it back in the bubble phase, where GTK leaves it
pub fn set_window_drag(widget: &gtk::Widget, enabled: bool) {
    if widget.is::<gtk::WindowHandle>() {
        let phase = if enabled {
            gtk::PropagationPhase::Bubble
        } else {
            gtk::PropagationPhase::None
        };
        let controllers = widget.observe_controllers();
        for drag in (0..controllers.n_items())
            .filter_map(|i| controllers.item(i))
            .filter_map(|c| c.downcast::<gtk::GestureDrag>().ok())
        {
            drag.set_propagation_phase(phase);
        }
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        set_window_drag(&widget, enabled);
        child = widget.next_sibling();
    }
}

// A WAV file of silence, 8000 bytes a second
#[cfg(test)]
pub fn write_test_wav(path: &std::path::Path, seconds: u32) {
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

#[cfg(test)]
mod tests {
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
    fn guarded_falls_back_after_a_panic() {
        assert_eq!(guarded("a file", || 1, || 0), 1);
        assert_eq!(guarded("a file", || panic!("malformed"), || 0), 0);
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
