// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading the tags of a file, with the items lofty leaves out

use super::*;

// The Vorbis comments, APE items, ID3v2 frames and MP4 atoms that lofty
// leaves out of the tags of the `TaggedFile`, as it has no `ItemKey` for
// them
#[derive(Debug, Default)]
pub struct ExtraComments {
    // The JSON written by the "Work Hierarchy" plugin of MusicBrainz Picard
    pub(super) work_hierarchy: Option<String>,
    // A CUE sheet that splits the file into tracks
    pub cue_sheet: Option<String>,
}

// The name of the tag of the "Work Hierarchy" plugin; MusicBrainz Picard
// writes the tags it has no standard field for as a Vorbis comment of that
// name in capitals, a "TXXX" frame described by it in MP3 files, and a
// "----:com.apple.iTunes:" atom named after it in MP4 files
const WORK_HIERARCHY: &str = "work_hierarchy";

impl ExtraComments {
    pub(super) fn from_vorbis(comments: Option<&VorbisComments>) -> Self {
        let get = |key| comments.and_then(|c| c.get(key)).map(ToString::to_string);
        ExtraComments {
            work_hierarchy: get("WORK_HIERARCHY"),
            cue_sheet: get("CUESHEET"),
        }
    }

    // Monkey's Audio and WavPack files have their CUE sheet in an APE tag
    pub(super) fn from_ape(tag: Option<&ApeTag>) -> Self {
        let cue_sheet =
            tag.and_then(|tag| tag.get("Cuesheet"))
                .and_then(|item| match item.value() {
                    ItemValue::Text(text) => Some(text.clone()),
                    _ => None,
                });
        ExtraComments {
            cue_sheet,
            ..Default::default()
        }
    }

    pub(super) fn from_id3v2(tag: Option<&Id3v2Tag>) -> Self {
        ExtraComments {
            work_hierarchy: tag
                .and_then(|tag| tag.get_user_text(WORK_HIERARCHY))
                .map(ToString::to_string),
            ..Default::default()
        }
    }

    pub(super) fn from_mp4(ilst: Option<&Ilst>) -> Self {
        let ident = AtomIdent::Freeform {
            mean: "com.apple.iTunes".into(),
            name: WORK_HIERARCHY.into(),
        };
        let work_hierarchy = ilst.and_then(|ilst| ilst.get(&ident)).and_then(|atom| {
            atom.data().find_map(|data| match data {
                AtomData::UTF8(text) => Some(text.clone()),
                _ => None,
            })
        });
        ExtraComments {
            work_hierarchy,
            ..Default::default()
        }
    }
}

// Reads a file, and the items in `ExtraComments` if it has them
pub fn read_tagged_file(
    probe: Probe<BufReader<File>>,
    options: ParseOptions,
) -> Result<(TaggedFile, ExtraComments), FileParseError> {
    fn read<F: AudioFile + Into<TaggedFile>>(
        probe: Probe<BufReader<File>>,
        options: ParseOptions,
        extra: fn(&F) -> ExtraComments,
    ) -> Result<(TaggedFile, ExtraComments), FileParseError> {
        let file = F::read_from(&mut probe.into_inner(), options)?;
        let extra = extra(&file);
        Ok((file.into(), extra))
    }

    match probe.file_type() {
        Some(FileType::Opus) => read(probe, options, |f: &OpusFile| {
            ExtraComments::from_vorbis(Some(f.vorbis_comments()))
        }),
        Some(FileType::Vorbis) => read(probe, options, |f: &VorbisFile| {
            ExtraComments::from_vorbis(Some(f.vorbis_comments()))
        }),
        Some(FileType::Speex) => read(probe, options, |f: &SpeexFile| {
            ExtraComments::from_vorbis(Some(f.vorbis_comments()))
        }),
        Some(FileType::Flac) => read(probe, options, |f: &FlacFile| {
            ExtraComments::from_vorbis(f.vorbis_comments())
        }),
        Some(FileType::Ape) => read(probe, options, |f: &ApeFile| {
            ExtraComments::from_ape(f.ape())
        }),
        Some(FileType::WavPack) => read(probe, options, |f: &WavPackFile| {
            ExtraComments::from_ape(f.ape())
        }),
        Some(FileType::Mpeg) => read(probe, options, |f: &MpegFile| {
            ExtraComments::from_id3v2(f.id3v2())
        }),
        Some(FileType::Mp4) => read(probe, options, |f: &Mp4File| {
            ExtraComments::from_mp4(f.ilst())
        }),
        _ => probe
            .options(options)
            .read()
            .map(|f| (f, ExtraComments::default())),
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
