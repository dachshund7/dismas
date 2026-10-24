// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::cover_image::CoverImage;

impl Window {
    // Fullscreen, with F11 or from the window manager, shows the player by
    // itself, larger and dark, over a blurred copy of the cover; its
    // controls fade out while the pointer stays still (window/zen.rs).
    // Leaving it goes back to the page and the pane from before
    pub(super) fn setup_fullscreen(&self) {
        self.connect_fullscreened_notify(|win| {
            let imp = win.imp();
            if win.is_fullscreen() {
                imp.player_backdrop.add_css_class("fullscreen");
                win.enter_fullscreen();
            } else {
                imp.player_backdrop.remove_css_class("fullscreen");
                win.leave_fullscreen();
            }
            win.update_backdrop();
        });
    }

    fn enter_fullscreen(&self) {
        let imp = self.imp();
        imp.page_before_fullscreen.replace(Some(self.page()));
        // Whether the pane is beside the player, which the collapsed split
        // view keeps for when it's wide again. The split view can uncollapse
        // before or after the window is fullscreen, and it doesn't show the
        // pane again in fullscreen (window/bindings.rs)
        let pane = if imp.split_view.is_collapsed() {
            imp.sidebar_visible_uncollapsed.get()
        } else {
            self.playlist_visible()
        };
        imp.pane_before_fullscreen.set(pane);

        // With nothing to play, the page stays
        self.set_page("player");
        self.set_playlist_visible(false);
    }

    fn leave_fullscreen(&self) {
        let imp = self.imp();
        if let Some(page) = imp.page_before_fullscreen.take()
            && self.page() == "player"
        {
            self.set_page(page);
        }

        // A window that collapses the split view once it's smaller keeps
        // the pane for when it's wide again, whichever comes first
        let pane = imp.pane_before_fullscreen.get();
        if imp.split_view.is_collapsed() {
            imp.sidebar_visible_uncollapsed.set(pane);
        } else {
            self.set_playlist_visible(pane);
        }
    }

    // The backdrop is blurred from the shared cover of the album, in another
    // thread, once per cover; high contrast has the plain dark background
    pub(super) fn update_backdrop(&self) {
        let imp = self.imp();
        let cover = self
            .player()
            .and_then(|player| player.state().cover())
            .filter(|_| self.is_fullscreen() && !adw::StyleManager::default().is_high_contrast());
        let Some(cover) = cover else {
            imp.backdrop_source.take();
            imp.player_backdrop.set_texture(None);
            return;
        };
        if imp.backdrop_source.borrow().as_ref() == Some(&cover) {
            return;
        }
        imp.backdrop_source.replace(Some(cover.clone()));

        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let source = cover.clone();
                let backdrop = gio::spawn_blocking(move || {
                    CoverImage::from_texture(&source)?
                        .backdrop()
                        .map(CoverImage::into_texture)
                })
                .await
                .ok()
                .flatten();

                // The cover might have changed in the meantime
                let imp = win.imp();
                if imp.backdrop_source.borrow().as_ref() == Some(&cover) {
                    imp.player_backdrop.set_texture(backdrop.as_ref());
                }
            }
        ));
    }
}
