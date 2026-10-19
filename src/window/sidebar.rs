// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

// The width of the sidebar when the user resizes it
const MIN_SIDEBAR_WIDTH: i32 = 240;

// The width of the sidebar when it's sized automatically
const AUTO_SIDEBAR_WIDTH: f64 = 330.0;
const AUTO_SIDEBAR_FRACTION: f64 = 0.4;

// The smallest size of the window, even when its content fits in less
const MIN_WINDOW_WIDTH: i32 = 360;
const MIN_WINDOW_HEIGHT: i32 = 660;

// What the content of the window needs, in pixels
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizeLimits {
    // The width of the sidebar and the content side by side, under which
    // the split view collapses
    side_by_side: i32,
    // The size of the window, frame included, with the split view
    // collapsed
    min_width: i32,
    min_height: i32,
}

impl Window {
    pub(super) fn setup_sidebar_handle(&self) {
        let imp = self.imp();

        imp.sidebar_width.set(imp.settings.int("sidebar-width"));
        imp.sidebar_handle.set_cursor_from_name(Some("col-resize"));

        // The whole window is a GtkWindowHandle, so the gestures on the
        // handle need to claim their events, or the window would be moved
        // instead of the sidebar being resized
        let drag = gtk::GestureDrag::new();
        let start_width = Rc::new(Cell::new(0));
        let start_x = Rc::new(Cell::new(0.0));
        let last_press = Rc::new(Cell::new(0u32));
        // Whether the current press reset the width, instead of starting
        // a drag
        let was_reset = Rc::new(Cell::new(false));
        let was_dragged = Rc::new(Cell::new(false));
        drag.connect_drag_begin(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            start_width,
            #[strong]
            start_x,
            #[strong]
            last_press,
            #[strong]
            was_reset,
            move |gesture, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);

                // A double click resets the width; we cannot use a separate
                // GtkGestureClick for it, as the drag gesture would reset its
                // count of presses
                let time = gesture.current_event_time();
                let double_click_time = win.settings().gtk_double_click_time() as u32;
                let previous = last_press.replace(time);
                let is_double_click =
                    previous != 0 && time.wrapping_sub(previous) <= double_click_time;
                was_reset.set(is_double_click);
                if is_double_click {
                    last_press.set(0);
                    win.imp().sidebar_width.set(0);
                    win.update_sidebar_width();
                    win.store_sidebar_width();
                    return;
                }

                let imp = win.imp();
                if let Some(sidebar) = imp.split_view.sidebar() {
                    start_width.set(sidebar.width());
                }
                start_x.set(win.handle_x_in_window(x, y));
                imp.sidebar_handle.add_css_class("dragging");
            }
        ));
        drag.connect_drag_update(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            start_width,
            #[strong]
            start_x,
            #[strong]
            was_reset,
            #[strong]
            was_dragged,
            move |gesture, _, _| {
                if was_reset.get() {
                    return;
                }

                // The handle moves while the sidebar is resized, so we
                // track the pointer in window coordinates instead of
                // using the offset relative to the handle; pointer events
                // have no sequence, so we cannot bail out if there isn't one
                let sequence = gesture.current_sequence();
                let Some((x, y)) = gesture.point(sequence.as_ref()) else {
                    return;
                };
                let offset_x = win.handle_x_in_window(x, y) - start_x.get();

                // The sidebar is at the start, so in RTL locales the
                // handle is on its left edge
                let offset_x = if win.direction() == gtk::TextDirection::Rtl {
                    -offset_x
                } else {
                    offset_x
                };

                if offset_x != 0.0 {
                    was_dragged.set(true);
                }

                let width = start_width.get() + offset_x.round() as i32;
                win.imp().sidebar_width.set(width.max(MIN_SIDEBAR_WIDTH));
                win.update_sidebar_width();
            }
        ));
        drag.connect_drag_end(clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            was_dragged,
            move |_, _, _| {
                let imp = win.imp();
                imp.sidebar_handle.remove_css_class("dragging");

                // Nothing to store for a click without a drag, or for the
                // double click that reset the width
                if !was_dragged.replace(false) {
                    return;
                }

                // Store the width the sidebar actually has, in case it
                // was clamped to leave room for the content
                if let Some(sidebar) = imp.split_view.sidebar() {
                    imp.sidebar_width.set(sidebar.width());
                }
                win.store_sidebar_width();
            }
        ));
        imp.sidebar_handle.add_controller(drag);

        self.update_sidebar_width();

        // What the content needs depends on the font, so it's measured
        // once the window has its style, before it's shown
        self.connect_realize(Self::update_size_limits);
    }

    // The sizes come from measuring the content, as they depend on the font
    // resolution, and on the font, rather than being fixed in the template
    fn measure_size_limits(&self) -> SizeLimits {
        let imp = self.imp();
        let split_view = &imp.split_view;
        let min_width = |widget: Option<gtk::Widget>| {
            widget.map_or(0, |w| w.measure(gtk::Orientation::Horizontal, -1).0)
        };

        // AdwOverlaySplitView gives the sidebar at least its minimum width
        let content_width = min_width(split_view.content());
        let sidebar_min = split_view
            .sidebar_width_unit()
            .to_px(split_view.min_sidebar_width(), Some(&self.settings()))
            .ceil() as i32;
        let sidebar_width = min_width(split_view.sidebar()).max(sidebar_min);

        // The size requests of the window include its frame, drawn around
        // its content when there is no compositor
        let frame = |orientation| {
            let window = self.measure(orientation, -1).1;
            let content = imp.drag_overlay.measure(orientation, -1).1;
            (window - content).max(0)
        };

        SizeLimits {
            side_by_side: content_width + sidebar_width,
            min_width: content_width.max(sidebar_width) + frame(gtk::Orientation::Horizontal),
            min_height: imp.drag_overlay.measure(gtk::Orientation::Vertical, -1).0
                + frame(gtk::Orientation::Vertical),
        }
    }

    // Collapses the split view where the sidebar no longer fits next to the
    // content, and keeps the window from getting smaller than the content
    fn update_size_limits(&self) {
        let imp = self.imp();
        let limits = self.measure_size_limits();
        if imp.size_limits.replace(Some(limits)) == Some(limits) {
            return;
        }

        // The breakpoint applies at the widths up to and including its own
        let condition = adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            f64::from(limits.side_by_side - 1),
            adw::LengthUnit::Px,
        );
        imp.narrow_breakpoint.set_condition(Some(&condition));

        self.set_size_request(
            limits.min_width.max(MIN_WINDOW_WIDTH),
            limits.min_height.max(MIN_WINDOW_HEIGHT),
        );
        self.queue_resize();
    }

    // Updates the limits outside of the allocation, as updating them queues
    // a resize; the measures are cached, so checking them is cheap
    pub(super) fn queue_size_limits_update(&self) {
        let imp = self.imp();
        if imp.size_limits_idle.get() || imp.size_limits.get() == Some(self.measure_size_limits()) {
            return;
        }

        imp.size_limits_idle.set(true);
        glib::idle_add_local_once(clone!(
            #[weak(rename_to = win)]
            self,
            move || {
                win.imp().size_limits_idle.set(false);
                win.update_size_limits();
            }
        ));
    }

    // Translates a point in the coordinates of the sidebar handle to the
    // horizontal coordinate in the window
    fn handle_x_in_window(&self, x: f64, y: f64) -> f64 {
        let handle = &self.imp().sidebar_handle;
        handle
            .compute_point(self, &graphene::Point::new(x as f32, y as f32))
            .map_or(x, |p| f64::from(p.x()))
    }

    pub(super) fn update_sidebar_width(&self) {
        let split_view = &self.imp().split_view;
        let wanted = self.imp().sidebar_width.get();

        // The automatic width scales with the text, like the rest of the
        // split view; the width set by the user is in pixels, as it's
        // measured from the pointer position
        let unit = if wanted <= 0 {
            adw::LengthUnit::Sp
        } else {
            adw::LengthUnit::Px
        };
        if split_view.sidebar_width_unit() != unit {
            split_view.set_sidebar_width_unit(unit);
        }

        let (max_width, fraction) = if wanted <= 0 {
            (AUTO_SIDEBAR_WIDTH, AUTO_SIDEBAR_FRACTION)
        } else {
            let total = split_view.width();
            let available = if split_view.is_collapsed() {
                total
            } else {
                let content_min = split_view
                    .content()
                    .map_or(0, |c| c.measure(gtk::Orientation::Horizontal, -1).0);
                total - content_min
            };

            // The window has not been allocated yet
            let width = if total > 0 {
                wanted.min(available).max(MIN_SIDEBAR_WIDTH)
            } else {
                wanted
            };

            (f64::from(width), 1.0)
        };

        // Only update the split view if something changed, as each change
        // queues a new resize
        if split_view.max_sidebar_width() != max_width {
            split_view.set_max_sidebar_width(max_width);
        }
        if split_view.sidebar_width_fraction() != fraction {
            split_view.set_sidebar_width_fraction(fraction);
        }
    }

    fn store_sidebar_width(&self) {
        let width = self.imp().sidebar_width.get();
        if let Err(e) = self.imp().settings.set_int("sidebar-width", width) {
            debug!("Unable to store sidebar-width: {e}");
        }
    }
}
