// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The buttons that zoom a view, for a menu: the album covers in the main
// menu of the album pages, and the lyrics in the menu of their header, as
// the keys and the mouse wheel aren't for everyone. The buttons go through
// the view's actions, `{prefix}.zoom-out`, `{prefix}.zoom-reset` and
// `{prefix}.zoom-in`; the outer two are off at the ends they go to

use std::cell::{Cell, RefCell};

use gtk::{CompositeTemplate, glib, glib::clone, prelude::*, subclass::prelude::*};

use crate::i18n::i18n_k;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate, glib::Properties)]
    #[template(resource = "/io/github/dachshund7/Dismas/zoom-row.ui")]
    #[properties(wrapper_type = super::ZoomRow)]
    pub(crate) struct ZoomRow {
        #[template_child]
        pub(crate) zoom_out_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(crate) reset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(crate) zoom_in_button: TemplateChild<gtk::Button>,

        // The group of the view's zoom actions, e.g. "library"
        #[property(get, set = Self::set_action_prefix)]
        pub(crate) action_prefix: RefCell<String>,
        // The size the view is zoomed to, in percent of its normal size
        #[property(get, set = Self::set_percent)]
        pub(crate) percent: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ZoomRow {
        const NAME: &'static str = "DismasZoomRow";
        type Type = super::ZoomRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for ZoomRow {
        fn constructed(&self) {
            self.parent_constructed();
            self.set_percent(100);

            // A button pressed with the keys that turns off, at the end it
            // goes to, loses the focus, which goes to the size between
            // them, so that the keys go on from there rather than from
            // nowhere
            let reset = self.reset_button.get();
            for button in [&*self.zoom_out_button, &*self.zoom_in_button] {
                button.connect_sensitive_notify(clone!(
                    #[weak]
                    reset,
                    move |button| {
                        let lost = button.root().is_some_and(|root| root.focus().is_none());
                        if !button.is_sensitive() && button.is_mapped() && lost {
                            reset.grab_focus();
                        }
                    }
                ));
            }
        }
    }

    impl WidgetImpl for ZoomRow {}
    impl BoxImpl for ZoomRow {}

    impl ZoomRow {
        fn set_action_prefix(&self, prefix: String) {
            for (button, action) in [
                (&*self.zoom_out_button, "zoom-out"),
                (&*self.reset_button, "zoom-reset"),
                (&*self.zoom_in_button, "zoom-in"),
            ] {
                button.set_action_name(Some(&format!("{prefix}.{action}")));
            }
            self.action_prefix.replace(prefix);
        }

        fn set_percent(&self, percent: u32) {
            self.percent.set(percent);
            self.reset_button.set_label(&i18n_k(
                // Translators: the size of the album covers or of the
                // lyrics, in percent of their normal size; `{percent}`
                // must be left unmodified
                "{percent}%",
                &[("percent", &percent.to_string())],
            ));
        }
    }
}

glib::wrapper! {
    pub(crate) struct ZoomRow(ObjectSubclass<imp::ZoomRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

// The buttons do nothing until they're given the prefix of their actions
impl Default for ZoomRow {
    fn default() -> Self {
        glib::Object::new()
    }
}
