// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, OnceCell, RefCell};

use adw::subclass::prelude::*;
use glib::{clone, subclass::Signal};
use gtk::{CompositeTemplate, gdk, gio, glib, prelude::*};

use crate::{
    application::Application,
    audio::Track,
    cover_picture::CoverPicture,
    i18n::{ni18n_f, pi18n},
    list_scroll::header_above,
    utils,
};

fn is_cancelled(target: &gtk::DropTarget) -> bool {
    utils::is_drag_cancelled(target.current_drop().as_ref())
}

mod imp {
    use std::sync::LazyLock;

    use super::*;

    #[derive(Debug, Default, CompositeTemplate, glib::Properties)]
    #[properties(wrapper_type = super::QueueRow)]
    #[template(resource = "/io/github/dachshund7/Dismas/queue-row.ui")]
    pub(crate) struct QueueRow {
        // Template widgets
        #[template_child]
        pub(crate) track_cover_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        #[property(
            name = "track-cover",
            get = |imp: &Self| imp.track_cover_image.cover(),
            set = Self::set_track_cover,
            nullable,
            type = Option<gdk::Texture>,
        )]
        pub(crate) track_cover_image: TemplateChild<CoverPicture>,
        #[template_child]
        #[property(
            name = "track-title",
            get = |imp: &Self| imp.track_title_label.label().into(),
            set = Self::set_track_title,
            type = String,
        )]
        pub(crate) track_title_label: TemplateChild<gtk::Label>,
        #[template_child]
        #[property(
            name = "track-artist",
            get = |imp: &Self| imp.track_artist_label.label().into(),
            set = Self::set_track_artist,
            type = String,
        )]
        pub(crate) track_artist_label: TemplateChild<gtk::Label>,
        #[template_child]
        #[property(
            name = "track-credits",
            get = |imp: &Self| imp.track_credits_label.label().into(),
            set = Self::set_track_credits,
            type = String,
        )]
        pub(crate) track_credits_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(crate) track_playing_image: TemplateChild<gtk::Image>,
        #[template_child]
        pub(crate) drag_handle: TemplateChild<gtk::Image>,

        #[property(get, set, nullable)]
        pub(crate) track: RefCell<Option<Track>>,
        #[property(get, set = Self::set_playing, explicit_notify)]
        pub(crate) playing: Cell<bool>,
        // Whether the player is paused, for the icon of the current track
        #[property(get, set = Self::set_paused, explicit_notify)]
        pub(crate) paused: Cell<bool>,
        #[property(get, set = Self::set_selection_mode, explicit_notify)]
        pub(crate) selection_mode: Cell<bool>,
        // Follows the track; in selection mode, selected rows are
        // highlighted
        #[property(get, set = Self::set_selected, explicit_notify)]
        pub(crate) selected: Cell<bool>,
        // Follows the track, which flashes to show where it is
        #[property(get, set = Self::set_flashing, explicit_notify)]
        pub(crate) flashing: Cell<bool>,
        // Follows the track, whose file wasn't found
        #[property(get, set = Self::set_missing, explicit_notify)]
        pub(crate) missing: Cell<bool>,
        // Whether the track can be dragged by its handle to another place
        // in the list
        #[property(get, set = Self::set_reorderable, explicit_notify)]
        pub(crate) reorderable: Cell<bool>,
        // Where the handle was pressed, for the drag icon
        pub(crate) drag_start: Cell<(f64, f64)>,
        // The handle's, to cancel a drag
        pub(crate) drag_source: OnceCell<gtk::DragSource>,
        // The list item showing the row, for its position in the list
        pub(crate) list_item: glib::WeakRef<gtk::ListItem>,
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
            self.obj().update_playing();
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

        fn set_missing(&self, missing: bool) {
            if missing != self.missing.replace(missing) {
                let obj = self.obj();
                if missing {
                    obj.add_css_class("missing");
                } else {
                    obj.remove_css_class("missing");
                }
                obj.notify_missing();
            }
        }

        fn set_track_title(&self, title: String) {
            self.track_title_label.set_label(&title);
        }

        fn set_track_artist(&self, artist: String) {
            self.track_artist_label.set_label(&artist);
        }

        fn set_track_credits(&self, credits: String) {
            self.track_credits_label.set_visible(!credits.is_empty());
            self.track_credits_label.set_label(&credits);
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
    pub(crate) struct QueueRow(ObjectSubclass<imp::QueueRow>)
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
    pub(crate) fn bind_list_item(&self, list_item: &gtk::ListItem) {
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
        item.chain_property::<Track>("missing")
            .bind(self, "missing", gtk::Widget::NONE);
        utils::keep_selected_state(
            list_item,
            self,
            Self::selection_mode,
            Self::update_selected_state,
        );

        if let Some(app) = gio::Application::default().and_downcast::<Application>() {
            app.player()
                .state()
                .bind_property("playing", self, "paused")
                .invert_boolean()
                .sync_create()
                .build();
        }
    }

    pub(crate) fn new() -> Self {
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
        self.connect_map(Self::update_selected_state);

        utils::setup_context_menu(
            self,
            clone!(
                #[weak(rename_to = this)]
                self,
                move |x, y| this.emit_by_name::<()>("menu-requested", &[&x, &y])
            ),
        );

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
                // The drag icon takes the natural width of the copy, which
                // for a label is its whole text on one line, so the clamp
                // keeps the copy to the row's width, with its lines wrapped
                // as in the row. The width is the row's bounds, padding
                // included, as the size request is; `width()` leaves the
                // padding out
                let width = this
                    .compute_bounds(&this)
                    .map_or(this.width(), |bounds| bounds.width().round() as i32);
                icon.set_width_request(width);
                let clamp = adw::Clamp::builder()
                    .maximum_size(width)
                    .tightening_threshold(width)
                    .unit(adw::LengthUnit::Px)
                    .child(&icon)
                    .build();
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

                gtk::DragIcon::for_drag(drag).set_child(Some(&clamp));
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
    pub(crate) fn drop_position(&self, y: f64) -> Option<bool> {
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
    pub(crate) fn cancel_drag(&self) -> bool {
        let Some(source) = self.imp().drag_source.get() else {
            return false;
        };
        if !utils::cancel_drag(source) {
            return false;
        }
        // As after a drop, the focus goes to the track that was dragged
        self.grab_row_focus();
        true
    }

    // Hides the line of a drop that won't happen
    pub(crate) fn hide_drop_position(&self) {
        self.show_drop_position(None);
    }

    // Shows the line where files dragged over the pane would go
    pub(crate) fn show_drop_line(&self, above: bool) {
        self.show_drop_position(Some(above));
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

    // Where the row is in its list
    pub(crate) fn position(&self) -> Option<u32> {
        self.imp().list_item.upgrade().map(|item| item.position())
    }

    // The header with the name of the work the row starts, if it's laid out
    pub(crate) fn work_header(&self) -> Option<gtk::Widget> {
        if !self.starts_work() {
            return None;
        }
        // The list's item, which the row is in, in the queue inside the bin
        // that slides it, see `SlideBin`
        let item = utils::list_item_of(self)?;
        let view = item.parent().and_downcast::<gtk::ListView>()?;
        let top = item.compute_bounds(&view)?.y();
        header_above(&view, top).map(|(header, _)| header)
    }

    // Whether the row is the first movement of a work, under its header
    pub(crate) fn starts_work(&self) -> bool {
        if !self.track().is_some_and(|t| t.is_movement()) {
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
        // The line above the first row would be out of the list, which
        // clips it, so it's on the row instead
        let first = above == Some(true) && self.position() == Some(0);
        for (class, shown) in [
            ("drop-above", above == Some(true)),
            ("drop-below", above == Some(false)),
            ("first", first),
        ] {
            if shown {
                self.add_css_class(class);
            } else {
                self.remove_css_class(class);
            }
        }
    }

    pub(crate) fn connect_move_requested<F: Fn(&Self, &Track, bool) + 'static>(
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

    pub(crate) fn connect_range_select_requested<F: Fn(&Self) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "range-select-requested",
            false,
            glib::closure_local!(move |row: &Self| f(row)),
        )
    }

    pub(crate) fn connect_select_requested<F: Fn(&Self) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "select-requested",
            false,
            glib::closure_local!(move |row: &Self| f(row)),
        )
    }

    // Asks for the menu of the row from the keyboard, in the middle of the
    // row, as a long press would
    pub(crate) fn request_menu(&self) {
        let x = f64::from(self.width()) / 2.0;
        let y = f64::from(self.height()) / 2.0;
        self.emit_by_name::<()>("menu-requested", &[&x, &y]);
    }

    pub(crate) fn connect_menu_requested<F: Fn(&Self, f64, f64) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "menu-requested",
            false,
            glib::closure_local!(move |row: &Self, x: f64, y: f64| f(row, x, y)),
        )
    }

    // The current track shows a pause icon over its cover while the player
    // is paused. The icon of the other rows is see-through, so it has no
    // name there, as a screen reader would read it on every row; GTK still
    // presents an icon with the hidden state
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
        if imp.playing.get() {
            imp.track_playing_image
                .update_property(&[gtk::accessible::Property::Label(&label)]);
        } else {
            imp.track_playing_image
                .reset_property(gtk::AccessibleProperty::Label);
        }
    }

    fn update_playing(&self) {
        let imp = self.imp();
        let opacity = if imp.playing.get() { 1.0 } else { 0.0 };
        imp.track_playing_image.set_opacity(opacity);
        self.update_playing_icon();
    }

    // Selected rows are highlighted in selection mode, in place of a
    // check button, so the cover stays visible
    fn update_selected(&self) {
        let imp = self.imp();
        let selection_mode = imp.selection_mode.get();
        let highlighted = selection_mode && imp.selected.get();
        // On the row itself, as the list's item is only taken while the row
        // is on screen, see below
        if highlighted {
            self.add_css_class("selected-item");
        } else {
            self.remove_css_class("selected-item");
        }
        self.update_selected_state();
    }

    // Screen readers present the list's item holding the row, so it says
    // whether the track is selected, in selection mode; otherwise, it's
    // not, as the list itself says (see `utils::keep_selected_state`)
    fn update_selected_state(&self) {
        let Some(item) = utils::list_item_of(self) else {
            return;
        };
        let imp = self.imp();
        let selected = imp.selection_mode.get() && imp.selected.get();
        item.update_state(&[gtk::accessible::State::Selected(Some(selected))]);
    }

    // The lines of a row are cut short after two lines, so the tooltip
    // shows the whole text of the row; for movements, that includes the
    // work from the header of their section, which may be scrolled away, and
    // the levels of the work go above the movement, from the largest down,
    // and for a missing track, where its file was looked for
    fn tooltip(&self) -> String {
        let imp = self.imp();
        let track = self.track();
        let title = imp.track_title_label.label();
        let artist = imp.track_artist_label.label();
        let credits = imp.track_credits_label.label();
        // Where the file was looked for
        let path = track
            .as_ref()
            .filter(|t| t.missing())
            .and_then(|t| t.file().path())
            .map(|path| path.display().to_string());
        let work = track.as_ref().and_then(Track::movement_work);

        let mut lines = vec![title.as_str(), artist.as_str()];
        if track.as_ref().is_some_and(Track::has_movement_levels) {
            lines.swap(0, 1);
        }
        lines.push(&credits);
        lines.extend(path.as_deref());
        if let Some(work) = &work {
            lines.insert(0, work);
        }

        lines
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}
