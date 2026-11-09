// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// Opening the booklets of albums, from the menus of the albums, of the
// tracks and of the playing track (see `booklets.rs`)

use std::path::PathBuf;

use gtk::{gio, glib, glib::clone, prelude::*};
use log::debug;

use super::{Window, albums::MenuLookups};
use crate::{booklets, i18n::i18n, utils};

impl Window {
    // The booklet opens in the app for PDF files that the desktop's
    // settings give (mimeapps.list). GTK's launcher would go through the
    // desktop portal wherever one runs, which asks which app to use while
    // several can open PDF files, until it remembers the choice for the
    // app's ID; outside a sandbox, on a session without systemd, it has no
    // ID for Dismas, so it asked every time. In the Flatpak, the portal is
    // the only way out, and knows the app; like Open Folder, it takes
    // the file's own path, which the launcher hands to the portal
    pub(super) async fn open_booklet(&self, file: gio::File) {
        let opened = if utils::in_flatpak() {
            gtk::FileLauncher::new(Some(&file))
                .launch_future(Some(self.upcast_ref::<gtk::Window>()))
                .await
        } else {
            let context = WidgetExt::display(self).app_launch_context();
            gio::AppInfo::launch_default_for_uri_future(&file.uri(), Some(&context)).await
        };
        if let Err(err) = opened {
            debug!("Unable to open the booklet {}: {err}", file.uri());
            self.add_toast(i18n("Unable to open booklet"));
        }
    }

    // Adds to the end of a section of a menu the item that opens the
    // booklets of the tracks `paths` gives, once they're found in a
    // separate thread before the menu opens (see `MenuLookups`); without
    // one, nothing is added
    pub(super) fn add_booklet_item(
        &self,
        section: &gio::Menu,
        lookups: &mut MenuLookups,
        paths: impl FnOnce() -> Vec<PathBuf> + Send + 'static,
    ) {
        let position = section.n_items();
        let adder = lookups.adder();
        lookups.spawn(clone!(
            #[weak]
            section,
            async move {
                let found = gio::spawn_blocking(move || booklets::find(&paths()))
                    .await
                    .unwrap_or_default();
                if let Some(item) = booklets::menu_item(&found) {
                    adder.insert_item(&section, position, &item);
                }
            }
        ));
    }
}
