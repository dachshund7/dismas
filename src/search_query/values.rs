// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! The values of the fields that aren't text: numbers, lengths, dates,
//! catalogue numbers and paths, as a search writes them and as the fields
//! of a track keep them

use gtk::glib;

use super::{Field, Op, ProblemKind, Test};
use crate::{audio::normalize_search_text, catalogue};

// Numbers between two bounds, both included; a missing one is open
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NumberRange {
    pub lo: Option<i64>,
    pub hi: Option<i64>,
}

impl NumberRange {
    pub fn contains(self, n: i64) -> bool {
        self.lo.is_none_or(|lo| n >= lo) && self.hi.is_none_or(|hi| n <= hi)
    }
}

// The test of a field of numbers, or of lengths, which `parse` reads.
// While a range is typed at the end of the search, an upper bound below
// the lower one reads as open, as "1960..1" is on its way to "1960..1970"
pub(super) fn numbers(
    op: Op,
    value: &str,
    at_end: bool,
    parse: impl Fn(&str) -> Option<i64>,
    error: ProblemKind,
) -> Result<Test, ProblemKind> {
    if let Some((lo, hi)) = value.split_once("..") {
        if !matches!(op, Op::Contains | Op::Equals) {
            return Err(error);
        }
        let bound = |s: &str| match s {
            "" => Ok(None),
            s => parse(s).map(Some).ok_or_else(|| error.clone()),
        };
        let (lo, mut hi) = (bound(lo)?, bound(hi)?);
        if lo.is_none() && hi.is_none() {
            return Err(error);
        }
        if let (Some(lo), Some(upper)) = (lo, hi)
            && upper < lo
        {
            if at_end {
                hi = None;
            } else {
                return Err(ProblemKind::ReversedRange);
            }
        }
        return Ok(Test::Numbers(NumberRange { lo, hi }));
    }

    let n = parse(value).ok_or(error)?;
    let (lo, hi) = match op {
        Op::Contains | Op::Equals => (Some(n), Some(n)),
        Op::Less => (None, Some(n.saturating_sub(1))),
        Op::LessOrEqual => (None, Some(n)),
        Op::Greater => (Some(n.saturating_add(1)), None),
        Op::GreaterOrEqual => (Some(n), None),
    };
    Ok(Test::Numbers(NumberRange { lo, hi }))
}

pub(super) fn number(s: &str) -> Option<i64> {
    s.parse().ok()
}

// A length in seconds: `3:30` or `1:02:03`, a number of seconds, or a
// number with a unit, `90s`, `5min` or `2h`, which can follow each other,
// as in `1h30min`; "m" isn't a unit, as it's months in dates
pub(crate) fn duration(s: &str) -> Option<i64> {
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() > 3 || parts.iter().any(|part| part.is_empty()) {
            return None;
        }
        return parts.iter().try_fold(0i64, |total, part| {
            let n: i64 = part.parse().ok()?;
            total.checked_mul(60)?.checked_add(n)
        });
    }
    if let Ok(seconds) = s.parse::<i64>() {
        return Some(seconds);
    }
    let mut total = 0i64;
    let mut rest = s;
    while !rest.is_empty() {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let n: i64 = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        let (unit, len) = if rest.starts_with("min") {
            (60, 3)
        } else if rest.starts_with('h') {
            (3600, 1)
        } else if rest.starts_with('s') {
            (1, 1)
        } else {
            return None;
        };
        rest = &rest[len..];
        total = total.checked_add(n.checked_mul(unit)?)?;
    }
    Some(total)
}

// A time to compare dates with
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DatePoint {
    // In seconds since the epoch
    At(i64),
    // An amount of time before the time the search is made at, which isn't
    // worked out until then, so that the same search is always the same
    Ago { count: u32, unit: DateUnit },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DateUnit {
    Day,
    Week,
    Month,
    Year,
}

impl DatePoint {
    // In seconds since the epoch, at `now`; months and years are counted on
    // the calendar, in local time
    pub fn at(self, now: i64) -> i64 {
        match self {
            Self::At(time) => time,
            Self::Ago { count, unit } => {
                let count = i64::from(count);
                let local = || glib::DateTime::from_unix_local(now).ok();
                let calendar = |months: i64| {
                    let months = i32::try_from(months).ok()?;
                    local()?.add_months(-months).ok().map(|date| date.to_unix())
                };
                let days = |days: i64| now.saturating_sub(days.saturating_mul(24 * 60 * 60));
                match unit {
                    DateUnit::Day => days(count),
                    DateUnit::Week => days(count.saturating_mul(7)),
                    DateUnit::Month => calendar(count).unwrap_or_else(|| days(count * 30)),
                    DateUnit::Year => calendar(count.saturating_mul(12))
                        .unwrap_or_else(|| days(count.saturating_mul(365))),
                }
            }
        }
    }
}

// Times from one, included, until another, left out; a missing one is open
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DateRange {
    pub from: Option<DatePoint>,
    pub until: Option<DatePoint>,
}

impl DateRange {
    // Whether the range and the times from `start` until `end` overlap
    pub fn overlaps(self, start: i64, end: i64, now: i64) -> bool {
        self.from.is_none_or(|from| end > from.at(now))
            && self.until.is_none_or(|until| start < until.at(now))
    }
}

// A date as a search writes it
enum DateValue {
    // A year, a month or a day, from its start until the next one's
    Period(i64, i64),
    Ago(DatePoint),
}

impl DateValue {
    fn start(&self) -> DatePoint {
        match self {
            Self::Period(start, _) => DatePoint::At(*start),
            Self::Ago(point) => *point,
        }
    }

    fn end(&self) -> DatePoint {
        match self {
            Self::Period(_, end) => DatePoint::At(*end),
            Self::Ago(point) => *point,
        }
    }
}

fn date_value(s: &str) -> Option<DateValue> {
    if let Some(ago) = s.strip_prefix('-') {
        let (digits, unit) = ago.split_at(ago.len().checked_sub(1)?);
        let count = digits.parse().ok()?;
        let unit = match unit {
            "d" => DateUnit::Day,
            "w" => DateUnit::Week,
            "m" => DateUnit::Month,
            "y" => DateUnit::Year,
            _ => return None,
        };
        return Some(DateValue::Ago(DatePoint::Ago { count, unit }));
    }
    let (start, end) = period(s)?;
    Some(DateValue::Period(start, end))
}

// A year, a month or a day, "2026", "2026-10" or "2026-10-31", from its
// start until the start of the next one, in local time
pub(crate) fn period(s: &str) -> Option<(i64, i64)> {
    let parts: Vec<&str> = s.split('-').collect();
    let number = |part: &str, digits: usize| {
        (part.len() == digits && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse::<i32>().ok())
            .flatten()
    };
    let zone = glib::TimeZone::local();
    let date = |y, m, d| glib::DateTime::new(&zone, y, m, d, 0, 0, 0.0).ok();
    let (start, end) = match parts.as_slice() {
        [y] => {
            let start = date(number(y, 4)?, 1, 1)?;
            let end = start.add_years(1).ok()?;
            (start, end)
        }
        [y, m] => {
            let start = date(number(y, 4)?, number(m, 2)?, 1)?;
            let end = start.add_months(1).ok()?;
            (start, end)
        }
        [y, m, d] => {
            let start = date(number(y, 4)?, number(m, 2)?, number(d, 2)?)?;
            let end = start.add_days(1).ok()?;
            (start, end)
        }
        _ => return None,
    };
    Some((start.to_unix(), end.to_unix()))
}

// The times a date in the tags covers, e.g. "1986", "1986-11-04", or a
// span, "1937-11-08/1938-03-08", ignoring a time of day
pub(crate) fn tagged_span(value: &str) -> Option<(i64, i64)> {
    let day = |s: &str| -> Option<(i64, i64)> {
        let s = s.trim();
        period(s.get(..10.min(s.len()))?)
    };
    match value.split_once('/') {
        Some((first, last)) => Some((day(first)?.0, day(last)?.1)),
        None => day(value),
    }
}

pub(super) fn dates(field: Field, op: Op, value: &str, at_end: bool) -> Result<Test, ProblemKind> {
    let error = || ProblemKind::NotADate(field);
    if let Some((from, until)) = value.split_once("..") {
        if !matches!(op, Op::Contains | Op::Equals) {
            return Err(error());
        }
        let bound = |s: &str| match s {
            "" => Ok(None),
            s => date_value(s).map(Some).ok_or_else(error),
        };
        let (from, until) = (bound(from)?, bound(until)?);
        if from.is_none() && until.is_none() {
            return Err(error());
        }
        let mut range = DateRange {
            from: from.as_ref().map(DateValue::start),
            until: until.as_ref().map(DateValue::end),
        };
        if let (Some(DatePoint::At(from)), Some(DatePoint::At(until))) = (range.from, range.until)
            && until <= from
        {
            if at_end {
                range.until = None;
            } else {
                return Err(ProblemKind::ReversedRange);
            }
        }
        return Ok(Test::Dates(range));
    }

    let value = date_value(value).ok_or_else(error)?;
    let (start, end) = (value.start(), value.end());
    let range = match (op, &value) {
        // A relative date alone is since then, as "in the last" 30 days
        (Op::Contains | Op::Equals, DateValue::Ago(_)) => DateRange {
            from: Some(start),
            until: None,
        },
        (Op::Contains | Op::Equals, DateValue::Period(..)) => DateRange {
            from: Some(start),
            until: Some(end),
        },
        (Op::Less, _) => DateRange {
            from: None,
            until: Some(start),
        },
        (Op::LessOrEqual, _) => DateRange {
            from: None,
            until: Some(end),
        },
        (Op::Greater, _) => DateRange {
            from: Some(end),
            until: None,
        },
        (Op::GreaterOrEqual, _) => DateRange {
            from: Some(start),
            until: None,
        },
    };
    Ok(Test::Dates(range))
}

// A catalogue number to find, e.g. "bwv1047", "op27", "bwv" or
// "bwv1046..1051"
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatalogueTest {
    catalogue: &'static str,
    number: CatalogueNumberTest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CatalogueNumberTest {
    Any,
    // The parts of the number it starts with, so that "op. 2" finds
    // "op. 2 no. 4"
    Starts(Vec<(u32, String)>),
    // The first part of the number between two, both included
    Range(Option<u32>, Option<u32>),
}

pub(super) fn catalogue(op: Op, value: &str, at_end: bool) -> Result<Test, ProblemKind> {
    let error = || ProblemKind::NotACatalogueNumber;
    if !matches!(op, Op::Contains | Op::Equals) {
        return Err(ProblemKind::CantCompare(Field::Catalogue));
    }
    if let Some((from, until)) = value.split_once("..") {
        let from = catalogue::catalogue_numbers(from)
            .into_iter()
            .next()
            .ok_or_else(error)?;
        let lo = from.order.first().map(|(value, _)| *value);
        let mut hi = match until {
            "" => None,
            until => Some(until.parse::<u32>().ok().ok_or_else(error)?),
        };
        if let (Some(lo), Some(upper)) = (lo, hi)
            && upper < lo
        {
            if at_end {
                hi = None;
            } else {
                return Err(ProblemKind::ReversedRange);
            }
        }
        return Ok(Test::Catalogue(CatalogueTest {
            catalogue: from.catalogue,
            number: CatalogueNumberTest::Range(lo, hi),
        }));
    }
    if let Some(number) = catalogue::catalogue_numbers(value).into_iter().next() {
        return Ok(Test::Catalogue(CatalogueTest {
            catalogue: number.catalogue,
            number: CatalogueNumberTest::Starts(number.order),
        }));
    }
    let catalogue = catalogue::catalogue_name(value).ok_or_else(error)?;
    Ok(Test::Catalogue(CatalogueTest {
        catalogue,
        number: CatalogueNumberTest::Any,
    }))
}

impl CatalogueTest {
    // Whether it finds a catalogue number as the fields keep it, see
    // `CatalogueNumber::search_text()`
    pub fn matches(&self, kept: &str) -> bool {
        let Some((catalogue, number)) = kept.split_once(' ') else {
            return false;
        };
        if catalogue != self.catalogue {
            return false;
        }
        let parts: Vec<(u32, &str)> = number
            .split('.')
            .map(|part| {
                let digits = part.chars().take_while(char::is_ascii_digit).count();
                (part[..digits].parse().unwrap_or(0), &part[digits..])
            })
            .collect();
        match &self.number {
            CatalogueNumberTest::Any => true,
            CatalogueNumberTest::Starts(start) => {
                start.len() <= parts.len()
                    && start
                        .iter()
                        .zip(&parts)
                        .all(|((value, rest), (kept, kept_rest))| {
                            value == kept && rest == kept_rest
                        })
            }
            CatalogueNumberTest::Range(lo, hi) => parts.first().is_some_and(|(value, _)| {
                lo.is_none_or(|lo| *value >= lo) && hi.is_none_or(|hi| *value <= hi)
            }),
        }
    }
}

// Where a track's path is to be found: from the start of a folder's or a
// file's name, or, anchored, from the start of the whole path
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathTest {
    // Folded
    text: String,
    anchored: bool,
}

pub(super) fn path(op: Op, value: &str) -> Result<Test, ProblemKind> {
    if !matches!(op, Op::Contains | Op::Equals) {
        return Err(ProblemKind::CantCompare(Field::Path));
    }
    let (value, anchored) = match value.strip_prefix("~/") {
        Some(rest) => (
            glib::home_dir().join(rest).to_string_lossy().into_owned(),
            true,
        ),
        None => (value.to_string(), value.starts_with('/')),
    };
    Ok(Test::Path(PathTest {
        text: normalize_search_text(&value),
        anchored,
    }))
}

impl PathTest {
    // Whether a path, folded, matches
    pub fn matches(&self, path: &str) -> bool {
        if self.anchored {
            return path.starts_with(&self.text);
        }
        path.match_indices(&self.text)
            .any(|(at, _)| at == 0 || path[..at].ends_with('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_are_read_in_any_of_their_forms() {
        assert_eq!(duration("3:30"), Some(210));
        assert_eq!(duration("1:02:03"), Some(3723));
        assert_eq!(duration("90"), Some(90));
        assert_eq!(duration("90s"), Some(90));
        assert_eq!(duration("5min"), Some(300));
        assert_eq!(duration("2h"), Some(7200));
        assert_eq!(duration("1h30min"), Some(5400));
        assert_eq!(duration("5m"), None);
        assert_eq!(duration("3::30"), None);
        assert_eq!(duration("min"), None);
    }

    #[test]
    fn periods_run_until_the_next_one() {
        let (start, end) = period("2026").unwrap();
        assert_eq!(period("2026-01-01").unwrap().0, start);
        assert_eq!(period("2027").unwrap().0, end);
        let (start, end) = period("2026-02").unwrap();
        assert_eq!(period("2026-03").unwrap().0, end);
        assert_eq!(period("2026-02-01").unwrap().0, start);
        assert!(period("26").is_none());
        assert!(period("2026-2").is_none());
        assert!(period("2026-13").is_none());
    }

    #[test]
    fn tagged_dates_cover_their_periods() {
        let (start, end) = tagged_span("1937-11-08/1938-03-08").unwrap();
        assert_eq!(start, period("1937-11-08").unwrap().0);
        assert_eq!(end, period("1938-03-09").unwrap().0);
        assert_eq!(tagged_span("1986-11-04T10:00:00"), period("1986-11-04"));
        assert_eq!(tagged_span("1986"), period("1986"));
        assert!(tagged_span("unknown").is_none());
    }

    #[test]
    fn relative_dates_are_moments_before_now() {
        let now = period("2026-10-31").unwrap().0 + 12 * 3600;
        let ago = |count, unit| DatePoint::Ago { count, unit }.at(now);
        assert_eq!(ago(61, DateUnit::Day), now - 61 * 24 * 3600);
        assert_eq!(ago(2, DateUnit::Week), now - 14 * 24 * 3600);
        assert_eq!(
            ago(1, DateUnit::Month),
            period("2026-09-30").unwrap().0 + 12 * 3600
        );
        assert_eq!(
            ago(1, DateUnit::Year),
            period("2025-10-31").unwrap().0 + 12 * 3600
        );
    }

    #[test]
    fn catalogue_numbers_match_by_their_parts() {
        let test = |value: &str| match catalogue(Op::Contains, value, false).unwrap() {
            Test::Catalogue(test) => test,
            _ => unreachable!(),
        };
        assert!(test("bwv1047").matches("bwv 1047"));
        assert!(test("BWV 1047").matches("bwv 1047"));
        assert!(!test("bwv10").matches("bwv 1047"));
        assert!(test("op2").matches("op 2.4"));
        assert!(!test("op2.3").matches("op 2.4"));
        assert!(test("bwv").matches("bwv 1"));
        assert!(!test("bwv").matches("k 525"));
        assert!(test("kv525").matches("k 525"));
        assert!(test("bwv1046..1051").matches("bwv 1050"));
        assert!(!test("bwv1046..1051").matches("bwv 1052"));
        assert!(test("bwv1046..").matches("bwv 1080"));
        assert!(catalogue(Op::Contains, "nothing", false).is_err());
        assert_eq!(
            catalogue(Op::Contains, "bwv1051..1046", false),
            Err(ProblemKind::ReversedRange)
        );
    }

    #[test]
    fn paths_match_from_the_start_of_a_name() {
        let test = |value: &str| match path(Op::Contains, value).unwrap() {
            Test::Path(test) => test,
            _ => unreachable!(),
        };
        let path = "/home/user/music/bach/cantatas/bwv 1.flac";
        assert!(test("bach/cantatas").matches(path));
        assert!(test("Bach/Cant").matches(path));
        assert!(!test("ach/cantatas").matches(path));
        assert!(!test("/bach").matches(path));
        assert!(test("/home/user/music").matches(path));
        assert!(!test("/music").matches(path));
        assert!(
            test(&format!("{}/x", glib::home_dir().display())).matches(&format!(
                "{}/x/y",
                normalize_search_text(&glib::home_dir().to_string_lossy())
            ))
        );
    }
}
