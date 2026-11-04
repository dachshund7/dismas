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
        fills.add(ItemKey::Performer, performers(tag));
        fills
    }

    fn add(&mut self, key: ItemKey, values: impl IntoIterator<Item = impl Into<String>>) {
        self.0.extend(
            values
                .into_iter()
                .map(|value| TagItem::new(key, ItemValue::Text(value.into()))),
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
    let Ok(read) = utils::open_regular_file(path).and_then(|mut file| file.read(&mut start)) else {
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

    // lofty 0.25.5 reads the frames inside an ID3v2 chapter frame (CHAP) or
    // table of contents (CTOC) as it reads the tag's, so a chapter frame
    // inside another is read inside that one's reading, with no limit on how
    // deep they go. Each level takes tens of kilobytes of stack, so a tag of a
    // few kilobytes, of chapters nested sixty deep, overflows a reader
    // thread's stack. That aborts the whole program rather than panicking, so
    // `utils::guarded` can't catch it, and a file in the music folders would
    // end Dismas at every start. Dismas builds with a fork of lofty that stops
    // at 16 levels (`[patch.crates-io]` in Cargo.toml), where such a tag is an
    // error, and GStreamer reads the file instead; these tests check that
    // the fork is still in use.

    const CHAP: FrameId<'static> = FrameId::Valid(Cow::Borrowed("CHAP"));

    fn synchsafe(size: usize) -> [u8; 4] {
        let size = u32::try_from(size).unwrap();
        [size >> 21, size >> 14, size >> 7, size].map(|byte| (byte & 0x7F) as u8)
    }

    // An ID3v2.4 frame, written by hand: lofty's writer would go through
    // nested chapters the way its reader does
    fn id3v2_frame(id: &[u8; 4], content: &[u8]) -> Vec<u8> {
        let mut frame = id.to_vec();
        frame.extend(synchsafe(content.len()));
        frame.extend([0, 0]);
        frame.extend(content);
        frame
    }

    // What follows the header of a chapter frame that holds another, and so
    // on, `levels` deep: 28 bytes a level
    fn nested_chapters(levels: usize) -> Vec<u8> {
        let mut content = Vec::new();
        for level in 0..levels {
            // Its ID, nul-terminated, then its times and offsets
            let mut chapter = b"c\0".to_vec();
            chapter.extend([0; 16]);
            if level > 0 {
                chapter.extend(id3v2_frame(b"CHAP", &content));
            }
            content = chapter;
        }
        content
    }

    // An MP3 file whose ID3v2.4 tag has a title, then chapters nested
    // `levels` deep, then MPEG-1 Layer III frames at 128 kbps and 44.1 kHz,
    // of 417 bytes each
    fn write_mp3_with_nested_chapters(path: &Path, levels: usize) {
        let mut frames = id3v2_frame(b"TIT2", b"\x03Nimrod");
        frames.extend(id3v2_frame(b"CHAP", &nested_chapters(levels)));
        let mut bytes = b"ID3\x04\0\0".to_vec();
        bytes.extend(synchsafe(frames.len()));
        bytes.extend(frames);
        for _ in 0..40 {
            bytes.extend([0xFF, 0xFB, 0x90, 0x64]);
            bytes.extend([0; 413]);
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn lofty_stops_at_chapters_nested_too_deep() {
        use lofty::id3::v2::{ChapterFrame, FrameFlags, Id3v2Version};

        // How deep a chapter frame's chapters go once lofty has read it, or
        // None if it refused it
        fn depth(levels: usize) -> Option<usize> {
            let content = nested_chapters(levels);
            // On a stack big enough for lofty without the fork, which a
            // debug build uses much more of, so that a missing fork fails
            // the test rather than aborting them all. The chapters are
            // dropped there too, as that goes as deep
            std::thread::Builder::new()
                .stack_size(256 << 20)
                .spawn(move || {
                    let chapter = ChapterFrame::parse(
                        &mut &content[..],
                        ParseOptions::new(),
                        Id3v2Version::V4,
                        FrameFlags::default(),
                    )
                    .ok()?;
                    let mut depth = 1;
                    let mut frames = &chapter.children;
                    while let Some(Frame::Chapter(inner)) = frames.get(&CHAP) {
                        depth += 1;
                        frames = &inner.children;
                    }
                    Some(depth)
                })
                .unwrap()
                .join()
                .unwrap()
        }

        // Real files nest one or two
        assert_eq!(depth(3), Some(3));
        assert_eq!(
            depth(200),
            None,
            "lofty reads chapters nested 200 deep: Cargo.toml no longer patches it with the \
             fork that limits them, so a crafted file overflows the stack"
        );
    }

    #[test]
    fn a_few_nested_chapters_are_read_by_lofty() {
        let dir = std::env::temp_dir().join(format!("dismas-chapters-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Nimrod.mp3");
        write_mp3_with_nested_chapters(&path, 3);
        let read = read_file_tags(&path, ReadOptions::SCAN);
        let _ = std::fs::remove_dir_all(&dir);
        let read = read.unwrap();
        // GStreamer would name the format "MPEG-1 Layer 3 (MP3)"
        assert_eq!(read.format.as_deref(), Some("MP3"));
        assert_eq!(read.tag.title().as_deref(), Some("Nimrod"));
    }

    // The variable that names the file `read_deeply_nested_chapters` reads
    const NESTED_CHAPTERS_FILE: &str = "DISMAS_NESTED_CHAPTERS";

    // Reading a file whose chapters would overflow a reader thread's stack
    // without the fork, in a process of its own, as an overflow ends the
    // process: the test binary started again to run only
    // `read_deeply_nested_chapters`
    #[test]
    fn deeply_nested_chapters_dont_crash_the_reader() {
        let dir = std::env::temp_dir().join(format!("dismas-deep-chapters-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Nimrod.mp3");
        write_mp3_with_nested_chapters(&path, 1000);

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "audio::track::lofty_gaps::tests::read_deeply_nested_chapters",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(NESTED_CHAPTERS_FILE, &path)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{}: {stdout}{stderr}",
            output.status
        );
        // A filter that matches no test succeeds too
        assert!(stdout.contains("test result: ok. 1 passed"), "{stderr}");
    }

    #[test]
    #[ignore = "deeply_nested_chapters_dont_crash_the_reader runs it, in a process of its own"]
    fn read_deeply_nested_chapters() {
        let Some(path) = std::env::var_os(NESTED_CHAPTERS_FILE).map(PathBuf::from) else {
            return;
        };
        // On a thread with the stack of a reader thread, Rust's default
        let read = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                let tags = read_file_tags(&path, ReadOptions::SCAN)?;
                Some((tags.format?.into_owned(), tags.tag.title()?.into_owned()))
            })
            .unwrap()
            .join()
            .unwrap();
        // lofty refused the tag, and GStreamer read the file
        assert_eq!(
            read,
            Some(("MPEG-1 Layer 3 (MP3)".to_string(), "Nimrod".to_string()))
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
