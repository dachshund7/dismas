// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The buttons that switch between the player, the album grid and the
// playlists, in the header of each; the one of the visible page is
// active, through the `win.page` action. The playlist pane's toggle
// is on their left, apart from them

use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib, prelude::*};

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/bassi/Amberol/page-switcher.ui")]
    pub struct PageSwitcher {
        #[template_child]
        pub player_button: TemplateChild<gtk::ToggleButton>,
        // The bindings to the window, while the switcher is in it
        pub bindings: RefCell<Vec<glib::Binding>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PageSwitcher {
        const NAME: &'static str = "AmberolPageSwitcher";
        type Type = super::PageSwitcher;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("pageswitcher");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PageSwitcher {
        fn constructed(&self) {
            self.parent_constructed();

            // Clicking toggles the button, which the page puts back
            self.player_button.connect_clicked(|button| {
                if let Err(e) = button.activate_action("win.page", Some(&"player".to_variant())) {
                    log::warn!("Unable to show the player: {e}");
                }
            });
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for PageSwitcher {
        // The button of the player follows the window, rather than the
        // `win.page` action like the others: the action would make it
        // sensitive whenever it's enabled, even with nothing to play
        fn root(&self) {
            self.parent_root();

            if let Some(win) = self.obj().root().and_downcast::<crate::window::Window>() {
                let bindings = vec![
                    win.bind_property("player-available", &*self.player_button, "sensitive")
                        .sync_create()
                        .build(),
                    win.bind_property("page", &*self.player_button, "active")
                        .transform_to(|_, page: String| Some(page == "player"))
                        .sync_create()
                        .build(),
                ];
                self.bindings.replace(bindings);
            }
        }

        fn unroot(&self) {
            for binding in self.bindings.take() {
                binding.unbind();
            }

            self.parent_unroot();
        }
    }
}

glib::wrapper! {
    pub struct PageSwitcher(ObjectSubclass<imp::PageSwitcher>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PageSwitcher {
    fn default() -> Self {
        glib::Object::new()
    }
}
