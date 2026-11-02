// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Zooming the views that can be made larger or smaller: the covers of the
//! album grid and of the works, and the lyrics' text. Each keeps the step
//! it's zoomed to in a setting, 0 for the normal size, and goes a step at a
//! time, with Ctrl+plus, Ctrl+minus and Ctrl+0, as the HIG has it, with
//! Ctrl and the mouse wheel, and with a pinch on a touchscreen or a
//! touchpad.

use std::{cell::RefCell, rc::Rc};

use gtk::{gdk, gio, glib, prelude::*};

// How a view is zoomed
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Zoom {
    In,
    Out,
    Reset,
}

impl Zoom {
    // The step a view goes to from `step`, among those from `min` to
    // `max`, or nothing at the end it's going to; Reset goes back to the
    // normal size, 0
    pub(crate) fn step_from(self, step: i32, min: i32, max: i32) -> Option<i32> {
        let to = match self {
            Zoom::In => step + 1,
            Zoom::Out => step - 1,
            Zoom::Reset => 0,
        };
        (to != step && (min..=max).contains(&to)).then_some(to)
    }
}

// The steps a view is zoomed to: the setting that keeps the step, and
// the smallest and the largest
pub(crate) struct ZoomSteps {
    pub(crate) key: &'static str,
    pub(crate) min: i32,
    pub(crate) max: i32,
}

impl ZoomSteps {
    // The step the view is at
    pub(crate) fn step(&self, settings: &gio::Settings) -> i32 {
        settings.int(self.key).clamp(self.min, self.max)
    }

    // Zooms the view a step, or back to its normal size; false when it's
    // at that end already
    pub(crate) fn zoom(&self, settings: &gio::Settings, zoom: Zoom) -> bool {
        zoom.step_from(settings.int(self.key), self.min, self.max)
            .is_some_and(|step| settings.set_int(self.key, step).is_ok())
    }

    // What a table with a value for each step, from the smallest, has at
    // `step`
    pub(crate) fn at<T: Copy>(&self, table: &[T], step: i32) -> T {
        let index = usize::try_from(step - self.min).unwrap_or(0);
        table[index.min(table.len() - 1)]
    }
}

// How many pixels a touchpad scrolls for a step, which takes about a
// third of its height on a laptop; also the volume's
pub(crate) const SCROLL_PIXELS_PER_STEP: f64 = 50.0;

// How long the scrolling stops before what's left of a step is dropped, so
// that it doesn't add to a scroll made later
const SCROLL_PAUSE_MS: u32 = 500;

// The most steps a scroll event makes: as many as the view with the most
// steps has from one end to the other, where more only ring the bell
const MAX_SCROLL_STEPS: f64 = 4.0;

// Adds up the scrolling done with Ctrl held into zoom steps. A notch of a
// mouse wheel is a step, which a high-resolution wheel sends in fractions,
// and a touchpad scrolls by pixels. Scrolling up zooms in, as in browsers
// and file managers
#[derive(Debug, Default)]
struct ScrollSteps {
    // Up to a step, in either direction; down is positive
    total: f64,
    // The time of the last event, in milliseconds
    last: Option<u32>,
}

impl ScrollSteps {
    // The steps that the scroll at `time` makes: positive to zoom in,
    // negative to zoom out. Turning back drops what had been scrolled the
    // other way, and so does a pause
    pub(crate) fn add(&mut self, dy: f64, unit: gdk::ScrollUnit, time: u32) -> i32 {
        let delta = match unit {
            gdk::ScrollUnit::Surface => dy / SCROLL_PIXELS_PER_STEP,
            _ => dy,
        };
        if !delta.is_finite() || delta == 0.0 {
            return 0;
        }
        // The times of events count from when the display started, and
        // go back to 0 after 49 days
        let paused = self
            .last
            .replace(time)
            .is_some_and(|last| time.wrapping_sub(last) > SCROLL_PAUSE_MS);
        if paused || (self.total != 0.0 && delta.signum() != self.total.signum()) {
            self.total = 0.0;
        }
        self.total += delta;
        let steps = self.total.trunc();
        self.total -= steps;
        -(steps.clamp(-MAX_SCROLL_STEPS, MAX_SCROLL_STEPS) as i32)
    }
}

// How much farther apart, or closer, the fingers of a pinch move for a
// step: a quarter of the distance between them
const PINCH_STEP: f64 = 1.25;

// Turns a pinch into zoom steps, as its fingers move apart or together
#[derive(Debug)]
struct PinchSteps {
    // The scale of the pinch at the last step, from 1 where it started
    base: f64,
}

impl Default for PinchSteps {
    fn default() -> Self {
        Self { base: 1.0 }
    }
}

impl PinchSteps {
    // The step the pinch makes as its scale changes, if it makes one
    pub(crate) fn update(&mut self, scale: f64) -> Option<Zoom> {
        let ratio = scale / self.base;
        let zoom = if ratio >= PINCH_STEP {
            Zoom::In
        } else if ratio <= 1.0 / PINCH_STEP {
            Zoom::Out
        } else {
            return None;
        };
        self.base = scale;
        Some(zoom)
    }
}

// Zooms the widget's view with Ctrl and the mouse wheel or a touchpad,
// and with a pinch. The scrolling is in the capture phase, so that the
// view doesn't scroll as well
pub(crate) fn add_zoom_input(widget: &impl IsA<gtk::Widget>, zoom: impl Fn(Zoom) + 'static) {
    let zoom = Rc::new(zoom);
    let steps = RefCell::new(ScrollSteps::default());
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    scroll.connect_scroll({
        let zoom = zoom.clone();
        move |controller, _, dy| {
            if !controller
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
            {
                return glib::Propagation::Proceed;
            }
            let n = steps
                .borrow_mut()
                .add(dy, controller.unit(), controller.current_event_time());
            let step = if n > 0 { Zoom::In } else { Zoom::Out };
            for _ in 0..n.unsigned_abs() {
                zoom(step);
            }
            glib::Propagation::Stop
        }
    });
    widget.add_controller(scroll);
    add_pinch(widget, move |step| zoom(step));
}

// Zooms the widget's view with a pinch, which GTK takes from two fingers on
// a touchscreen or on a touchpad, never from the mouse. It's in the capture
// phase, and claims the fingers as it begins, which cancels the scrolling
// and the long press that the first finger started
fn add_pinch(widget: &impl IsA<gtk::Widget>, zoom: impl Fn(Zoom) + 'static) {
    let pinch = gtk::GestureZoom::new();
    pinch.set_propagation_phase(gtk::PropagationPhase::Capture);
    let steps = Rc::new(RefCell::new(PinchSteps::default()));
    pinch.connect_begin({
        let steps = steps.clone();
        move |gesture, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            *steps.borrow_mut() = PinchSteps::default();
        }
    });
    pinch.connect_scale_changed(move |_, scale| {
        let step = steps.borrow_mut().update(scale);
        if let Some(step) = step {
            zoom(step);
        }
    });
    widget.add_controller(pinch);
}

#[cfg(test)]
mod tests {
    use gtk::gdk::ScrollUnit;

    use super::{PinchSteps, ScrollSteps, Zoom};

    #[test]
    fn a_pinch_steps_as_the_fingers_move_a_quarter_apart() {
        let mut pinch = PinchSteps::default();
        assert_eq!(pinch.update(1.1), None);
        assert_eq!(pinch.update(1.25), Some(Zoom::In));
        // From there, a quarter farther again
        assert_eq!(pinch.update(1.5), None);
        assert_eq!(pinch.update(1.6), Some(Zoom::In));
        // And back together
        assert_eq!(pinch.update(1.3), None);
        assert_eq!(pinch.update(1.25), Some(Zoom::Out));
        assert_eq!(pinch.update(0.9), Some(Zoom::Out));
    }

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
        assert_eq!(steps.add(-1.0, ScrollUnit::Wheel, 0), 1);
        assert_eq!(steps.add(1.0, ScrollUnit::Wheel, 0), -1);
        assert_eq!(steps.add(2.0, ScrollUnit::Wheel, 0), -2);
    }

    #[test]
    fn fractions_of_a_notch_add_up_to_a_step() {
        let mut steps = ScrollSteps::default();
        assert_eq!(steps.add(-0.5, ScrollUnit::Wheel, 0), 0);
        assert_eq!(steps.add(-0.25, ScrollUnit::Wheel, 0), 0);
        assert_eq!(steps.add(-0.25, ScrollUnit::Wheel, 0), 1);
        // Turning back starts again
        assert_eq!(steps.add(-0.75, ScrollUnit::Wheel, 0), 0);
        assert_eq!(steps.add(0.5, ScrollUnit::Wheel, 0), 0);
        assert_eq!(steps.add(0.5, ScrollUnit::Wheel, 0), -1);
    }

    #[test]
    fn what_is_left_of_a_step_goes_after_a_pause() {
        let mut steps = ScrollSteps::default();
        assert_eq!(steps.add(-0.75, ScrollUnit::Wheel, 1_000), 0);
        assert_eq!(steps.add(-0.25, ScrollUnit::Wheel, 1_500), 1);
        assert_eq!(steps.add(-0.75, ScrollUnit::Wheel, 2_000), 0);
        // A scroll made later starts again
        assert_eq!(steps.add(-0.25, ScrollUnit::Wheel, 2_501), 0);
        assert_eq!(steps.add(-0.75, ScrollUnit::Wheel, 2_600), 1);
        // The times of events go back to 0 after 49 days
        assert_eq!(steps.add(-0.75, ScrollUnit::Wheel, u32::MAX - 50), 0);
        assert_eq!(steps.add(-0.25, ScrollUnit::Wheel, 50), 1);
    }

    #[test]
    fn a_scroll_makes_no_more_steps_than_a_view_has() {
        let mut steps = ScrollSteps::default();
        assert_eq!(steps.add(-1e12, ScrollUnit::Wheel, 0), 4);
        assert_eq!(steps.add(f64::MAX, ScrollUnit::Wheel, 0), -4);
        assert_eq!(steps.add(f64::MIN, ScrollUnit::Surface, 0), 4);
        // Nothing is left over for the next one
        assert_eq!(steps.add(-0.5, ScrollUnit::Wheel, 0), 0);
    }

    #[test]
    fn a_touchpad_scrolls_by_pixels() {
        let mut steps = ScrollSteps::default();
        assert_eq!(steps.add(-20.0, ScrollUnit::Surface, 0), 0);
        assert_eq!(steps.add(-20.0, ScrollUnit::Surface, 0), 0);
        assert_eq!(steps.add(-20.0, ScrollUnit::Surface, 0), 1);
        assert_eq!(steps.add(0.0, ScrollUnit::Surface, 0), 0);
    }
}
