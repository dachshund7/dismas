// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{collections::HashMap, path::PathBuf, sync::OnceLock};

use adw::{prelude::*, subclass::prelude::*};
use gtk::{CompositeTemplate, gio, glib};
use log::warn;

use crate::{
    audio::{FileDetails, MusicBrainzIds, Track, TrackCredits},
    i18n::{i18n, i18n_f, i18n_k, ni18n_f},
};

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
        #[template_child]
        pub musicbrainz_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub file_group: TemplateChild<adw::PreferencesGroup>,
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

        let mut production = vec![
            (i18n("Producer"), credits.producers.clone()),
            (i18n("Engineer"), credits.engineers.clone()),
            (i18n("Mixer"), credits.mixers.clone()),
            (i18n("Remixer"), credits.remixers.clone()),
            (i18n("DJ-Mixer"), credits.dj_mixers.clone()),
        ];
        // The roles without a field of their own, e.g. "mastering engineer"
        for credit in &credits.other_production {
            if let Some(role) = &credit.role {
                production.push((capitalize(role), vec![credit.name.clone()]));
            }
        }
        fill_group(&imp.production_group, &production);

        fill_group(&imp.release_group, &release_fields(track, &credits));
        fill_musicbrainz_group(&imp.musicbrainz_group, &credits.musicbrainz);
        fill_group(&imp.file_group, &file_fields(&track.file_details()));
    }
}

fn release_fields(track: &Track, credits: &TrackCredits) -> Vec<(String, Vec<String>)> {
    // Only show the album artist if it adds anything
    let album_artist = credits
        .album_artist
        .clone()
        .filter(|a| *a != track.artist());

    // Only show the disc for albums with more than one
    let disc = position(credits.disc_number, credits.disc_total)
        .filter(|_| credits.disc_number > Some(1) || credits.disc_total > Some(1));

    vec![
        (i18n("Album"), vec![track.album()]),
        (i18n("Album Artist"), album_artist.into_iter().collect()),
        (i18n("Disc"), disc.into_iter().collect()),
        (
            i18n("Disc Subtitle"),
            credits.disc_subtitle.clone().into_iter().collect(),
        ),
        (
            i18n("Track"),
            position(credits.track_number, credits.track_total)
                .into_iter()
                .collect(),
        ),
        (i18n("Date"), credits.date.clone().into_iter().collect()),
        (
            i18n("Original Date"),
            credits
                .distinct_original_date()
                .map(str::to_string)
                .into_iter()
                .collect(),
        ),
        (
            i18n("Release Type"),
            credits
                .release_types
                .iter()
                .map(|t| release_type_name(t))
                .collect(),
        ),
        (i18n("Medium"), credits.media.clone()),
        (
            i18n("Country"),
            credits
                .release_country
                .as_deref()
                .and_then(country_name)
                .into_iter()
                .collect(),
        ),
        (i18n("Label"), credits.labels.clone()),
        (i18n("Catalog Number"), credits.catalog_numbers.clone()),
        (i18n("Barcode"), credits.barcodes.clone()),
        (i18n("ISRC"), credits.isrcs.clone()),
        (i18n("Genre"), credits.genres.clone()),
        (i18n("Copyright"), credits.copyright.clone()),
    ]
}

fn file_fields(details: &FileDetails) -> Vec<(String, Vec<String>)> {
    let channels = details.channels.map(|channels| match channels {
        1 => i18n("Mono"),
        2 => i18n("Stereo"),
        n => ni18n_f("{} channel", "{} channels", n.into(), &[&n.to_string()]),
    });

    // e.g. "44.1 kHz"
    let sample_rate = details
        .sample_rate
        .map(|rate| i18n_f("{} kHz", &[&(f64::from(rate) / 1000.0).to_string()]));

    let gain = |gain: &Option<String>| gain.clone().into_iter().collect();

    vec![
        (
            i18n("Location"),
            details
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .into_iter()
                .collect(),
        ),
        (
            i18n("Format"),
            details.format.map(str::to_string).into_iter().collect(),
        ),
        (
            i18n("Bitrate"),
            details
                .bitrate
                .map(|b| i18n_f("{} kbps", &[&b.to_string()]))
                .into_iter()
                .collect(),
        ),
        (i18n("Sample Rate"), sample_rate.into_iter().collect()),
        (
            i18n("Bit Depth"),
            details
                .bit_depth
                .map(|d| i18n_f("{}-bit", &[&d.to_string()]))
                .into_iter()
                .collect(),
        ),
        (i18n("Channels"), channels.into_iter().collect()),
        (
            i18n("Size"),
            details
                .size
                .map(|s| glib::format_size(s).to_string())
                .into_iter()
                .collect(),
        ),
        (i18n("Track Gain"), gain(&details.replay_gain.track)),
        (i18n("Album Gain"), gain(&details.replay_gain.album)),
    ]
}

// A row for each page of the track on MusicBrainz, which opens it in the
// browser
fn fill_musicbrainz_group(group: &adw::PreferencesGroup, ids: &MusicBrainzIds) {
    let pages = [
        (i18n("Recording"), "recording", &ids.recording),
        (i18n("Release"), "release", &ids.release),
        (i18n("Work"), "work", &ids.work),
    ];

    let mut has_rows = false;
    for (title, kind, id) in pages {
        let Some(id) = id else {
            continue;
        };

        let uri = format!("https://musicbrainz.org/{kind}/{id}");
        let row = adw::ActionRow::builder()
            .title(title.as_str())
            .tooltip_text(uri.as_str())
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
        row.connect_activated(move |row| {
            let window = row.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&uri).launch(window.as_ref(), gio::Cancellable::NONE, |res| {
                if let Err(e) = res {
                    warn!("Unable to open the MusicBrainz page: {e}");
                }
            });
        });
        group.add(&row);
        has_rows = true;
    }

    group.set_visible(has_rows);
}

// e.g. "6 of 13", or just "6" without a total
fn position(number: Option<u32>, total: Option<u32>) -> Option<String> {
    let number = number?;
    match total {
        Some(total) if total >= number => Some(i18n_k(
            // Translators: the position of a track or a disc; the
            // `{number}` and `{total}` must be left unmodified
            "{number} of {total}",
            &[
                ("number", &number.to_string()),
                ("total", &total.to_string()),
            ],
        )),
        _ => Some(number.to_string()),
    }
}

// The release types of MusicBrainz, as Picard stores them
fn release_type_name(release_type: &str) -> String {
    match release_type.to_lowercase().as_str() {
        "album" => i18n("Album"),
        "single" => i18n("Single"),
        "ep" => i18n("EP"),
        "broadcast" => i18n("Broadcast"),
        "other" => i18n("Other"),
        "compilation" => i18n("Compilation"),
        "soundtrack" => i18n("Soundtrack"),
        "spokenword" => i18n("Spoken Word"),
        "interview" => i18n("Interview"),
        "audiobook" => i18n("Audiobook"),
        "audio drama" => i18n("Audio Drama"),
        "live" => i18n("Live"),
        "remix" => i18n("Remix"),
        "dj-mix" => i18n("DJ-Mix"),
        "mixtape/street" => i18n("Mixtape/Street"),
        "demo" => i18n("Demo"),
        "field recording" => i18n("Field Recording"),
        _ => capitalize(release_type),
    }
}

// The name of a country in the language of the user, from the iso-codes
// package, or the code if it's not installed; MusicBrainz has some codes
// of its own for regions
fn country_name(code: &str) -> Option<String> {
    match code {
        "XW" => return Some(i18n("Worldwide")),
        "XE" => return Some(i18n("Europe")),
        "XU" | "" => return None,
        _ => (),
    }

    static NAMES: OnceLock<HashMap<String, String>> = OnceLock::new();
    let name = NAMES.get_or_init(load_country_names).get(code);
    Some(match name {
        Some(name) => gettextrs::dgettext(ISO_3166_DOMAIN, name),
        None => code.to_string(),
    })
}

const ISO_3166_DOMAIN: &str = "iso_3166-1";

// The English names of the countries by code
fn load_country_names() -> HashMap<String, String> {
    let mut names = HashMap::new();

    let Some((data_dir, json)) = glib::system_data_dirs().into_iter().find_map(|dir| {
        let path: PathBuf = dir.join("iso-codes/json/iso_3166-1.json");
        std::fs::read_to_string(path).ok().map(|json| (dir, json))
    }) else {
        return names;
    };

    // The translations are installed with the names
    let _ = gettextrs::bindtextdomain(ISO_3166_DOMAIN, data_dir.join("locale"));
    let _ = gettextrs::bind_textdomain_codeset(ISO_3166_DOMAIN, "UTF-8");

    let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) else {
        return names;
    };
    for country in value["3166-1"].as_array().into_iter().flatten() {
        let code = country["alpha_2"].as_str();
        // Some countries have a shorter, common name, e.g. "Taiwan"
        let name = country["common_name"]
            .as_str()
            .or_else(|| country["name"].as_str());
        if let (Some(code), Some(name)) = (code, name) {
            names.insert(code.to_string(), name.to_string());
        }
    }

    names
}

// Adds a row for each field with a value, and hides the group if
// none of them have one
fn fill_group(group: &adw::PreferencesGroup, fields: &[(String, Vec<String>)]) {
    let mut has_rows = false;

    for (title, values) in fields {
        if values.is_empty() {
            continue;
        }

        // The titles of the performers are their roles, from the tags, so
        // neither line is markup
        // The text is set after turning off the markup, as the builder can
        // set it first, and warn about "&" in it
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .subtitle_selectable(true)
            .build();
        row.set_title(title);
        row.set_subtitle(&values.join(", "));
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

#[cfg(test)]
mod tests {
    use super::{country_name, position};

    #[test]
    fn positions_have_a_total_if_known() {
        assert_eq!(position(Some(6), Some(13)).as_deref(), Some("6 of 13"));
        assert_eq!(position(Some(6), None).as_deref(), Some("6"));
        // A total that's too small is wrong
        assert_eq!(position(Some(6), Some(3)).as_deref(), Some("6"));
        assert_eq!(position(None, Some(13)), None);
    }

    #[test]
    fn musicbrainz_regions_have_names() {
        assert_eq!(country_name("XW").as_deref(), Some("Worldwide"));
        assert_eq!(country_name("XE").as_deref(), Some("Europe"));
        assert_eq!(country_name("XU"), None);
    }
}
