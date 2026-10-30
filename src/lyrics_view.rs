// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The lyrics of the playing track, in the playlist pane. They're only read
// while the pane shows them: when it opens, when the track changes, and
// when the track's tags or its LRC file change, as a program that finds
// lyrics can write the file while the track plays

use std::cell::{Cell, OnceCell, RefCell};

use adw::{prelude::*, subclass::prelude::*};
use glib::{clone, subclass::Signal};
use gtk::{CompositeTemplate, gio, glib};
use log::debug;
use std::sync::LazyLock;

use crate::{
    audio::{PlayerState, Track},
    lyrics::{self, Lyrics},
    utils,
};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/dachshund7/Dismas/lyrics-view.ui")]
    pub struct LyricsView {
        #[template_child]
        pub back_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub text_view: TemplateChild<gtk::TextView>,

        pub state: OnceCell<PlayerState>,
        // The track whose lyrics show, or are being read
        pub track: RefCell<Option<Track>>,
        // The number of reads so far: the lyrics of a read only show if no
        // other read started after it, as the same track can be read again
        // before the read before is done, e.g. when its tags change
        pub reads: Cell<u64>,
        // While the view is mapped
        pub track_handler: RefCell<Option<glib::SignalHandlerId>>,
        pub lrc_monitor: RefCell<Option<gio::FileMonitor>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LyricsView {
        const NAME: &'static str = "DismasLyricsView";
        type Type = super::LyricsView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("lyricsview");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
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
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> =
                LazyLock::new(|| vec![Signal::builder("back").build()]);
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
    pub struct LyricsView(ObjectSubclass<imp::LyricsView>)
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
    pub fn set_state(&self, state: &PlayerState) {
        let _ = self.imp().state.set(state.clone());
        if self.is_mapped() {
            self.start_following();
        }
    }

    pub fn connect_back<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "back",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    // The tags of a track were read again; if it's the one whose lyrics
    // show, its lyrics are too. Once the view is shown again, they're read
    // anyway
    pub fn track_changed(&self, track: &Track) {
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
        if imp.track_handler.borrow().is_none() {
            let handler = state.connect_track_notify(clone!(
                #[weak(rename_to = view)]
                self,
                move |_| view.follow_track()
            ));
            imp.track_handler.replace(Some(handler));
        }
        self.follow_track();
    }

    fn stop_following(&self) {
        let imp = self.imp();
        if let Some(handler) = imp.track_handler.take()
            && let Some(state) = imp.state.get()
        {
            state.disconnect(handler);
        }
        if let Some(monitor) = imp.lrc_monitor.take() {
            monitor.cancel();
        }
    }

    fn follow_track(&self) {
        self.watch_lrc_file();
        self.read();
    }

    // Reads the lyrics again when the LRC file of the playing track is
    // written, or removed
    fn watch_lrc_file(&self) {
        let imp = self.imp();
        if let Some(monitor) = imp.lrc_monitor.take() {
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
        let file = gio::File::for_path(lyrics::lrc_path(&path));
        let monitor = match file.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
            Ok(monitor) => monitor,
            Err(e) => {
                debug!("Unable to watch {path:?} for lyrics: {e}");
                return;
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
        imp.lrc_monitor.replace(Some(monitor));
    }

    // Reads the lyrics of the playing track, in another thread
    fn read(&self) {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return;
        };
        let track = state.track();
        let reads = imp.reads.get() + 1;
        imp.reads.set(reads);

        if *imp.track.borrow() != track {
            imp.window_title
                .set_subtitle(&track.as_ref().map(Track::title).unwrap_or_default());
            // Without a transition, which would show the lyrics of the
            // track before fading out
            imp.stack
                .set_visible_child_full("reading", gtk::StackTransitionType::None);
            imp.track.replace(track.clone());
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
            Some(Lyrics::Plain(text)) => {
                let buffer = imp.text_view.buffer();
                let (start, end) = buffer.bounds();
                // The same lyrics, read again, keep their place and the
                // text selected
                if buffer.text(&start, &end, false) != text {
                    buffer.set_text(&text);
                    // A caret at the start reads from the top
                    buffer.place_cursor(&buffer.start_iter());
                    if let Some(adjustment) = imp.text_view.vadjustment() {
                        adjustment.set_value(0.0);
                    }
                }
                imp.stack.set_visible_child_name("plain");
            }
            None => imp.stack.set_visible_child_name("none"),
        }
    }
}
