// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use adw::{prelude::*, subclass::prelude::*};
use gtk::{CompositeTemplate, glib};

use crate::{audio::Track, i18n::i18n};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/bassi/Amberol/track-info-dialog.ui")]
    pub struct TrackInfoDialog {
        #[template_child]
        pub work_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub performance_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub production_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub release_group: TemplateChild<adw::PreferencesGroup>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TrackInfoDialog {
        const NAME: &'static str = "AmberolTrackInfoDialog";
        type Type = super::TrackInfoDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TrackInfoDialog {}
    impl WidgetImpl for TrackInfoDialog {}
    impl AdwDialogImpl for TrackInfoDialog {}
}

glib::wrapper! {
    pub struct TrackInfoDialog(ObjectSubclass<imp::TrackInfoDialog>)
        @extends gtk::Widget, adw::Dialog,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TrackInfoDialog {
    pub fn new(track: &Track) -> Self {
        let dialog: Self = glib::Object::new();
        dialog.set_track(track);
        dialog
    }

    fn set_track(&self, track: &Track) {
        let imp = self.imp();
        let credits = track.credit_details();
        let album = track.album();

        // For movements of a larger work, the title just repeats the work
        // and the movement, so only show it for other tracks
        let movement = track.movement();
        let title =
            Some(track.title()).filter(|t| movement.is_none() && credits.work.as_ref() != Some(t));

        let work = [
            (i18n("Title"), title.into_iter().collect()),
            (i18n("Work"), credits.work.clone().into_iter().collect()),
            (i18n("Movement"), movement.into_iter().collect()),
            (i18n("Composer"), credits.composers.clone()),
            (i18n("Lyricist"), credits.lyricists.clone()),
            (i18n("Arranger"), credits.arrangers.clone()),
        ];
        fill_group(&imp.work_group, &work);

        let mut performance: Vec<(String, Vec<String>)> =
            vec![(i18n("Conductor"), credits.conductors.clone())];
        for performer in &credits.performers {
            let role = match &performer.role {
                Some(role) => capitalize(role),
                None => i18n("Performer"),
            };
            performance.push((role, vec![performer.name.clone()]));
        }
        fill_group(&imp.performance_group, &performance);

        let production = [
            (i18n("Producer"), credits.producers.clone()),
            (i18n("Engineer"), credits.engineers.clone()),
            (i18n("Mixer"), credits.mixers.clone()),
            (i18n("Remixer"), credits.remixers.clone()),
            (i18n("DJ-Mixer"), credits.dj_mixers.clone()),
        ];
        fill_group(&imp.production_group, &production);

        // Only show the album artist if it adds anything
        let album_artist = credits
            .album_artist
            .clone()
            .filter(|a| *a != track.artist());
        let release = [
            (i18n("Album"), vec![album]),
            (i18n("Album Artist"), album_artist.into_iter().collect()),
            (i18n("Date"), credits.date.clone().into_iter().collect()),
            (i18n("Label"), credits.labels.clone()),
            (i18n("Catalog Number"), credits.catalog_numbers.clone()),
            (i18n("Genre"), credits.genres.clone()),
        ];
        fill_group(&imp.release_group, &release);
    }
}

// Adds a row for each field with a value, and hides the group if
// none of them have one
fn fill_group(group: &adw::PreferencesGroup, fields: &[(String, Vec<String>)]) {
    let mut has_rows = false;

    for (title, values) in fields {
        if values.is_empty() {
            continue;
        }

        let row = adw::ActionRow::builder()
            .title(title.as_str())
            .subtitle(glib::markup_escape_text(&values.join(", ")).as_str())
            .subtitle_selectable(true)
            .build();
        row.add_css_class("property");
        group.add(&row);
        has_rows = true;
    }

    group.set_visible(has_rows);
}

// Roles in the tags are lower case, e.g. "solo violin"
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
