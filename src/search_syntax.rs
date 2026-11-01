// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The help on the search syntax, from the main menu and from the line
// under a search that has a problem. It's made from the fields and flags of
// `search_query`, so that it lists all of them, and none that isn't there.
// The syntax itself, as in "composer:", stays in English, as it's typed

use adw::prelude::*;
use gtk::glib;

use crate::{
    i18n::i18n,
    search_query::{Field, IsFlag},
};

pub fn show(parent: &impl IsA<gtk::Widget>) {
    let page = adw::PreferencesPage::new();
    page.add(&group(
        &i18n("Words"),
        Some(&i18n(
            "A search finds the tracks with every word in their tags, ignoring case and accents",
        )),
        &[
            ("bach cantata", i18n("Both words, anywhere in the tags")),
            ("\"ach gott\"", i18n("A phrase, as it's written")),
            ("bach OR handel", i18n("Either word")),
            ("-live", i18n("Leaves out the tracks with a word; NOT live does the same")),
            (
                "(bach OR handel) cantata",
                i18n("Parentheses group words; without them, words next to each other go together before OR"),
            ),
        ],
    ));
    page.add(&group(
        &i18n("Fields"),
        Some(&i18n(
            "A field looks into one tag, e.g. composer:bach. composer:=… is the whole name; year:<1970, year:>=1960 and year:1960..1969 compare; composer:(bach OR handel) looks for either; -composer:bach or composer:!=bach leaves out",
        )),
        &Field::ALL
            .iter()
            .map(|field| (format!("{}:", field.name()), field.description()))
            .collect::<Vec<_>>(),
    ));
    page.add(&group(
        &i18n("Dates"),
        None,
        &[
            (
                "released:2026",
                i18n("A year, a month (2026-10) or a day (2026-10-31)"),
            ),
            (
                "released:-30d",
                i18n("In the last 30 days; -2w is two weeks, -6m six months, -1y a year"),
            ),
            ("recorded:<1960", i18n("Before a date; > is after it")),
            ("released:2020..2025", i18n("From one date to another")),
        ],
    ));
    page.add(&group(
        &i18n("Flags"),
        None,
        &IsFlag::ALL
            .iter()
            .map(|flag| (format!("is:{}", flag.name()), flag.description()))
            .chain([(
                "has:composer".to_string(),
                i18n("Tracks with a value in a field; -has:composer, without"),
            )])
            .collect::<Vec<_>>(),
    ));

    let header = adw::HeaderBar::new();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));

    let dialog = adw::Dialog::builder()
        .title(i18n("Search Syntax"))
        .content_width(560)
        .content_height(640)
        .child(&toolbar)
        .build();
    dialog.present(Some(parent));
}

// A group of rows, each the syntax as its title and what it does as its
// subtitle, which screen readers say in that order
fn group(
    title: &str,
    description: Option<&str>,
    rows: &[(impl AsRef<str>, String)],
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(title).build();
    if let Some(description) = description {
        group.set_description(Some(description));
    }
    for (syntax, what) in rows {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(syntax.as_ref()))
            .subtitle(glib::markup_escape_text(what))
            .build();
        group.add(&row);
    }
    group
}
