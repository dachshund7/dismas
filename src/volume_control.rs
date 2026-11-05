// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::Cell;

use adw::subclass::prelude::*;
use glib::clone;
use gtk::{CompositeTemplate, gdk, glib, prelude::*};
use log::debug;

use crate::audio::VOLUME_STEP;

mod imp {
    use glib::subclass::Signal;
    use std::sync::LazyLock;

    use super::*;

    #[derive(Debug, Default, CompositeTemplate, glib::Properties)]
    #[properties(wrapper_type = super::VolumeControl)]
    #[template(resource = "/io/github/dachshund7/Dismas/volume-control.ui")]
    pub(crate) struct VolumeControl {
        // Template widgets
        #[template_child]
        pub(crate) volume_low_button: TemplateChild<gtk::Button>,
        // The adjustment of the scale notifies changes to the volume
        #[template_child]
        #[property(
            name = "volume",
            get = |imp: &Self| imp.volume_scale.value(),
            set = Self::show_volume,
            explicit_notify,
            minimum = 0.0,
            maximum = 1.0,
            default = 1.0,
            type = f64,
        )]
        pub(crate) volume_scale: TemplateChild<gtk::Scale>,
        #[template_child]
        pub(crate) volume_high_image: TemplateChild<gtk::Image>,

        #[property(get, set = Self::set_toggle_mute, explicit_notify)]
        pub(crate) toggle_mute: Cell<bool>,
        pub(crate) prev_volume: Cell<f64>,
        // Whether the slider is following the player, see `show_volume()`
        pub(crate) showing_volume: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VolumeControl {
        const NAME: &'static str = "DismasVolumeControl";
        type Type = super::VolumeControl;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BoxLayout>();
            klass.set_css_name("volume");
            klass.set_accessible_role(gtk::AccessibleRole::Group);

            klass.install_property_action("volume.toggle-mute", "toggle-mute");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for VolumeControl {
        fn constructed(&self) {
            self.parent_constructed();

            self.obj().setup_adjustment();
            self.obj().setup_controller();
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder("volume-changed")
                        .param_types([f64::static_type()])
                        .build(),
                ]
            });

            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for VolumeControl {}

    impl VolumeControl {
        // Shows the volume of the player, which it doesn't need to be told
        // back. GStreamer reports each volume it's given, later: a report
        // sent back would be a new request, so that two reports that are
        // behind, as after quick changes over MPRIS or from the tray, would
        // go round for ever, and the main thread would never get to anything
        // else
        fn show_volume(&self, volume: f64) {
            self.showing_volume.set(true);
            self.volume_scale.set_value(volume);
            self.showing_volume.set(false);
        }

        fn set_toggle_mute(&self, muted: bool) {
            if muted != self.toggle_mute.replace(muted) {
                if muted {
                    self.prev_volume.replace(self.volume_scale.value());
                    self.volume_scale.set_value(0.0);
                } else {
                    self.volume_scale.set_value(self.prev_volume.get());
                }
                self.obj().notify_toggle_mute();
            }
        }
    }
}

glib::wrapper! {
    pub(crate) struct VolumeControl(ObjectSubclass<imp::VolumeControl>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for VolumeControl {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl VolumeControl {
    fn setup_adjustment(&self) {
        let adj = gtk::Adjustment::builder()
            .lower(0.0)
            .upper(1.0)
            .step_increment(VOLUME_STEP)
            .value(1.0)
            .build();
        self.imp().volume_scale.set_adjustment(&adj);
        adj.connect_notify_local(
            Some("value"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |adj, _| {
                    let value = adj.value();
                    if value == adj.lower() {
                        this.imp()
                            .volume_low_button
                            .set_icon_name("audio-volume-muted-symbolic");
                    } else {
                        this.imp()
                            .volume_low_button
                            .set_icon_name("audio-volume-low-symbolic");
                    }
                    this.notify_volume();
                    if !this.imp().showing_volume.get() {
                        this.emit_by_name::<()>("volume-changed", &[&value]);
                    }
                }
            ),
        );
    }

    fn setup_controller(&self) {
        let controller = gtk::EventControllerScroll::builder()
            .name("volume-scroll")
            .flags(gtk::EventControllerScrollFlags::VERTICAL)
            .build();
        controller.connect_scroll(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |controller, _, dy| {
                debug!("Scroll delta: {dy}");
                // A notch of a wheel is a step, which a high-resolution wheel
                // sends in fractions, and a touchpad scrolls by pixels, as
                // for zooming: a step a pixel sent the volume to an end
                let steps = match controller.unit() {
                    gdk::ScrollUnit::Surface => dy / crate::zoom::SCROLL_PIXELS_PER_STEP,
                    _ => dy,
                };
                if steps.is_finite() {
                    this.step(-steps);
                }
                glib::Propagation::Stop
            }
        ));
        self.imp().volume_scale.add_controller(controller);
    }

    /// Changes the volume by a number of steps, negative to lower it.
    pub(crate) fn step(&self, steps: f64) {
        let adj = self.imp().volume_scale.adjustment();
        let value = adj.value() + steps * adj.step_increment();
        adj.set_value(value.clamp(adj.lower(), adj.upper()));
    }

    // The user changed the volume here; the `volume` property also follows
    // the changes that come from elsewhere
    pub(crate) fn connect_volume_changed<F: Fn(&Self, f64) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "volume-changed",
            false,
            glib::closure_local!(move |control: &Self, volume: f64| f(control, volume)),
        )
    }
}
