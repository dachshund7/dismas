// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The background of the player in fullscreen: a small blurred copy of the
// cover, scaled up to fill it, which fades into the next cover. It's blurred
// once per cover (`CoverImage::backdrop`) rather than drawn with a blur
// node, which the renderer could blur again over the whole screen each time
// the waveform or the time labels change

use std::cell::{Cell, RefCell};

use adw::{prelude::*, subclass::prelude::*};
use gtk::{gdk, glib, graphene, gsk};

// As long as the transition of the colors of the window
const FADE_MS: u32 = 250;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct CoverBackdrop {
        pub texture: RefCell<Option<gdk::Texture>>,
        // The cover from before, which the new one fades in over
        pub previous: RefCell<Option<gdk::Texture>>,
        // How far the new cover has faded in, from 0 to 1
        pub fade: Cell<f64>,
        pub animation: RefCell<Option<adw::TimedAnimation>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CoverBackdrop {
        const NAME: &'static str = "DismasCoverBackdrop";
        type Type = super::CoverBackdrop;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("coverbackdrop");
        }
    }

    impl ObjectImpl for CoverBackdrop {
        fn constructed(&self) {
            self.parent_constructed();
            self.fade.set(1.0);
        }
    }

    // Over the CSS background, which shows without a cover, and under the
    // child
    impl WidgetImpl for CoverBackdrop {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (width, height) = (obj.width() as f32, obj.height() as f32);
            let fade = self.fade.get();

            if fade < 1.0
                && let Some(previous) = self.previous.borrow().as_ref()
            {
                draw_filling(snapshot, previous, width, height);
            }
            if let Some(texture) = self.texture.borrow().as_ref() {
                if fade < 1.0 {
                    snapshot.push_opacity(fade);
                }
                draw_filling(snapshot, texture, width, height);
                if fade < 1.0 {
                    snapshot.pop();
                }
            }

            self.parent_snapshot(snapshot);
        }
    }

    impl BinImpl for CoverBackdrop {}
}

glib::wrapper! {
    pub struct CoverBackdrop(ObjectSubclass<imp::CoverBackdrop>)
        @extends gtk::Widget, adw::Bin,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CoverBackdrop {
    // Fades the new cover in over the one before; without a cover, the one
    // before goes at once, as the player is no longer in fullscreen, or
    // the track has none
    pub fn set_texture(&self, texture: Option<&gdk::Texture>) {
        let imp = self.imp();
        if imp.texture.borrow().as_ref() == texture {
            return;
        }

        // A playing animation keeps itself alive, so it's paused rather
        // than only dropped
        if let Some(animation) = imp.animation.take() {
            animation.pause();
        }
        let previous = imp.texture.replace(texture.cloned());
        if texture.is_none() {
            imp.previous.take();
            imp.fade.set(1.0);
            self.queue_draw();
            return;
        }
        imp.previous.replace(previous);
        imp.fade.set(0.0);

        let target = adw::CallbackAnimationTarget::new(glib::clone!(
            #[weak(rename_to = backdrop)]
            self,
            move |value| {
                let imp = backdrop.imp();
                imp.fade.set(value);
                if value >= 1.0 {
                    imp.previous.take();
                }
                backdrop.queue_draw();
            }
        ));
        let animation = adw::TimedAnimation::builder()
            .widget(self)
            .value_from(0.0)
            .value_to(1.0)
            .duration(FADE_MS)
            .easing(adw::Easing::Ease)
            .target(&target)
            .build();
        imp.animation.replace(Some(animation.clone()));
        animation.play();
    }
}

// Draws the texture over the whole area, at its own aspect ratio, cropping
// what goes past the edges
fn draw_filling(snapshot: &gtk::Snapshot, texture: &gdk::Texture, width: f32, height: f32) {
    let (texture_width, texture_height) = (texture.width() as f32, texture.height() as f32);
    if texture_width <= 0.0 || texture_height <= 0.0 {
        return;
    }
    let scale = (width / texture_width).max(height / texture_height);
    let (w, h) = (texture_width * scale, texture_height * scale);

    snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, width, height));
    snapshot.append_scaled_texture(
        texture,
        gsk::ScalingFilter::Linear,
        &graphene::Rect::new((width - w) / 2.0, (height - h) / 2.0, w, h),
    );
    snapshot.pop();
}
