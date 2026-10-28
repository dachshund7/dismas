// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, OnceCell, RefCell},
    time::{Duration, Instant},
};

use async_channel::Sender;
use glib::clone;
use gtk::{glib, prelude::*, subclass::prelude::*};
use log::error;

use crate::audio::PlaybackAction;

// What the music stops at the end of, rather than when the timer runs out
#[derive(Clone, Copy, Debug, glib::Enum, PartialEq, Eq, Default)]
#[enum_type(name = "DismasTimerEnd")]
pub enum TimerEnd {
    #[default]
    None,
    // The track playing: the timer ran out and it's playing to its end, or
    // the music was asked to stop at its end
    Track,
    // The work playing, which goes on while its movements follow each
    // other: the music was asked to stop at its end
    Work,
}

mod imp {
    use super::*;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::SleepTimer)]
    pub struct SleepTimer {
        // Whether the timer is counting down, or waiting for the end of
        // the track once it ran out
        #[property(get)]
        pub active: Cell<bool>,
        // The seconds left before the music stops, 0 once the timer ran out
        #[property(get)]
        pub remaining: Cell<u64>,
        // Whether the music stops at the end of the track that is playing
        // when the timer runs out, instead of right away
        #[property(get, set)]
        pub finish_track: Cell<bool>,
        #[property(get, builder(TimerEnd::default()))]
        pub waiting_for: Cell<TimerEnd>,

        pub deadline: Cell<Option<Instant>>,
        pub source: RefCell<Option<glib::SourceId>>,
        pub sender: OnceCell<Sender<PlaybackAction>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SleepTimer {
        const NAME: &'static str = "DismasSleepTimer";
        type Type = super::SleepTimer;
    }

    #[glib::derived_properties]
    impl ObjectImpl for SleepTimer {
        fn dispose(&self) {
            if let Some(source) = self.source.take() {
                source.remove();
            }
        }
    }
}

// Counts down to the time when the music stops, and tells the player
// with a SleepTimerExpired action once it's there
glib::wrapper! {
    pub struct SleepTimer(ObjectSubclass<imp::SleepTimer>);
}

impl SleepTimer {
    pub fn new(sender: Sender<PlaybackAction>) -> Self {
        let res: Self = glib::Object::new();
        res.imp().sender.set(sender).unwrap();
        res
    }

    // Starts counting down again from the given number of minutes
    pub fn start(&self, minutes: u32) {
        self.stop_countdown();

        let imp = self.imp();
        imp.deadline.set(Some(
            Instant::now() + Duration::from_secs(u64::from(minutes) * 60),
        ));
        self.set_waiting_for(TimerEnd::None);
        self.set_active(true);
        self.update_remaining();

        let source = glib::timeout_add_local(
            Duration::from_secs(1),
            clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || this.tick()
            ),
        );
        imp.source.replace(Some(source));
    }

    // Turns the timer off, so the music keeps playing
    pub fn cancel(&self) {
        self.stop_countdown();
        self.imp().deadline.set(None);
        self.set_remaining(0);
        self.set_waiting_for(TimerEnd::None);
        self.set_active(false);
    }

    // Stops the music at the end of the track that is playing, or at the
    // end of its work, without counting down; the player works out where
    // the work ends, as the tracks after it can change meanwhile
    pub fn stop_at_end(&self, work: bool) {
        self.stop_countdown();
        self.imp().deadline.set(None);
        self.set_remaining(0);
        self.set_waiting_for(if work {
            TimerEnd::Work
        } else {
            TimerEnd::Track
        });
        self.set_active(true);
    }

    // Waits for the end of the track instead of stopping the music now
    pub fn wait_for_track_end(&self) {
        self.set_waiting_for(TimerEnd::Track);
    }

    // There's no track playing any more, as the queue was emptied or
    // another playlist was selected. Waiting for the end of the track or
    // the work is over, so the timer turns off; a countdown goes on, as it
    // doesn't depend on a track
    pub fn track_gone(&self) {
        if self.waiting_for() != TimerEnd::None {
            self.cancel();
        }
    }

    fn tick(&self) -> glib::ControlFlow {
        if self.update_remaining() > 0 {
            return glib::ControlFlow::Continue;
        }

        // The source is removed by returning Break
        self.imp().source.take();
        self.imp().deadline.set(None);
        if let Some(sender) = self.imp().sender.get()
            && let Err(e) = sender.try_send(PlaybackAction::SleepTimerExpired)
        {
            error!("Failed to send SleepTimerExpired: {e}");
        }

        glib::ControlFlow::Break
    }

    // Rounds up, so the timer shows 0:00 only when it runs out
    fn update_remaining(&self) -> u64 {
        let remaining = self.imp().deadline.get().map_or(0, |deadline| {
            let left = deadline.saturating_duration_since(Instant::now());
            left.as_secs() + u64::from(left.subsec_nanos() > 0)
        });
        self.set_remaining(remaining);
        remaining
    }

    fn stop_countdown(&self) {
        if let Some(source) = self.imp().source.take() {
            source.remove();
        }
    }

    fn set_active(&self, active: bool) {
        if self.imp().active.replace(active) != active {
            self.notify_active();
        }
    }

    fn set_remaining(&self, remaining: u64) {
        if self.imp().remaining.replace(remaining) != remaining {
            self.notify_remaining();
        }
    }

    fn set_waiting_for(&self, end: TimerEnd) {
        if self.imp().waiting_for.replace(end) != end {
            self.notify_waiting_for();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_at_the_end_of_the_track_or_the_work() {
        let (sender, _receiver) = async_channel::unbounded();
        let timer = SleepTimer::new(sender);

        timer.stop_at_end(true);
        assert!(timer.active());
        assert_eq!(timer.waiting_for(), TimerEnd::Work);
        assert_eq!(timer.remaining(), 0);

        timer.stop_at_end(false);
        assert_eq!(timer.waiting_for(), TimerEnd::Track);

        timer.stop_at_end(true);
        timer.cancel();
        assert!(!timer.active());
        assert_eq!(timer.waiting_for(), TimerEnd::None);
    }

    #[test]
    fn waiting_for_the_end_stops_once_the_track_is_gone() {
        let (sender, _receiver) = async_channel::unbounded();
        let timer = SleepTimer::new(sender);

        for work in [false, true] {
            timer.stop_at_end(work);
            timer.track_gone();
            assert!(!timer.active());
        }

        // The track the timer ran out during, with Finish Last Track
        timer.start(10);
        timer.wait_for_track_end();
        timer.track_gone();
        assert!(!timer.active());

        // A countdown isn't about a track
        timer.start(10);
        timer.track_gone();
        assert!(timer.active());
        assert!(timer.remaining() > 0);
        timer.cancel();
    }
}
