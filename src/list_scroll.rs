// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Gliding a list of tracks to one of them, to show it, and flashing its
//! row: the queue and an album's tracks go to a track this way, and the
//! works and the lines of lyrics glide the same way

use std::cell::Cell;

use glib::clone;
use gtk::{glib, graphene, prelude::*};

use crate::{audio::Track, queue_row::QueueRow};

// The frames a list is waited for to get its size, when it's just shown:
// one hidden again before it gets it isn't scrolled to a track, rather
// than redrawing the window at each frame until it's shown again
const SIZE_WAIT_FRAMES: u32 = 60;

// How long gliding to a track takes, however far it is: the list gets up
// to speed quickly, most of the way goes by in the first half, and the
// second half settles on the track (`ease`)
pub(crate) const GLIDE_DURATION: std::time::Duration = std::time::Duration::from_millis(350);

// Glides the scrolled place of a list to a value, quickly, then settling,
// as `flash_track` glides to a track; libadwaita jumps instead when
// animations are off. The glide can be stopped with the animation
pub(crate) fn glide_adjustment(
    widget: &impl IsA<gtk::Widget>,
    adjustment: &gtk::Adjustment,
    target: f64,
) -> adw::TimedAnimation {
    let animation = adw::TimedAnimation::new(
        widget,
        adjustment.value(),
        target,
        GLIDE_DURATION.as_millis() as u32,
        adw::PropertyAnimationTarget::new(adjustment, "value"),
    );
    animation.set_easing(adw::Easing::Ease);
    adw::prelude::AnimationExt::play(&animation);
    animation
}

// Scrolls a list to a track, gliding to it, places it in the middle of the
// list, if it can scroll that far, and makes it flash for a moment, like
// its CSS animation. The list only lays out the rows in its view, so the
// glide aims at where the row should be until the row is laid out, and at
// the row from then on; tracks can go in or out of the list meanwhile, and
// `pos`, where the track was, is looked up again when they do. Without
// animations, as GTK's settings can ask, the list jumps to the track. The
// list was usually just shown, and can only scroll once it has its size,
// so this waits for a frame where it has it. The list may still be going
// on from a fling, which stops. `done` runs once the glide ends, whether it
// got to the track or gave up, but not when it's stopped with its id
pub(crate) fn flash_track(
    view: &gtk::ListView,
    pos: u32,
    track: &Track,
    done: impl FnOnce() + 'static,
) -> gtk::TickCallbackId {
    stop_kinetic_scrolling(view);
    let waited = Cell::new(0);
    let glide = Cell::new(None::<Glide>);
    let pos = Cell::new(pos);
    // The frames since the list was asked to scroll to the track itself,
    // when the glide ended where its row wasn't
    let fallback = Cell::new(None::<u32>);
    let done = Cell::new(Some(done));
    view.add_tick_callback(clone!(
        #[strong]
        track,
        move |view, clock| {
            let step = || {
                if view.height() == 0 {
                    return wait_for_size(&waited);
                }
                let Some(adjustment) = view.vadjustment() else {
                    return glib::ControlFlow::Break;
                };
                // Only reads where the row is, as changing the rows of the
                // list crashes GTK
                let bounds = track_bounds(view, &track);
                if bounds.is_none() {
                    pos.set(position_now(view, &track, pos.get()));
                }

                if let Some(frames) = fallback.get() {
                    if bounds.is_none() && frames < 5 {
                        fallback.set(Some(frames + 1));
                        return glib::ControlFlow::Continue;
                    }
                    if let Some(bounds) = bounds {
                        center_bounds(view, &bounds);
                    }
                    arrive(view, &track);
                    return glib::ControlFlow::Break;
                }

                let target = match &bounds {
                    Some(bounds) => centered_value(view, &adjustment, bounds),
                    None => estimated_value(view, &adjustment, pos.get()),
                };
                let now = clock.frame_time();
                let Some(mut started) = glide.get() else {
                    // The first step goes none of the way, or all of it
                    // without animations, and the row is only laid out from
                    // the next
                    let mut new = Glide::new(now, view.settings().is_gtk_enable_animations());
                    adjustment.set_value(new.step(adjustment.value(), target, now));
                    glide.set(Some(new));
                    return glib::ControlFlow::Continue;
                };

                adjustment.set_value(started.step(adjustment.value(), target, now));
                glide.set(Some(started));
                if started.progress(now) < 1.0 {
                    return glib::ControlFlow::Continue;
                }

                if bounds.is_none() {
                    // The row wasn't where it should have been; the list
                    // knows where it is
                    view.scroll_to(pos.get(), gtk::ListScrollFlags::NONE, None);
                    fallback.set(Some(0));
                    return glib::ControlFlow::Continue;
                }
                arrive(view, &track);
                glib::ControlFlow::Break
            };
            let flow = step();
            if flow == glib::ControlFlow::Break
                && let Some(done) = done.take()
            {
                done();
            }
            flow
        }
    ))
}

// A glide of a list's scroll to a place that can move as the list lays out
// its rows, as `flash_track()` glides to a track and the works to an open
// one: it lasts `GLIDE_DURATION` from the frame time it starts at, or no
// time when animations are off, as GTK's settings can ask, and it jumps
#[derive(Clone, Copy, Debug)]
pub(crate) struct Glide {
    start: i64,
    duration: i64,
    // The share of the way still to go after the last step
    left: f64,
}

impl Glide {
    pub(crate) fn new(start: i64, animate: bool) -> Self {
        let duration = if animate {
            GLIDE_DURATION.as_micros() as i64
        } else {
            0
        };
        Self {
            start,
            duration,
            left: 1.0,
        }
    }

    // The share of its time that has passed at the frame time `now`, from
    // 0 to 1, which `glide_step()` eases
    pub(crate) fn progress(&self, now: i64) -> f64 {
        if self.duration > 0 {
            ((now - self.start) as f64 / self.duration as f64).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    // Where the list goes from `value` at the frame time `now`, on its way
    // to `target`, see `glide_step()`
    pub(crate) fn step(&mut self, value: f64, target: f64, now: i64) -> f64 {
        let (value, left) = glide_step(value, target, self.left, self.progress(now));
        self.left = left;
        value
    }

    // Whether it went all its way
    pub(crate) fn is_done(&self) -> bool {
        self.left == 0.0
    }
}

// A frame of a glide: where it moves to from `value`, and the share of the
// way still to go, once `progress` of its time has passed. Each frame goes
// its part of what's left to `target`, rather than of the way from the
// start, so that the glide follows the row as the list lays it out, and
// still ends on it
fn glide_step(value: f64, target: f64, left_before: f64, progress: f64) -> (f64, f64) {
    let left = 1.0 - ease(progress);
    let moved = if left_before > 0.0 {
        1.0 - left / left_before
    } else {
        1.0
    };
    (value + (target - value) * moved, left)
}

// CSS's "ease", libadwaita's `Easing::Ease`, whose binding only works once
// libadwaita is set up, unlike in the tests: a cubic Bézier curve with the
// control points (0.25, 0.1) and (0.25, 1). It starts at less than half
// the average speed, rather than all of it, as an ease-out would, which
// looks like a jump, and gets up to speed within a few frames
fn ease(progress: f64) -> f64 {
    if progress <= 0.0 {
        return 0.0;
    }
    if progress >= 1.0 {
        return 1.0;
    }
    let bezier = |t: f64, p1: f64, p2: f64| {
        let u = 1.0 - t;
        3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t
    };
    // The point of the curve at that much of the time, which only grows
    // along it, so halving finds it
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..30 {
        let middle = (low + high) / 2.0;
        if bezier(middle, 0.25, 0.25) < progress {
            low = middle;
        } else {
            high = middle;
        }
    }
    bezier((low + high) / 2.0, 0.1, 1.0)
}

// The end of a glide to a track: its row flashes, and takes the focus,
// as the track was asked for, e.g. with Show the Playing Track, so that
// the keys go on from it
fn arrive(view: &gtk::ListView, track: &Track) {
    flash(track);
    if let Some((item, _)) = track_item(view, track) {
        item.grab_focus();
    }
}

// How long a track or an album gone to flashes: its CSS animation (style.css,
// `track-flash`), twice 0.6 s
pub(crate) const FLASH_DURATION: std::time::Duration = std::time::Duration::from_millis(1200);

// Makes the row of a track flash, like its CSS animation
fn flash(track: &Track) {
    track.set_flashing(true);
    glib::timeout_add_local_once(
        FLASH_DURATION,
        clone!(
            #[weak]
            track,
            move || track.set_flashing(false)
        ),
    );
}

// Shows the playing track in the list, keeping the rows around it in view:
// the list stays where it is if the row is in it, scrolls just enough if
// it's cut off, and centers it if it's further away. Right after the list
// is filled, e.g. switching playlists, its rows aren't laid out, and
// scrolling to one right away would make it the top row, so this waits
// for frames where they are
pub(crate) fn reveal_track(view: &gtk::ListView, track: &Track) -> gtk::TickCallbackId {
    let frames = Cell::new(0);
    let waited = Cell::new(0);
    let centering = Cell::new(false);
    view.add_tick_callback(clone!(
        #[strong]
        track,
        move |view, _| {
            if view.height() == 0 {
                return wait_for_size(&waited);
            }
            let frame = frames.get();
            frames.set(frame + 1);

            let bounds = track_bounds(view, &track);
            if centering.get() {
                // Scrolled to it in a previous frame; where the row is can
                // take a few frames to settle, so it's centered again until
                // the list stops moving
                let moved = bounds.as_ref().is_some_and(|b| center_bounds(view, b));
                if (bounds.is_some() && !moved) || frame >= 10 {
                    return glib::ControlFlow::Break;
                }
                return glib::ControlFlow::Continue;
            }

            let height = view.height() as f32;
            match bounds {
                Some(b) if b.y() >= 0.0 && b.y() + b.height() <= height => {}
                // Scrolling to the row itself could leave the header of
                // its work out
                Some(b) => {
                    if let Some(adjustment) = view.vadjustment() {
                        let offset = if b.y() < 0.0 {
                            b.y()
                        } else {
                            b.y() + b.height() - height
                        };
                        adjustment.set_value(adjustment.value() + f64::from(offset));
                    }
                }
                // The rows can be laid out in the next frame
                None if frame < 1 => return glib::ControlFlow::Continue,
                None => {
                    // A search can hide it
                    if let Some(pos) = track_position(view, &track) {
                        view.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
                        centering.set(true);
                        return glib::ControlFlow::Continue;
                    }
                }
            }
            glib::ControlFlow::Break
        }
    ))
}

// Stops a list going on by itself after a fling of the touchpad or a quick
// spin of the wheel, so that a glide that the user asked for goes where it
// was asked. Its scrolled window decelerates at each frame, setting where
// the list is scrolled, which undid the glide: a list scrolled to its end
// went back there at each frame, and a glide without animations, which
// takes two frames, didn't move it at all. Turning kinetic scrolling off is
// GTK's only way to stop it, and it's turned on again at once.
//
// Turning it off also resets the scrolled window's gestures, which only
// follow touches: a finger dragging the list stopped moving it, and the
// reset swipe started a fling of its own, with the finger's speed. A
// finger on the list has stopped any fling already, as the scrolled window
// stops one when a drag starts, so nothing is turned off while one of its
// gestures is active
pub(crate) fn stop_kinetic_scrolling(widget: &impl IsA<gtk::Widget>) {
    let Some(scrolled) = widget
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
    else {
        return;
    };
    if scrolled.is_kinetic_scrolling() && !is_touched(&scrolled) {
        scrolled.set_kinetic_scrolling(false);
        scrolled.set_kinetic_scrolling(true);
    }
}

// Whether a gesture of a widget is following a touch or a press. The
// scrolled window's gestures are its own, so they're found among its
// controllers. GTK keeps that list up to date only while it's held, which
// slows down adding and removing controllers, and it's dropped at once
fn is_touched(widget: &impl IsA<gtk::Widget>) -> bool {
    let controllers = widget.observe_controllers();
    (0..controllers.n_items()).any(|i| {
        controllers
            .item(i)
            .and_downcast::<gtk::Gesture>()
            .is_some_and(|gesture| gesture.is_active())
    })
}

// Waits one more frame for a list to get its size, unless it was waited for
// long enough
pub(crate) fn wait_for_size(waited: &Cell<u32>) -> glib::ControlFlow {
    waited.set(waited.get() + 1);
    if waited.get() < SIZE_WAIT_FRAMES {
        glib::ControlFlow::Continue
    } else {
        glib::ControlFlow::Break
    }
}

// Where a track is in a list, which can be filtered by a search
pub(crate) fn track_position(view: &gtk::ListView, track: &Track) -> Option<u32> {
    let model = view.model()?;
    (0..model.n_items()).find(|&i| model.item(i).as_ref() == Some(track.upcast_ref()))
}

// Where a track is in a list now: at `hint`, where it was, unless tracks
// went in or out before it since, as only then is the list searched
fn position_now(view: &gtk::ListView, track: &Track, hint: u32) -> u32 {
    let at_hint = view.model().and_then(|model| model.item(hint));
    if at_hint.as_ref() == Some(track.upcast_ref()) {
        return hint;
    }
    track_position(view, track).unwrap_or(hint)
}

// Where the row of a track is in the list, if it has one, with the header
// above it if it's the first movement of a work. Right after the list is
// filled, a row can be there without being laid out, at the top and
// without a height, which isn't where it will be
fn track_bounds(view: &gtk::ListView, track: &Track) -> Option<graphene::Rect> {
    let (item, row) = track_item(view, track)?;
    let bounds = row
        .compute_bounds(view)
        .filter(|bounds| bounds.height() > 0.0)?;
    if !row.starts_work() {
        return Some(bounds);
    }
    if let Some((_, header)) = item
        .compute_bounds(view)
        .and_then(|item| header_above(view, item.y()))
    {
        return Some(header.union(&bounds));
    }
    // The header of the first work can be out of the list's view, without
    // being laid out: everything above the first row goes with it
    if track_position(view, track) == Some(0)
        && let Some(adjustment) = view.vadjustment()
    {
        let top = (adjustment.lower() - adjustment.value()) as f32;
        let height = bounds.y() + bounds.height() - top;
        return Some(graphene::Rect::new(bounds.x(), top, bounds.width(), height));
    }
    Some(bounds)
}

// GTK has no API for the widgets of a list's rows and headers, so
// header_above and track_item go through its children, which are GTK's
// own: a widget per row, holding the row the factory made, and one of this
// type per header. A GTK release could change them; the queue would then
// scroll to its tracks less precisely, without the work over them
const LIST_HEADER_TYPE: &str = "GtkListHeaderWidget";

// The header of a section ending where a row starts, if it's laid out,
// and where it is
pub(crate) fn header_above(
    view: &gtk::ListView,
    top: f32,
) -> Option<(gtk::Widget, graphene::Rect)> {
    let mut child = view.first_child();
    while let Some(widget) = child {
        if widget.type_().name() == LIST_HEADER_TYPE
            && let Some(bounds) = widget.compute_bounds(view)
            && bounds.height() > 0.0
            && (bounds.y() + bounds.height() - top).abs() < 1.0
        {
            return Some((widget, bounds));
        }
        child = widget.next_sibling();
    }
    None
}

// Scrolls the list to put the row in its middle, as far as it can scroll,
// and says whether the list moved
fn center_bounds(view: &gtk::ListView, bounds: &graphene::Rect) -> bool {
    let Some(adjustment) = view.vadjustment() else {
        return false;
    };
    let old = adjustment.value();
    adjustment.set_value(centered_value(view, &adjustment, bounds));
    (adjustment.value() - old).abs() >= 1.0
}

// Where the list scrolls to for a row to be in its middle, as far as it
// can scroll
fn centered_value(
    view: &gtk::ListView,
    adjustment: &gtk::Adjustment,
    bounds: &graphene::Rect,
) -> f64 {
    let middle = f64::from(bounds.y() + bounds.height() / 2.0);
    within_scroll(
        adjustment,
        adjustment.value() + middle - f64::from(view.height()) / 2.0,
    )
}

// The same for a row that isn't laid out: the list reckons the rows it
// hasn't laid out at the height of the others, on average
pub(crate) fn estimated_value(view: &gtk::ListView, adjustment: &gtk::Adjustment, pos: u32) -> f64 {
    let n_items = view.model().map_or(0, |model| model.n_items());
    if n_items == 0 {
        return adjustment.value();
    }
    let height = adjustment.upper() - adjustment.lower();
    let middle = adjustment.lower() + height * (f64::from(pos) + 0.5) / f64::from(n_items);
    within_scroll(adjustment, middle - adjustment.page_size() / 2.0)
}

// A value of a scrolled list's adjustment, as far as the list scrolls
pub(crate) fn within_scroll(adjustment: &gtk::Adjustment, value: f64) -> f64 {
    let last = adjustment.upper() - adjustment.page_size();
    value.min(last).max(adjustment.lower())
}

// The row showing a track in a list, if it has one, and the child of the
// list holding it
fn track_item(view: &gtk::ListView, track: &Track) -> Option<(gtk::Widget, QueueRow)> {
    laid_out_rows(view).find(|(_, row)| row.track().as_ref() == Some(track))
}

// The child of a list holding the top row in its view: the first whose
// middle isn't above it, as a row cut off by more than half is mostly out
// of sight
pub(crate) fn top_item(view: &gtk::ListView) -> Option<gtk::Widget> {
    laid_out_rows(view)
        .filter_map(|(item, _)| {
            let bounds = item
                .compute_bounds(view)
                .filter(|bounds| bounds.height() > 0.0)?;
            let middle = bounds.y() + bounds.height() / 2.0;
            (middle >= 0.0 && middle < view.height() as f32).then(|| (bounds.y(), item))
        })
        .min_by(|(a, _), (b, _)| a.total_cmp(b))
        .map(|(_, item)| item)
}

// The rows a list has laid out, each with the child of the list holding
// it, which leaves out the headers of its sections. The list keeps some
// rows out of its view, e.g. the one of the item it tracks, hidden where
// they last were, which isn't where they are now; those are left out too
fn laid_out_rows(view: &gtk::ListView) -> impl Iterator<Item = (gtk::Widget, QueueRow)> {
    std::iter::successors(view.first_child(), WidgetExt::next_sibling)
        .filter(WidgetExt::is_child_visible)
        .filter_map(|widget| item_row(&widget).map(|row| (widget, row)))
}

// The row in a child of a list; the rows of the queue are in a SlideBin
fn item_row(item: &gtk::Widget) -> Option<QueueRow> {
    match item.first_child()?.downcast::<QueueRow>() {
        Ok(row) => Some(row),
        Err(child) => child.first_child().and_downcast(),
    }
}

#[cfg(test)]
mod tests {
    use super::{GLIDE_DURATION, Glide, glide_step};

    #[test]
    fn a_glide_starts_in_place_or_jumps_without_animations() {
        let duration = GLIDE_DURATION.as_micros() as i64;
        // Its first step goes none of the way, and once its time is up,
        // each step goes all the rest of it, to where the row turned out
        let mut glide = Glide::new(1_000, true);
        assert_eq!(glide.step(0.0, 100.0, 1_000), 0.0);
        assert!(!glide.is_done());
        assert_eq!(glide.progress(1_000 + duration / 2), 0.5);
        assert_eq!(glide.step(0.0, 100.0, 1_000 + duration), 100.0);
        assert!(glide.is_done());
        assert_eq!(glide.step(100.0, 130.0, 1_000 + duration * 2), 130.0);

        // Without animations, the first step goes all the way
        let mut jump = Glide::new(1_000, false);
        assert_eq!(jump.progress(1_000), 1.0);
        assert_eq!(jump.step(0.0, 100.0, 1_000), 100.0);
        assert!(jump.is_done());
        assert_eq!(jump.step(100.0, 130.0, 1_017), 130.0);
    }

    #[test]
    fn a_glide_eases_and_ends_on_its_target() {
        // To a target that stays put, it goes along CSS's "ease"
        let (mut value, mut left) = (0.0, 1.0);
        for (progress, expected) in [(0.25, 40.85), (0.5, 80.24), (0.75, 96.05), (1.0, 100.0)] {
            (value, left) = glide_step(value, 100.0, left, progress);
            assert!((value - expected).abs() < 0.01, "{value} at {progress}");
        }
        assert_eq!(left, 0.0);

        // When the row is laid out halfway and turns out further away, the
        // glide goes on toward it, and still ends on it
        let (value, left) = glide_step(0.0, 100.0, 1.0, 0.5);
        let (value, left) = glide_step(value, 140.0, left, 0.75);
        assert!(value > 96.05 && value < 140.0);
        assert_eq!(glide_step(value, 140.0, left, 1.0).0, 140.0);
    }
}
