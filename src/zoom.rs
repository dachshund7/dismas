// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Zooming the views that can be made larger or smaller: the covers of the
//! album grid and of the works, and the lyrics' text. Each keeps the step
//! it's zoomed to in a setting, 0 for the normal size, and goes a step at a
//! time, with Ctrl+plus, Ctrl+minus and Ctrl+0, as the HIG has it, and with
//! Ctrl and the mouse wheel.

use gtk::gdk;

// How a view is zoomed
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zoom {
    In,
    Out,
    Reset,
}

impl Zoom {
    // The step a view goes to from `step`, among those from `min` to
    // `max`, or nothing at the end it's going to; Reset goes back to the
    // normal size, 0
    pub fn step_from(self, step: i32, min: i32, max: i32) -> Option<i32> {
        let to = match self {
            Zoom::In => step + 1,
            Zoom::Out => step - 1,
            Zoom::Reset => 0,
        };
        (to != step && (min..=max).contains(&to)).then_some(to)
    }
}

// How many pixels a touchpad scrolls for a step, which takes about a
// third of its height on a laptop
const SCROLL_PIXELS_PER_STEP: f64 = 50.0;

// Adds up the scrolling done with Ctrl held into zoom steps. A notch of a
// mouse wheel is a step, which a high-resolution wheel sends in fractions,
// and a touchpad scrolls by pixels. Scrolling up zooms in, as in browsers
// and file managers
#[derive(Debug, Default)]
pub struct ScrollSteps {
    // Up to a step, in either direction; down is positive
    total: f64,
}

impl ScrollSteps {
    // The steps that the scroll makes: positive to zoom in, negative to
    // zoom out. Turning back drops what had been scrolled the other way
    pub fn add(&mut self, dy: f64, unit: gdk::ScrollUnit) -> i32 {
        let delta = match unit {
            gdk::ScrollUnit::Surface => dy / SCROLL_PIXELS_PER_STEP,
            _ => dy,
        };
        if !delta.is_finite() || delta == 0.0 {
            return 0;
        }
        if self.total != 0.0 && delta.signum() != self.total.signum() {
            self.total = 0.0;
        }
        self.total += delta;
        let steps = self.total.trunc();
        self.total -= steps;
        -(steps as i32)
    }
}

#[cfg(test)]
mod tests {
    use gtk::gdk::ScrollUnit;

    use super::{ScrollSteps, Zoom};

    #[test]
    fn zooming_goes_a_step_at_a_time_within_the_steps() {
        assert_eq!(Zoom::In.step_from(0, -2, 1), Some(1));
        assert_eq!(Zoom::In.step_from(1, -2, 1), None);
        assert_eq!(Zoom::Out.step_from(-1, -2, 1), Some(-2));
        assert_eq!(Zoom::Out.step_from(-2, -2, 1), None);
        assert_eq!(Zoom::Reset.step_from(-2, -2, 1), Some(0));
        assert_eq!(Zoom::Reset.step_from(0, -2, 1), None);
    }

    #[test]
    fn a_notch_of_the_wheel_is_a_step() {
        let mut steps = ScrollSteps::default();
        // Up zooms in
        assert_eq!(steps.add(-1.0, ScrollUnit::Wheel), 1);
        assert_eq!(steps.add(1.0, ScrollUnit::Wheel), -1);
        assert_eq!(steps.add(2.0, ScrollUnit::Wheel), -2);
    }

    #[test]
    fn fractions_of_a_notch_add_up_to_a_step() {
        let mut steps = ScrollSteps::default();
        assert_eq!(steps.add(-0.5, ScrollUnit::Wheel), 0);
        assert_eq!(steps.add(-0.25, ScrollUnit::Wheel), 0);
        assert_eq!(steps.add(-0.25, ScrollUnit::Wheel), 1);
        // Turning back starts again
        assert_eq!(steps.add(-0.75, ScrollUnit::Wheel), 0);
        assert_eq!(steps.add(0.5, ScrollUnit::Wheel), 0);
        assert_eq!(steps.add(0.5, ScrollUnit::Wheel), -1);
    }

    #[test]
    fn a_touchpad_scrolls_by_pixels() {
        let mut steps = ScrollSteps::default();
        assert_eq!(steps.add(-20.0, ScrollUnit::Surface), 0);
        assert_eq!(steps.add(-20.0, ScrollUnit::Surface), 0);
        assert_eq!(steps.add(-20.0, ScrollUnit::Surface), 1);
        assert_eq!(steps.add(0.0, ScrollUnit::Surface), 0);
    }
}
