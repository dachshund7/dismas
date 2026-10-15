// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::Cell;

use adw::subclass::prelude::*;
use glib::clone;
use gtk::{CompositeTemplate, glib, prelude::*};
use log::debug;

mod imp {
    use glib::subclass::Signal;
    use std::sync::LazyLock;

    use super::*;

    #[derive(Debug, Default, CompositeTemplate, glib::Properties)]
    #[properties(wrapper_type = super::VolumeControl)]
    #[template(resource = "/io/bassi/Amberol/volume-control.ui")]
    pub struct VolumeControl {
        // Template widgets
        #[template_child]
        pub volume_low_button: TemplateChild<gtk::Button>,
        // The adjustment of the scale notifies changes to the volume
        #[template_child]
        #[property(
            name = "volume",
            get = |imp: &Self| imp.volume_scale.value(),
            set = |imp: &Self, volume: f64| imp.volume_scale.set_value(volume),
            explicit_notify,
            minimum = 0.0,
            maximum = 1.0,
            default = 1.0,
            type = f64,
        )]
        pub volume_scale: TemplateChild<gtk::Scale>,
        #[template_child]
        pub volume_high_image: TemplateChild<gtk::Image>,

        #[property(get, set = Self::set_toggle_mute, explicit_notify)]
        pub toggle_mute: Cell<bool>,
        pub prev_volume: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VolumeControl {
        const NAME: &'static str = "AmberolVolumeControl";
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
    pub struct VolumeControl(ObjectSubclass<imp::VolumeControl>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for VolumeControl {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl VolumeControl {
    pub fn new() -> Self {
        Self::default()
    }

    fn setup_adjustment(&self) {
        let adj = gtk::Adjustment::builder()
            .lower(0.0)
            .upper(1.0)
            .step_increment(0.05)
            .value(1.0)
            .build();
        self.imp().volume_scale.set_adjustment(&adj);
        adj.connect_notify_local(
            Some("value"),
            clone!(
                #[strong(rename_to = this)]
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
                    this.emit_by_name::<()>("volume-changed", &[&value]);
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
            #[strong(rename_to = this)]
            self,
            move |_, _, dy| {
                debug!("Scroll delta: {}", dy);
                let adj = this.imp().volume_scale.adjustment();
                let delta = dy * adj.step_increment();
                let d = (adj.value() - delta).clamp(adj.lower(), adj.upper());
                adj.set_value(d);
                glib::Propagation::Stop
            }
        ));
        self.imp().volume_scale.add_controller(controller);
    }

    pub fn connect_volume_changed<F: Fn(&Self, f64) + 'static>(
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
