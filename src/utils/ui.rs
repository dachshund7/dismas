// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

// The drag that Esc cancelled. The windowing system can go on with it until
// the button is released (on X11, it does), so its drop is refused
thread_local! {
    static CANCELLED_DRAG: RefCell<glib::WeakRef<gdk::Drag>> = RefCell::default();
}

// Fills the heart of a toggle button while it's on, and gives it the
// tooltip that says what a click does then, if there are `tooltips`: to
// add, and to remove
pub(crate) fn setup_heart_button(button: &gtk::ToggleButton, tooltips: Option<(String, String)>) {
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
pub(crate) fn cancel_drag(source: &gtk::DragSource) -> bool {
    let Some(drag) = source.drag() else {
        return false;
    };
    CANCELLED_DRAG.with_borrow(|cancelled| cancelled.set(Some(&drag)));
    source.drag_cancel();
    true
}

// Whether a drop is the one of a drag that was cancelled
pub(crate) fn is_drag_cancelled(drop: Option<&gdk::Drop>) -> bool {
    let Some(drag) = drop.and_then(gdk::Drop::drag) else {
        return false;
    };
    CANCELLED_DRAG.with_borrow(|cancelled| cancelled.upgrade().as_ref() == Some(&drag))
}

// The main menu, for the menu buttons of each page
pub(crate) fn primary_menu() -> gio::MenuModel {
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
pub(crate) fn setup_primary_menu_button(
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
pub(crate) fn focused_child<W: IsA<gtk::Widget>>(widget: &impl IsA<gtk::Widget>) -> Option<W> {
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
pub(crate) fn list_item_of(widget: &impl IsA<gtk::Widget>) -> Option<gtk::Widget> {
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
pub(crate) fn setup_swipe_back(widget: &impl IsA<gtk::Widget>, back: impl Fn() + 'static) {
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
pub(crate) fn keep_selected_state<W: IsA<gtk::Widget>>(
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
pub(crate) fn setup_menu_key<W: IsA<gtk::Widget>>(
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
pub(crate) fn setup_context_menu(
    widget: &impl IsA<gtk::Widget>,
    open: impl Fn(f64, f64) + 'static,
) {
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
pub(crate) fn set_playlist_button_label(button: &adw::SplitButton, label: &str) {
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
pub(crate) fn scroll_to_top_when_laid_out(
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
pub(crate) fn forget_focus_below(widget: &gtk::Widget, focus: Option<&gtk::Widget>) {
    let mut parent = widget.clone();
    while let Some(child) = parent.focus_child() {
        if focus.is_some_and(|focus| *focus == child || focus.is_ancestor(&child)) {
            break;
        }
        parent.set_focus_child(None::<&gtk::Widget>);
        parent = child;
    }
}

pub(crate) fn forget_focus_above(widget: &gtk::Widget, focus: Option<&gtk::Widget>) {
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
pub(crate) fn focus_when_laid_out(list: &impl IsA<gtk::Widget>) {
    when_laid_out(list, |list| {
        list.grab_focus();
    });
}

// Runs `f` at the frame after the next one, once the widget is laid out:
// a list lays out the items that changed, and a widget that was just shown
// its children, at the next frame
pub(crate) fn when_laid_out<W: IsA<gtk::Widget>>(widget: &W, f: impl FnOnce(&W) + 'static) {
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

// Gives a row of a list the tooltip that `text` makes from the item it's
// bound to, made only when it shows. Setting the tooltip as the row is bound
// makes GTK look for the window under the pointer, which grabs the X
// server, for each row bound, and lists bind many rows a frame while they
// scroll, which stalls the whole display
pub(crate) fn set_row_tooltip(
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
pub(crate) fn wrap_tooltip(widget: &impl IsA<gtk::Widget>) {
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
pub(crate) fn focus_on_clear(entry: &gtk::SearchEntry) {
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

// The position of a track in a list of tracks
pub(crate) fn track_position(model: &gio::ListModel, track: &Track) -> Option<u32> {
    (0..model.n_items()).find(|&i| model.item(i).and_downcast_ref::<Track>() == Some(track))
}

// Selects the tracks of a list from `from` to `to`, both included, in the
// order of the list; returns false if either is not in it
pub(crate) fn select_track_range(model: &gio::ListModel, from: &Track, to: &Track) -> bool {
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
pub(crate) fn toggle_track_section(model: &gio::ListModel, start: u32, end: u32) -> Option<Track> {
    let mut tracks: Vec<Track> = (start..end.min(model.n_items()))
        .filter_map(|i| model.item(i).and_downcast::<Track>())
        .collect();
    let selected = !tracks.iter().all(Track::selected);
    for track in &tracks {
        track.set_selected(selected);
    }
    tracks.pop()
}

// A toast whose title is plain text: the titles of toasts are markup by
// default, and they can have the names of playlists and folders, which can
// have "&"
pub(crate) fn plain_toast(title: &str) -> adw::Toast {
    adw::Toast::builder().title(title).use_markup(false).build()
}

// Header bars have a window handle inside, which moves the window when
// dragged; turning off its drag gesture keeps its clicks, like the window
// menu. Turning it on puts it back in the bubble phase, where GTK leaves it
pub(crate) fn set_window_drag(widget: &gtk::Widget, enabled: bool) {
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
