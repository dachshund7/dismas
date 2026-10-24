// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

use adw::prelude::AnimationExt;

use super::*;

// How long the window is left alone before the controls fade out
const ZEN_DELAY: Duration = Duration::from_secs(3);

// The controls fade out slowly, and come back quickly when they're wanted
const FADE_OUT_MS: u32 = 600;
const FADE_IN_MS: u32 = 200;

impl Window {
    // Zen mode: with the playlist hidden, the controls of the player fade out
    // once the pointer leaves the window, focused or not: with focus following
    // the pointer, the window keeps the focus when the pointer goes to an empty
    // part of the screen. They come back when the pointer enters the window,
    // or when it gets the focus, and on touchscreens with a tap
    pub(super) fn setup_zen_mode(&self) {
        let imp = self.imp();

        // Entering and leaving the window. GTK can believe the pointer is
        // still over it once it's gone: a drag let go outside the window,
        // e.g. resizing it, puts the pointer back over the window, without a
        // leave to follow. So where the pointer is gets asked for, and
        // moving over the window brings the controls back, as it might not
        // count as entering it. Moving into a popover leaves the window and
        // enters it again at once, which the delay before fading out makes
        // up for, and moving over the popover keeps the controls there
        let motion = gtk::EventControllerMotion::new();
        // Before the widgets under the pointer, which can stop the motion
        motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        motion.connect_contains_pointer_notify(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.update_zen()
        ));
        motion.connect_motion(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _, _| {
                let imp = win.imp();
                if imp.zen_timeout.borrow().is_some() || imp.main_box.zen_progress() > 0.0 {
                    win.show_controls();
                }
            }
        ));
        self.add_controller(motion);

        // A tap brings the controls back, as touch has no pointer to enter
        // the window with, and the wait before fading them out starts again,
        // so they stay while they're used. Faded out, they don't take the
        // tap, which would press a button that can't be seen
        let tap = gtk::GestureClick::new();
        tap.set_touch_only(true);
        tap.set_propagation_phase(gtk::PropagationPhase::Capture);
        tap.connect_pressed(clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _, _, _| {
                win.show_controls();
                win.update_zen();
            }
        ));
        self.add_controller(tap);

        // Getting the focus brings the controls back, for a while if the
        // pointer is elsewhere, e.g. with a keybinding of the window manager
        self.connect_is_active_notify(|win| {
            if win.is_active() {
                win.show_controls();
            }
            win.update_zen();
        });
        self.connect_playlist_visible_notify(Self::update_zen);
        self.connect_page_notify(Self::update_zen);
        // The wait starts again at each step of a resize, as dragging an edge
        // of the window has the pointer on the edge, outside the window. A
        // window resized by the window manager can also end up away from the
        // pointer without telling GTK the pointer left
        let resized = |win: &Self| {
            win.cancel_zen_timeout();
            win.update_zen();
        };
        self.connect_default_width_notify(resized);
        self.connect_default_height_notify(resized);
        imp.settings.connect_changed(
            Some("zen-mode"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| win.update_zen()
            ),
        );

        // A window hidden to play in the background comes back with its
        // controls
        self.connect_unmap(|win| {
            win.cancel_zen_timeout();
            if let Some(animation) = win.imp().zen_animation.take() {
                animation.pause();
            }
            win.set_zen_progress(0.0);
        });
    }

    // Whether the controls can fade out: in zen mode, on the player without
    // the playlist, with the pointer elsewhere
    fn is_left_alone(&self) -> bool {
        self.imp().settings.boolean("zen-mode")
            && !self.playlist_visible()
            && self.page() == "player"
            && self.pointer_widget().is_none()
    }

    fn update_zen(&self) {
        if !self.is_left_alone() {
            self.show_controls();
            return;
        }

        let imp = self.imp();
        if imp.zen_timeout.borrow().is_some() || self.is_fading_out() {
            return;
        }
        let source = glib::timeout_add_local_once(
            ZEN_DELAY,
            clone!(
                #[weak(rename_to = win)]
                self,
                move || {
                    win.imp().zen_timeout.take();
                    if win.is_left_alone() {
                        win.animate_zen(true);
                    }
                }
            ),
        );
        imp.zen_timeout.replace(Some(source));
    }

    // Whether the controls are fading out, or have faded out
    fn is_fading_out(&self) -> bool {
        let imp = self.imp();
        match imp.zen_animation.borrow().as_ref() {
            Some(animation) if animation.state() == adw::AnimationState::Playing => {
                animation.value_to() == 1.0
            }
            _ => imp.main_box.zen_progress() == 1.0,
        }
    }

    fn show_controls(&self) {
        self.cancel_zen_timeout();
        self.animate_zen(false);
    }

    fn cancel_zen_timeout(&self) {
        if let Some(source) = self.imp().zen_timeout.take() {
            source.remove();
        }
    }

    // Fades the controls out or back in, from where they are
    fn animate_zen(&self, zen: bool) {
        let imp = self.imp();
        let to = if zen { 1.0 } else { 0.0 };
        if let Some(animation) = imp.zen_animation.borrow().as_ref()
            && animation.value_to() == to
            && animation.state() == adw::AnimationState::Playing
        {
            return;
        }
        // A playing animation keeps itself alive, so it's paused rather
        // than only dropped
        if let Some(animation) = imp.zen_animation.take() {
            animation.pause();
        }

        let from = imp.main_box.zen_progress();
        if from == to {
            return;
        }

        let target = adw::CallbackAnimationTarget::new(clone!(
            #[weak(rename_to = win)]
            self,
            move |value| win.set_zen_progress(value)
        ));
        let (duration, easing) = if zen {
            (FADE_OUT_MS, adw::Easing::EaseInOutCubic)
        } else {
            (FADE_IN_MS, adw::Easing::EaseOutCubic)
        };
        let animation = adw::TimedAnimation::builder()
            .widget(&*imp.main_box)
            .value_from(from)
            .value_to(to)
            .duration(duration)
            .easing(easing)
            .target(&target)
            .build();
        imp.zen_animation.replace(Some(animation.clone()));
        animation.play();
    }

    fn set_zen_progress(&self, progress: f64) {
        let imp = self.imp();
        imp.main_box.set_zen_progress(progress);
        imp.player_header_bar.set_opacity(1.0 - progress);
        imp.player_header_bar.set_can_target(progress < 1.0);
    }
}
