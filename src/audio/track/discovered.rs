// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading the files lofty can't, like Ogg FLAC, Matroska and WMA files,
//! with GStreamer's Discoverer.
//!
//! GStreamer parses them in C, and a file that crashed one of its plugins
//! would take Dismas down with it, at every start until the file was moved,
//! as the library is read at every start. So each file is read in a child
//! process, Dismas itself started with `--discover-file`, which writes what
//! it found as JSON: a crash or a hang only leaves that one file unread.
//! GApplication would pass the arguments on to the Dismas already running,
//! so `main` looks for the child's before it makes the application.

use std::{
    ffi::OsString,
    io::Write,
    process::{Command, Stdio},
};

use glib::translate::IntoGlib;
use gst_pbutils::{
    Discoverer, DiscovererContainerInfo, DiscovererInfo, DiscovererResult, prelude::*,
};
use lofty::{
    picture::{MimeType, Picture, PictureType},
    properties::FileProperties,
};
use serde::Serialize;

use super::*;

// The argument that starts Dismas as the child that reads a file
pub const CHILD_ARG: &str = "--discover-file";

// How long Discoverer waits for a file
const DISCOVERER_SECONDS: u64 = 5;
// How long the child lives at most, so the kernel ends it if it hangs,
// even in Discoverer
const CHILD_SECONDS: u32 = 15;

// The program started as the child, Dismas itself, which `main` sets
static HELPER: OnceLock<PathBuf> = OnceLock::new();

pub fn set_helper(program: PathBuf) {
    let _ = HELPER.set(program);
}

// What the child found in a file
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct Discovered {
    // The tags as Vorbis comments, keys and values, so that they're read
    // like those of an Ogg Vorbis file
    comments: Vec<(String, String)>,
    pictures: Vec<DiscoveredPicture>,
    duration_ms: u64,
    // In kbps
    bitrate: Option<u32>,
    sample_rate: Option<u32>,
    bit_depth: Option<u8>,
    channels: Option<u8>,
    format: Option<String>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct DiscoveredPicture {
    // Its type as in ID3v2, e.g. 3 for the front cover
    kind: u8,
    mime_type: Option<String>,
    // In base64
    data: String,
}

impl Discovered {
    fn into_file_tags(self) -> FileTags {
        let mut comments = VorbisComments::default();
        for (key, value) in self.comments {
            comments.push(key, value);
        }
        let properties = FileProperties::new(
            Duration::from_millis(self.duration_ms),
            self.bitrate,
            self.bitrate,
            self.sample_rate,
            self.bit_depth,
            self.channels,
            None,
        );
        let mut tags = FileTags::from_comments(comments, properties, self.format.map(Cow::Owned));
        for picture in self.pictures {
            let mut builder = Picture::unchecked(glib::base64_decode(&picture.data))
                .pic_type(PictureType::from_u8(picture.kind));
            if let Some(mime_type) = picture.mime_type {
                builder = builder.mime_type(MimeType::from_str(&mime_type));
            }
            tags.tag.push_picture(builder.build());
        }
        tags
    }
}

// Reads a file that lofty can't, in the child process
pub(super) fn discover(path: &Path, options: ReadOptions) -> Option<FileTags> {
    let discovered = match HELPER.get() {
        Some(helper) => run_child(helper, path, options.covers)?,
        // The tests aren't Dismas, so they read the file in their own
        // process
        None if cfg!(test) => discover_file(path, options.covers)?,
        None => return None,
    };
    Some(discovered.into_file_tags())
}

fn run_child(helper: &Path, path: &Path, covers: bool) -> Option<Discovered> {
    let output = Command::new(helper)
        .arg(CHILD_ARG)
        .arg(if covers { "covers" } else { "no-covers" })
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output();
    match output {
        Ok(output) if output.status.success() => match serde_json::from_slice(&output.stdout) {
            Ok(discovered) => discovered,
            Err(e) => {
                warn!("Unable to read what GStreamer found in {path:?}: {e}");
                None
            }
        },
        Ok(output) => {
            warn!(
                "GStreamer couldn't read {path:?}: its reader ended with {}",
                output.status
            );
            None
        }
        Err(e) => {
            warn!("Unable to start the reader of {path:?}: {e}");
            None
        }
    }
}

// The child: reads a file, and writes what it found as JSON, or null
pub fn child_main(args: &[OsString]) -> glib::ExitCode {
    // SAFETY: alarm() only sets a timer, whose signal ends the process
    unsafe {
        libc::alarm(CHILD_SECONDS);
    }
    let [covers, path] = args else {
        return glib::ExitCode::FAILURE;
    };
    let discovered = discover_file(Path::new(path), covers.to_str() == Some("covers"));
    let mut stdout = std::io::stdout().lock();
    match serde_json::to_writer(&mut stdout, &discovered) {
        Ok(()) if stdout.flush().is_ok() => glib::ExitCode::SUCCESS,
        _ => glib::ExitCode::FAILURE,
    }
}

// What GStreamer finds in a file: nothing unless it has an audio stream
// that can be decoded, as the file couldn't play otherwise
fn discover_file(path: &Path, covers: bool) -> Option<Discovered> {
    gst::init().ok()?;
    let uri = glib::filename_to_uri(path, None).ok()?;
    let discoverer = Discoverer::new(gst::ClockTime::from_seconds(DISCOVERER_SECONDS)).ok()?;
    let info = match discoverer.discover_uri(&uri) {
        Ok(info) if info.result() == DiscovererResult::Ok => info,
        Ok(info) => {
            debug!("GStreamer can't read {path:?}: {:?}", info.result());
            return None;
        }
        Err(e) => {
            debug!("GStreamer can't read {path:?}: {e}");
            return None;
        }
    };
    let audio = info.audio_streams().into_iter().next()?;
    let caps = audio.caps()?;
    if !can_decode(&caps) {
        debug!("No decoder for the {caps} of {path:?}");
        return None;
    }

    // The container's tags, then the stream's for those it doesn't have:
    // the stream's title can be the name of the track in the container,
    // like "Audio" in Matroska files
    let mut tags = info
        .stream_info()
        .and_then(|stream| stream.downcast::<DiscovererContainerInfo>().ok())
        .and_then(|container| container.tags())
        .unwrap_or_default();
    if let Some(stream_tags) = audio.tags() {
        tags = tags.merge(&stream_tags, gst::TagMergeMode::Keep);
    }

    let duration_ms = info.duration().map_or(0, gst::ClockTime::mseconds);
    let size_bitrate = || {
        let size = std::fs::metadata(path).ok()?.len();
        // Bits a millisecond are kbps
        u32::try_from(size * 8 / duration_ms.max(1)).ok()
    };
    let bitrate = [audio.bitrate(), audio.max_bitrate()]
        .into_iter()
        .find(|b| *b > 0)
        .map(|b| b / 1000)
        .or_else(size_bitrate);
    let (comments, pictures) = comments(&tags, covers);
    Some(Discovered {
        comments,
        pictures,
        duration_ms,
        bitrate,
        sample_rate: Some(audio.sample_rate()).filter(|r| *r > 0),
        bit_depth: u8::try_from(audio.depth()).ok().filter(|d| *d > 0),
        channels: u8::try_from(audio.channels()).ok().filter(|c| *c > 0),
        format: format_name(&info, &caps),
    })
}

// Whether a stream can be decoded: Discoverer doesn't need a decoder, as it
// only parses the file
fn can_decode(caps: &gst::CapsRef) -> bool {
    if caps.structure(0).is_some_and(|s| s.name() == "audio/x-raw") {
        return true;
    }
    gst::ElementFactory::factories_with_type(
        gst::ElementFactoryType::DECODER | gst::ElementFactoryType::MEDIA_AUDIO,
        gst::Rank::MARGINAL,
    )
    .iter()
    .any(|factory| factory.can_sink_any_caps(caps))
}

// The tags as Vorbis comments, under the keys GStreamer gives them, and the
// pictures
fn comments(
    tags: &gst::TagListRef,
    covers: bool,
) -> (Vec<(String, String)>, Vec<DiscoveredPicture>) {
    let mut comments = Vec::new();
    let mut pictures = Vec::new();
    // The date can be there twice, as a date and as a date and time
    let has_date_time = tags.get::<gst::tags::DateTime>().is_some();
    for (name, values) in tags.iter_generic() {
        match name.as_str() {
            "image" => {
                if covers {
                    pictures.extend(values.filter_map(|v| picture(&v.get().ok()?)));
                }
            }
            // The comments GStreamer has no tag for, as "KEY=value", like
            // the Work Hierarchy plugin's
            "extended-comment" => {
                for value in values.filter_map(|v| v.get::<&str>().ok()) {
                    if let Ok(comment) = gst_tag::tag_parse_extended_comment(value, true)
                        && let Some(key) = comment.key
                    {
                        comments.push((key.to_string(), comment.value.to_string()));
                    }
                }
            }
            "date" if has_date_time => (),
            _ => {
                let Some(key) = gst_tag::tag::to_vorbis_tag(name) else {
                    continue;
                };
                for value in values.filter_map(|v| comment_value(name, v)) {
                    comments.push((key.to_string(), value));
                }
            }
        }
    }
    (comments, pictures)
}

// A tag's value as a Vorbis comment's
fn comment_value(name: &str, value: &glib::SendValue) -> Option<String> {
    if let Ok(text) = value.get::<&str>() {
        Some(text.to_string())
    } else if let Ok(number) = value.get::<u32>() {
        Some(number.to_string())
    } else if let Ok(number) = value.get::<f64>() {
        // As ReplayGain's gains are written, e.g. "-6.20 dB"
        Some(if name.ends_with("-gain") {
            format!("{number:.2} dB")
        } else {
            number.to_string()
        })
    } else if let Ok(date) = value.get::<gst::DateTime>() {
        date.to_iso8601_string().ok().map(Into::into)
    } else if let Ok(date) = value.get::<glib::Date>() {
        Some(format!(
            "{:04}-{:02}-{:02}",
            date.year(),
            date.month().into_glib(),
            date.day()
        ))
    } else {
        None
    }
}

fn picture(sample: &gst::Sample) -> Option<DiscoveredPicture> {
    let buffer = sample.buffer()?.map_readable().ok()?;
    let mime_type = sample
        .caps()
        .and_then(|caps| caps.structure(0))
        .map(|s| s.name().to_string());
    let image_type = sample
        .info()
        .and_then(|info| info.get::<gst_tag::TagImageType>("image-type").ok())
        .map_or(0, IntoGlib::into_glib);
    // GStreamer's types are ID3v2's without its two icons, with -1 for none
    let kind = u8::try_from(image_type + 2)
        .ok()
        .filter(|_| image_type > 0)
        .unwrap_or(0);
    Some(DiscoveredPicture {
        kind,
        mime_type,
        data: glib::base64_encode(&buffer).to_string(),
    })
}

// The name of the format: the codec's, after the container's when it isn't
// the codec's own
fn format_name(info: &DiscovererInfo, caps: &gst::CapsRef) -> Option<String> {
    let codec = match caps.structure(0)?.name().as_str() {
        "audio/x-flac" => "FLAC".to_string(),
        "audio/x-opus" => "Opus".to_string(),
        "audio/x-vorbis" => "Vorbis".to_string(),
        "audio/x-wma" => "WMA".to_string(),
        "audio/x-tta" => "TrueAudio".to_string(),
        "audio/x-ac3" | "audio/ac3" => "AC-3".to_string(),
        "audio/x-eac3" => "E-AC-3".to_string(),
        "audio/x-dts" => "DTS".to_string(),
        "audio/x-alac" => "ALAC".to_string(),
        "audio/x-raw" | "audio/x-alaw" | "audio/x-mulaw" => "PCM".to_string(),
        _ => gst_pbutils::pb_utils_get_codec_description(caps).to_string(),
    };
    let container = info
        .stream_info()
        .and_then(|stream| stream.caps())
        .and_then(|caps| caps.structure(0).map(|s| s.name().to_string()));
    Some(match container.as_deref() {
        Some("audio/ogg" | "application/ogg") => format!("Ogg {codec}"),
        Some("video/x-matroska" | "audio/x-matroska") => format!("{codec} in Matroska"),
        Some("video/webm" | "audio/webm") => format!("{codec} in WebM"),
        Some("audio/x-au") => "AU".to_string(),
        _ => codec,
    })
}

#[cfg(test)]
mod tests {
    use gst_tag::prelude::*;

    use super::*;

    // A PNG of one pixel
    const PNG: [u8; 67] = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    // Writes a second of sound to a file through `encoder`, with the tags
    // set on the element named "tags"; false if an element is missing
    fn write_with_gstreamer(path: &Path, encoder: &str, tags: &gst::TagList) -> bool {
        gst::init().unwrap();
        let description = format!(
            "audiotestsrc num-buffers=10 samplesperbuffer=4410 ! audioconvert ! {encoder} \
             ! filesink location=\"{}\"",
            path.display()
        );
        let Ok(pipeline) = gst::parse::launch(&description) else {
            return false;
        };
        let pipeline = pipeline.downcast::<gst::Pipeline>().unwrap();
        let setter = pipeline.by_name("tags").unwrap();
        setter
            .dynamic_cast_ref::<gst::TagSetter>()
            .unwrap()
            .merge_tags(tags, gst::TagMergeMode::Replace);
        pipeline.set_state(gst::State::Playing).unwrap();
        let done = pipeline.bus().unwrap().timed_pop_filtered(
            gst::ClockTime::from_seconds(10),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        pipeline.set_state(gst::State::Null).unwrap();
        done.is_some_and(|message| message.type_() == gst::MessageType::Eos)
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dismas-discovered-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn what_the_child_finds_goes_to_the_parent_as_it_is() {
        let discovered = Discovered {
            comments: vec![("TITLE".to_string(), "Nimrod".to_string())],
            pictures: vec![DiscoveredPicture {
                kind: 3,
                mime_type: Some("image/png".to_string()),
                data: glib::base64_encode(&PNG).to_string(),
            }],
            duration_ms: 1000,
            bitrate: Some(320),
            sample_rate: Some(44100),
            bit_depth: Some(16),
            channels: Some(2),
            format: Some("Ogg FLAC".to_string()),
        };
        let json = serde_json::to_vec(&Some(&discovered)).unwrap();
        let read: Option<Discovered> = serde_json::from_slice(&json).unwrap();
        assert_eq!(read.as_ref(), Some(&discovered));
        let none: Option<Discovered> = serde_json::from_slice(b"null").unwrap();
        assert_eq!(none, None);

        let tags = discovered.into_file_tags();
        assert_eq!(tags.tag.title().as_deref(), Some("Nimrod"));
        assert_eq!(tags.tag.pictures()[0].pic_type(), PictureType::CoverFront);
        assert_eq!(&tags.tag.pictures()[0].data()[..4], &PNG[..4]);
        assert_eq!(tags.properties.audio_bitrate(), Some(320));
        assert_eq!(tags.format.as_deref(), Some("Ogg FLAC"));
    }

    #[test]
    fn a_child_that_fails_only_leaves_its_file_unread() {
        // A child that crashes, is killed or ends with an error has no
        // success, like false; one that writes nothing, like true, wrote
        // nothing that can be read
        let path = Path::new("/nonexistent.mka");
        assert_eq!(run_child(Path::new("/bin/false"), path, true), None);
        assert_eq!(run_child(Path::new("/bin/true"), path, true), None);
        assert_eq!(run_child(Path::new("/nonexistent"), path, true), None);
    }

    #[test]
    fn pictures_keep_their_types() {
        gst::init().unwrap();
        let kind = |image_type| {
            let sample = gst::Sample::from_image_data(&PNG, image_type).unwrap();
            let picture = picture(&sample).unwrap();
            assert_eq!(picture.mime_type.as_deref(), Some("image/png"));
            PictureType::from_u8(picture.kind)
        };
        assert_eq!(
            kind(gst_tag::TagImageType::FrontCover),
            PictureType::CoverFront
        );
        assert_eq!(
            kind(gst_tag::TagImageType::BackCover),
            PictureType::CoverBack
        );
        assert_eq!(
            kind(gst_tag::TagImageType::BandArtistLogo),
            PictureType::BandLogo
        );
        assert_eq!(kind(gst_tag::TagImageType::Undefined), PictureType::Other);
        assert_eq!(kind(gst_tag::TagImageType::None), PictureType::Other);
    }

    #[test]
    fn ogg_flac_files_are_read_with_gstreamer() {
        gst::init().unwrap();
        let dir = test_dir("oga");
        let path = dir.join("Nimrod.oga");
        let json = r#"{"version":1,"segments":[]}"#;
        let mut tags = gst::TagList::new();
        {
            let tags = tags.get_mut().unwrap();
            tags.add::<gst::tags::Title>(&"Nimrod", gst::TagMergeMode::Append);
            tags.add::<gst::tags::Album>(&"Enigma Variations", gst::TagMergeMode::Append);
            tags.add::<gst::tags::Composer>(&"Edward Elgar", gst::TagMergeMode::Append);
            tags.add::<gst::tags::TrackGain>(&-6.2, gst::TagMergeMode::Append);
            let comment = format!("WORK_HIERARCHY={json}");
            tags.add::<gst::tags::ExtendedComment>(&comment.as_str(), gst::TagMergeMode::Append);
            let cover =
                gst::Sample::from_image_data(&PNG, gst_tag::TagImageType::FrontCover).unwrap();
            tags.add::<gst::tags::Image>(&cover, gst::TagMergeMode::Append);
        }
        // flacenc and oggmux come with the good and base plugins
        if !write_with_gstreamer(&path, "flacenc name=tags ! oggmux", &tags) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let read = read_file_tags(&path, ReadOptions::ALL);
        let without_covers = read_file_tags(&path, ReadOptions::TAGS);
        let _ = std::fs::remove_dir_all(&dir);
        let read = read.unwrap();
        assert_eq!(read.format.as_deref(), Some("Ogg FLAC"));
        assert_eq!(read.tag.title().as_deref(), Some("Nimrod"));
        assert_eq!(read.tag.album().as_deref(), Some("Enigma Variations"));
        assert_eq!(read.tag.get_string(ItemKey::Composer), Some("Edward Elgar"));
        assert_eq!(
            read.tag.get_string(ItemKey::ReplayGainTrackGain),
            Some("-6.20 dB")
        );
        assert_eq!(read.extra.work_hierarchy.as_deref(), Some(json));
        assert_eq!(read.properties.duration().as_secs_f64().round(), 1.0);
        assert_eq!(read.properties.sample_rate(), Some(44100));
        // flacenc writes the type of a picture two places off, so only the
        // picture is compared
        assert_eq!(read.tag.pictures().len(), 1);
        assert_eq!(read.tag.pictures()[0].data(), PNG);
        assert!(without_covers.unwrap().tag.pictures().is_empty());
    }

    #[test]
    fn matroska_files_are_read_with_gstreamer() {
        gst::init().unwrap();
        let dir = test_dir("mka");
        let path = dir.join("Nimrod.mka");
        let mut tags = gst::TagList::new();
        {
            let tags = tags.get_mut().unwrap();
            tags.add::<gst::tags::Title>(&"Nimrod", gst::TagMergeMode::Append);
            tags.add::<gst::tags::Album>(&"Enigma Variations", gst::TagMergeMode::Append);
        }
        // matroskamux comes with the good plugins
        if !write_with_gstreamer(&path, "flacenc ! matroskamux name=tags", &tags) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let read = read_file_tags(&path, ReadOptions::ALL);
        let _ = std::fs::remove_dir_all(&dir);
        let read = read.unwrap();
        assert_eq!(read.format.as_deref(), Some("FLAC in Matroska"));
        assert_eq!(read.tag.title().as_deref(), Some("Nimrod"));
        assert_eq!(read.tag.album().as_deref(), Some("Enigma Variations"));
        assert_eq!(read.properties.duration().as_secs_f64().round(), 1.0);
    }
}
