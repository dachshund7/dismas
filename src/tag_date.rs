// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! A date as the tags of a file give it, which is often only a year, or a
//! year and a month, and is written in many ways: "1965", "1965-03-12",
//! "1965-3" (by hand), "2005-00-00" (rippers that know only the year),
//! "1965-03-12T20:00:00" (with a time), or "1937-11-08/1938-03-08" (an
//! interval, for a recording made over several sessions).
//!
//! The library sorts the albums by these dates, and the search finds them,
//! so both read them here: a part that's missing or makes no sense is left
//! out, and the year alone is kept rather than the date dropped.

use std::fmt;

// No `Ord`: the order of the album sorts puts a missing month or day last
// in both directions, which no single order of the dates does
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TagDate {
    pub(crate) year: u16,
    // From 1 to 12; never set without the year
    pub(crate) month: Option<u8>,
    // A day of that month; never set without the month
    pub(crate) day: Option<u8>,
}

impl TagDate {
    // The start of the date that `text` begins with, or None when it doesn't
    // begin with a year of four digits
    pub(crate) fn parse(text: &str) -> Option<Self> {
        // The start of an interval
        let text = text.trim().split('/').next().unwrap_or_default();
        let year = text.get(..4)?;
        if !year.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut date = TagDate {
            year: year.parse().ok()?,
            month: None,
            day: None,
        };
        // A month and a day only after dashes, of one or two digits, which
        // a time ("12T20:00") or anything else can follow
        let Some(rest) = text[4..].strip_prefix('-') else {
            return Some(date);
        };
        let mut parts = rest.split('-');
        let mut number = || {
            let part = parts.next()?;
            let end = part
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(part.len());
            (1..=2)
                .contains(&end)
                .then(|| part[..end].parse::<u8>().ok())
                .flatten()
        };
        let Some(month) = number().filter(|m| (1..=12).contains(m)) else {
            return Some(date);
        };
        date.month = Some(month);
        date.day = number().filter(|d| (1..=days_in(date.year, month)).contains(d));
        Some(date)
    }

    // The text the library keeps the date as, at a fixed width with "~~"
    // for a missing month or day: "1965-03-12", "1965-03-~~", "1965-~~-~~".
    // Its order is the order of the album sorts from the earliest, which the
    // library takes the earliest of an album's tracks in with MIN(): "~"
    // comes after the digits, so a date with only a year goes after the
    // others of that year. It isn't a digit, so it can't be read as a month
    pub(crate) fn sort_text(self) -> String {
        let part = |n: Option<u8>| n.map_or_else(|| "~~".to_string(), |n| format!("{n:02}"));
        format!("{:04}-{}-{}", self.year, part(self.month), part(self.day))
    }

    // The date that `sort_text()` wrote
    pub(crate) fn from_sort_text(text: &str) -> Option<Self> {
        let mut parts = text.split('-');
        let year = parts.next()?.parse().ok()?;
        let mut part = || parts.next().and_then(|part| part.parse().ok());
        let month = part();
        let day = month.and_then(|_| part());
        Some(TagDate { year, month, day })
    }
}

// Written as the parts it has, "1965", "1965-03" or "1965-03-12", as a
// search writes a date: the search's `period()` takes only a date that
// reads back as it's written. The library keeps the dates as `sort_text()`
impl fmt::Display for TagDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}", self.year)?;
        if let Some(month) = self.month {
            write!(f, "-{month:02}")?;
            if let Some(day) = self.day {
                write!(f, "-{day:02}")?;
            }
        }
        Ok(())
    }
}

// The dates the albums can be sorted by, see `TrackCredits::sort_dates()`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SortDates {
    // For the Released sort, see `TrackCredits::released_date()`
    pub(crate) date: Option<TagDate>,
    pub(crate) original: Option<TagDate>,
    pub(crate) recorded: Option<TagDate>,
}

// One of the dates of `SortDates`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DateKind {
    Released,
    OriginalRelease,
    Recorded,
}

impl SortDates {
    pub(crate) fn get(self, kind: DateKind) -> Option<TagDate> {
        match kind {
            DateKind::Released => self.date,
            DateKind::OriginalRelease => self.original,
            DateKind::Recorded => self.recorded,
        }
    }

    // The release's date, the original release's and the recording's, ""
    // for none
    #[cfg(test)]
    pub(crate) fn parse(date: &str, original: &str, recorded: &str) -> Self {
        Self {
            date: TagDate::parse(date),
            original: TagDate::parse(original),
            recorded: TagDate::parse(recorded),
        }
    }
}

fn days_in(year: u16, month: u8) -> u8 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Option<String> {
        TagDate::parse(text).map(|date| date.to_string())
    }

    #[test]
    fn dates_keep_the_parts_they_have() {
        assert_eq!(read("1965").as_deref(), Some("1965"));
        assert_eq!(read("1965-03").as_deref(), Some("1965-03"));
        assert_eq!(read("1965-03-12").as_deref(), Some("1965-03-12"));
        assert_eq!(read(" 1965-03-12 ").as_deref(), Some("1965-03-12"));
    }

    #[test]
    fn parts_written_by_hand_are_padded() {
        assert_eq!(read("1965-3").as_deref(), Some("1965-03"));
        assert_eq!(read("1965-3-7").as_deref(), Some("1965-03-07"));
    }

    #[test]
    fn parts_that_make_no_sense_leave_the_year() {
        // As rippers that only know the year write it
        assert_eq!(read("2005-00-00").as_deref(), Some("2005"));
        assert_eq!(read("2005-13").as_deref(), Some("2005"));
        assert_eq!(read("2005-03-00").as_deref(), Some("2005-03"));
        assert_eq!(read("2005-02-30").as_deref(), Some("2005-02"));
        assert_eq!(read("2004-02-29").as_deref(), Some("2004-02-29"));
        assert_eq!(read("1900-02-29").as_deref(), Some("1900-02"));
        assert_eq!(read("2000-02-29").as_deref(), Some("2000-02-29"));
        assert_eq!(read("1965-").as_deref(), Some("1965"));
        assert_eq!(read("1965-031").as_deref(), Some("1965"));
        assert_eq!(read("1965 (reissue)").as_deref(), Some("1965"));
    }

    #[test]
    fn times_and_intervals_give_their_start() {
        assert_eq!(read("1965-03-12T20:00:00").as_deref(), Some("1965-03-12"));
        assert_eq!(read("1937-11-08/1938-03-08").as_deref(), Some("1937-11-08"));
        assert_eq!(read("1937-11/1938-03").as_deref(), Some("1937-11"));
        assert_eq!(read("1965/1966").as_deref(), Some("1965"));
    }

    #[test]
    fn without_a_year_there_is_no_date() {
        assert_eq!(read(""), None);
        assert_eq!(read("65"), None);
        assert_eq!(read("196"), None);
        assert_eq!(read("March 1965"), None);
        assert_eq!(read("19x5"), None);
    }

    #[test]
    fn the_sort_text_has_a_fixed_width_and_reads_back() {
        for (text, sort_text) in [
            ("1965-03-12", "1965-03-12"),
            ("1965-03", "1965-03-~~"),
            ("1965", "1965-~~-~~"),
        ] {
            let date = TagDate::parse(text).unwrap();
            assert_eq!(date.sort_text(), sort_text);
            assert_eq!(TagDate::from_sort_text(sort_text), Some(date));
        }
    }

    #[test]
    fn the_sort_text_sorts_as_the_sorts_do() {
        let texts = [
            "1965-11",
            "1966",
            "1965",
            "1965-03-12",
            "1965-03",
            "1964-12-31",
        ];
        let mut by_text = texts.map(|text| TagDate::parse(text).unwrap());
        by_text.sort_by_key(|date| date.sort_text());
        assert_eq!(
            by_text.map(|date| date.to_string()),
            [
                "1964-12-31",
                "1965-03-12",
                "1965-03",
                "1965-11",
                "1965",
                "1966"
            ]
        );
    }
}
