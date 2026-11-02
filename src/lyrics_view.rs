// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The lyrics of the playing track, in the playlist pane. They're only read
// while the pane shows them: when it opens, when the track changes, and
// when the track's tags or its files of lyrics change, as a program that
// finds lyrics can write a file while the track plays. Synchronized lyrics
// show the line sung in the middle, and follow the music

use std::{
    cell::{Cell, OnceCell, RefCell},
    time::Duration,
};

use adw::{prelude::*, subclass::prelude::*};
use glib::{clone, subclass::Signal};
use gtk::{CompositeTemplate, gdk, gio, glib, pango};
use log::debug;
use std::sync::LazyLock;

use crate::{
    audio::{PlayerState, Track},
    i18n::pi18n,
    list_scroll::{glide_adjustment, within_scroll},
    lyrics::{self, Line, Lyrics, Place},
    utils,
    zoom::{self, Zoom},
    zoom_row::ZoomRow,
};

// How far a position can be before the line that shows and still keep it:
// the next line shows at its time, ahead of the next position the player
// reports, which can be from a little before it
const BACK_TOLERANCE: Duration = Duration::from_millis(300);

// How long the lines stay where the user scrolled them, before they follow
// the music again
const FOLLOW_PAUSE: Duration = Duration::from_secs(4);

// The sizes of the text, in percent of the normal size, at each step of the
// zoom from the smallest (`LYRICS_ZOOM`, with 0 for the normal size); each but the normal size has a style class
// `zoom-{percent}` in style.css
const ZOOM_PERCENTS: [u32; 5] = [85, 100, 125, 150, 200];
const LYRICS_ZOOM: zoom::ZoomSteps = zoom::ZoomSteps {
    key: "lyrics-zoom",
    min: -1,
    max: 3,
};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/dachshund7/Dismas/lyrics-view.ui")]
    pub(crate) struct LyricsView {
        #[template_child]
        pub(crate) back_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(crate) window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub(crate) zoom_row: TemplateChild<ZoomRow>,
        #[template_child]
        pub(crate) stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub(crate) text_view: TemplateChild<gtk::TextView>,
        #[template_child]
        pub(crate) synced_scrolled: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub(crate) lines_box: TemplateChild<gtk::ListBox>,

        pub(crate) state: OnceCell<PlayerState>,
        // The track whose lyrics show, or are being read, and which of its
        // plays, see `PlayerState::starts`
        pub(crate) track: RefCell<Option<Track>>,
        pub(crate) start: Cell<u64>,
        // The number of reads so far: the lyrics of a read only show if no
        // other read started after it, as the same track can be read again
        // before the read before is done, e.g. when its tags change
        pub(crate) reads: Cell<u64>,
        // The lines of the synchronized lyrics that show, and where the
        // music is in them
        pub(crate) lines: RefCell<Vec<Line>>,
        pub(crate) current: Cell<Place>,

        // While the view is mapped
        pub(crate) handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub(crate) file_monitors: RefCell<Vec<gio::FileMonitor>>,
        // When the line sung changes next, while the track plays, between
        // the positions the player reports
        pub(crate) next_change: RefCell<Option<glib::SourceId>>,
        // While the lines stay where the user scrolled them
        pub(crate) follow_pause: RefCell<Option<glib::SourceId>>,
        pub(crate) glide: RefCell<Option<adw::TimedAnimation>>,
        // Where the text's zoom is kept, which is kept, as its signals stop
        // once it's dropped
        pub(crate) settings: OnceCell<gio::Settings>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LyricsView {
        const NAME: &'static str = "DismasLyricsView";
        type Type = super::LyricsView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            ZoomRow::static_type();
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("lyricsview");
            klass.set_accessible_role(gtk::AccessibleRole::Group);

            // The text of the lyrics, larger or smaller; the window's
            // zoom actions come here when the focus is on them
            for (name, zoom) in [
                ("lyrics.zoom-in", Zoom::In),
                ("lyrics.zoom-out", Zoom::Out),
                ("lyrics.zoom-reset", Zoom::Reset),
            ] {
                klass.install_action(name, None, move |view, _, _| {
                    view.zoom(zoom);
                });
            }
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LyricsView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();
            self.back_button.connect_clicked(clone!(
                #[weak]
                obj,
                move |_| obj.emit_by_name::<()>("back", &[])
            ));
            utils::setup_swipe_back(
                &*obj,
                clone!(
                    #[weak]
                    obj,
                    move || obj.imp().back_button.emit_clicked()
                ),
            );
            obj.setup_lines();
            obj.setup_zoom();
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder("back").build(),
                    // A line was activated: where to play from, in
                    // milliseconds from the start of the track
                    Signal::builder("seek")
                        .param_types([u64::static_type()])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for LyricsView {
        fn map(&self) {
            self.parent_map();
            self.obj().start_following();
        }

        fn unmap(&self) {
            self.obj().stop_following();
            self.parent_unmap();
        }
    }
}

glib::wrapper! {
    pub(crate) struct LyricsView(ObjectSubclass<imp::LyricsView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for LyricsView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl LyricsView {
    // The state of the player, whose track's lyrics show; set once
    pub(crate) fn set_state(&self, state: &PlayerState) {
        let _ = self.imp().state.set(state.clone());
        if self.is_mapped() {
            self.start_following();
        }
    }

    pub(crate) fn connect_back<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "back",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub(crate) fn connect_seek<F: Fn(&Self, Duration) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "seek",
            false,
            glib::closure_local!(move |view: &Self, millis: u64| f(
                view,
                Duration::from_millis(millis)
            )),
        )
    }

    // The tags of a track were read again; if it's the one whose lyrics
    // show, its lyrics are too. Once the view is shown again, they're read
    // anyway
    pub(crate) fn track_changed(&self, track: &Track) {
        if self.is_mapped() && self.imp().track.borrow().as_ref() == Some(track) {
            self.read();
        }
    }

    // Follows the playing track while the view is shown. Its lyrics are
    // read again each time, as its files can have changed in the meantime,
    // but the ones that show stay until they're read, if it's the same track
    fn start_following(&self) {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return;
        };
        if imp.handlers.borrow().is_empty() {
            let handlers = vec![
                state.connect_track_notify(clone!(
                    #[weak(rename_to = view)]
                    self,
                    move |_| view.follow_track()
                )),
                state.connect_starts_notify(clone!(
                    #[weak(rename_to = view)]
                    self,
                    move |_| view.follow_start()
                )),
                state.connect_position_notify(clone!(
                    #[weak(rename_to = view)]
                    self,
                    move |state| view.follow(state.position_time())
                )),
                state.connect_playing_notify(clone!(
                    #[weak(rename_to = view)]
                    self,
                    move |state| view.follow(state.position_time())
                )),
            ];
            imp.handlers.replace(handlers);
        }
        self.follow_track();
        self.center_when_laid_out();
    }

    fn stop_following(&self) {
        let imp = self.imp();
        if let Some(state) = imp.state.get() {
            for handler in imp.handlers.take() {
                state.disconnect(handler);
            }
        }
        for monitor in imp.file_monitors.take() {
            monitor.cancel();
        }
        if let Some(source) = imp.next_change.take() {
            source.remove();
        }
        self.resume_following();
        self.stop_glide();
    }

    fn follow_track(&self) {
        self.watch_lyrics_files();
        self.read();
    }

    // The same track played again, as when it repeats, starts its plain
    // lyrics from the top too, like another track's; the lines glide back
    // there with the music, see `set_current()`. Another track is left to
    // `read()`, which its own notification calls
    fn follow_start(&self) {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return;
        };
        let start = state.starts();
        if *imp.track.borrow() != state.track() || imp.start.replace(start) == start {
            return;
        }
        self.text_to_top();
    }

    // Reads the lyrics again when a file of lyrics of the playing track is
    // written, or removed
    fn watch_lyrics_files(&self) {
        let imp = self.imp();
        for monitor in imp.file_monitors.take() {
            monitor.cancel();
        }
        let path = imp
            .state
            .get()
            .and_then(PlayerState::track)
            .and_then(|track| track.audio_file().path());
        let Some(path) = path else {
            return;
        };
        let mut monitors = Vec::new();
        for lyrics_path in lyrics::lyrics_file_paths(&path) {
            let file = gio::File::for_path(&lyrics_path);
            let monitor =
                match file.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
                    Ok(monitor) => monitor,
                    Err(e) => {
                        debug!("Unable to watch {lyrics_path:?} for lyrics: {e}");
                        continue;
                    }
                };
            monitor.connect_changed(clone!(
                #[weak(rename_to = view)]
                self,
                move |_, _, _, event| {
                    if matches!(
                        event,
                        gio::FileMonitorEvent::Created
                            | gio::FileMonitorEvent::ChangesDoneHint
                            | gio::FileMonitorEvent::Deleted
                    ) {
                        view.read();
                    }
                }
            ));
            monitors.push(monitor);
        }
        imp.file_monitors.replace(monitors);
    }

    // Reads the lyrics of the playing track, in another thread
    fn read(&self) {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return;
        };
        let track = state.track();
        let start = state.starts();
        let reads = imp.reads.get() + 1;
        imp.reads.set(reads);

        // Another track, or the same one played again while the view was
        // hidden, see `follow_start()` for while it shows
        if *imp.track.borrow() != track || imp.start.get() != start {
            imp.window_title
                .set_subtitle(&track.as_ref().map(Track::title).unwrap_or_default());
            // Without a transition, which would show the lyrics of the
            // track before fading out
            imp.stack
                .set_visible_child_full("reading", gtk::StackTransitionType::None);
            imp.track.replace(track.clone());
            imp.start.set(start);
            self.start_from_top();
        }

        let Some(track) = track else {
            self.show(None);
            return;
        };
        let Some(path) = track.audio_file().path() else {
            self.show(None);
            return;
        };
        let part = track.part();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let found = gio::spawn_blocking(move || {
                    utils::guarded(
                        format_args!("the lyrics of {}", path.display()),
                        || lyrics::find(&path, part),
                        || None,
                    )
                })
                .await
                .unwrap_or_default();
                if view.imp().reads.get() == reads {
                    view.show(found);
                }
            }
        ));
    }

    fn show(&self, lyrics: Option<Lyrics>) {
        let imp = self.imp();
        match lyrics {
            Some(Lyrics::Synced(lines)) => self.show_lines(lines),
            Some(Lyrics::Plain(text)) => {
                self.clear_lines();
                let buffer = imp.text_view.buffer();
                let (start, end) = buffer.bounds();
                // The same lyrics, read again, keep their place and the
                // text selected
                if buffer.text(&start, &end, false) != text {
                    buffer.set_text(&text);
                    self.text_to_top();
                }
                imp.stack.set_visible_child_name("plain");
            }
            None => {
                self.clear_lines();
                imp.stack.set_visible_child_name("none");
            }
        }
    }

    // Another track's lyrics start from the top, wherever the user or the
    // music left the ones before, even when they're the same lyrics, as
    // when a song is in the queue twice. The pages are hidden until the
    // lyrics are read, and keep the place for when they show. The lines
    // before go now, or the positions of the new track, which come while
    // its lyrics are read, would be followed in them, and scroll the list
    // away from the top again
    fn start_from_top(&self) {
        let imp = self.imp();
        self.clear_lines();
        self.resume_following();
        self.stop_glide();
        let adjustment = imp.synced_scrolled.vadjustment();
        adjustment.set_value(adjustment.lower());
        self.text_to_top();
    }

    // A caret at the start reads the plain lyrics from the top
    fn text_to_top(&self) {
        let text_view = &*self.imp().text_view;
        let buffer = text_view.buffer();
        buffer.place_cursor(&buffer.start_iter());
        if let Some(adjustment) = text_view.vadjustment() {
            adjustment.set_value(adjustment.lower());
        }
    }

    // Synchronized lyrics: a row for each line. The same lines, read
    // again, keep their rows and their place
    fn show_lines(&self, lines: Vec<Line>) {
        let imp = self.imp();
        if *imp.lines.borrow() != lines {
            self.clear_lines();
            for line in &lines {
                let label = gtk::Label::builder()
                    .label(&line.text)
                    .wrap(true)
                    .wrap_mode(pango::WrapMode::WordChar)
                    .xalign(0.0)
                    .build();
                let row = gtk::ListBoxRow::builder().child(&label).build();
                // Screen readers read the row, which has no name of its own
                row.update_property(&[gtk::accessible::Property::Label(&line.text)]);
                imp.lines_box.append(&row);
            }
            imp.lines.replace(lines);
            self.center_when_laid_out();
        }
        imp.stack.set_visible_child_name("synced");
        if let Some(state) = imp.state.get() {
            self.follow(state.position_time());
        }
    }

    fn clear_lines(&self) {
        let imp = self.imp();
        if imp.lines.borrow().is_empty() {
            return;
        }
        if let Some(source) = imp.next_change.take() {
            source.remove();
        }
        imp.current.set(Place::Start);
        imp.lines.replace(Vec::new());
        imp.lines_box.remove_all();
    }

    // Shows the line sung at a position, and when the next one will be,
    // while the track plays
    fn follow(&self, position: Duration) {
        let imp = self.imp();
        let place = {
            let lines = imp.lines.borrow();
            // A position a little before the line that shows keeps it, see
            // `BACK_TOLERANCE`; seeking and starting over go back further
            let keep = imp
                .current
                .get()
                .line()
                .and_then(|current| lines.get(current))
                .is_some_and(|line| position < line.time && line.time - position <= BACK_TOLERANCE);
            if keep {
                imp.current.get()
            } else {
                lyrics::place_at(&lines, position)
            }
        };
        self.set_current(place);
        self.schedule_next_change(position);
    }

    // The player reports the position four times a second, so the next
    // line shows at its time with a timeout, rather than at the next
    // position after it
    fn schedule_next_change(&self, position: Duration) {
        let imp = self.imp();
        if let Some(source) = imp.next_change.take() {
            source.remove();
        }
        let playing = imp.state.get().is_some_and(PlayerState::playing);
        if !playing || !self.is_mapped() {
            return;
        }
        let Some(at) = lyrics::next_change(&imp.lines.borrow(), position) else {
            return;
        };
        let source = glib::timeout_add_local_once(
            at - position,
            clone!(
                #[weak(rename_to = view)]
                self,
                move || {
                    view.imp().next_change.take();
                    view.follow(at);
                }
            ),
        );
        imp.next_change.replace(Some(source));
    }

    fn row(&self, line: usize) -> Option<gtk::ListBoxRow> {
        self.imp().lines_box.row_at_index(i32::try_from(line).ok()?)
    }

    // The line sung is highlighted, and screen readers describe it as such,
    // without announcing each line. Back before the first line, as when the
    // track repeats or is sought to its start, the lines glide back to the
    // top; in a pause, they stay where they are
    fn set_current(&self, place: Place) {
        let imp = self.imp();
        let before = imp.current.replace(place);
        if before == place {
            return;
        }
        if let Some(row) = before.line().and_then(|line| self.row(line)) {
            row.remove_css_class("current");
            row.reset_property(gtk::AccessibleProperty::Description);
        }
        match place {
            Place::Line(line) => {
                let Some(row) = self.row(line) else {
                    return;
                };
                row.add_css_class("current");
                row.update_property(&[gtk::accessible::Property::Description(&pi18n(
                    "a11y",
                    "Current line",
                ))]);
            }
            Place::Start => {}
            Place::Pause => return,
        }
        if imp.follow_pause.borrow().is_none() {
            self.center_current(true);
        }
    }

    // Scrolls the line sung to the middle, as far as the list scrolls, or
    // the top before the first line, gliding to it like the queue to a
    // track; libadwaita jumps instead when animations are off. In a pause,
    // the lines stay where they are, and a glide that was on its way goes
    // on, rather than stopping between two lines
    fn center_current(&self, glide: bool) {
        let imp = self.imp();
        let adjustment = imp.synced_scrolled.vadjustment();
        let target = match imp.current.get() {
            Place::Start => adjustment.lower(),
            Place::Line(line) => {
                // Rows that aren't laid out yet, as right after the lines
                // are shown again, have no place to scroll to; a glide
                // toward the rows they replaced would go on until
                // `center_when_laid_out()` jumps, so it stops now
                let Some(bounds) = self
                    .row(line)
                    .and_then(|row| row.compute_bounds(&*imp.lines_box))
                else {
                    self.stop_glide();
                    return;
                };
                let middle = f64::from(bounds.y() + bounds.height() / 2.0);
                within_scroll(&adjustment, middle - adjustment.page_size() / 2.0)
            }
            Place::Pause => return,
        };
        self.stop_glide();
        // Already there, as at the start of each track, there's nowhere to
        // glide to
        if !glide || adjustment.value() == target {
            adjustment.set_value(target);
            return;
        }
        let animation = glide_adjustment(&*imp.synced_scrolled, &adjustment, target);
        imp.glide.replace(Some(animation));
    }

    fn stop_glide(&self) {
        if let Some(animation) = self.imp().glide.take() {
            animation.pause();
        }
    }

    // The text is as large as the `lyrics-zoom` setting says, with Ctrl
    // and the mouse wheel over it as well as with the keys
    fn setup_zoom(&self) {
        self.settings().connect_changed(
            Some(LYRICS_ZOOM.key),
            clone!(
                #[weak(rename_to = view)]
                self,
                move |_, _| view.apply_zoom()
            ),
        );
        self.apply_zoom();

        // On the view, so that it comes before the scrolling of the lines
        // that leaves them where the user scrolls them for a while, see
        // `pause_following()`, and the lines don't scroll either
        zoom::add_zoom_input(
            self,
            clone!(
                #[weak(rename_to = view)]
                self,
                move |zoom| {
                    view.zoom(zoom);
                }
            ),
        );
    }

    fn settings(&self) -> &gio::Settings {
        self.imp().settings.get_or_init(utils::settings_manager)
    }

    // Makes the text of the lyrics a step larger or smaller, or its normal
    // size; false when it's at that end already
    pub(crate) fn zoom(&self, zoom: Zoom) -> bool {
        LYRICS_ZOOM.zoom(self.settings(), zoom)
    }

    fn apply_zoom(&self) {
        let imp = self.imp();
        let step = LYRICS_ZOOM.step(self.settings());
        let percent = LYRICS_ZOOM.at(&ZOOM_PERCENTS, step);

        // The plain lyrics keep the line at the top where it is
        let text_view = &*imp.text_view;
        let top = text_view.visible_rect();
        let top_mark = text_view
            .iter_at_location(top.x(), top.y())
            .filter(|_| imp.stack.visible_child_name().as_deref() == Some("plain"))
            .map(|iter| text_view.buffer().create_mark(None, &iter, true));

        for percent in ZOOM_PERCENTS {
            self.remove_css_class(&format!("zoom-{percent}"));
        }
        if percent != 100 {
            self.add_css_class(&format!("zoom-{percent}"));
        }

        imp.zoom_row.set_percent(percent);
        // The size between them stays on, so that it reads as the size
        self.action_set_enabled("lyrics.zoom-in", step < LYRICS_ZOOM.max);
        self.action_set_enabled("lyrics.zoom-out", step > LYRICS_ZOOM.min);

        // The line sung goes back to the middle, as the user asked for the
        // lyrics to change
        self.resume_following();
        self.center_when_laid_out();
        if let Some(mark) = top_mark {
            utils::when_laid_out(text_view, move |text_view| {
                text_view.scroll_to_mark(&mark, 0.0, true, 0.0, 0.0);
                text_view.buffer().delete_mark(&mark);
            });
        }
    }

    // New lines are only laid out at the next frame, as is the list once
    // it's shown again, so they're centered at the one after
    fn center_when_laid_out(&self) {
        utils::when_laid_out(self, |view| {
            if view.imp().follow_pause.borrow().is_none() {
                view.center_current(false);
            }
        });
    }

    // The lines stay where the user scrolls them for a while, see
    // `FOLLOW_PAUSE`, and then glide back to the line sung
    fn pause_following(&self) {
        let imp = self.imp();
        self.stop_glide();
        if let Some(source) = imp.follow_pause.take() {
            source.remove();
        }
        let source = glib::timeout_add_local_once(
            FOLLOW_PAUSE,
            clone!(
                #[weak(rename_to = view)]
                self,
                move || {
                    view.imp().follow_pause.take();
                    view.center_current(true);
                }
            ),
        );
        imp.follow_pause.replace(Some(source));
    }

    fn resume_following(&self) {
        if let Some(source) = self.imp().follow_pause.take() {
            source.remove();
        }
    }

    fn setup_lines(&self) {
        let imp = self.imp();

        // A line activated with Enter, a click or a tap is sung from its
        // start
        imp.lines_box.connect_row_activated(clone!(
            #[weak(rename_to = view)]
            self,
            move |_, row| view.activate_line(row.index())
        ));
        // Enter on the line with the focus, which the list leaves to the
        // window, where it would press the play button
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, modifiers| {
                let enter = matches!(
                    key,
                    gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter
                );
                let row = view
                    .imp()
                    .lines_box
                    .focus_child()
                    .and_downcast::<gtk::ListBoxRow>();
                match row {
                    Some(row)
                        if enter
                            && (modifiers & gtk::accelerator_get_default_mod_mask()).is_empty() =>
                    {
                        view.activate_line(row.index());
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            }
        ));
        imp.lines_box.add_controller(keys);

        // The list follows a change of its size, e.g. as the window is
        // resized, right away
        imp.synced_scrolled.vadjustment().connect_changed(clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                let imp = view.imp();
                if imp.follow_pause.borrow().is_none()
                    && imp.stack.visible_child_name().as_deref() == Some("synced")
                {
                    view.center_current(false);
                }
            }
        ));

        // What the user does to scroll the lines, rather than their value
        // changing, which it also does with new lines and a new size: the
        // wheel and the touchpad, whose scrolling goes on after the fingers
        // leave it
        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::KINETIC,
        );
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        scroll.connect_scroll(clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, _, _| {
                view.pause_following();
                glib::Propagation::Proceed
            }
        ));
        scroll.connect_decelerate(clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _, _| view.pause_following()
        ));
        imp.synced_scrolled.add_controller(scroll);

        // A finger dragging the lines, which the list goes on scrolling
        // after it lets go, and which never claims the touch, as a tap
        // activates a line
        let drag = gtk::GestureDrag::new();
        drag.set_touch_only(true);
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let pause = clone!(
            #[weak(rename_to = view)]
            self,
            move || view.pause_following()
        );
        drag.connect_drag_begin(clone!(
            #[strong]
            pause,
            move |_, _, _| pause()
        ));
        drag.connect_drag_end(clone!(
            #[strong]
            pause,
            move |_, _, _| pause()
        ));
        drag.connect_cancel(clone!(
            #[strong]
            pause,
            move |_, _| pause()
        ));
        imp.synced_scrolled.add_controller(drag);

        // The scrollbar
        let scrollbar_drag = gtk::GestureDrag::new();
        scrollbar_drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        scrollbar_drag.connect_drag_begin(clone!(
            #[strong]
            pause,
            move |_, _, _| pause()
        ));
        scrollbar_drag.connect_drag_end(clone!(
            #[strong]
            pause,
            move |_, _, _| pause()
        ));
        imp.synced_scrolled
            .vscrollbar()
            .add_controller(scrollbar_drag);

        // Page Up, Page Down, Home and End, which the window gives the
        // list, and the arrows, which move the focus from line to line
        imp.synced_scrolled.connect_scroll_child(clone!(
            #[strong]
            pause,
            move |_, _, _| {
                pause();
                false
            }
        ));
        imp.lines_box
            .connect_move_cursor(move |_, _, _, _, _| pause());

        // Tab into the lines goes to the line sung, rather than to the
        // first or the last line, which the list would scroll to
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.focus_current_line()
        ));
        imp.lines_box.add_controller(focus);
    }

    // Only for the keys, which show the focus: a click on a line keeps the
    // focus on it
    fn focus_current_line(&self) {
        let keys = self
            .root()
            .and_downcast::<gtk::Window>()
            .is_some_and(|window| window.gets_focus_visible());
        let Some(row) = self
            .imp()
            .current
            .get()
            .line()
            .and_then(|line| self.row(line))
        else {
            return;
        };
        if keys && !row.has_focus() {
            // Once the focus has finished moving into the list
            glib::idle_add_local_once(move || {
                row.grab_focus();
            });
        }
    }

    fn activate_line(&self, line: i32) {
        let imp = self.imp();
        let Some((line, time)) = usize::try_from(line)
            .ok()
            .and_then(|line| Some((line, imp.lines.borrow().get(line)?.time)))
        else {
            return;
        };
        self.resume_following();
        self.set_current(Place::Line(line));
        let millis = u64::try_from(time.as_millis()).unwrap_or(u64::MAX);
        self.emit_by_name::<()>("seek", &[&millis]);
    }
}
