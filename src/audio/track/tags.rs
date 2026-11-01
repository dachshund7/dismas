// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading the tags of a file, with the items lofty leaves out

use std::{borrow::Cow, collections::HashSet};

use lofty::{
    id3::v2::{Frame, SyncTextContentType, SynchronizedTextFrame, TimestampFormat},
    properties::FileProperties,
    tag::TagType,
};

use super::*;

// The Vorbis comments, APE items, ID3v2 frames and MP4 atoms that lofty
// leaves out of the tags of the `TaggedFile`, as it has no `ItemKey` for
// them
#[derive(Debug, Default)]
pub struct ExtraComments {
    // The JSON written by the "Work Hierarchy" plugin of MusicBrainz Picard
    pub(super) work_hierarchy: Option<String>,
    // When the track was recorded, which the same plugin writes from
    // MusicBrainz, e.g. "1937-11-08/1938-03-08"
    pub(super) recording_date: Option<String>,
    // A CUE sheet that splits the file into tracks
    pub cue_sheet: Option<String>,
    // The gains of an Opus file, as its format gives them (RFC 7845), in
    // place of ReplayGain tags, e.g. "-1536"; see `ReplayGain::r128`
    pub(super) r128_track_gain: Option<String>,
    pub(super) r128_album_gain: Option<String>,
    // The synchronized lyrics of an ID3v2 SYLT frame: the time of each
    // entry and its text, as the frame has them, which can be a line or a
    // word. Only read when asked, see `ReadOptions::lyrics`
    pub synced_lyrics: Option<Vec<(Duration, String)>>,
}

// The name of the tag of the "Work Hierarchy" plugin; MusicBrainz Picard
// writes the tags it has no standard field for as a Vorbis comment of that
// name in capitals, an APE item of that name capitalized ("Work_Hierarchy")
// in Monkey's Audio, WavPack and Musepack files, a "TXXX" frame described
// by it in MP3, AIFF, WAV and AAC files, and a "----:com.apple.iTunes:" atom
// named after it in MP4 files
const WORK_HIERARCHY: &str = "work_hierarchy";

// The name of the tag with the recording date, written the same way
const RECORDING_DATE: &str = "recordingdate";

// The synchronized lyrics of an ID3v2 tag, from a SYLT frame whose times
// are in milliseconds, as those in MPEG frames would need the frame rate.
// A frame of lyrics or of a transcription comes before the others, as
// another can hold the chords, say
fn synced_lyrics(tag: &Id3v2Tag) -> Option<Vec<(Duration, String)>> {
    let frames: Vec<SynchronizedTextFrame> = tag
        .into_iter()
        .filter_map(|frame| match frame {
            Frame::Binary(binary) if binary.id().as_str() == "SYLT" => {
                SynchronizedTextFrame::parse(&binary.data, binary.flags()).ok()
            }
            _ => None,
        })
        .filter(|frame| frame.timestamp_format == TimestampFormat::MS)
        .collect();
    let frame = frames
        .iter()
        .find(|frame| {
            matches!(
                frame.content_type,
                SyncTextContentType::Lyrics | SyncTextContentType::TextTranscription
            )
        })
        .or(frames.first())?;
    let entries = frame
        .content
        .iter()
        .map(|(ms, text)| (Duration::from_millis(u64::from(*ms)), text.clone()))
        .collect();
    Some(entries)
}

impl ExtraComments {
    // The items of a file with a "Work Hierarchy" tag, for the tests of
    // other modules
    #[cfg(test)]
    pub fn with_work_hierarchy(json: &str) -> Self {
        ExtraComments {
            work_hierarchy: Some(json.to_string()),
            ..Default::default()
        }
    }

    // The items of a file with a recording date, for the tests of other
    // modules
    #[cfg(test)]
    pub fn with_recording_date(date: &str) -> Self {
        ExtraComments {
            recording_date: Some(date.to_string()),
            ..Default::default()
        }
    }

    pub(super) fn from_vorbis(comments: Option<&VorbisComments>) -> Self {
        let get = |key| comments.and_then(|c| c.get(key)).map(ToString::to_string);
        ExtraComments {
            work_hierarchy: get("WORK_HIERARCHY"),
            recording_date: get("RECORDINGDATE"),
            cue_sheet: get("CUESHEET"),
            r128_track_gain: get("R128_TRACK_GAIN"),
            r128_album_gain: get("R128_ALBUM_GAIN"),
            synced_lyrics: None,
        }
    }

    // Monkey's Audio, WavPack and Musepack files have an APE tag, whose
    // keys are compared ignoring case
    pub(super) fn from_ape(tag: Option<&ApeTag>) -> Self {
        let get = |key| {
            tag.and_then(|tag| tag.get(key))
                .and_then(|item| match item.value() {
                    ItemValue::Text(text) => Some(text.clone()),
                    _ => None,
                })
        };
        // R128 gains are only for Opus files, which have Vorbis comments
        ExtraComments {
            work_hierarchy: get(WORK_HIERARCHY),
            recording_date: get(RECORDING_DATE),
            cue_sheet: get("Cuesheet"),
            ..Default::default()
        }
    }

    // The descriptions of "TXXX" frames are compared as they are, and other
    // taggers write them in capitals
    pub(super) fn from_id3v2(tag: Option<&Id3v2Tag>, lyrics: bool) -> Self {
        let get = |name: &str| {
            let tag = tag?;
            tag.get_user_text(name)
                .or_else(|| tag.get_user_text(&name.to_uppercase()))
                .map(ToString::to_string)
        };
        ExtraComments {
            work_hierarchy: get(WORK_HIERARCHY),
            recording_date: get(RECORDING_DATE),
            synced_lyrics: tag.filter(|_| lyrics).and_then(synced_lyrics),
            ..Default::default()
        }
    }

    pub(super) fn from_mp4(ilst: Option<&Ilst>) -> Self {
        let get = |name: &str| {
            [name.to_string(), name.to_uppercase()]
                .into_iter()
                .find_map(|name| {
                    let ident = AtomIdent::Freeform {
                        mean: "com.apple.iTunes".into(),
                        name: name.into(),
                    };
                    ilst?.get(&ident)?.data().find_map(|data| match data {
                        AtomData::UTF8(text) => Some(text.clone()),
                        _ => None,
                    })
                })
        };
        ExtraComments {
            work_hierarchy: get(WORK_HIERARCHY),
            recording_date: get(RECORDING_DATE),
            ..Default::default()
        }
    }
}

// What lofty leaves out of a file's tag: the items it has no `ItemKey` for,
// and those it has one for but doesn't read
struct LeftOut(ExtraComments, Fills);

impl LeftOut {
    fn vorbis(comments: Option<&VorbisComments>) -> Self {
        Self(
            ExtraComments::from_vorbis(comments),
            Fills::from_vorbis(comments),
        )
    }

    fn ape(tag: Option<&ApeTag>) -> Self {
        Self(ExtraComments::from_ape(tag), Fills::from_ape(tag))
    }

    fn id3v2(tag: Option<&Id3v2Tag>, lyrics: bool) -> Self {
        Self(
            ExtraComments::from_id3v2(tag, lyrics),
            Fills::from_id3v2(tag),
        )
    }

    fn mp4(ilst: Option<&Ilst>) -> Self {
        Self(ExtraComments::from_mp4(ilst), Fills::default())
    }
}

// What to read of a file besides its tags
#[derive(Debug, Clone, Copy)]
pub struct ReadOptions {
    pub covers: bool,
    pub properties: bool,
    // The synchronized lyrics of ID3v2 tags, which only the lyrics of the
    // playing track need
    pub lyrics: bool,
}

impl ReadOptions {
    pub const ALL: Self = Self {
        covers: true,
        properties: true,
        lyrics: false,
    };
    // Without the covers and the properties, which the library doesn't keep
    pub const TAGS: Self = Self {
        covers: false,
        properties: false,
        lyrics: false,
    };
    // The tags with the synchronized lyrics
    pub const LYRICS: Self = Self {
        covers: false,
        properties: false,
        lyrics: true,
    };

    // Each of lofty's setters returns a copy, so they're chained on a value
    fn parse_options(self) -> ParseOptions {
        ParseOptions::new()
            .read_cover_art(self.covers)
            .read_properties(self.properties)
    }
}

// What the tracks of a file take from it, whatever its format
pub struct FileTags {
    // The tags of the file, the same in the queue, the album grid and the
    // covers: its primary tag, or else its other tags together, as one can
    // fill in for another, or else an empty tag
    pub tag: Tag,
    // The items lofty leaves out of the tag
    pub extra: ExtraComments,
    pub properties: FileProperties,
    pub format: Option<Cow<'static, str>>,
    // Whether the file has a video stream too, which only GStreamer tells
    pub video: bool,
}

impl FileTags {
    // The tags are taken out of the file rather than copied, as they hold
    // the covers
    fn new(mut file: TaggedFile, LeftOut(extra, fills): LeftOut) -> Self {
        let primary = file.primary_tag_type();
        let mut tag = file.remove(primary).unwrap_or_else(|| {
            let types: Vec<TagType> = file.tags().iter().map(Tag::tag_type).collect();
            let tags = types.into_iter().filter_map(|t| file.remove(t)).collect();
            merged_tag(tags).unwrap_or_else(|| Tag::new(primary))
        });
        fills.apply(&mut tag);
        Self {
            tag,
            extra,
            properties: file.properties().clone(),
            format: format_name(file.file_type()).map(Cow::Borrowed),
            video: false,
        }
    }
}

impl FileTags {
    // The tags of a file that lofty can't read, as Vorbis comments, read
    // like those of an Ogg Vorbis file
    pub(super) fn from_comments(
        comments: VorbisComments,
        properties: FileProperties,
        format: Option<Cow<'static, str>>,
    ) -> Self {
        let LeftOut(extra, fills) = LeftOut::vorbis(Some(&comments));
        let mut tag = Tag::from(comments);
        fills.apply(&mut tag);
        Self {
            tag,
            extra,
            properties,
            format,
            video: false,
        }
    }
}

// Opens a file and reads its tags, and the items in `ExtraComments` if it
// has them; the options can leave out the covers and the properties. A
// file that lofty can't read, GStreamer may
pub fn read_file_tags(path: &Path, options: ReadOptions) -> Option<FileTags> {
    let parse_options = options.parse_options();
    let probe = match Probe::open(path).map(|probe| probe.options(parse_options).guess_file_type())
    {
        Ok(Ok(probe)) => probe,
        Ok(Err(e)) => {
            warn!("Unable to parse file type {path:?}: {e}");
            return None;
        }
        Err(e) => {
            warn!("Unable to open file {path:?}: {e}");
            return None;
        }
    };
    if lofty_gaps::guessed_from_junk(path, probe.file_type())
        && let Some(tags) = discovered::discover(path, options)
    {
        debug!("Read {path:?} with GStreamer, as lofty only guessed it was MPEG audio");
        return Some(tags);
    }
    match read_tagged_file(probe, options) {
        Ok((file, left_out)) => Some(FileTags::new(file, left_out)),
        Err(e) => {
            let tags = discovered::discover(path, options);
            match tags {
                Some(_) => debug!("Read {path:?} with GStreamer, as lofty couldn't: {e}"),
                None => warn!("Unable to read file {path:?}: {e}"),
            }
            tags
        }
    }
}

// The tags of a file without a primary tag together, the first one's items
// and pictures over the next ones'
fn merged_tag(tags: Vec<Tag>) -> Option<Tag> {
    let mut tags = tags.into_iter();
    let mut merged = tags.next()?;
    for mut tag in tags {
        let keys: HashSet<ItemKey> = merged.items().map(TagItem::key).collect();
        // The items of another format, whose keys the first one may not
        // have, are kept as they are, to be read
        for item in tag.items().filter(|item| !keys.contains(&item.key())) {
            merged.push_unchecked(item.clone());
        }
        if merged.pictures().is_empty() {
            while !tag.pictures().is_empty() {
                merged.push_picture(tag.remove_picture(0));
            }
        }
    }
    Some(merged)
}

// Reads a file, and what lofty leaves out of its tag
fn read_tagged_file(
    probe: Probe<BufReader<File>>,
    read_options: ReadOptions,
) -> Result<(TaggedFile, LeftOut), FileParseError> {
    fn read<F: AudioFile + Into<TaggedFile>>(
        probe: Probe<BufReader<File>>,
        options: ParseOptions,
        left_out: impl Fn(&F) -> LeftOut,
    ) -> Result<(TaggedFile, LeftOut), FileParseError> {
        let file = F::read_from(&mut probe.into_inner(), options)?;
        let left_out = left_out(&file);
        Ok((file.into(), left_out))
    }

    let options = read_options.parse_options();
    let lyrics = read_options.lyrics;
    match probe.file_type() {
        Some(FileType::Opus) => read(probe, options, |f: &OpusFile| {
            LeftOut::vorbis(Some(f.vorbis_comments()))
        }),
        Some(FileType::Vorbis) => read(probe, options, |f: &VorbisFile| {
            LeftOut::vorbis(Some(f.vorbis_comments()))
        }),
        Some(FileType::Speex) => read(probe, options, |f: &SpeexFile| {
            LeftOut::vorbis(Some(f.vorbis_comments()))
        }),
        Some(FileType::Flac) => read(probe, options, |f: &FlacFile| {
            LeftOut::vorbis(f.vorbis_comments())
        }),
        Some(FileType::Ape) => read(probe, options, |f: &ApeFile| LeftOut::ape(f.ape())),
        Some(FileType::WavPack) => read(probe, options, |f: &WavPackFile| LeftOut::ape(f.ape())),
        Some(FileType::Mpc) => read(probe, options, |f: &MpcFile| LeftOut::ape(f.ape())),
        Some(FileType::Mpeg) => read(probe, options, |f: &MpegFile| {
            LeftOut::id3v2(f.id3v2(), lyrics)
        }),
        Some(FileType::Aiff) => read(probe, options, |f: &AiffFile| {
            LeftOut::id3v2(f.id3v2(), lyrics)
        }),
        Some(FileType::Wav) => read(probe, options, |f: &WavFile| {
            LeftOut::id3v2(f.id3v2(), lyrics)
        }),
        Some(FileType::Aac) => read(probe, options, |f: &AacFile| {
            LeftOut::id3v2(f.id3v2(), lyrics)
        }),
        Some(FileType::Mp4) => read(probe, options, |f: &Mp4File| LeftOut::mp4(f.ilst())),
        _ => probe.options(options).read().map(|f| {
            let left_out = LeftOut(ExtraComments::default(), Fills::default());
            (f, left_out)
        }),
    }
}

// The artists of a track, if the tags name any
pub(super) fn track_artist(tag: &Tag) -> Option<String> {
    let artists = tag.get_strings(ItemKey::TrackArtist).join(", ");
    (!artists.is_empty()).then(|| plain_commas(&artists))
}

// The title in a tag, unless it's blank
pub fn track_title(tag: &Tag) -> Option<String> {
    tag.title()
        .filter(|title| !title.trim().is_empty())
        .map(|title| title.to_string())
}

// The album in a tag, unless it's blank
pub(super) fn track_album(tag: &Tag) -> Option<String> {
    tag.album()
        .filter(|album| !album.trim().is_empty())
        .map(|album| album.to_string())
}

// Replaces the full-width commas in a name with plain ones: tracks
// downloaded from YouTube separate their artists with commas, and use
// full-width ones for the commas within a name, e.g. "Orquesta del Teatro
// Real， Madrid", which look like large gaps
pub fn plain_commas(name: &str) -> String {
    name.replace('，', ",")
}

#[cfg(test)]
mod tests {
    use lofty::{picture::Picture, tag::TagType};

    use super::*;

    fn tag(tag_type: TagType, items: &[(ItemKey, &str)]) -> Tag {
        let mut tag = Tag::new(tag_type);
        for (key, value) in items {
            tag.push_unchecked(TagItem::new(*key, ItemValue::Text(value.to_string())));
        }
        tag
    }

    #[test]
    fn the_tags_of_a_file_without_a_primary_one_fill_in_for_each_other() {
        // An MP3 file with ID3v1 and APE tags, but no ID3v2 tag
        let id3v1 = tag(
            TagType::Id3v1,
            &[
                (ItemKey::TrackTitle, "Short"),
                (ItemKey::TrackArtist, "Artist"),
            ],
        );
        let mut ape = tag(
            TagType::Ape,
            &[
                (ItemKey::TrackTitle, "Long"),
                (ItemKey::AlbumTitle, "Album"),
                (ItemKey::TrackArtist, "Other"),
                (ItemKey::TrackArtist, "Another"),
                (ItemKey::MusicBrainzReleaseId, "id"),
            ],
        );
        ape.push_picture(Picture::unchecked(vec![1, 2, 3]).build());

        let merged = merged_tag(vec![id3v1.clone(), ape]).unwrap();
        assert_eq!(merged.title().as_deref(), Some("Short"));
        assert_eq!(
            merged.get_strings(ItemKey::TrackArtist).collect::<Vec<_>>(),
            ["Artist"]
        );
        assert_eq!(merged.album().as_deref(), Some("Album"));
        // ID3v1 has no MusicBrainz ids, but the APE tag's is kept
        assert_eq!(merged.get_string(ItemKey::MusicBrainzReleaseId), Some("id"));
        assert_eq!(merged.pictures().len(), 1);

        // A single tag is read as it is
        let single = merged_tag(vec![id3v1]).unwrap();
        assert_eq!(single.title().as_deref(), Some("Short"));
        assert_eq!(single.album(), None);
        assert!(merged_tag(Vec::new()).is_none());
    }

    #[test]
    fn a_file_with_a_primary_tag_is_read_from_it_alone() {
        // A WAV file, whose primary tag is its ID3v2 chunk
        let dir = std::env::temp_dir().join(format!("dismas-tags-primary-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Track.wav");
        crate::utils::write_test_wav(&path, 1);
        let mut id3v2 = Id3v2Tag::new();
        id3v2.set_title("Primary".to_string());
        crate::utils::write_test_id3v2(&path, &id3v2, false);

        let read = read_file_tags(&path, ReadOptions::ALL);
        let _ = std::fs::remove_dir_all(&dir);
        let read = read.unwrap();
        assert_eq!(read.tag.tag_type(), TagType::Id3v2);
        assert_eq!(read.tag.title().as_deref(), Some("Primary"));
        assert_eq!(read.format.as_deref(), Some("WAV"));
        assert_eq!(read.properties.duration().as_secs(), 1);
    }

    #[test]
    fn work_hierarchy_is_read_from_wav_files() {
        let dir = std::env::temp_dir().join(format!("dismas-tags-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Track.wav");
        crate::utils::write_test_wav(&path, 1);
        // As MusicBrainz Picard writes it, in the file's ID3v2 chunk
        let json = r#"{"version":1,"segments":[]}"#;
        let mut id3v2 = Id3v2Tag::new();
        let _ = id3v2.insert_user_text(WORK_HIERARCHY.to_string(), json.to_string());
        crate::utils::write_test_id3v2(&path, &id3v2, false);

        let read = read_file_tags(&path, ReadOptions::ALL);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read.unwrap().extra.work_hierarchy.as_deref(), Some(json));
    }

    #[test]
    fn performers_are_read_from_id3v2_tags_of_both_versions() {
        use lofty::id3::v2::{Frame, FrameId, KeyValueFrame};

        let credits = |id: &'static str, pairs: &[(&str, &str)]| {
            let pairs: Vec<(Cow<str>, Cow<str>)> = pairs
                .iter()
                .map(|(role, name)| (role.to_string().into(), name.to_string().into()))
                .collect();
            Frame::KeyValue(KeyValueFrame::new(
                FrameId::Valid(Cow::Borrowed(id)),
                lofty::TextEncoding::UTF8,
                pairs,
            ))
        };
        // As MusicBrainz Picard writes them in ID3v2.4; in ID3v2.3, which
        // has no TMCL, both go in one IPLS frame, which is read as TIPL
        let mut id3v2 = Id3v2Tag::new();
        id3v2.insert(credits(
            "TMCL",
            &[("cello", "Jacqueline du Pré"), ("performer", "Janet Baker")],
        ));
        id3v2.insert(credits("TIPL", &[("producer", "Suvi Raj Grubb")]));

        let dir = std::env::temp_dir().join(format!("dismas-tags-tmcl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let read = [false, true].map(|id3v23| {
            let path = dir.join(format!("Track{}.wav", u8::from(id3v23)));
            crate::utils::write_test_wav(&path, 1);
            crate::utils::write_test_id3v2(&path, &id3v2, id3v23);
            read_file_tags(&path, ReadOptions::ALL)
        });
        let _ = std::fs::remove_dir_all(&dir);

        for read in read {
            let read = read.unwrap();
            let credits = TrackCredits::from_tag(&read.tag, &read.extra);
            let mut performers: Vec<(&str, Option<&str>)> = credits
                .performers
                .iter()
                .map(|c| (c.name.as_str(), c.role.as_deref()))
                .collect();
            performers.sort();
            assert_eq!(
                performers,
                [("Jacqueline du Pré", Some("cello")), ("Janet Baker", None)]
            );
            assert_eq!(credits.producers, ["Suvi Raj Grubb"]);
        }
    }

    // A raw AAC file, a few ADTS frames of AAC LC at 44.1 kHz in stereo,
    // each a 7-byte header and 10 bytes of nothing
    fn write_test_aac(path: &Path) {
        let frame = [0xFF, 0xF1, 0x50, 0x80, 0x02, 0x3F, 0xFC];
        let mut bytes = Vec::new();
        for _ in 0..100 {
            bytes.extend(frame);
            bytes.extend([0; 10]);
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn id3v2_tags_give_their_labels_and_aac_files_their_extra_items() {
        let dir = std::env::temp_dir().join(format!("dismas-tags-label-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("Track.wav");
        crate::utils::write_test_wav(&wav, 1);
        let aac = dir.join("Track.aac");
        write_test_aac(&aac);
        // As MusicBrainz Picard writes them
        let mut id3v2 = Id3v2Tag::new();
        id3v2.set_title("Enigma Variations".to_string());
        id3v2.insert(lofty::id3::v2::Frame::Text(
            lofty::id3::v2::TextInformationFrame::new(
                lofty::id3::v2::FrameId::Valid(Cow::Borrowed("TPUB")),
                lofty::TextEncoding::UTF8,
                "EMI".to_string(),
            ),
        ));
        let _ = id3v2.insert_user_text(RECORDING_DATE.to_string(), "1962-05-25".to_string());
        crate::utils::write_test_id3v2(&wav, &id3v2, false);
        crate::utils::write_test_id3v2(&aac, &id3v2, false);

        let read = [&wav, &aac].map(|path| read_file_tags(path, ReadOptions::ALL));
        let _ = std::fs::remove_dir_all(&dir);
        for (read, format) in read.into_iter().zip(["WAV", "AAC"]) {
            let read = read.unwrap();
            assert_eq!(read.format.as_deref(), Some(format));
            assert_eq!(read.tag.title().as_deref(), Some("Enigma Variations"));
            assert_eq!(read.tag.get_string(ItemKey::Label), Some("EMI"));
            assert_eq!(read.extra.recording_date.as_deref(), Some("1962-05-25"));
        }
    }

    // Times reading every audio file under DISMAS_SCAN_DIR without the
    // properties, as the library scan does, and with them, as a search for
    // the length of tracks would need. For each format, it gives the time
    // per file in one round with each file dropped from the page cache
    // first, and in two with it cached, and counts the files that fail
    // only with the properties:
    //   DISMAS_SCAN_DIR=~/Music cargo test time_tag_reads -- --ignored --nocapture
    #[test]
    #[ignore]
    fn time_tag_reads() {
        use std::{
            collections::BTreeMap,
            os::fd::AsRawFd,
            path::PathBuf,
            time::{Duration, Instant},
        };

        // So that the next read of the file comes from the disk
        fn drop_from_cache(path: &Path) {
            if let Ok(file) = File::open(path) {
                // SAFETY: the descriptor is open for the whole call
                unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
            }
        }

        fn timed(path: &Path, options: ReadOptions) -> (Duration, bool) {
            let start = Instant::now();
            let read = read_file_tags(path, options).is_some();
            (start.elapsed(), read)
        }

        #[derive(Default)]
        struct Times {
            files: u32,
            // Without the properties, then with them
            cold: [Duration; 2],
            warm: [Duration; 2],
            unread: [u32; 2],
            unread_only_with_properties: u32,
        }

        const OPTIONS: [ReadOptions; 2] = [
            ReadOptions::TAGS,
            ReadOptions {
                properties: true,
                ..ReadOptions::TAGS
            },
        ];
        const WARM_ROUNDS: u32 = 2;

        let root = std::env::var_os("DISMAS_SCAN_DIR").expect("DISMAS_SCAN_DIR is not set");
        let mut files = Vec::new();
        let mut dirs = vec![PathBuf::from(root)];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if !crate::library::is_library_entry(&path) {
                    continue;
                }
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => dirs.push(path),
                    Ok(kind) if kind.is_file() && crate::utils::is_audio_path(&path) => {
                        files.push(path);
                    }
                    _ => (),
                }
            }
        }
        files.sort();

        let format = |path: &Path| {
            path.extension().map_or_else(
                || "(none)".to_string(),
                |extension| extension.to_string_lossy().to_lowercase(),
            )
        };
        let mut times: BTreeMap<String, Times> = BTreeMap::new();
        for path in &files {
            let times = times.entry(format(path)).or_default();
            times.files += 1;
            let mut read = [false; 2];
            for (i, options) in OPTIONS.into_iter().enumerate() {
                drop_from_cache(path);
                let (elapsed, ok) = timed(path, options);
                times.cold[i] += elapsed;
                read[i] = ok;
                times.unread[i] += u32::from(!ok);
            }
            times.unread_only_with_properties += u32::from(read[0] && !read[1]);
        }
        // Each round starts with the other read, so that neither always
        // has the other's pages
        for round in 0..WARM_ROUNDS as usize {
            for path in &files {
                let times = times.get_mut(&format(path)).unwrap();
                for i in [round % 2, (round + 1) % 2] {
                    times.warm[i] += timed(path, OPTIONS[i]).0;
                }
            }
        }

        let per_file = |total: Duration, files: u32| total.as_secs_f64() * 1e6 / f64::from(files);
        println!(
            "{:<8} {:>7} {:>11} {:>11} {:>11} {:>11} {:>11} {:>11} {:>7} {:>7} {:>9}",
            "format",
            "files",
            "cold µs",
            "+props µs",
            "extra µs",
            "warm µs",
            "+props µs",
            "extra µs",
            "unread",
            "+props",
            "only+prop"
        );
        let mut all = Times::default();
        let rows = times.iter().map(|(format, times)| (format.as_str(), times));
        for (format, times) in rows {
            let warm = times.warm.map(|total| total / WARM_ROUNDS);
            println!(
                "{format:<8} {:>7} {:>11.0} {:>11.0} {:>11.0} {:>11.0} {:>11.0} {:>11.0} {:>7} {:>7} {:>9}",
                times.files,
                per_file(times.cold[0], times.files),
                per_file(times.cold[1], times.files),
                per_file(times.cold[1].saturating_sub(times.cold[0]), times.files),
                per_file(warm[0], times.files),
                per_file(warm[1], times.files),
                per_file(warm[1].saturating_sub(warm[0]), times.files),
                times.unread[0],
                times.unread[1],
                times.unread_only_with_properties,
            );
            all.files += times.files;
            for (i, warm) in warm.into_iter().enumerate() {
                all.cold[i] += times.cold[i];
                all.warm[i] += warm;
                all.unread[i] += times.unread[i];
            }
            all.unread_only_with_properties += times.unread_only_with_properties;
        }
        println!(
            "all: {} files; cold {:.1} s, {:.1} s with the properties; warm {:.1} s, {:.1} s; \
             {} unread, {} with the properties, {} only with them",
            all.files,
            all.cold[0].as_secs_f64(),
            all.cold[1].as_secs_f64(),
            all.warm[0].as_secs_f64(),
            all.warm[1].as_secs_f64(),
            all.unread[0],
            all.unread[1],
            all.unread_only_with_properties,
        );
    }
}
