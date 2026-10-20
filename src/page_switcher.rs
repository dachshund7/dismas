// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The buttons that switch between the player, the album grid and the
// playlists, in the header of each; the one of the visible page is
// active, through the `win.page` action. The playlist pane's toggle
// is on their left, apart from them.
//
// Where the header bar is too narrow for the buttons of the pages, they
// fold into a menu button showing the visible page. The switcher asks for
// the width of the menu button only, so that the window can get as narrow
// as with the menu button, whichever is shown

use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib, prelude::*};

// Between the toggle of the playlist pane and the pages
const SPACING: i32 = 6;

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/dachshund7/Dismas/page-switcher.ui")]
    pub struct PageSwitcher {
        #[template_child]
        pub queue_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub pages_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub player_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub pages_menu_button: TemplateChild<gtk::MenuButton>,
        // The bindings to the window, while the switcher is in it
        pub bindings: RefCell<Vec<glib::Binding>>,
        pub player_handler: RefCell<Option<(glib::WeakRef<gtk::Window>, glib::SignalHandlerId)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PageSwitcher {
        const NAME: &'static str = "DismasPageSwitcher";
        type Type = super::PageSwitcher;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_css_name("pageswitcher");

            // The items of the menu of the pages
            for page in ["player", "library", "playlists"] {
                klass.install_action(&format!("switcher.{page}"), None, move |obj, _, _| {
                    obj.show_page(page);
                });
            }
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PageSwitcher {
        fn constructed(&self) {
            self.parent_constructed();

            // Clicking toggles the button, which the page puts back
            self.player_button.connect_clicked(glib::clone!(
                #[weak(rename_to = obj)]
                self.obj(),
                move |_| obj.show_page("player")
            ));
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for PageSwitcher {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let queue = self.queue_button.measure(orientation, -1);
            let pages = self.pages_box.measure(orientation, -1);
            let menu = self.pages_menu_button.measure(orientation, -1);
            if orientation == gtk::Orientation::Horizontal {
                (
                    queue.0 + SPACING + menu.0,
                    queue.1 + SPACING + pages.1,
                    -1,
                    -1,
                )
            } else {
                (
                    queue.0.max(pages.0).max(menu.0),
                    queue.1.max(pages.1).max(menu.1),
                    -1,
                    -1,
                )
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let horizontal = gtk::Orientation::Horizontal;
            let queue_width = self.queue_button.measure(horizontal, -1).1;
            let pages_width = self.pages_box.measure(horizontal, -1).1;
            let menu_width = self.pages_menu_button.measure(horizontal, -1).1;

            // Hidden children aren't drawn, and don't need an allocation
            let unfolded = width >= queue_width + SPACING + pages_width;
            self.pages_box.set_child_visible(unfolded);
            self.pages_menu_button.set_child_visible(!unfolded);
            let (pages, pages_width): (&gtk::Widget, i32) = if unfolded {
                (self.pages_box.upcast_ref(), pages_width)
            } else {
                (self.pages_menu_button.upcast_ref(), menu_width)
            };

            let rtl = self.obj().direction() == gtk::TextDirection::Rtl;
            let x = |start: i32, child_width: i32| {
                if rtl {
                    width - start - child_width
                } else {
                    start
                }
            };
            self.queue_button.size_allocate(
                &gtk::Allocation::new(x(0, queue_width), 0, queue_width, height),
                baseline,
            );
            pages.size_allocate(
                &gtk::Allocation::new(
                    x(queue_width + SPACING, pages_width),
                    0,
                    pages_width,
                    height,
                ),
                baseline,
            );
        }

        // The button of the player follows the window, rather than the
        // `win.page` action like the others: the action would make it
        // sensitive whenever it's enabled, even with nothing to play
        fn root(&self) {
            self.parent_root();

            let obj = self.obj();
            let Some(win) = obj.root().and_downcast::<crate::window::Window>() else {
                return;
            };
            let bindings = vec![
                win.bind_property("player-available", &*self.player_button, "sensitive")
                    .sync_create()
                    .build(),
                win.bind_property("page", &*self.player_button, "active")
                    .transform_to(|_, page: String| Some(page == "player"))
                    .sync_create()
                    .build(),
                win.bind_property("page", &*self.pages_menu_button, "icon-name")
                    .transform_to(|_, page: String| {
                        Some(match page.as_str() {
                            "library" => "view-grid-symbolic",
                            "playlists" => "view-list-bullet-symbolic",
                            _ => "audio-x-generic-symbolic",
                        })
                    })
                    .sync_create()
                    .build(),
            ];
            self.bindings.replace(bindings);

            let update_player = glib::clone!(
                #[weak]
                obj,
                move |win: &crate::window::Window| {
                    obj.action_set_enabled("switcher.player", win.property("player-available"));
                }
            );
            update_player(&win);
            let handler = win.connect_notify_local(Some("player-available"), move |win, _| {
                update_player(win);
            });
            self.player_handler
                .replace(Some((win.upcast_ref::<gtk::Window>().downgrade(), handler)));
        }

        fn unroot(&self) {
            for binding in self.bindings.take() {
                binding.unbind();
            }
            if let Some((win, handler)) = self.player_handler.take()
                && let Some(win) = win.upgrade()
            {
                win.disconnect(handler);
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

impl PageSwitcher {
    fn show_page(&self, page: &str) {
        if let Err(e) = self.activate_action("win.page", Some(&page.to_variant())) {
            log::warn!("Unable to show the page {page}: {e}");
        }
    }
}
