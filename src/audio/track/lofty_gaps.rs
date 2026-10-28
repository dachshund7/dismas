// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! What lofty reads wrongly or leaves out of a file's tag, where it has an
//! `ItemKey` for it; the tags it has no key for are in `ExtraComments`.
//!
//! Each fill adds the items to the tag under the key lofty uses for the
//! other formats, so the code that reads the tag never knows. It only adds
//! the keys the tag has none of, so a version of lofty that reads one
//! itself makes its fill do nothing rather than add it twice. Each gap has
//! a canary test, `still_…`, which fails once lofty fills the gap and says
//! what to delete: after upgrading lofty, `cargo test lofty_gaps` lists
//! what it now does itself.

use std::{collections::HashSet, io::Read};

use lofty::{
    ape::ApeItem,
    id3::v2::{Frame, FrameId},
    tag::SplitTag,
};

use super::*;

const TPUB: FrameId<'static> = FrameId::Valid(Cow::Borrowed("TPUB"));
const TMCL: FrameId<'static> = FrameId::Valid(Cow::Borrowed("TMCL"));
const TIPL: FrameId<'static> = FrameId::Valid(Cow::Borrowed("TIPL"));

// The items lofty left out of a file's tag, to add to it
#[derive(Debug, Default)]
pub(super) struct Fills(Vec<TagItem>);

impl Fills {
    // lofty 0.25.4 maps COMPOSERSORT in neither Vorbis comments nor APE
    // tags, though it maps the ID3v2 frame TSOC and the MP4 atom "soco" to
    // `ComposerSortOrder`
    pub(super) fn from_vorbis(comments: Option<&VorbisComments>) -> Self {
        let mut fills = Self::default();
        if let Some(comments) = comments {
            fills.add(ItemKey::ComposerSortOrder, comments.get_all("COMPOSERSORT"));
        }
        fills
    }

    // APE keys are compared ignoring case, and an item's values are
    // separated by nul characters
    pub(super) fn from_ape(tag: Option<&ApeTag>) -> Self {
        let mut fills = Self::default();
        if let Some(ItemValue::Text(text)) =
            tag.and_then(|t| t.get("ComposerSort")).map(ApeItem::value)
        {
            fills.add(ItemKey::ComposerSortOrder, text.split('\0'));
        }
        fills
    }

    // lofty 0.25.4 reads TPUB as `Publisher` only: it's the ID3v2 frame for
    // both `Publisher` and `Label`, and lofty takes the first key a frame
    // has. MusicBrainz Picard writes the record label there
    pub(super) fn from_id3v2(tag: Option<&Id3v2Tag>) -> Self {
        let mut fills = Self::default();
        let Some(tag) = tag else {
            return fills;
        };
        if let Some(labels) = tag.get_texts(&TPUB) {
            fills.add(ItemKey::Label, labels);
        }
        let performers = performers(tag);
        fills.add(ItemKey::Performer, performers.iter().map(String::as_str));
        fills
    }

    fn add<'a>(&mut self, key: ItemKey, values: impl IntoIterator<Item = &'a str>) {
        self.0.extend(
            values
                .into_iter()
                .map(|value| TagItem::new(key, ItemValue::Text(value.to_string()))),
        );
    }

    // Adds the items whose keys the tag has none of
    pub(super) fn apply(self, tag: &mut Tag) {
        let present: HashSet<ItemKey> = self
            .0
            .iter()
            .map(TagItem::key)
            .filter(|key| tag.get(*key).is_some())
            .collect();
        for item in self.0 {
            if !present.contains(&item.key()) {
                tag.push_unchecked(item);
            }
        }
    }
}

// lofty 0.25.4 takes a file whose start it doesn't know, like an Ogg FLAC
// file, for MPEG or AAC audio after some junk, as soon as it finds what looks
// like a frame sync in its first kilobyte. When the properties aren't read,
// as in the library's scans, nothing checks that guess, and the file is read
// as one without tags. Such a guess is the only one that comes from neither
// the start of the file nor what follows an ID3v2 tag there
pub(super) fn guessed_from_junk(path: &Path, file_type: Option<FileType>) -> bool {
    if !matches!(file_type, Some(FileType::Mpeg | FileType::Aac)) {
        return false;
    }
    let mut start = [0; 36];
    let Ok(read) = File::open(path).and_then(|mut file| file.read(&mut start)) else {
        return false;
    };
    let start = &start[..read];
    FileType::from_buffer(start).is_none() && !start.starts_with(b"ID3")
}

// lofty 0.25.4 leaves the performers of an ID3v2 tag out of the tag: it
// doesn't read TMCL, the musicians' credits, at all, and of TIPL, the people
// involved, it reads only the roles it has a key for, like "producer".
// MusicBrainz Picard writes each performer with their instrument or voice
// as the role, in TMCL, or in TIPL in ID3v2.3, which has no TMCL. The pairs
// are those lofty's own split of the tag leaves, so a role it learns to
// read leaves this fill by itself. They're written as in Picard's Vorbis
// comments, "Name (role)", or the name alone for the role "performer" or
// none, as Picard reads them
fn performers(tag: &Id3v2Tag) -> Vec<String> {
    let mut credits = Id3v2Tag::new();
    for id in [&TMCL, &TIPL] {
        if let Some(frame) = tag.get(id) {
            credits.insert(frame.clone());
        }
    }
    let (left, _) = credits.split_tag();

    let mut performers = Vec::new();
    for id in [&TMCL, &TIPL] {
        let Some(Frame::KeyValue(frame)) = left.get(id) else {
            continue;
        };
        for (role, name) in frame.key_value_pairs.iter() {
            let (role, name) = (role.trim(), name.trim());
            if name.is_empty() {
                continue;
            }
            performers.push(if role.is_empty() || role == "performer" {
                name.to_string()
            } else {
                format!("{name} ({role})")
            });
        }
    }
    performers
}

#[cfg(test)]
mod tests {
    use lofty::{
        TextEncoding,
        id3::v2::{KeyValueFrame, TextInformationFrame},
        tag::TagType,
    };

    use super::*;

    // The formats GStreamer plays that lofty can't read, which a survey of
    // the formats found, and which `discovered` reads with GStreamer
    const UNREAD_EXTENSIONS: [&str; 7] = ["oga", "mka", "wma", "tta", "ac3", "dts", "au"];

    // The start of an Ogg FLAC file: an Ogg page whose first packet is the
    // FLAC mapping's header, then the FLAC stream's own
    fn ogg_flac_header() -> Vec<u8> {
        let mut bytes = b"OggS\0\x02".to_vec();
        // Granule position, serial number, sequence number and checksum
        bytes.extend([0; 20]);
        // One segment of 51 bytes
        bytes.extend([1, 51]);
        bytes.extend(b"\x7fFLAC\x01\x00\x00\x01fLaC");
        bytes.resize(28 + 51, 0);
        bytes
    }

    #[test]
    fn still_unread_file_types() {
        // `FileType` is `#[non_exhaustive]`, so a type that lofty learns
        // would otherwise go to the `_` arm of `read_tagged_file` without
        // a word, and lose the items in `ExtraComments`
        for ext in UNREAD_EXTENSIONS {
            assert_eq!(
                FileType::from_ext(ext),
                None,
                "lofty now knows .{ext}, whose files it reads rather than GStreamer: give its \
                 FileType an arm in read_tagged_file and a name in format_name, and drop it \
                 from UNREAD_EXTENSIONS"
            );
        }
        assert_eq!(
            FileType::from_buffer(&ogg_flac_header()),
            None,
            "lofty now reads Ogg FLAC, rather than GStreamer: give its FileType an arm in \
             read_tagged_file and a name in format_name"
        );
    }

    // Some junk, then the header of an MP3 frame
    fn junk_then_frame_sync() -> Vec<u8> {
        let mut bytes = b"JUNK".repeat(16);
        bytes.extend([0xFF, 0xFB, 0x90, 0x64]);
        bytes.resize(2048, 0);
        bytes
    }

    #[test]
    fn still_takes_junk_for_mpeg_audio() {
        let probe = lofty::probe::Probe::new(std::io::Cursor::new(junk_then_frame_sync()))
            .guess_file_type()
            .unwrap();
        assert_eq!(
            probe.file_type(),
            Some(FileType::Mpeg),
            "lofty no longer takes a file it doesn't know for MPEG audio after junk: delete \
             guessed_from_junk and its use in read_file_tags"
        );
    }

    #[test]
    fn only_a_guess_from_junk_is_one() {
        let dir = std::env::temp_dir().join(format!("dismas-junk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let junk = dir.join("junk");
        std::fs::write(&junk, junk_then_frame_sync()).unwrap();
        let frame = dir.join("frame");
        std::fs::write(&frame, &junk_then_frame_sync()[64..]).unwrap();
        let id3 = dir.join("id3");
        let mut bytes = b"ID3\x04\0\0\0\0\0\0".to_vec();
        bytes.extend(&junk_then_frame_sync()[64..]);
        std::fs::write(&id3, bytes).unwrap();

        let guessed = |path: &Path, file_type| guessed_from_junk(path, Some(file_type));
        let results = [
            guessed(&junk, FileType::Mpeg),
            guessed(&junk, FileType::Aac),
            guessed(&junk, FileType::Flac),
            guessed(&frame, FileType::Mpeg),
            guessed(&id3, FileType::Mpeg),
        ];
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(results, [true, true, false, false, false]);
    }

    #[test]
    fn still_no_composer_sort_in_vorbis_comments_or_ape_tags() {
        let mut comments = VorbisComments::default();
        comments.push("COMPOSERSORT".to_string(), "Elgar, Edward".to_string());
        assert!(
            Tag::from(comments)
                .get(ItemKey::ComposerSortOrder)
                .is_none(),
            "lofty now maps COMPOSERSORT in Vorbis comments: delete Fills::from_vorbis"
        );

        let mut ape = ApeTag::new();
        ape.insert(
            ApeItem::new(
                "ComposerSort".to_string(),
                ItemValue::Text("Elgar, Edward".to_string()),
            )
            .unwrap(),
        );
        assert!(
            Tag::from(ape).get(ItemKey::ComposerSortOrder).is_none(),
            "lofty now maps ComposerSort in APE tags: delete Fills::from_ape"
        );
    }

    fn id3v2_with_labels(labels: &str) -> Id3v2Tag {
        let mut id3v2 = Id3v2Tag::new();
        id3v2.insert(Frame::Text(TextInformationFrame::new(
            TPUB,
            TextEncoding::UTF8,
            labels.to_string(),
        )));
        id3v2
    }

    #[test]
    fn still_no_label_from_tpub() {
        assert!(
            Tag::from(id3v2_with_labels("EMI"))
                .get(ItemKey::Label)
                .is_none(),
            "lofty now reads TPUB as Label: delete the TPUB fill in Fills::from_id3v2"
        );
    }

    #[test]
    fn labels_are_filled_in_from_tpub() {
        // Two labels, as ID3v2.4 separates them
        let id3v2 = id3v2_with_labels("EMI\0His Master's Voice");
        let fills = Fills::from_id3v2(Some(&id3v2));
        let mut tag = Tag::from(id3v2);
        fills.apply(&mut tag);
        assert_eq!(
            tag.get_strings(ItemKey::Label).collect::<Vec<_>>(),
            ["EMI", "His Master's Voice"]
        );
        // lofty's own reading is kept
        assert_eq!(tag.get_string(ItemKey::Publisher), Some("EMI"));
    }

    // A TMCL or TIPL frame, of roles and names
    fn credits_frame(id: FrameId<'static>, pairs: &[(&str, &str)]) -> Frame<'static> {
        let pairs: Vec<(Cow<str>, Cow<str>)> = pairs
            .iter()
            .map(|(role, name)| (role.to_string().into(), name.to_string().into()))
            .collect();
        Frame::KeyValue(KeyValueFrame::new(id, TextEncoding::UTF8, pairs))
    }

    #[test]
    fn still_no_performers_from_tmcl_or_tipl() {
        let mut tmcl = Id3v2Tag::new();
        tmcl.insert(credits_frame(TMCL, &[("cello", "Jacqueline du Pré")]));
        assert!(
            Tag::from(tmcl).get(ItemKey::Performer).is_none(),
            "lofty now reads TMCL as Performer: performers() only needs TIPL's roles"
        );
        let mut tipl = Id3v2Tag::new();
        tipl.insert(credits_frame(TIPL, &[("cello", "Jacqueline du Pré")]));
        assert!(
            Tag::from(tipl).get(ItemKey::Performer).is_none(),
            "lofty now reads TIPL's other roles as Performer: performers() only needs TMCL"
        );
    }

    #[test]
    fn performers_are_filled_in_from_tmcl_and_the_roles_left_in_tipl() {
        let mut id3v2 = Id3v2Tag::new();
        id3v2.insert(credits_frame(
            TMCL,
            &[("cello", "Jacqueline du Pré"), ("performer", "Janet Baker")],
        ));
        id3v2.insert(credits_frame(
            TIPL,
            &[
                ("producer", "Suvi Raj Grubb"),
                ("orchestra", "London Symphony Orchestra"),
            ],
        ));
        let fills = Fills::from_id3v2(Some(&id3v2));
        let mut tag = Tag::from(id3v2);
        fills.apply(&mut tag);
        assert_eq!(
            tag.get_strings(ItemKey::Performer).collect::<Vec<_>>(),
            [
                "Jacqueline du Pré (cello)",
                "Janet Baker",
                "London Symphony Orchestra (orchestra)"
            ]
        );
        // The producer is lofty's, and isn't a performer too
        assert_eq!(
            tag.get_strings(ItemKey::Producer).collect::<Vec<_>>(),
            ["Suvi Raj Grubb"]
        );
    }

    #[test]
    fn composer_sort_names_are_filled_in() {
        let mut comments = VorbisComments::default();
        comments.push("COMPOSER".to_string(), "Edward Elgar".to_string());
        comments.push("COMPOSER".to_string(), "Hubert Parry".to_string());
        comments.push("COMPOSERSORT".to_string(), "Elgar, Edward".to_string());
        comments.push("COMPOSERSORT".to_string(), "Parry, Hubert".to_string());
        let fills = Fills::from_vorbis(Some(&comments));
        let mut tag = Tag::from(comments);
        fills.apply(&mut tag);
        assert_eq!(
            tag.get_strings(ItemKey::ComposerSortOrder)
                .collect::<Vec<_>>(),
            ["Elgar, Edward", "Parry, Hubert"]
        );

        let mut ape = ApeTag::new();
        ape.insert(
            ApeItem::new(
                "Composersort".to_string(),
                ItemValue::Text("Elgar, Edward\0Parry, Hubert".to_string()),
            )
            .unwrap(),
        );
        let mut tag = Tag::new(TagType::Ape);
        Fills::from_ape(Some(&ape)).apply(&mut tag);
        assert_eq!(
            tag.get_strings(ItemKey::ComposerSortOrder)
                .collect::<Vec<_>>(),
            ["Elgar, Edward", "Parry, Hubert"]
        );
    }

    #[test]
    fn a_fill_never_adds_to_a_key_the_tag_has() {
        // As in a version of lofty that maps COMPOSERSORT itself
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.push_unchecked(TagItem::new(
            ItemKey::ComposerSortOrder,
            ItemValue::Text("Elgar, Edward".to_string()),
        ));
        let mut comments = VorbisComments::default();
        comments.push("COMPOSERSORT".to_string(), "Elgar, Edward".to_string());
        Fills::from_vorbis(Some(&comments)).apply(&mut tag);
        assert_eq!(tag.get_strings(ItemKey::ComposerSortOrder).count(), 1);
    }
}
