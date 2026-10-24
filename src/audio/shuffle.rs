// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The order of a shuffle. It's still random, but it goes by the play stats:
// the tracks heard in the last few days tend to go toward the end, and the
// tracks played often that haven't been heard for a while tend to come
// early. The movements of a work move together, in their order, and the
// same artist doesn't come twice in a row when other artists can come in
// between.

use std::{collections::HashMap, ops::Range};

use rand::{Rng, RngExt};

use crate::play_stats::TrackStats;

const DAY: f64 = 24.0 * 60.0 * 60.0;
// A track heard just now has a tenth of the weight of a track never heard,
const FLOOR: f64 = 0.1;
// and it gets halfway back to the weight of a track never heard in a week
const HALF_LIFE: f64 = 7.0 * DAY;
// A track played this many times has twice the weight of one never played,
// once it hasn't been heard for a while
const FAVORITE_PLAYS: u32 = 10;

// What the shuffle knows of a track
#[derive(Clone, Debug, Default)]
pub struct Entry {
    // Whether the track is a movement that continues the work of the
    // track before it, as in the headers of the queue (`continues_work()`)
    pub continues_work: bool,
    // The artist that shouldn't come twice in a row: for a movement, the
    // composers. Tracks without one are never the same artist as another
    pub artist: Option<String>,
    // None for a track never heard
    pub stats: Option<TrackStats>,
}

// A track, or the movements of a work that are next to each other, which
// the shuffle moves together
#[derive(Debug)]
struct Unit {
    // The positions of the tracks
    tracks: Range<usize>,
    // An id for the artist, the same for the same artist in any case
    artist: Option<usize>,
    weight: f64,
}

// The positions of the tracks in their shuffled order. The track to keep
// comes first, with the other movements of its work around it; the tracks
// that came before it are shuffled too
pub fn shuffled_order(
    entries: &[Entry],
    keep: Option<usize>,
    now: i64,
    rng: &mut (impl Rng + ?Sized),
) -> Vec<usize> {
    let mut units = units(entries, now);
    let kept = keep
        .and_then(|keep| units.iter().position(|unit| unit.tracks.contains(&keep)))
        .map(|pos| units.remove(pos));

    // A random order where each unit is more likely to come before another
    // the more weight it has: a unit of weight a comes before one of weight
    // b with a probability of a / (a + b). Efraimidis and Spirakis's key,
    // u^(1/w) for a random u in (0, 1], is compared through its logarithm
    let mut keyed: Vec<(f64, Unit)> = units
        .into_iter()
        .map(|unit| {
            let u: f64 = rng.random();
            ((1.0 - u).ln() / unit.weight, unit)
        })
        .collect();
    keyed.sort_by(|a, b| b.0.total_cmp(&a.0));
    let weighted: Vec<Unit> = keyed.into_iter().map(|(_, unit)| unit).collect();

    let last = kept.as_ref().and_then(|unit| unit.artist);
    kept.into_iter()
        .chain(spread_artists(weighted, last))
        .flat_map(|unit| unit.tracks)
        .collect()
}

// The tracks in units: the movements of the same work next to each other
// are one, and every other track is one of its own
fn units(entries: &[Entry], now: i64) -> Vec<Unit> {
    let mut artists: HashMap<String, usize> = HashMap::new();
    let mut units: Vec<Unit> = Vec::new();
    for (pos, entry) in entries.iter().enumerate() {
        match units.last_mut() {
            Some(unit) if entry.continues_work => unit.tracks.end = pos + 1,
            _ => {
                let artist = entry.artist.as_ref().map(|artist| {
                    let next = artists.len();
                    *artists.entry(artist.to_lowercase()).or_insert(next)
                });
                units.push(Unit {
                    tracks: pos..pos + 1,
                    artist,
                    weight: 0.0,
                });
            }
        }
    }

    // A work was heard when its last movement heard was, and it was played
    // as often as its movement played the most
    for unit in &mut units {
        let stats = entries[unit.tracks.clone()]
            .iter()
            .filter_map(|entry| entry.stats);
        let last_heard = stats.clone().map(|stats| stats.last_heard).max();
        let plays = stats.map(|stats| stats.plays).max().unwrap_or(0);
        let age = last_heard.map(|last_heard| now - last_heard);
        unit.weight = freshness(age) * favorite(plays);
    }
    units
}

// From a tenth, for a track heard just now, to 1, for a track not heard
// for weeks or never heard, given the seconds since it was last heard
fn freshness(age: Option<i64>) -> f64 {
    match age {
        None => 1.0,
        // A time in the future, after the clock was changed, is now
        Some(age) => 1.0 - (1.0 - FLOOR) * (-(age.max(0) as f64) / HALF_LIFE).exp2(),
    }
}

// From 1, for a track never played, to 2 for the tracks played the most
fn favorite(plays: u32) -> f64 {
    1.0 + f64::from(plays.min(FAVORITE_PLAYS)) / f64::from(FAVORITE_PLAYS)
}

// Puts off the units whose artist is the one just before, so the same
// artist doesn't come twice in a row, while keeping as much as possible of
// the order they're in. An artist with more units than all the others
// together can't be kept apart, but its runs are kept as short as they can
// be, rather than piling up at the end
fn spread_artists(mut units: Vec<Unit>, last: Option<usize>) -> Vec<Unit> {
    let mut spread = Spread::new(&units, last);
    let mut order = Vec::with_capacity(units.len());
    while !units.is_empty() {
        let pos = units
            .iter()
            .position(|unit| spread.allows(unit.artist))
            .unwrap_or(0);
        let unit = units.remove(pos);
        spread.place(unit.artist);
        order.push(unit);
    }
    order
}

// The units left to place, by artist, to tell which artists can come next
struct Spread {
    // The number of units left of each artist
    left: Vec<usize>,
    // The number of units left, with an artist or not
    n_left: usize,
    // The artist of the unit placed last, and the number of its units in a
    // row so far
    last: Option<(usize, usize)>,
    // The most units of the same artist in a row: 1, unless an artist has
    // more units than all the others together
    max_run: usize,
    // The artist with the most units left, and the number of units of the
    // artists with the most and the second most
    most: (Option<usize>, usize, usize),
}

impl Spread {
    fn new(units: &[Unit], last: Option<usize>) -> Self {
        let n_artists = units
            .iter()
            .filter_map(|unit| unit.artist)
            .chain(last)
            .max()
            .map_or(0, |max| max + 1);
        let mut left = vec![0; n_artists];
        for artist in units.iter().filter_map(|unit| unit.artist) {
            left[artist] += 1;
        }
        let mut spread = Self {
            left,
            n_left: units.len(),
            last: last.map(|artist| (artist, 1)),
            max_run: 1,
            most: (None, 0, 0),
        };

        // The units of an artist, with the run of the last one before them,
        // go in runs between the units of the other artists, which make
        // room for one run more than there are of them
        spread.max_run = (0..n_artists)
            .map(|artist| {
                let run = spread.run(artist);
                let others = spread.n_left - spread.left[artist];
                (spread.left[artist] + run).div_ceil(others + 1)
            })
            .fold(1, usize::max);
        spread.count_most();
        spread
    }

    // The number of units of the artist in a row just before
    fn run(&self, artist: usize) -> usize {
        match self.last {
            Some((last, run)) if last == artist => run,
            _ => 0,
        }
    }

    fn count_most(&mut self) {
        let mut most = (None, 0, 0);
        for (artist, &left) in self.left.iter().enumerate() {
            if left > most.1 {
                most = (Some(artist), left, most.1);
            } else if left > most.2 {
                most.2 = left;
            }
        }
        self.most = most;
    }

    // Whether a unit of the artist can come next, leaving room for all
    // the units left in runs no longer than `max_run`
    fn allows(&self, artist: Option<usize>) -> bool {
        // The units left after it
        let n_left = self.n_left - 1;
        if let Some(artist) = artist {
            let run = self.run(artist) + 1;
            let left = self.left[artist] - 1;
            if run > self.max_run || left + run > self.max_run * (n_left - left + 1) {
                return false;
            }
        }
        // Of the other artists, the one with the most units left is the
        // only one that could be short of room
        let other = if artist.is_some() && artist == self.most.0 {
            self.most.2
        } else {
            self.most.1
        };
        other <= self.max_run * (n_left - other + 1)
    }

    fn place(&mut self, artist: Option<usize>) {
        self.n_left -= 1;
        self.last = artist.map(|artist| {
            self.left[artist] -= 1;
            (artist, self.run(artist) + 1)
        });
        self.count_most();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{SeedableRng, rngs::StdRng};

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 24 * 60 * 60;

    fn seeded(seed: u64) -> StdRng {
        StdRng::seed_from_u64(seed)
    }

    fn track(artist: &str) -> Entry {
        Entry {
            artist: Some(artist.to_string()),
            ..Entry::default()
        }
    }

    fn heard(artist: &str, plays: u32, days_ago: i64) -> Entry {
        Entry {
            stats: Some(TrackStats {
                plays,
                last_heard: NOW - days_ago * DAY,
            }),
            ..track(artist)
        }
    }

    // The movements of a work, each after the first continuing it
    fn work(movements: usize) -> impl Iterator<Item = Entry> {
        (0..movements).map(|i| Entry {
            continues_work: i > 0,
            ..track("Beethoven")
        })
    }

    // The same tracks, each once
    fn is_permutation(order: &[usize], n: usize) -> bool {
        let mut sorted = order.to_vec();
        sorted.sort_unstable();
        sorted == (0..n).collect::<Vec<_>>()
    }

    // The mean position of some tracks over many shuffles
    fn mean_position(entries: &[Entry], tracks: Range<usize>) -> f64 {
        let mut sum = 0;
        let mut count = 0;
        for seed in 0..200 {
            let order = shuffled_order(entries, None, NOW, &mut seeded(seed));
            for (pos, track) in order.iter().enumerate() {
                if tracks.contains(track) {
                    sum += pos;
                    count += 1;
                }
            }
        }
        sum as f64 / f64::from(count)
    }

    // How often the first track comes before the second one
    fn comes_first(entries: &[Entry]) -> f64 {
        let first = (0..1000)
            .filter(|&seed| shuffled_order(entries, None, NOW, &mut seeded(seed))[0] == 0)
            .count();
        first as f64 / 1000.0
    }

    #[test]
    fn freshness_grows_back_with_time() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(close(freshness(Some(0)), FLOOR));
        assert!(close(freshness(None), 1.0));
        let ages = [0, 1, 2, 7, 14, 30, 365].map(|days| freshness(Some(days * DAY)));
        assert!(ages.windows(2).all(|pair| pair[0] < pair[1]), "{ages:?}");
        assert!(close(freshness(Some(7 * DAY)), 0.55));
        assert!(freshness(Some(365 * DAY)) > 0.999);
        // The clock was set back
        assert!(close(freshness(Some(-DAY)), FLOOR));
    }

    #[test]
    fn a_shuffle_has_every_track_once() {
        let entries: Vec<Entry> = (0..20)
            .map(|i| heard(&format!("{}", i % 3), i, i64::from(i)))
            .collect();
        for seed in 0..20 {
            let order = shuffled_order(&entries, None, NOW, &mut seeded(seed));
            assert!(is_permutation(&order, 20), "{order:?}");
        }
    }

    #[test]
    fn the_kept_track_comes_first() {
        let entries: Vec<Entry> = (0..10).map(|i| track(&format!("{i}"))).collect();
        for seed in 0..20 {
            let order = shuffled_order(&entries, Some(6), NOW, &mut seeded(seed));
            assert_eq!(order[0], 6);
            assert!(is_permutation(&order, 10));
        }
    }

    #[test]
    fn the_movements_of_a_work_stay_together_in_order() {
        let mut entries: Vec<Entry> = (0..6).map(|i| track(&format!("{i}"))).collect();
        // Two works next to each other, then the first one again
        entries.splice(2..2, work(4));
        entries.splice(6..6, work(3));
        entries.extend(work(1));
        let n = entries.len();

        let mut apart = false;
        for seed in 0..50 {
            let order = shuffled_order(&entries, None, NOW, &mut seeded(seed));
            assert!(is_permutation(&order, n));
            let fifth = order.iter().position(|&t| t == 2).unwrap();
            assert_eq!(order[fifth..fifth + 4], [2, 3, 4, 5]);
            let seventh = order.iter().position(|&t| t == 6).unwrap();
            assert_eq!(order[seventh..seventh + 3], [6, 7, 8]);
            apart |= seventh != fifth + 4 && fifth != seventh + 3;
        }
        assert!(apart, "the works were always next to each other");
    }

    #[test]
    fn a_work_played_again_is_a_unit_of_its_own() {
        // Two recordings of a work, or the same one added twice
        let entries: Vec<Entry> = work(3).chain(work(3)).collect();
        let units: Vec<Range<usize>> = units(&entries, NOW)
            .into_iter()
            .map(|unit| unit.tracks)
            .collect();
        assert_eq!(units, [0..3, 3..6]);
    }

    #[test]
    fn a_kept_movement_keeps_its_work_first() {
        let mut entries: Vec<Entry> = (0..5).map(|i| track(&format!("{i}"))).collect();
        entries.extend(work(4));
        for seed in 0..20 {
            let order = shuffled_order(&entries, Some(6), NOW, &mut seeded(seed));
            assert_eq!(order[..4], [5, 6, 7, 8]);
            assert!(is_permutation(&order, 9));
        }
    }

    #[test]
    fn tracks_heard_recently_tend_to_come_last() {
        // Each track by its own artist, so that only the play stats count;
        // in a plain shuffle, both would be at 9.5 on average
        let mut entries: Vec<Entry> = (0..10).map(|i| heard(&format!("{i}"), 1, 0)).collect();
        entries.extend((10..20).map(|i| track(&format!("{i}"))));
        let recent = mean_position(&entries, 0..10);
        let never = mean_position(&entries, 10..20);
        assert!(
            recent > 12.0 && never < 7.0,
            "recent: {recent}, never: {never}"
        );

        // A track heard months ago is like one never heard
        let first = comes_first(&[heard("a", 0, 90), track("b")]);
        assert!((0.45..0.55).contains(&first), "{first}");
    }

    #[test]
    fn favorites_not_heard_for_a_while_tend_to_come_first() {
        let first = comes_first(&[heard("a", 20, 90), track("b")]);
        assert!(first > 0.6, "{first}");

        // Unless they were heard today
        let first = comes_first(&[heard("a", 20, 0), track("b")]);
        assert!(first < 0.3, "{first}");
    }

    #[test]
    fn the_same_artist_never_comes_twice_in_a_row_with_others_in_between() {
        let entries: Vec<Entry> = (0..15)
            .map(|i| heard(["A", "b", "C"][i % 3], (i % 4) as u32, (i % 5) as i64))
            .collect();
        for seed in 0..50 {
            let order = shuffled_order(&entries, None, NOW, &mut seeded(seed));
            assert!(is_permutation(&order, 15));
            let artists: Vec<usize> = order.iter().map(|&t| t % 3).collect();
            assert!(
                artists.windows(2).all(|pair| pair[0] != pair[1]),
                "{artists:?}"
            );
        }

        // Artists are the same in any case
        let entries = [track("Nina Simone"), track("nina simone"), track("Other")];
        for seed in 0..20 {
            let order = shuffled_order(&entries, None, NOW, &mut seeded(seed));
            assert_eq!(order[1], 2, "{order:?}");
        }
    }

    // The most tracks of the same artist in a row
    fn longest_run(entries: &[Entry], order: &[usize]) -> usize {
        let artists: Vec<Option<String>> = order
            .iter()
            .map(|&t| entries[t].artist.as_ref().map(|a| a.to_lowercase()))
            .collect();
        let mut longest = 0;
        let mut run = 0;
        for (i, artist) in artists.iter().enumerate() {
            run = if artist.is_some() && i > 0 && artists[i - 1] == *artist {
                run + 1
            } else {
                1
            };
            longest = longest.max(run);
        }
        longest
    }

    #[test]
    fn an_artist_with_most_of_the_tracks_has_runs_as_short_as_they_can_be() {
        // 8 tracks by A can't be in runs shorter than 3 with 2 tracks by B
        // between them
        let entries: Vec<Entry> = (0..10)
            .map(|i| track(if i < 8 { "A" } else { "B" }))
            .collect();
        for seed in 0..50 {
            let order = shuffled_order(&entries, None, NOW, &mut seeded(seed));
            assert!(is_permutation(&order, 10));
            assert_eq!(longest_run(&entries, &order), 3, "{order:?}");
        }
    }

    #[test]
    fn artists_are_spread_out_whatever_the_tracks() {
        // Playlists of a few artists, some tracks without one, and a track
        // to keep or not
        let mut rng = seeded(0);
        for _ in 0..300 {
            let n = rng.random_range(1..30);
            let n_artists = rng.random_range(1..6);
            let entries: Vec<Entry> = (0..n)
                .map(|_| match rng.random_range(0..=n_artists) {
                    0 => Entry::default(),
                    artist => heard(
                        &artist.to_string(),
                        rng.random_range(0..20),
                        rng.random_range(0..60),
                    ),
                })
                .collect();
            let keep = rng.random_bool(0.5).then(|| rng.random_range(0..n));

            // The shortest runs the artists allow: those of the artist with
            // the most tracks, between the tracks of the others
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for artist in entries.iter().filter_map(|entry| entry.artist.as_deref()) {
                *counts.entry(artist).or_default() += 1;
            }
            let shortest = counts
                .values()
                .map(|&count| count.div_ceil(n - count + 1))
                .fold(1, usize::max);

            let order = shuffled_order(&entries, keep, NOW, &mut rng);
            assert!(is_permutation(&order, n), "{order:?}");
            if let Some(keep) = keep {
                assert_eq!(order[0], keep);
            }
            // Keeping a track can make the runs one longer, when its artist
            // has the most tracks
            let longest = longest_run(&entries, &order);
            assert!(
                longest <= shortest + usize::from(keep.is_some()),
                "{longest} > {shortest}: {:?}",
                order
                    .iter()
                    .map(|&t| &entries[t].artist)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn tracks_without_an_artist_are_not_the_same_artist() {
        let entries = [Entry::default(), Entry::default(), Entry::default()];
        let mut orders = std::collections::HashSet::new();
        for seed in 0..50 {
            let order = shuffled_order(&entries, None, NOW, &mut seeded(seed));
            assert!(is_permutation(&order, 3));
            orders.insert(order);
        }
        assert_eq!(orders.len(), 6);
    }

    #[test]
    fn a_single_artist_is_still_shuffled() {
        let entries: Vec<Entry> = (0..10).map(|_| track("A")).collect();
        let order = shuffled_order(&entries, None, NOW, &mut seeded(1));
        assert!(is_permutation(&order, 10));
        assert_ne!(order, (0..10).collect::<Vec<_>>());
    }
}
