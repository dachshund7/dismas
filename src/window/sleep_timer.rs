// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Window {
    // win.sleep-timer-start starts the timer for the minutes picked in the
    // popover of the sleep timer button, and win.sleep-timer-cancel turns
    // it off
    pub(super) fn setup_sleep_timer(&self) {
        let Some(player) = self.player() else {
            return;
        };
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
                    if let Some(player) = win.player() {
                        player
                            .sleep_timer()
                            .start(spin.value_as_int().max(1) as u32);
                    }
                    control.close_sleep_timer();
                })
                .build(),
            gio::ActionEntry::builder("sleep-timer-cancel")
                .activate(|win: &Window, _, _| {
                    if let Some(player) = win.player() {
                        player.sleep_timer().cancel();
                    }
                    win.imp().playback_control.close_sleep_timer();
                })
                .build(),
        ]);

        for property in ["active", "remaining", "finishing-track"] {
            timer.connect_notify_local(
                Some(property),
                clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| win.update_sleep_timer()
                ),
            );
        }
        self.update_sleep_timer();
    }

    fn update_sleep_timer(&self) {
        let Some(player) = self.player() else {
            return;
        };
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
            let remaining = format_duration(timer.remaining());
            control.set_sleep_timer(Some(&i18n_k("Stopping in {time}", &[("time", &remaining)])));
        }
    }
}

// Formats seconds as m:ss, or h:mm:ss from an hour
fn format_duration(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::format_duration;

    #[test]
    fn formats_duration() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(59), "0:59");
        assert_eq!(format_duration(15 * 60), "15:00");
        assert_eq!(format_duration(3600), "1:00:00");
        assert_eq!(format_duration(2 * 3600 - 1), "1:59:59");
    }
}
