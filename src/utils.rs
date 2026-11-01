// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use core::cmp::Ordering;
use std::cell::{Cell, RefCell};

use gtk::{gdk, gio, glib, prelude::*};
use log::{debug, warn};

use crate::{audio::Track, config::APPLICATION_ID, zoom_row::ZoomRow};

// The path of a file outside the sandbox, for the files that came through
// the document portal
pub const ATTRIBUTE_HOST_PATH: &str = "xattr::document-portal.host-path";

// The drag that Esc cancelled. The windowing system can go on with it until
// the button is released (on X11, it does), so its drop is refused
thread_local! {
    static CANCELLED_DRAG: RefCell<glib::WeakRef<gdk::Drag>> = RefCell::default();
}

// Fills the heart of a toggle button while it's on, and gives it the
// tooltip that says what a click does then, if there are `tooltips`: to
// add, and to remove
pub fn setup_heart_button(button: &gtk::ToggleButton, tooltips: Option<(String, String)>) {
    button.connect_active_notify(move |button| {
        let on = button.is_active();
        button.set_icon_name(if on {
            "heart-filled-symbolic"
        } else {
            "heart-outline-symbolic"
        });
        if let Some((add, remove)) = &tooltips {
            button.set_tooltip_text(Some(if on { remove } else { add }));
        }
    });
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

// Gives a page's menu button the main menu, with the buttons that zoom the
// page's view at its top, if it has one, as Text Editor's menu has them,
// and the page's own items, `page_items`, under them.
// GTK opens the primary menu of the visible page with F10, but doesn't
// close it with F10 again, as the menu has the keyboard; its popover takes
// care of that
pub fn setup_primary_menu_button(
    button: &gtk::MenuButton,
    zoom: Option<&ZoomRow>,
    page_items: Option<&gio::MenuModel>,
) {
    let menu = primary_menu();
    if zoom.is_some() || page_items.is_some() {
        let full = gio::Menu::new();
        if zoom.is_some() {
            let item = gio::MenuItem::new(None, None);
            item.set_attribute_value("custom", Some(&"zoom".to_variant()));
            let section = gio::Menu::new();
            section.append_item(&item);
            full.append_section(None, &section);
        }
        if let Some(items) = page_items {
            full.append_section(None, items);
        }
        // The main menu is all sections
        for i in 0..menu.n_items() {
            if let Some(section) = menu.item_link(i, gio::MENU_LINK_SECTION) {
                full.append_section(None, &section);
            }
        }
        button.set_menu_model(Some(&full));
    } else {
        button.set_menu_model(Some(&menu));
    }
    button.set_primary(true);

    if let (Some(zoom), Some(popover)) = (zoom, button.popover().and_downcast::<gtk::PopoverMenu>())
    {
        popover.add_child(zoom, "zoom");
    }
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

// The row or the cell that has the focus, in a list or a grid, or in any
// list in a widget, like the window. The focus is on a widget in the row,
// or on the list's item, or a flow box's, whose child is the row or a
// widget around it, like the SlideBin of the queue's rows
pub fn focused_child<W: IsA<gtk::Widget>>(widget: &impl IsA<gtk::Widget>) -> Option<W> {
    let focus = widget.root()?.focus()?;
    if !focus.is_ancestor(widget) {
        return None;
    }
    if let Some(row) = focus.ancestor(W::static_type()) {
        return row.downcast().ok();
    }
    // Only from an item of a list, as the first rows of a list with the
    // focus itself aren't the ones with it
    if !focus.parent().is_some_and(|parent| holds_items(&parent)) {
        return None;
    }
    let mut child = focus.first_child();
    while let Some(widget) = child {
        match widget.downcast::<W>() {
            Ok(row) => return Some(row),
            Err(widget) => child = widget.first_child(),
        }
    }
    None
}

// The child of a list, a grid or a flow box that a widget is in: the list's
// item, or the flow box's, which screen readers present, so it's the one
// that says e.g. whether the widget's item is selected. Only while the
// widget is on screen: a list binds a row before it takes the item the row
// is in, which is still floating then, and a reference to it would sink
// it, so that the list would lose it when the reference is dropped, which
// crashed GTK
pub fn list_item_of(widget: &impl IsA<gtk::Widget>) -> Option<gtk::Widget> {
    if !widget.is_mapped() {
        return None;
    }
    let mut item = widget.upcast_ref::<gtk::Widget>().clone();
    loop {
        let parent = item.parent()?;
        if holds_items(&parent) {
            return Some(item);
        }
        item = parent;
    }
}

fn holds_items(widget: &gtk::Widget) -> bool {
    widget.is::<gtk::ListBase>() || widget.is::<gtk::FlowBox>()
}

// How fast a swipe to the right has to be to go back, in pixels a second
const SWIPE_BACK_VELOCITY: f64 = 500.0;

// On a touchscreen, a swipe to the right over a page of the playlist pane
// goes back, as in libadwaita's navigation views. The swipe never claims
// the touch: a flick up or down is the list's, which cancels it, and the
// rows' long presses and taps stay theirs. The collapsed split view hides
// the pane with a swipe to the left, so the two don't meet
pub fn setup_swipe_back(widget: &impl IsA<gtk::Widget>, back: impl Fn() + 'static) {
    let swipe = gtk::GestureSwipe::new();
    swipe.set_touch_only(true);
    swipe.set_propagation_phase(gtk::PropagationPhase::Capture);
    swipe.connect_swipe(move |swipe, vx, vy| {
        let rtl = swipe
            .widget()
            .is_some_and(|widget| widget.direction() == gtk::TextDirection::Rtl);
        let vx = if rtl { -vx } else { vx };
        if vx > SWIPE_BACK_VELOCITY && vx > 2.0 * vy.abs() {
            back();
        }
    });
    widget.add_controller(swipe);
}

// A list sets the Selected state of each of its items from its selection
// model, none here, each time the item changes, once its row is bound
// (`gtk_list_item_base_update`), which undoes the state a row sets for a
// selection of its own. So while `selecting(row)` says it's in selection
// mode, `update(row)` sets it again once the list is done with the item
pub fn keep_selected_state<W: IsA<gtk::Widget>>(
    list_item: &gtk::ListItem,
    row: &W,
    selecting: fn(&W) -> bool,
    update: fn(&W),
) {
    for property in ["item", "position"] {
        let row = row.downgrade();
        list_item.connect_notify_local(Some(property), move |_, _| {
            let Some(row) = row.upgrade().filter(selecting) else {
                return;
            };
            let row = row.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(row) = row.upgrade() {
                    update(&row);
                }
            });
        });
    }
}

// The Menu key and Shift+F10 open the menu of the row or the cell of a
// list or a grid that has the focus, as a right click on it does. Keys go
// from the focus up, so the shortcut is on the list rather than the rows
pub fn setup_menu_key<W: IsA<gtk::Widget>>(
    list: &impl IsA<gtk::Widget>,
    open: impl Fn(&W) + 'static,
) {
    let controller = gtk::ShortcutController::new();
    controller.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Menu|<Shift>F10"),
        Some(gtk::CallbackAction::new(
            move |list, _| match focused_child::<W>(list) {
                Some(child) => {
                    open(&child);
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            },
        )),
    ));
    list.add_controller(controller);
}

// A right click opens a menu at its point, and so does a long press on a
// touchscreen, but not a mouse held down. Both claim the sequence as it's
// pressed, so the scrolled window around doesn't take it
pub fn setup_context_menu(widget: &impl IsA<gtk::Widget>, open: impl Fn(f64, f64) + 'static) {
    let open = std::rc::Rc::new(open);
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    click.connect_pressed(glib::clone!(
        #[strong]
        open,
        move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            open(x, y);
        }
    ));
    widget.add_controller(click);

    let long_press = gtk::GestureLongPress::builder().touch_only(true).build();
    long_press.connect_pressed(move |gesture, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        open(x, y);
    });
    widget.add_controller(long_press);
}

// Sets the label of a button that names the selected playlist, which can
// have a long name
pub fn set_playlist_button_label(button: &adw::SplitButton, label: &str) {
    button.set_label(label);
    // The full label, when it doesn't fit
    button.set_tooltip_text(Some(label));
    if let Some(label) = button.child().and_downcast::<gtk::Label>() {
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(16);
    }
}

// The frames a list scrolled to its top is kept there at most, while the
// heights of its rows settle
const TOP_FRAMES: u32 = 10;

// Scrolls a list to its very top, with the header of its first section,
// once it's laid out, as long as it's `still_wanted`. GTK keeps a list's
// place by an item: once the items change, it shows the first one at the
// top, with its header out of view, and setting the top in the same frame
// does nothing, or makes a long list go to its end (GTK 4.22). In the
// frames after, while the heights of the rows it hadn't measured settle,
// GTK can put the first item back at the top, below its header, as after
// sorting a long queue, so the top is set again until it stays there for
// two frames in a row
pub fn scroll_to_top_when_laid_out(
    list: &gtk::ListView,
    still_wanted: impl Fn() -> bool + 'static,
) {
    when_laid_out(list, move |list| {
        let frames = Cell::new(0);
        let stayed = Cell::new(0);
        let keep_at_top = move |list: &gtk::ListView| {
            let Some(adjustment) = list.vadjustment().filter(|_| still_wanted()) else {
                return glib::ControlFlow::Break;
            };
            frames.set(frames.get() + 1);
            if adjustment.value() > adjustment.lower() + 0.5 {
                adjustment.set_value(adjustment.lower());
                stayed.set(0);
            } else {
                stayed.set(stayed.get() + 1);
            }
            if stayed.get() >= 2 || frames.get() >= TOP_FRAMES {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        };
        // Right away, as the list would otherwise be drawn where it was
        if keep_at_top(list) == glib::ControlFlow::Continue {
            list.add_tick_callback(move |list, _| keep_at_top(list));
        }
    });
}

// When the focus is lost rather than moved, as when the album or the work
// that had it goes from a list, or a menu closes whose item was taken away,
// GTK keeps the chain of focus children that led to it. A list item whose
// focus child can't take the focus refuses it, and the containers of a
// chain that leads nowhere let the keys skip them, or stop at them without
// moving the focus. These drop the links that don't lead to the window's
// focus, `focus`, as GTK does when the focus moves: from `widget` down,
// and from `widget` up to the window
pub fn forget_focus_below(widget: &gtk::Widget, focus: Option<&gtk::Widget>) {
    let mut parent = widget.clone();
    while let Some(child) = parent.focus_child() {
        if focus.is_some_and(|focus| *focus == child || focus.is_ancestor(&child)) {
            break;
        }
        parent.set_focus_child(None::<&gtk::Widget>);
        parent = child;
    }
}

pub fn forget_focus_above(widget: &gtk::Widget, focus: Option<&gtk::Widget>) {
    let mut child = widget.clone();
    while let Some(parent) = child.parent() {
        if parent.focus_child().as_ref() == Some(&child)
            && !focus.is_some_and(|focus| *focus == child || focus.is_ancestor(&child))
        {
            parent.set_focus_child(None::<&gtk::Widget>);
        }
        child = parent;
    }
}

// Gives the focus to the item a list or a grid was scrolled to with
// `ListScrollFlags::FOCUS`, once it's laid out. Scrolling only moves the
// focus there if the list has it already (`gtk_list_base_scroll_to`), and
// otherwise keeps the item for when the list takes it, and a list that was
// just shown, as with another page, lays out its items at the next frame
pub fn focus_when_laid_out(list: &impl IsA<gtk::Widget>) {
    when_laid_out(list, |list| {
        list.grab_focus();
    });
}

// Runs `f` at the frame after the next one, once the widget is laid out:
// a list lays out the items that changed, and a widget that was just shown
// its children, at the next frame
pub fn when_laid_out<W: IsA<gtk::Widget>>(widget: &W, f: impl FnOnce(&W) + 'static) {
    let laid_out = Cell::new(false);
    let f = Cell::new(Some(f));
    widget.add_tick_callback(move |widget, _| {
        if !laid_out.replace(true) {
            return glib::ControlFlow::Continue;
        }
        if let Some(f) = f.take() {
            f(widget);
        }
        glib::ControlFlow::Break
    });
}

// The part of a list that changed, past the items it has in common at its
// start and at its end, which `same(old, new)` compares: where it starts,
// the number of old items removed there, and the number of new ones put in
// their place. A list keeps its place by its items, so replacing only
// those keeps it where it was
pub fn changed_range(
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

// Gives a row of a list the tooltip that `text` makes from the item it's
// bound to, made only when it shows. Setting the tooltip as the row is bound
// makes GTK look for the window under the pointer, which grabs the X
// server, for each row bound, and lists bind many rows a frame while they
// scroll, which stalls the whole display
pub fn set_row_tooltip(
    row: &impl IsA<gtk::Widget>,
    item: &gtk::ListItem,
    text: impl Fn(&glib::Object) -> Option<String> + 'static,
) {
    row.set_has_tooltip(true);
    row.connect_query_tooltip(glib::clone!(
        #[weak]
        item,
        #[upgrade_or]
        false,
        move |_, _, _, _, tooltip| {
            let Some(text) = item.item().and_then(|object| text(&object)) else {
                return false;
            };
            tooltip.set_text(Some(&text));
            true
        }
    ));
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

// A playlist name in a menu, shortened, as a menu is as wide as its
// longest item; underscores in names would be taken for mnemonics
pub fn playlist_menu_label(name: &str) -> String {
    short_playlist_name(name).replace('_', "__")
}

// The position of a track in a list of tracks
pub fn track_position(model: &gio::ListModel, track: &Track) -> Option<u32> {
    (0..model.n_items()).find(|&i| model.item(i).and_downcast_ref::<Track>() == Some(track))
}

// Selects the tracks of a list from `from` to `to`, both included, in the
// order of the list; returns false if either is not in it
pub fn select_track_range(model: &gio::ListModel, from: &Track, to: &Track) -> bool {
    let (Some(from), Some(to)) = (track_position(model, from), track_position(model, to)) else {
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
    let mut tracks: Vec<Track> = (start..end.min(model.n_items()))
        .filter_map(|i| model.item(i).and_downcast::<Track>())
        .collect();
    let selected = !tracks.iter().all(Track::selected);
    for track in &tracks {
        track.set_selected(selected);
    }
    tracks.pop()
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

// How many files are read at a time where reading them mostly waits, as
// decoding a cover waits for its sandboxed loader: a few at a time go
// faster, even on fewer cores
pub const READING_THREADS: usize = 4;

// Runs `work` on each of the `items` in up to `threads` threads, and hands
// each result to `each` on this thread as it comes, with the position of
// its item, in no particular order. It returns once every item is done, or
// with the first error of `each`, once the threads have finished the items
// they were on
pub fn in_threads<T: Sync, R: Send, E>(
    items: &[T],
    threads: usize,
    work: impl Fn(&T) -> R + Sync,
    mut each: impl FnMut(usize, R) -> Result<(), E>,
) -> Result<(), E> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let next = AtomicUsize::new(0);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..threads.min(items.len()) {
            let sender = sender.clone();
            let (next, work) = (&next, &work);
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    // The receiver is gone once `each` failed
                    if sender.send((index, work(item))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        for (index, result) in receiver {
            each(index, result)?;
        }
        Ok(())
    })
}

// Whether a file is an audio track, guessing from its name only, which is
// much faster than looking at its contents when scanning a whole library
pub fn is_audio_path(path: &std::path::Path) -> bool {
    let (mime_type, _) = gio::content_type_guess(Some(path), None);
    is_audio_type(&mime_type)
}

// Playlists have audio types too. WebM files have a video type, but are
// nearly always downloaded music in the music folders, with a video or
// not, and only their sound is played
pub fn is_audio_type(mime_type: &str) -> bool {
    const PLAYLISTS: [&str; 4] = [
        "audio/x-mpegurl",
        "audio/mpegurl",
        "audio/x-scpls",
        "audio/x-ms-asx",
    ];
    (mime_type.starts_with("audio/") && !PLAYLISTS.contains(&mime_type))
        || mime_type == "video/webm"
}

// What a walk through a folder found
#[derive(Default)]
pub struct FolderTree {
    // In the order a file manager shows them
    pub files: Vec<gio::File>,
    // The folders read, the first one included, in no order
    pub folders: Vec<gio::File>,
    // The symbolic links to folders that were followed
    pub links: Vec<gio::File>,
    // Whether a symbolic link leads nowhere, as one to a drive that isn't
    // mounted does
    pub broken_links: bool,
}

// Following symbolic links, the type is that of what they lead to
const WALK_ATTRIBUTES: &str = "standard::name,standard::type,standard::is-symlink,\
                               unix::device,unix::inode";

struct FolderWalk<'a> {
    recursive: bool,
    keep: &'a dyn Fn(&std::path::Path) -> bool,
    // The device and inode of the folders and files read, which are the
    // same however they're reached
    read: std::collections::HashSet<(u32, u64)>,
    // The links found, which are followed once the folders are read
    links: Vec<(gio::File, gio::FileInfo)>,
    tree: FolderTree,
}

impl FolderWalk<'_> {
    fn read_folder(&mut self, folder: &gio::File) {
        self.tree.folders.push(folder.clone());
        let Ok(mut enumerator) = folder.enumerate_children(
            WALK_ATTRIBUTES,
            gio::FileQueryInfoFlags::NONE,
            None::<&gio::Cancellable>,
        ) else {
            warn!("Unable to enumerate {}", folder.uri());
            return;
        };
        while let Some(info) = enumerator.next().and_then(Result::ok) {
            let child = enumerator.child(&info);
            if child.path().is_some_and(|path| !(self.keep)(&path)) {
                continue;
            }
            if info.is_symlink() {
                self.links.push((child, info));
            } else {
                self.add(child, &info, false);
            }
        }
    }

    // A folder is read once, so a link can't make a loop; a file a link
    // leads to is only kept if it wasn't read already, but the files
    // themselves are all kept, as hard links to one file can be meant
    fn add(&mut self, file: gio::File, info: &gio::FileInfo, linked: bool) {
        match info.file_type() {
            gio::FileType::Directory if self.recursive => {
                if self.first_read(info) {
                    if linked {
                        self.tree.links.push(file.clone());
                    }
                    self.read_folder(&file);
                }
            }
            gio::FileType::Regular => {
                if self.first_read(info) || !linked {
                    self.tree.files.push(file);
                }
            }
            // A link that leads nowhere keeps its own type
            gio::FileType::SymbolicLink => self.tree.broken_links = true,
            _ => (),
        }
    }

    // Files that aren't local have no inode, and can't be told apart
    fn first_read(&mut self, info: &gio::FileInfo) -> bool {
        let inode = info.attribute_uint64(gio::FILE_ATTRIBUTE_UNIX_INODE);
        inode == 0
            || self.read.insert((
                info.attribute_uint32(gio::FILE_ATTRIBUTE_UNIX_DEVICE),
                inode,
            ))
    }
}

// The files and folders in a folder, without the ones whose paths `keep`
// turns down. Symbolic links are followed once the folders without them
// are read, so that one leading back into the folder adds nothing: each
// round follows the links found the round before, in the order of their
// paths, so that of two links to one folder, the same one is read at
// each scan
pub fn read_folder_tree(
    folder: &gio::File,
    recursive: bool,
    keep: &dyn Fn(&std::path::Path) -> bool,
) -> FolderTree {
    let mut walk = FolderWalk {
        recursive,
        keep,
        read: std::collections::HashSet::new(),
        links: Vec::new(),
        tree: FolderTree::default(),
    };
    if let Ok(info) = folder.query_info(
        WALK_ATTRIBUTES,
        gio::FileQueryInfoFlags::NONE,
        None::<&gio::Cancellable>,
    ) {
        walk.first_read(&info);
    }
    walk.read_folder(folder);
    while !walk.links.is_empty() {
        let mut links = std::mem::take(&mut walk.links);
        links.sort_by_cached_key(|(file, _)| file.path());
        for (file, info) in links {
            walk.add(file, &info, true);
        }
    }
    // gio::FileEnumerator has no guaranteed order. The files are sorted
    // once, with the keys made once for each file: a library of thousands
    // of files was sorted again at each level of its folders, with the keys
    // made again at each comparison
    walk.tree
        .files
        .sort_by_cached_key(|file| file_sort_key(Some(folder), file));
    walk.tree
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

// A path to show, with the home folder as "~"
pub fn home_path(path: &std::path::Path) -> String {
    match path.strip_prefix(glib::home_dir()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

// Whether Dismas runs in the Flatpak sandbox, which Flatpak marks with
// this file
pub fn in_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").is_file()
}

// The key a name sorts by: the names starting with "." or "#" go last, and
// the others in the order of file names in the locale, with numbers in
// their order, e.g. "Disc 2" before "Disc 10"
pub fn file_name_key(name: &str) -> (bool, glib::FilenameCollationKey) {
    (
        name.starts_with(['.', '#']),
        glib::FilenameCollationKey::from(name),
    )
}

#[cfg(test)]
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
    let res = read_folder_tree(folder, recursive, keep).files;
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
pub fn write_test_id3v2(path: &std::path::Path, tag: &lofty::id3::v2::Id3v2Tag, id3v23: bool) {
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
    fn writes_hashes_as_before() {
        use sha2::{Digest, Sha256};

        assert_eq!(
            hex(&Sha256::digest("abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
