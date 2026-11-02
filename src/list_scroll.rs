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
// the row from then on. Without animations, as GTK's settings can ask, the
// list jumps to the track. The list was usually just shown, and can only
// scroll once it has its size, so this waits for a frame where it has it
pub(crate) fn flash_track(view: &gtk::ListView, pos: u32, track: &Track) -> gtk::TickCallbackId {
    let waited = Cell::new(0);
    // When the glide started, in the frame clock's microseconds, and the
    // share of the way that was left at the last frame
    let glide = Cell::new(None::<(i64, f64)>);
    // The frames since the list was asked to scroll to the track itself,
    // when the glide ended where its row wasn't
    let fallback = Cell::new(None::<u32>);
    view.add_tick_callback(clone!(
        #[strong]
        track,
        move |view, clock| {
            if view.height() == 0 {
                return wait_for_size(&waited);
            }
            let Some(adjustment) = view.vadjustment() else {
                return glib::ControlFlow::Break;
            };
            // Only reads where the row is, as changing the rows of the
            // list crashes GTK
            let bounds = track_bounds(view, &track);

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
                None => estimated_value(view, &adjustment, pos),
            };
            let duration = if view.settings().is_gtk_enable_animations() {
                GLIDE_DURATION.as_micros() as i64
            } else {
                0
            };
            let now = clock.frame_time();
            let Some((start, left_before)) = glide.get() else {
                glide.set(Some((now, 1.0)));
                if duration == 0 {
                    adjustment.set_value(target);
                }
                return glib::ControlFlow::Continue;
            };

            let progress = if duration > 0 {
                ((now - start) as f64 / duration as f64).min(1.0)
            } else {
                1.0
            };
            let (value, left) = glide_step(adjustment.value(), target, left_before, progress);
            adjustment.set_value(value);
            glide.set(Some((start, left)));
            if progress < 1.0 {
                return glib::ControlFlow::Continue;
            }

            if bounds.is_none() {
                // The row wasn't where it should have been; the list knows
                // where it is
                view.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
                fallback.set(Some(0));
                return glib::ControlFlow::Continue;
            }
            arrive(view, &track);
            glib::ControlFlow::Break
        }
    ))
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

// Waits one more frame for a list to get its size, unless it was waited for
// long enough
fn wait_for_size(waited: &Cell<u32>) -> glib::ControlFlow {
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
fn estimated_value(view: &gtk::ListView, adjustment: &gtk::Adjustment, pos: u32) -> f64 {
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
// list holding it. The list keeps some rows out of its view, e.g. the one
// of the item it tracks, hidden where they last were, which isn't where
// they are now
fn track_item(view: &gtk::ListView, track: &Track) -> Option<(gtk::Widget, QueueRow)> {
    let mut child = view.first_child();
    while let Some(widget) = child {
        if !widget.is_child_visible() {
            child = widget.next_sibling();
            continue;
        }
        // The rows of the queue are in a SlideBin
        let row = widget
            .first_child()
            .and_then(|child| match child.downcast::<QueueRow>() {
                Ok(row) => Some(row),
                Err(child) => child.first_child().and_downcast(),
            });
        if let Some(row) = row
            && row.track().as_ref() == Some(track)
        {
            return Some((widget, row));
        }
        child = widget.next_sibling();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::glide_step;

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
