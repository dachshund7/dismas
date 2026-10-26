// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Window {
    // win.sleep-timer-start starts the timer for the minutes picked in the
    // popover of the sleep timer button, and win.sleep-timer-cancel turns
    // it off
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
            gio::ActionEntry::builder("sleep-timer-cancel")
                .activate(|win: &Window, _, _| {
                    win.player().sleep_timer().cancel();
                    win.imp().playback_control.close_sleep_timer();
                })
                .build(),
        ]);

        for property in ["active", "remaining", "finishing-track"] {
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

        if !timer.active() {
            // The timer ran out, or was turned off
            control.set_sleep_timer(None);
        } else if timer.finishing_track() {
            control.set_sleep_timer(Some(&i18n("Stopping After This Track")));
        } else {
            let remaining = utils::format_duration(timer.remaining());
            control.set_sleep_timer(Some(&i18n_k("Stopping in {time}", &[("time", &remaining)])));
        }
    }
}
