// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::audio::TimerEnd;

impl Window {
    // win.sleep-timer-start starts the timer for the minutes picked in the
    // popover of the sleep timer button, win.sleep-timer-stop-after
    // stops the music at the end of the "track" or the "work" that is
    // playing, its state being the one it waits for, and
    // win.sleep-timer-cancel turns it off
    pub(super) fn setup_sleep_timer(&self) {
        let player = self.player();
        let timer = player.sleep_timer();
        let settings = &self.imp().settings;

        self.add_action(&settings.create_action("sleep-timer-finish-track"));
        settings
            .bind("sleep-timer-finish-track", timer, "finish-track")
            .get_only()
            .build();
        settings
            .bind(
                "sleep-timer-minutes",
                &self.imp().playback_control.sleep_timer_minutes(),
                "value",
            )
            .build();

        self.add_action_entries([
            gio::ActionEntry::builder("sleep-timer-start")
                .activate(|win: &Window, _, _| {
                    let control = &win.imp().playback_control;
                    let spin = control.sleep_timer_minutes();
                    // Takes in minutes typed without pressing Enter
                    spin.update();
                    win.player()
                        .sleep_timer()
                        .start(spin.value_as_int().max(1) as u32);
                    control.close_sleep_timer();
                })
                .build(),
            gio::ActionEntry::builder("sleep-timer-stop-after")
                .parameter_type(Some(glib::VariantTy::STRING))
                .state("".to_variant())
                .activate(|win: &Window, action, target| {
                    let Some(end) = target.and_then(String::from_variant) else {
                        return;
                    };
                    let player = win.player();
                    let timer = player.sleep_timer();
                    // The one that's waiting turns off again
                    if action.state().and_then(|s| s.get::<String>()) == Some(end.clone()) {
                        timer.cancel();
                    } else {
                        match end.as_str() {
                            "track" => timer.stop_at_end(false),
                            "work" => timer.stop_at_end(true),
                            _ => return,
                        }
                    }
                    win.imp().playback_control.close_sleep_timer();
                })
                .build(),
            gio::ActionEntry::builder("sleep-timer-cancel")
                .activate(|win: &Window, _, _| {
                    win.player().sleep_timer().cancel();
                    win.imp().playback_control.close_sleep_timer();
                })
                .build(),
        ]);

        // What can stop follows the playing track, also while the popover
        // is open, e.g. as the queue loads at startup
        let id = player.state().connect_track_notify(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.update_stop_after()
        ));
        self.disconnect_on_close(player.state(), id);
        self.update_stop_after();

        for property in ["active", "remaining", "waiting-for"] {
            let id = timer.connect_notify_local(
                Some(property),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| win.update_sleep_timer()
                ),
            );
            self.disconnect_on_close(timer, id);
        }
        self.update_sleep_timer();
    }

    fn update_sleep_timer(&self) {
        let player = self.player();
        let timer = player.sleep_timer();
        let control = &self.imp().playback_control;
        if let Some(action) = self
            .lookup_action("sleep-timer-cancel")
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_enabled(timer.active());
        }

        // The button of the end it waits for is highlighted
        let end = match (timer.active(), timer.waiting_for()) {
            (true, TimerEnd::Track) => "track",
            (true, TimerEnd::Work) => "work",
            _ => "",
        };
        if let Some(action) = self
            .lookup_action("sleep-timer-stop-after")
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_state(&end.to_variant());
        }

        if !timer.active() {
            // The timer ran out, or was turned off
            control.set_sleep_timer(None);
            control.set_sleep_timer_remaining(None);
        } else if timer.waiting_for() == TimerEnd::Work {
            control.set_sleep_timer(Some(&i18n("Stopping After This Work")));
            control.set_sleep_timer_remaining(None);
        } else if timer.waiting_for() == TimerEnd::Track {
            control.set_sleep_timer(Some(&i18n("Stopping After This Track")));
            control.set_sleep_timer_remaining(None);
        } else {
            let remaining = utils::format_duration(timer.remaining());
            let status = i18n_k("Stopping in {time}", &[("time", &remaining)]);
            control.set_sleep_timer(Some(&status));
            control.set_sleep_timer_remaining(Some(&status));
        }
    }

    // The music can stop after the track that is playing, and after its
    // work while it's a movement of one
    fn update_stop_after(&self) {
        let track = self.player().state().track();
        if let Some(action) = self
            .lookup_action("sleep-timer-stop-after")
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_enabled(track.is_some());
        }
        let movement = track.is_some_and(|track| track.is_movement());
        self.imp()
            .playback_control
            .set_stop_after_work_visible(movement);
    }
}
