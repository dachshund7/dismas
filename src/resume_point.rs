// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// Where playback was, kept in the resume-point setting, to go on from
// there when the playlist is opened at startup. It's only used when it
// names the playlist being opened and its track is found in it, so a
// stale one is harmless, and it's never reset

use gtk::{gio, prelude::*};
use log::warn;

const KEY: &str = "resume-point";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumePoint {
    // Empty for the queue
    pub playlist: String,
    pub uri: String,
    // In the order the tracks were added, even when shuffled
    pub index: u32,
    // In seconds
    pub position: u32,
}

impl ResumePoint {
    pub fn load(settings: &gio::Settings) -> Self {
        let (playlist, uri, index, position) = settings.get::<(String, String, u32, u32)>(KEY);
        Self {
            playlist,
            uri,
            index,
            position,
        }
    }

    pub fn store(&self, settings: &gio::Settings) {
        if *self == Self::load(settings) {
            return;
        }
        let value = (
            self.playlist.as_str(),
            self.uri.as_str(),
            self.index,
            self.position,
        );
        if let Err(e) = settings.set(KEY, value) {
            warn!("Unable to store the resume point: {e}");
        }
    }

    // The point, if it's in the playlist
    pub fn for_playlist(self, name: &str) -> Option<Self> {
        (self.playlist == name && !self.uri.is_empty()).then_some(self)
    }

    // The point follows the playlist to its new name
    pub fn rename(settings: &gio::Settings, name: &str, new_name: &str) {
        let point = Self::load(settings);
        if point.playlist == name {
            Self {
                playlist: new_name.to_string(),
                ..point
            }
            .store(settings);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn point(uri: &str, index: u32) -> ResumePoint {
        ResumePoint {
            playlist: String::new(),
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
    fn only_applies_to_its_playlist() {
        let point = ResumePoint {
            playlist: "Mix".to_string(),
            ..point("file:///a", 0)
        };
        assert!(point.clone().for_playlist("").is_none());
        assert_eq!(point.clone().for_playlist("Mix"), Some(point));
        assert!(ResumePoint::default().for_playlist("").is_none());
    }
}
