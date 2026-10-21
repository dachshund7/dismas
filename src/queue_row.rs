// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, OnceCell, RefCell};

use adw::subclass::prelude::*;
use glib::{clone, subclass::Signal};
use gtk::{CompositeTemplate, gdk, gio, glib, graphene, prelude::*};

use crate::{
    application::Application,
    audio::Track,
    cover_picture::CoverPicture,
    i18n::{ni18n_f, pi18n},
};

// The drag that Esc cancelled. The windowing system can go on with it until
// the button is released (on X11, it does), so the rows refuse its drop
thread_local! {
    static CANCELLED_DRAG: RefCell<glib::WeakRef<gdk::Drag>> = RefCell::default();
}

fn is_cancelled(target: &gtk::DropTarget) -> bool {
    let Some(drag) = target.current_drop().and_then(|drop| drop.drag()) else {
        return false;
    };
    CANCELLED_DRAG.with_borrow(|cancelled| cancelled.upgrade().as_ref() == Some(&drag))
}

mod imp {
    use std::sync::LazyLock;

    use super::*;

    #[derive(Debug, Default, CompositeTemplate, glib::Properties)]
    #[properties(wrapper_type = super::QueueRow)]
    #[template(resource = "/io/github/dachshund7/Dismas/queue-row.ui")]
    pub struct QueueRow {
        // Template widgets
        #[template_child]
        pub track_cover_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        #[property(
            name = "track-cover",
            get = |imp: &Self| imp.track_cover_image.cover(),
            set = Self::set_track_cover,
            nullable,
            type = Option<gdk::Texture>,
        )]
        pub track_cover_image: TemplateChild<CoverPicture>,
        #[template_child]
        #[property(
            name = "track-title",
            get = |imp: &Self| imp.track_title_label.text().unwrap_or_default().into(),
            set = Self::set_track_title,
            type = String,
        )]
        pub track_title_label: TemplateChild<gtk::Inscription>,
        #[template_child]
        #[property(
            name = "track-artist",
            get = |imp: &Self| imp.track_artist_label.text().unwrap_or_default().into(),
            set = Self::set_track_artist,
            type = String,
        )]
        pub track_artist_label: TemplateChild<gtk::Inscription>,
        #[template_child]
        #[property(
            name = "track-credits",
            get = |imp: &Self| imp.track_credits_label.text().unwrap_or_default().into(),
            set = Self::set_track_credits,
            type = String,
        )]
        pub track_credits_label: TemplateChild<gtk::Inscription>,
        #[template_child]
        pub track_playing_image: TemplateChild<gtk::Image>,
        #[template_child]
        pub drag_handle: TemplateChild<gtk::Image>,

        #[property(get, set, nullable)]
        pub track: RefCell<Option<Track>>,
        #[property(get, set = Self::set_playing, explicit_notify)]
        pub playing: Cell<bool>,
        // Whether the player is paused, for the icon of the current track
        #[property(get, set = Self::set_paused, explicit_notify)]
        pub paused: Cell<bool>,
        #[property(get, set = Self::set_selection_mode, explicit_notify)]
        pub selection_mode: Cell<bool>,
        // Follows the track; in selection mode, selected rows are
        // highlighted
        #[property(get, set = Self::set_selected, explicit_notify)]
        pub selected: Cell<bool>,
        // Follows the track, which flashes to show where it is
        #[property(get, set = Self::set_flashing, explicit_notify)]
        pub flashing: Cell<bool>,
        // Whether the track can be dragged by its handle to another place
        // in the list
        #[property(get, set = Self::set_reorderable, explicit_notify)]
        pub reorderable: Cell<bool>,
        // Where the handle was pressed, for the drag icon
        pub drag_start: Cell<(f64, f64)>,
        // The handle's, to cancel a drag
        pub drag_source: OnceCell<gtk::DragSource>,
        // The list item showing the row, for its position in the list
        pub list_item: glib::WeakRef<gtk::ListItem>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for QueueRow {
        const NAME: &'static str = "DismasQueueRow";
        type Type = super::QueueRow;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BoxLayout>();
            klass.set_css_name("queuerow");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for QueueRow {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            self.obj().init_widgets();
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    // A right click or long press, at the given point
                    Signal::builder("menu-requested")
                        .param_types([f64::static_type(), f64::static_type()])
                        .build(),
                    // A Ctrl+click, to select the track
                    Signal::builder("select-requested").build(),
                    // A Shift+click in selection mode, to select the tracks
                    // from the last one clicked to this one
                    Signal::builder("range-select-requested").build(),
                    // A track dropped on this row, to be moved above it, or
                    // below it
                    Signal::builder("move-requested")
                        .param_types([Track::static_type(), bool::static_type()])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for QueueRow {}

    impl QueueRow {
        fn set_playing(&self, playing: bool) {
            if playing != self.playing.replace(playing) {
                let obj = self.obj();
                obj.update_playing();
                obj.notify_playing();
            }
        }

        fn set_paused(&self, paused: bool) {
            if paused != self.paused.replace(paused) {
                let obj = self.obj();
                obj.update_playing_icon();
                obj.notify_paused();
            }
        }

        fn set_selection_mode(&self, selection_mode: bool) {
            if selection_mode != self.selection_mode.replace(selection_mode) {
                let obj = self.obj();
                obj.update_selected();
                obj.notify_selection_mode();
            }
        }

        fn set_reorderable(&self, reorderable: bool) {
            if reorderable != self.reorderable.replace(reorderable) {
                self.drag_handle.set_visible(reorderable);
                self.obj().notify_reorderable();
            }
        }

        fn set_selected(&self, selected: bool) {
            if selected != self.selected.replace(selected) {
                let obj = self.obj();
                obj.update_selected();
                obj.notify_selected();
            }
        }

        fn set_flashing(&self, flashing: bool) {
            if flashing != self.flashing.replace(flashing) {
                let obj = self.obj();
                // On the row itself, like the selection
                if flashing {
                    obj.add_css_class("flash");
                } else {
                    obj.remove_css_class("flash");
                }
                obj.notify_flashing();
            }
        }

        fn set_track_title(&self, title: String) {
            self.track_title_label.set_text(Some(&title));
        }

        fn set_track_artist(&self, artist: String) {
            self.track_artist_label.set_text(Some(&artist));
        }

        fn set_track_credits(&self, credits: String) {
            self.track_credits_label.set_visible(!credits.is_empty());
            self.track_credits_label.set_text(Some(&credits));
        }

        fn set_track_cover(&self, cover: Option<gdk::Texture>) {
            if let Some(texture) = cover {
                self.track_cover_image.set_cover(Some(&texture));
                self.track_cover_stack.set_visible_child_name("cover");
            } else {
                self.track_cover_image.set_cover(None::<&gdk::Texture>);
                self.track_cover_stack.set_visible_child_name("no-cover");
            }
        }
    }
}

glib::wrapper! {
    pub struct QueueRow(ObjectSubclass<imp::QueueRow>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for QueueRow {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl QueueRow {
    // Shows the track of the list item, like in the queue
    pub fn bind_list_item(&self, list_item: &gtk::ListItem) {
        self.imp().list_item.set(Some(list_item));
        list_item
            .bind_property("item", self, "track")
            .sync_create()
            .build();

        let item = list_item.property_expression("item");
        item.chain_property::<Track>("row-subtitle")
            .bind(self, "track-artist", gtk::Widget::NONE);
        item.chain_property::<Track>("row-title")
            .bind(self, "track-title", gtk::Widget::NONE);
        item.chain_property::<Track>("row-credits")
            .bind(self, "track-credits", gtk::Widget::NONE);
        item.chain_property::<Track>("row-cover")
            .bind(self, "track-cover", gtk::Widget::NONE);
        item.chain_property::<Track>("playing")
            .bind(self, "playing", gtk::Widget::NONE);
        item.chain_property::<Track>("selected")
            .bind(self, "selected", gtk::Widget::NONE);
        item.chain_property::<Track>("flashing")
            .bind(self, "flashing", gtk::Widget::NONE);

        if let Some(app) = gio::Application::default().and_downcast::<Application>() {
            app.player()
                .state()
                .bind_property("playing", self, "paused")
                .invert_boolean()
                .sync_create()
                .build();
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    fn init_widgets(&self) {
        // The tooltip is made when it's shown: setting it as the row is bound
        // to a track makes GTK look for the window under the pointer, which
        // grabs the X server, for each row rebound while scrolling
        self.set_has_tooltip(true);
        self.connect_query_tooltip(|this, _, _, _, tooltip| {
            let text = this.tooltip();
            if text.is_empty() {
                return false;
            }
            tooltip.set_text(Some(&text));
            true
        });

        let click = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        click.connect_pressed(clone!(
            #[weak(rename_to = this)]
            self,
            move |gesture, _, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.emit_by_name::<()>("menu-requested", &[&x, &y]);
            }
        ));
        self.add_controller(click);

        let long_press = gtk::GestureLongPress::builder().touch_only(true).build();
        long_press.connect_pressed(clone!(
            #[weak(rename_to = this)]
            self,
            move |gesture, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.emit_by_name::<()>("menu-requested", &[&x, &y]);
            }
        ));
        self.add_controller(long_press);

        let modifier_click = gtk::GestureClick::new();
        modifier_click.connect_pressed(clone!(
            #[weak(rename_to = this)]
            self,
            move |gesture, _, _, _| {
                let state = gesture.current_event_state();
                let signal = if state.contains(gdk::ModifierType::SHIFT_MASK)
                    && this.imp().selection_mode.get()
                {
                    "range-select-requested"
                } else if state.contains(gdk::ModifierType::CONTROL_MASK) {
                    "select-requested"
                } else {
                    return;
                };
                // Instead of activating the row, which plays the track or
                // toggles its selection
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.grab_row_focus();
                this.emit_by_name::<()>(signal, &[]);
            }
        ));
        self.add_controller(modifier_click);

        self.setup_drag_and_drop();
    }

    // The handle drags the track, and the rows take it, above or below
    // them depending on the half it's dropped on
    fn setup_drag_and_drop(&self) {
        let handle = &*self.imp().drag_handle;
        handle.set_cursor_from_name(Some("grab"));

        let drag_source = gtk::DragSource::new();
        drag_source.set_actions(gdk::DragAction::MOVE);
        drag_source.connect_prepare(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            None,
            move |_, x, y| {
                if !this.reorderable() {
                    return None;
                }
                let track = this.track()?;
                this.imp().drag_start.set((x, y));
                Some(gdk::ContentProvider::for_value(&track.to_value()))
            }
        ));
        drag_source.connect_drag_begin(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, drag| {
                let (x, y) = this.imp().drag_start.get();
                let point = this
                    .imp()
                    .drag_handle
                    .compute_point(&this, &gtk::graphene::Point::new(x as f32, y as f32))
                    .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));

                // The dragged track is shown as a copy of the row with a
                // background: the row's own is transparent, which shows as
                // black without a compositor
                let icon = Self::new();
                icon.set_track_title(this.track_title());
                icon.set_track_artist(this.track_artist());
                icon.set_track_credits(this.track_credits());
                icon.set_track_cover(this.track_cover());
                icon.set_reorderable(true);
                icon.set_width_request(this.width());
                icon.add_css_class("drag-icon");
                if this.display().is_composited() {
                    icon.add_css_class("composited");
                }

                // A selected track takes the other selected ones with it,
                // which the copy shows by their number, and which all fade
                // in the list
                let n_tracks = this.n_dragged_tracks();
                if n_tracks > 1 {
                    let count = gtk::Label::builder()
                        .label(ni18n_f(
                            // Translators: The '{}' must be left unmodified, and
                            // it is expanded to the number of tracks being moved
                            "{} track",
                            "{} tracks",
                            n_tracks,
                            &[&n_tracks.to_string()],
                        ))
                        .valign(gtk::Align::Center)
                        .css_classes(["drag-count"])
                        .build();
                    count.set_parent(&icon);
                    if let Some(list) = this.ancestor(gtk::ListView::static_type()) {
                        list.add_css_class("dragging-selection");
                    }
                }

                gtk::DragIcon::for_drag(drag).set_child(Some(&icon));
                drag.set_hotspot(point.x() as i32, point.y() as i32);

                this.add_css_class("dragging");
            }
        ));
        drag_source.connect_drag_end(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _, _| {
                this.remove_css_class("dragging");
                if let Some(list) = this.ancestor(gtk::ListView::static_type()) {
                    list.remove_css_class("dragging-selection");
                }
            }
        ));
        handle.add_controller(drag_source.clone());
        let _ = self.imp().drag_source.set(drag_source.clone());

        // A click on the handle doesn't play the track like a click on the
        // rest of the row; grouped with the drag source, which still gets
        // the press. A click that didn't drag the track focuses the row
        // once it's released, when scrolling it into view can't move it
        // from under the pointer
        let click = gtk::GestureClick::new();
        click.connect_pressed(|gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        click.connect_released(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _, _, _| this.grab_row_focus()
        ));
        // Gestures can only be grouped once they're on the same widget
        handle.add_controller(click.clone());
        click.group_with(&drag_source);

        let drop_target = gtk::DropTarget::new(Track::static_type(), gdk::DragAction::MOVE);
        drop_target.connect_motion(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |target, _, y| {
                let position = this.drop_position(y).filter(|_| !is_cancelled(target));
                this.show_drop_position(position);
                if position.is_some() {
                    gdk::DragAction::MOVE
                } else {
                    gdk::DragAction::empty()
                }
            }
        ));
        drop_target.connect_leave(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.show_drop_position(None)
        ));
        drop_target.connect_drop(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            false,
            move |target, value, _, y| {
                this.show_drop_position(None);
                if is_cancelled(target) {
                    return false;
                }
                let Ok(track) = value.get::<Track>() else {
                    return false;
                };
                let Some(above) = this.drop_position(y) else {
                    return false;
                };
                if this.track().as_ref() == Some(&track) {
                    return false;
                }
                this.emit_by_name::<()>("move-requested", &[&track, &above]);
                true
            }
        ));
        self.add_controller(drop_target);
    }

    // The number of tracks dragged with this one's handle: in selection
    // mode, a selected track moves with all the selected ones
    fn n_dragged_tracks(&self) -> u32 {
        if !self.selection_mode() || !self.selected() {
            return 1;
        }
        gio::Application::default()
            .and_downcast::<Application>()
            .map_or(1, |app| app.player().queue().n_selected_tracks().max(1))
    }

    // Whether a track dropped at this height goes above or below the row,
    // if it can be dropped there. Above the first movement of a work is
    // between the movement and the header with the name of the work, but
    // tracks that are not part of the work go above the header; tracks go
    // there by being dropped below the row before the header instead
    fn drop_position(&self, y: f64) -> Option<bool> {
        if !self.reorderable() {
            return None;
        }
        let above = y < f64::from(self.height()) / 2.0;
        if above && self.starts_work() {
            return None;
        }
        Some(above)
    }

    // Cancels the drag of the track, if it's being dragged, and says if it
    // was. The dragged track slides back to its place, but the windowing
    // system can go on with the drag until the button is released, so the
    // rows refuse its drop
    pub fn cancel_drag(&self) -> bool {
        let Some(source) = self.imp().drag_source.get() else {
            return false;
        };
        let Some(drag) = source.drag() else {
            return false;
        };
        CANCELLED_DRAG.with_borrow(|cancelled| cancelled.set(Some(&drag)));
        source.drag_cancel();
        // As after a drop, the focus goes to the track that was dragged
        self.grab_row_focus();
        true
    }

    // Hides the line of a drop that won't happen
    pub fn hide_drop_position(&self) {
        self.show_drop_position(None);
    }

    // The modifier clicks the row claims don't reach the list, which moves
    // the focus to the row clicked; so that Ctrl+A goes to the queue after
    // them. The focus goes to the list's item holding the row, the first
    // focusable widget above it. Presses on the handle leave the focus, as
    // the list scrolls a row that's cut off into view when it's focused,
    // which would move it under the pointer as a drag starts; the row is
    // focused when the click is released or the drag cancelled, and the
    // window focuses a dropped track
    fn grab_row_focus(&self) {
        let mut widget = self.parent();
        while let Some(w) = widget {
            if w.is_focusable() {
                w.grab_focus();
                return;
            }
            widget = w.parent();
        }
    }

    // Whether the row is the first movement of a work, under its header
    fn starts_work(&self) -> bool {
        if self.track().and_then(|t| t.movement_work()).is_none() {
            return false;
        }
        let Some(position) = self.imp().list_item.upgrade().map(|item| item.position()) else {
            return false;
        };
        let sections = self
            .ancestor(gtk::ListView::static_type())
            .and_downcast::<gtk::ListView>()
            .and_then(|list| list.model())
            .and_dynamic_cast::<gtk::SectionModel>()
            .ok();
        sections.is_some_and(|sections| sections.section(position).0 == position)
    }

    // A line above or below the row, where a dragged track would go
    fn show_drop_position(&self, above: Option<bool>) {
        for (class, shown) in [
            ("drop-above", above == Some(true)),
            ("drop-below", above == Some(false)),
        ] {
            if shown {
                self.add_css_class(class);
            } else {
                self.remove_css_class(class);
            }
        }
    }

    pub fn connect_move_requested<F: Fn(&Self, &Track, bool) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "move-requested",
            false,
            glib::closure_local!(move |row: &Self, track: &Track, above: bool| f(
                row, track, above
            )),
        )
    }

    pub fn connect_range_select_requested<F: Fn(&Self) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "range-select-requested",
            false,
            glib::closure_local!(move |row: &Self| f(row)),
        )
    }

    pub fn connect_select_requested<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "select-requested",
            false,
            glib::closure_local!(move |row: &Self| f(row)),
        )
    }

    pub fn connect_menu_requested<F: Fn(&Self, f64, f64) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "menu-requested",
            false,
            glib::closure_local!(move |row: &Self, x: f64, y: f64| f(row, x, y)),
        )
    }

    // The current track shows a pause icon while the player is paused
    fn update_playing_icon(&self) {
        let imp = self.imp();
        let (icon, label) = if imp.paused.get() {
            ("media-playback-pause-symbolic", pi18n("a11y", "Paused"))
        } else {
            (
                "media-playback-start-symbolic",
                pi18n("a11y", "Currently playing"),
            )
        };
        imp.track_playing_image.set_icon_name(Some(icon));
        imp.track_playing_image
            .update_property(&[gtk::accessible::Property::Label(&label)]);
    }

    fn update_playing(&self) {
        let imp = self.imp();
        let opacity = if imp.playing.get() { 1.0 } else { 0.0 };
        imp.track_playing_image.set_opacity(opacity);
    }

    // Selected rows are highlighted in selection mode, in place of a
    // check button, so the cover stays visible
    fn update_selected(&self) {
        let imp = self.imp();
        let selection_mode = imp.selection_mode.get();
        let highlighted = selection_mode && imp.selected.get();
        // On the row itself: changing the list's own row widget while the
        // list rebinds its rows, as when tracks move, crashes GTK
        if highlighted {
            self.add_css_class("selected-item");
        } else {
            self.remove_css_class("selected-item");
        }
        if selection_mode {
            self.update_state(&[gtk::accessible::State::Selected(Some(highlighted))]);
        } else {
            self.reset_state(gtk::AccessibleState::Selected);
        }
    }

    // Long titles get truncated in the queue, so we show the full
    // text of the row on hover; for movements, that includes the work from
    // the header of their section, which may be scrolled away, and the
    // levels of the work go above the movement, from the largest down
    fn tooltip(&self) -> String {
        let imp = self.imp();
        let track = self.track();
        let mut lines = vec![
            imp.track_title_label.text().unwrap_or_default().to_string(),
            imp.track_artist_label
                .text()
                .unwrap_or_default()
                .to_string(),
        ];

        if track.as_ref().and_then(Track::movement_levels).is_some() {
            lines.swap(0, 1);
        }
        lines.push(
            imp.track_credits_label
                .text()
                .unwrap_or_default()
                .to_string(),
        );
        if let Some(work) = track.and_then(|t| t.movement_work()) {
            lines.insert(0, work);
        }

        lines
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// Scrolls a list to a track, places it in the middle of the list, if it can
// scroll that far, and makes it flash for a moment, like its CSS
// animation. The list was usually just shown, and can only scroll once
// it has its size, so this waits for a frame where it has it; the row
// of the track is then there in the next frame, to be centered
pub fn flash_track(view: &gtk::ListView, pos: u32, track: &Track) {
    let frames = Cell::new(0);
    view.add_tick_callback(clone!(
        #[strong]
        track,
        move |view, _| {
            if view.height() == 0 {
                return glib::ControlFlow::Continue;
            }
            let frame = frames.get();
            frames.set(frame + 1);
            if frame == 0 {
                view.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
                return glib::ControlFlow::Continue;
            }

            // Only reads where the row is, as changing the rows of the
            // list crashes GTK; a few frames in, it flashes anyway
            let bounds = track_bounds(view, &track);
            if bounds.is_none() && frame < 5 {
                return glib::ControlFlow::Continue;
            }
            if let Some(bounds) = bounds {
                center_bounds(view, &bounds);
            }

            track.set_flashing(true);
            glib::timeout_add_local_once(
                std::time::Duration::from_millis(1200),
                clone!(
                    #[weak]
                    track,
                    move || track.set_flashing(false)
                ),
            );
            glib::ControlFlow::Break
        }
    ));
}

// Shows the playing track in the list, keeping the rows around it in view:
// the list stays where it is if the row is in it, scrolls just enough if
// it's cut off, and centers it if it's further away. Right after the list
// is filled, e.g. switching playlists, its rows aren't laid out, and
// scrolling to one right away would make it the top row, so this waits
// for frames where they are
pub fn reveal_track(view: &gtk::ListView, track: &Track) -> gtk::TickCallbackId {
    let frames = Cell::new(0);
    let centering = Cell::new(false);
    view.add_tick_callback(clone!(
        #[strong]
        track,
        move |view, _| {
            if view.height() == 0 {
                return glib::ControlFlow::Continue;
            }
            let frame = frames.get();
            frames.set(frame + 1);

            let bounds = track_bounds(view, &track);
            if centering.get() {
                // Scrolled to it in the previous frame
                if let Some(bounds) = bounds {
                    center_bounds(view, &bounds);
                }
                if bounds.is_some() || frame >= 5 {
                    return glib::ControlFlow::Break;
                }
                return glib::ControlFlow::Continue;
            }

            let height = view.height() as f32;
            match bounds {
                Some(b) if b.y() >= 0.0 && b.y() + b.height() <= height => {}
                Some(_) => {
                    if let Some(pos) = track_position(view, &track) {
                        view.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
                    }
                }
                // The rows can be laid out in the next frame
                None if frame < 1 => return glib::ControlFlow::Continue,
                None => {
                    // A search can hide it
                    if let Some(pos) = track_position(view, &track) {
                        view.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
                        centering.set(true);
                        return glib::ControlFlow::Continue;
                    }
                }
            }
            glib::ControlFlow::Break
        }
    ))
}

// Where a track is in a list, which can be filtered by a search
fn track_position(view: &gtk::ListView, track: &Track) -> Option<u32> {
    let model = view.model()?;
    (0..model.n_items()).find(|&i| model.item(i).as_ref() == Some(track.upcast_ref()))
}

// Where the row of a track is in the list, if it has one. Right after the
// list is filled, a row can be there without being laid out, at the top
// and without a height, which isn't where it will be
fn track_bounds(view: &gtk::ListView, track: &Track) -> Option<graphene::Rect> {
    track_row(view, track)
        .and_then(|row| row.compute_bounds(view))
        .filter(|bounds| bounds.height() > 0.0)
}

// Scrolls the list to put the row in its middle, as far as it can scroll
fn center_bounds(view: &gtk::ListView, bounds: &graphene::Rect) {
    if let Some(adjustment) = view.vadjustment() {
        let middle = f64::from(bounds.y() + bounds.height() / 2.0);
        adjustment.set_value(adjustment.value() + middle - f64::from(view.height()) / 2.0);
    }
}

// The row showing a track in a list, if it has one
fn track_row(view: &gtk::ListView, track: &Track) -> Option<QueueRow> {
    let mut child = view.first_child();
    while let Some(widget) = child {
        // The rows of the queue are in a SlideBin
        let row = widget.first_child().and_then(|child| {
            child
                .clone()
                .downcast::<QueueRow>()
                .ok()
                .or_else(|| child.first_child().and_downcast::<QueueRow>())
        });
        if let Some(row) = row
            && row.track().as_ref() == Some(track)
        {
            return Some(row);
        }
        child = widget.next_sibling();
    }
    None
}
