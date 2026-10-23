// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// Where playback was in each playlist, kept in the resume-points setting,
// to go on from there when the playlist is opened, at startup or when
// switching back to it. A point is only used when its track is found in
// the playlist, so a stale one is harmless; they follow the playlists when
// they're renamed, and go with them when they're deleted

use std::collections::HashMap;

use gtk::{gio, prelude::*};
use log::warn;

const KEY: &str = "resume-points";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumePoint {
    pub uri: String,
    // In the order of the playlist
    pub index: u32,
    // In seconds
    pub position: u32,
}

impl ResumePoint {
    // The point of the playlist, empty for the queue
    pub fn load(settings: &gio::Settings, playlist: &str) -> Option<Self> {
        ResumePoints::load(settings).get(playlist).cloned()
    }

    // The position of the track among the URIs of the playlist: the one at
    // its index, unless the playlist changed since, e.g. tracks that can't
    // be read anymore; then the first one with its URI
    pub fn find_track(&self, uris: &[String]) -> Option<u32> {
        if uris.get(self.index as usize) == Some(&self.uri) {
            return Some(self.index);
        }
        uris.iter()
            .position(|uri| *uri == self.uri)
            .map(|pos| pos as u32)
    }
}

// The points by playlist name, empty for the queue
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumePoints(HashMap<String, ResumePoint>);

impl ResumePoints {
    pub fn load(settings: &gio::Settings) -> Self {
        let points = settings.get::<HashMap<String, (String, u32, u32)>>(KEY);
        Self(
            points
                .into_iter()
                .map(|(playlist, (uri, index, position))| {
                    let point = ResumePoint {
                        uri,
                        index,
                        position,
                    };
                    (playlist, point)
                })
                .collect(),
        )
    }

    // Stored every 30 s while playing, so unchanged points aren't written
    pub fn store(&self, settings: &gio::Settings) {
        if *self == Self::load(settings) {
            return;
        }
        let points: HashMap<&str, (&str, u32, u32)> = self
            .0
            .iter()
            .map(|(playlist, p)| (playlist.as_str(), (p.uri.as_str(), p.index, p.position)))
            .collect();
        if let Err(e) = settings.set(KEY, points) {
            warn!("Unable to store the resume points: {e}");
        }
    }

    // Loads the points, changes them, and stores them if they changed
    pub fn update(settings: &gio::Settings, change: impl FnOnce(&mut Self)) {
        let mut points = Self::load(settings);
        change(&mut points);
        points.store(settings);
    }

    pub fn get(&self, playlist: &str) -> Option<&ResumePoint> {
        self.0.get(playlist).filter(|p| !p.uri.is_empty())
    }

    pub fn set(&mut self, playlist: &str, point: ResumePoint) {
        self.0.insert(playlist.to_string(), point);
    }

    pub fn remove(&mut self, playlist: &str) {
        self.0.remove(playlist);
    }

    // The point follows the playlist to its new name
    pub fn rename(&mut self, playlist: &str, new_name: &str) {
        if let Some(point) = self.0.remove(playlist) {
            self.0.insert(new_name.to_string(), point);
        }
    }

    // A playlist made with the same tracks starts where the other one is
    pub fn copy(&mut self, playlist: &str, new_name: &str) {
        if let Some(point) = self.0.get(playlist).cloned() {
            self.0.insert(new_name.to_string(), point);
        }
    }

    // Drops the points of playlists that are gone, e.g. removed outside
    // the app; the queue is always there
    pub fn retain_playlists(&mut self, names: &[String]) {
        self.0
            .retain(|playlist, _| playlist.is_empty() || names.contains(playlist));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(uri: &str, index: u32) -> ResumePoint {
        ResumePoint {
            uri: uri.to_string(),
            index,
            position: 90,
        }
    }

    fn uris(uris: &[&str]) -> Vec<String> {
        uris.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn finds_the_track_at_its_index() {
        let uris = uris(&["file:///a", "file:///b", "file:///c"]);
        assert_eq!(point("file:///b", 1).find_track(&uris), Some(1));
    }

    #[test]
    fn finds_a_track_that_moved() {
        let uris = uris(&["file:///b", "file:///c"]);
        assert_eq!(point("file:///c", 2).find_track(&uris), Some(1));
        assert_eq!(point("file:///b", 1).find_track(&uris), Some(0));
    }

    #[test]
    fn misses_a_track_that_is_gone() {
        let uris = uris(&["file:///a", "file:///b"]);
        assert_eq!(point("file:///c", 1).find_track(&uris), None);
        assert_eq!(point("file:///c", 0).find_track(&[]), None);
    }

    #[test]
    fn tells_duplicates_apart() {
        let uris = uris(&["file:///a", "file:///b", "file:///a"]);
        assert_eq!(point("file:///a", 2).find_track(&uris), Some(2));
        assert_eq!(point("file:///a", 0).find_track(&uris), Some(0));
        // The index points at another track: the first copy
        assert_eq!(point("file:///a", 1).find_track(&uris), Some(0));
    }

    #[test]
    fn keeps_a_point_per_playlist() {
        let mut points = ResumePoints::default();
        points.set("", point("file:///a", 0));
        points.set("Mix", point("file:///b", 1));
        assert_eq!(points.get(""), Some(&point("file:///a", 0)));
        assert_eq!(points.get("Mix"), Some(&point("file:///b", 1)));
        assert_eq!(points.get("Other"), None);

        points.set("Mix", point("file:///c", 2));
        assert_eq!(points.get("Mix"), Some(&point("file:///c", 2)));
        assert_eq!(points.get(""), Some(&point("file:///a", 0)));
    }

    #[test]
    fn ignores_a_point_without_a_track() {
        let mut points = ResumePoints::default();
        points.set("", ResumePoint::default());
        assert_eq!(points.get(""), None);
    }

    #[test]
    fn renaming_moves_the_point() {
        let mut points = ResumePoints::default();
        points.set("Mix", point("file:///a", 0));
        points.rename("Mix", "Road Trip");
        assert_eq!(points.get("Mix"), None);
        assert_eq!(points.get("Road Trip"), Some(&point("file:///a", 0)));

        // Renaming a playlist without a point leaves the others alone
        points.rename("Other", "Road Trip");
        assert_eq!(points.get("Road Trip"), Some(&point("file:///a", 0)));
    }

    #[test]
    fn copying_keeps_the_point() {
        let mut points = ResumePoints::default();
        points.set("", point("file:///a", 3));
        points.copy("", "Saved");
        assert_eq!(points.get(""), Some(&point("file:///a", 3)));
        assert_eq!(points.get("Saved"), Some(&point("file:///a", 3)));
    }

    #[test]
    fn drops_the_points_of_playlists_that_are_gone() {
        let mut points = ResumePoints::default();
        points.set("", point("file:///a", 0));
        points.set("Mix", point("file:///b", 0));
        points.set("Gone", point("file:///c", 0));
        points.retain_playlists(&["Mix".to_string()]);
        assert!(points.get("").is_some());
        assert!(points.get("Mix").is_some());
        assert_eq!(points.get("Gone"), None);

        points.remove("Mix");
        assert_eq!(points.get("Mix"), None);
    }
}
