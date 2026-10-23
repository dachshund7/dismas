// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::RefCell;

use glib::clone;
use gst::prelude::*;
use gtk::{gio, glib, prelude::*, subclass::prelude::*};
use log::{debug, warn};

use crate::{
    audio::{Controller, PlaybackState, RepeatMode, Track},
    utils,
};

mod imp {
    use super::*;

    #[derive(Debug, glib::Properties)]
    #[properties(wrapper_type = super::WaveformGenerator)]
    pub struct WaveformGenerator {
        pub track: RefCell<Option<Track>>,
        #[property(name = "has-peaks", get = |imp: &Self| imp.peaks.borrow().is_some(), type = bool)]
        pub peaks: RefCell<Option<Vec<(f64, f64)>>>,
        pub pipeline: RefCell<Option<(gst::Element, gst::bus::BusWatchGuard)>>,
        pub settings: gio::Settings,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for WaveformGenerator {
        const NAME: &'static str = "WaveformGenerator";
        type Type = super::WaveformGenerator;

        fn new() -> Self {
            Self {
                track: RefCell::default(),
                peaks: RefCell::default(),
                pipeline: RefCell::default(),
                settings: utils::settings_manager(),
            }
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for WaveformGenerator {
        fn constructed(&self) {
            self.parent_constructed();

            // Turning the waveform off stops making it, and turning it on
            // makes the one of the current track
            self.settings.connect_changed(
                Some("show-waveform"),
                clone!(
                    #[weak(rename_to = this)]
                    self.obj(),
                    move |_, _| this.load_peaks()
                ),
            );
        }

        fn dispose(&self) {
            self.obj().stop_pipeline();
        }
    }
}

glib::wrapper! {
    pub struct WaveformGenerator(ObjectSubclass<imp::WaveformGenerator>);
}

impl Default for WaveformGenerator {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Controller for WaveformGenerator {
    fn set_playback_state(&self, _playback_state: PlaybackState) {}

    fn set_track(&self, track: &Track) {
        // A track that repeats keeps its peaks, or the pipeline making them,
        // which starts them empty
        let imp = self.imp();
        if imp.track.borrow().as_ref() == Some(track) && imp.peaks.borrow().is_some() {
            return;
        }
        imp.track.replace(Some(track.clone()));
        self.load_peaks();
    }

    fn set_position(&self, _position: u64, _notify: bool) {}
    fn set_repeat_mode(&self, _mode: RepeatMode) {}
}

impl WaveformGenerator {
    pub fn new() -> Self {
        WaveformGenerator::default()
    }

    pub fn peaks(&self) -> Option<Vec<(f64, f64)>> {
        (*self.imp().peaks.borrow()).as_ref().cloned()
    }

    // Shows the peaks once the pipeline is done, and caches them if it read
    // the whole track. The peaks of a pipeline that failed, e.g. without
    // the decoder for the track, are not cached, so they are read again
    // the next time; they are still shown, if there are any
    fn finish_peaks(&self, complete: bool) {
        if !complete {
            if self
                .imp()
                .peaks
                .borrow()
                .as_ref()
                .is_some_and(Vec::is_empty)
            {
                self.imp().peaks.replace(None);
            }
            self.notify_has_peaks();
            return;
        }

        if let Some(peaks) = self.peaks() {
            let Some(track) = self.imp().track.borrow().clone() else {
                self.notify_has_peaks();
                return;
            };

            if let Some(uuid) = track.uuid() {
                let mut cache = glib::user_cache_dir();
                cache.push("dismas");
                cache.push("waveforms");
                glib::mkdir_with_parents(&cache, 0o755);

                cache.push(format!("{uuid}.json"));

                let j = serde_json::to_string(&peaks).unwrap();
                let file = gio::File::for_path(&cache);
                file.replace_contents_async(
                    j,
                    None,
                    false,
                    gio::FileCreateFlags::NONE,
                    gio::Cancellable::NONE,
                    move |res| match res {
                        Ok(_) => debug!("Waveform cached at: {cache:?}"),
                        Err((_, e)) => warn!("Unable to cache the waveform at {cache:?}: {e}"),
                    },
                );
            }
        }

        self.notify_has_peaks();
    }

    async fn load_peaks_cache(&self, track: &Track, cache: impl AsRef<std::path::Path>) {
        let file = gio::File::for_path(&cache);
        let contents = file.load_contents_future().await;

        // Another track might have been set while we were loading
        if self.imp().track.borrow().as_ref() != Some(track) {
            return;
        }

        if let Ok((bytes, _tag)) = contents {
            match serde_json::from_slice(&bytes[..]) {
                Ok(p) => {
                    debug!("Loaded cached waveform file");
                    self.imp().peaks.replace(Some(p));
                    self.notify_has_peaks();
                }
                Err(err) => {
                    debug!("Could not parse cached waveform file: {err}");
                    self.generate_peaks();
                }
            }
        } else {
            debug!("Could not read cached waveform file");
            self.generate_peaks();
        }
    }

    fn load_peaks(&self) {
        // The pipeline of the previous track would otherwise keep adding
        // its peaks to the ones of this track, and save them as its own
        self.stop_pipeline();

        // The player shows a seek bar instead, so the track isn't read
        // a second time, and nothing is cached
        if !self.imp().settings.boolean("show-waveform") {
            self.clear_peaks();
            return;
        }

        let track = match self.imp().track.borrow().as_ref() {
            Some(s) => s.clone(),
            None => return,
        };

        if let Some(uuid) = track.uuid() {
            let mut cache = glib::user_cache_dir();
            cache.push("dismas");
            cache.push("waveforms");
            cache.push(format!("{uuid}.json"));

            let ctx = glib::MainContext::default();
            ctx.spawn_local(clone!(
                #[weak(rename_to = this)]
                self,
                async move { this.load_peaks_cache(&track, &cache).await }
            ));
        } else {
            // Without a name for its cache, there's no waveform to show;
            // the previous track's would stay otherwise
            self.clear_peaks();
        }
    }

    // Drops the peaks, and the view draws the waveform flat
    fn clear_peaks(&self) {
        self.imp().peaks.replace(None);
        self.notify_has_peaks();
    }

    // Stops generating the peaks, if a pipeline is running
    fn stop_pipeline(&self) {
        if let Some((pipeline, _bus_watch)) = self.imp().pipeline.take() {
            pipeline.send_event(gst::event::Eos::new());
            match pipeline.set_state(gst::State::Null) {
                Ok(_) => {}
                Err(err) => warn!("Unable to set existing pipeline to Null state: {err}"),
            }
        }
    }

    fn generate_peaks(&self) {
        self.stop_pipeline();

        let Some(track) = self.imp().track.borrow().clone() else {
            self.clear_peaks();
            return;
        };

        // Reset the peaks vector. The view draws the waveform flat until
        // the peaks are ready, rather than keep the previous track's; a
        // waveform from the cache comes soon enough to replace the
        // previous one directly
        let peaks: Vec<(f64, f64)> = Vec::new();
        self.imp().peaks.replace(Some(peaks));
        self.notify_has_peaks();

        let pipeline_str = "uridecodebin name=uridecodebin ! audioconvert ! audio/x-raw,channels=2 ! level name=level interval=250000000 ! fakesink name=faked";
        let pipeline = match gst::parse::launch(pipeline_str) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                warn!("Unable to generate the waveform: {err}");
                self.clear_peaks();
                return;
            }
        };

        let uridecodebin = pipeline
            .downcast_ref::<gst::Bin>()
            .unwrap()
            .by_name("uridecodebin")
            .unwrap();
        uridecodebin.set_property("uri", track.audio_uri());

        // A track of a CUE sheet is a part of its file, which the pipeline
        // seeks to once it's ready
        let (start, end) = track.segment();
        let segment = (!start.is_zero() || end.is_some()).then(|| {
            let clock_time =
                |d: std::time::Duration| gst::ClockTime::from_nseconds(d.as_nanos() as u64);
            (clock_time(start), end.map(clock_time))
        });
        let mut seeked = segment.is_none();

        let fakesink = pipeline
            .downcast_ref::<gst::Bin>()
            .unwrap()
            .by_name("faked")
            .unwrap();
        fakesink.set_property("qos", false);
        fakesink.set_property("sync", false);

        let bus = pipeline
            .bus()
            .expect("Pipeline without bus. Shouldn't happen!");

        debug!("Adding bus watch");
        let bus_watch = bus
            .add_watch_local(clone!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                pipeline,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move |_, msg| {
                    use gst::MessageView;

                    match msg.view() {
                        MessageView::AsyncDone(..) if !seeked => {
                            seeked = true;
                            let (start, end) = segment.unwrap();
                            let stop_type = if end.is_some() {
                                gst::SeekType::Set
                            } else {
                                gst::SeekType::None
                            };
                            let seek = pipeline.seek(
                                1.0,
                                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                                gst::SeekType::Set,
                                Some(start),
                                stop_type,
                                end,
                            );
                            let playing =
                                seek.is_ok() && pipeline.set_state(gst::State::Playing).is_ok();
                            if !playing {
                                warn!("Unable to generate the waveform of the track");
                                pipeline
                                    .set_state(gst::State::Null)
                                    .expect("Unable to set 'null' state");
                                this.imp().pipeline.replace(None);
                                this.finish_peaks(false);
                                return glib::ControlFlow::Break;
                            }
                        }
                        MessageView::Eos(..) => {
                            debug!("End of waveform stream");
                            pipeline
                                .set_state(gst::State::Null)
                                .expect("Unable to set 'null' state");
                            // We're done
                            this.imp().pipeline.replace(None);
                            this.finish_peaks(true);
                            return glib::ControlFlow::Break;
                        }
                        MessageView::Error(err) => {
                            warn!("Pipeline error: {err:?}");
                            pipeline
                                .set_state(gst::State::Null)
                                .expect("Unable to set 'null' state");
                            this.imp().pipeline.replace(None);
                            this.finish_peaks(false);
                            return glib::ControlFlow::Break;
                        }
                        MessageView::Element(element) => {
                            // Until the seek, from the start of the file
                            if let Some(s) = element.structure()
                                && s.has_name("level")
                                && seeked
                            {
                                let peaks_array = s.get::<&glib::ValueArray>("peak").unwrap();
                                let v1 = peaks_array[0].get::<f64>().unwrap();
                                let v2 = peaks_array[1].get::<f64>().unwrap();
                                // Normalize peaks between 0 and 1
                                let peak1 = f64::powf(10.0, v1 / 20.0);
                                let peak2 = f64::powf(10.0, v2 / 20.0);
                                if let Some(ref mut peaks) = *this.imp().peaks.borrow_mut() {
                                    peaks.push((peak1, peak2));
                                }
                            }
                        }
                        _ => (),
                    }

                    glib::ControlFlow::Continue
                }
            ))
            .expect("failed to add bus watch");

        let state = if segment.is_some() {
            gst::State::Paused
        } else {
            gst::State::Playing
        };
        match pipeline.set_state(state) {
            Ok(_) => {
                self.imp().pipeline.replace(Some((pipeline, bus_watch)));
            }
            Err(err) => {
                warn!("Unable to generate the waveform: {err}");
                pipeline
                    .set_state(gst::State::Null)
                    .expect("Pipeline reset failed");
                self.clear_peaks();
            }
        }
    }
}
