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

use std::collections::HashSet;

use lofty::{ape::ApeItem, id3::v2::FrameId};

use super::*;

const TPUB: FrameId<'static> = FrameId::Valid(Cow::Borrowed("TPUB"));

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
        if let Some(labels) = tag.and_then(|tag| tag.get_texts(&TPUB)) {
            fills.add(ItemKey::Label, labels);
        }
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

#[cfg(test)]
mod tests {
    use lofty::{
        TextEncoding,
        id3::v2::{Frame, TextInformationFrame},
        tag::TagType,
    };

    use super::*;

    // The formats GStreamer plays that lofty can't read, which a survey of
    // the formats found
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
                "lofty now knows .{ext}: give its FileType an arm in read_tagged_file and a \
                 name in format_name, and drop it from UNREAD_EXTENSIONS"
            );
        }
        assert_eq!(
            FileType::from_buffer(&ogg_flac_header()),
            None,
            "lofty now reads Ogg FLAC: give its FileType an arm in read_tagged_file and a \
             name in format_name"
        );
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
