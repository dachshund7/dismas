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
//! so `main` looks for the child's before it makes the application. The
//! child confines itself before it starts GStreamer (`crate::sandbox`), so a
//! file that exploits a plugin can't reach anything else either.

use std::{
    collections::HashSet,
    ffi::OsString,
    io::{Read, Write},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
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
use crate::sandbox::{self, EXIT_NO_SANDBOX};

// The argument that starts Dismas as the child that reads a file
pub(crate) const CHILD_ARG: &str = "--discover-file";

// How long Discoverer waits for a file
const DISCOVERER_SECONDS: u64 = 5;
// How long the child lives at most, so the kernel ends it if it hangs,
// even in Discoverer
const CHILD_SECONDS: u32 = 15;
// How long the parent lets the child run before it ends it: a little longer
// than the child's alarm, which a file that took over the child could block
const READER_TIME: Duration = Duration::from_secs(CHILD_SECONDS as u64 + 5);

// The most bytes of pictures the child sends, as many as a cover file next
// to the tracks that's read: a crafted file can have dozens of pictures of
// 16 MiB, which the parent kept all of, twice over, as it read them
const MAX_PICTURES_SIZE: usize = 32 * 1024 * 1024;

// The most bytes the parent reads from the child: its pictures, a third
// larger in base64, and the tags; a child that writes more is ended, and
// the file is left unread
const MAX_REPORT_SIZE: u64 = 64 * 1024 * 1024;

// The most characters of why a child couldn't set up its sandbox that the
// parent logs
const MAX_REASON_CHARS: usize = 500;

// The program started as the child, Dismas itself, which `main` sets
static HELPER: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn set_helper(program: PathBuf) {
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
    // Whether the file has a video stream too, as a WebM file can
    video: bool,
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
        tags.video = self.video;
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
    run_child_for(helper, path, covers, READER_TIME)
}

// Reads a file in a child that runs for `time` at most
fn run_child_for(helper: &Path, path: &Path, covers: bool, time: Duration) -> Option<Discovered> {
    #[cfg(not(target_os = "linux"))]
    {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| {
            warn!("Files that only GStreamer can read are read without a sandbox on this system");
        });
    }
    let spawned = Command::new(helper)
        .arg(CHILD_ARG)
        .arg(if covers { "covers" } else { "no-covers" })
        .arg(path)
        // Dismas brought GStreamer's registry up to date as it started, and
        // the child, which can't write it, reads it as it is: it neither
        // looks at every plugin again nor starts GStreamer's plugin scanner,
        // which it couldn't. Without a registry, GStreamer reads the plugins
        // in the child, which is slower but works
        .env("GST_REGISTRY_UPDATE", "no")
        .env("GST_REGISTRY_FORK", "no")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            warn!("Unable to start the reader of {path:?}: {e}");
            return None;
        }
    };
    let watchdog = Watchdog::start(&child, time);
    let mut report = Vec::new();
    let read = child.stdout.take().is_some_and(|stdout| {
        stdout
            .take(MAX_REPORT_SIZE + 1)
            .read_to_end(&mut report)
            .is_ok()
    });
    let too_much = report.len() as u64 > MAX_REPORT_SIZE;
    if too_much || !read {
        // It would go on writing to a pipe that's no longer read
        let _ = child.kill();
    }
    let timed_out = watchdog.stop();
    let status = child.wait();
    if timed_out {
        warn!("GStreamer's reader of {path:?} took too long, and was ended");
        return None;
    }
    if too_much {
        warn!("GStreamer found more in {path:?} than is read");
        return None;
    }
    match status {
        Ok(status) if status.success() && read => match serde_json::from_slice(&report) {
            Ok(discovered) => discovered,
            Err(e) => {
                warn!("Unable to read what GStreamer found in {path:?}: {e}");
                None
            }
        },
        Ok(status) if status.code() == Some(EXIT_NO_SANDBOX.into()) => {
            without_sandbox(path, &report);
            None
        }
        // A child that made a system call the sandbox ends it for, like one
        // to run a program, ends with SIGSYS
        Ok(status) => {
            warn!("GStreamer couldn't read {path:?}: its reader ended with {status}");
            None
        }
        Err(e) => {
            warn!("Unable to wait for the reader of {path:?}: {e}");
            None
        }
    }
}

// Ends a child that runs for longer than it's given, whatever it does.
// Discoverer's timeout and the child's alarm end a hang, but a file that
// took over the child could block the alarm's signal and keep the child
// running, and the parent would wait on it for ever, at every start
struct Watchdog {
    pid: u32,
    stop: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<bool>,
}

impl Watchdog {
    fn start(child: &std::process::Child, time: Duration) -> Self {
        let pid = child.id();
        let (stop, stopped) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let late =
                stopped.recv_timeout(time) == Err(std::sync::mpsc::RecvTimeoutError::Timeout);
            if late {
                // SAFETY: kill() takes no pointers. The child isn't reaped
                // before the watchdog stops, so its pid can't have been given
                // to another process yet
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
            }
            late
        });
        Watchdog { pid, stop, thread }
    }

    // Waits for the child to end, without reaping it, so that its pid stays
    // its own while the watchdog could still end it, then stops the
    // watchdog; true if the watchdog ended the child
    fn stop(self) -> bool {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        loop {
            // SAFETY: waitid only writes `info`, which is large enough
            let waited = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid as libc::id_t,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if waited == 0
                || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                break;
            }
        }
        let _ = self.stop.send(());
        self.thread.join().unwrap_or(false)
    }
}

// Says that a file was left unread, as the child couldn't set up its
// sandbox: once a run, with why, which the child wrote in place of its
// report, as every other child will fail in the same way
fn without_sandbox(path: &Path, report: &[u8]) {
    static SAID: AtomicBool = AtomicBool::new(false);
    if SAID.swap(true, Ordering::Relaxed) {
        debug!("Left {path:?} unread, as GStreamer's reader has no sandbox");
        return;
    }
    let reason: String = String::from_utf8_lossy(report)
        .chars()
        .take(MAX_REASON_CHARS)
        .collect();
    warn!(
        "The sandbox of the reader that reads files with GStreamer can't be set up on this \
         system, so the files that only GStreamer can read, such as {path:?}, are left unread: \
         {reason}"
    );
}

// The child: reads a file, and writes what it found as JSON, or null
pub(crate) fn child_main(args: &[OsString]) -> glib::ExitCode {
    // SAFETY: signal() puts back the default of SIGALRM, which ends the
    // process, in case Dismas was started with it ignored, and alarm()
    // only sets a timer, whose signal then ends the process
    unsafe {
        libc::signal(libc::SIGALRM, libc::SIG_DFL);
        libc::alarm(CHILD_SECONDS);
    }
    let [covers, path] = args else {
        return glib::ExitCode::FAILURE;
    };
    let path = Path::new(path);
    // GStreamer's initialization has GLib read the program's name from
    // /proc/self/cmdline, which the sandbox doesn't let the child read
    glib::set_prgname(Some("dismas"));
    // Before GStreamer starts, and with it the threads the sandbox wouldn't
    // cover; a child that can't confine itself doesn't read the file
    match sandbox::restrict_to_reading(path) {
        Ok(status) => debug!("Reading {path:?} in a sandbox: {status}"),
        Err(reason) => {
            let mut stdout = std::io::stdout().lock();
            let _ = write!(stdout, "{reason}").and_then(|()| stdout.flush());
            return EXIT_NO_SANDBOX.into();
        }
    }
    let discovered = discover_file(path, covers.to_str() == Some("covers"));
    let mut stdout = std::io::stdout().lock();
    match serde_json::to_writer(&mut stdout, &discovered) {
        Ok(()) if stdout.flush().is_ok() => glib::ExitCode::SUCCESS,
        _ => glib::ExitCode::FAILURE,
    }
}

// What GStreamer finds in a file: nothing unless it has an audio stream
// that can be decoded, as the file couldn't play otherwise. A video, as a
// WebM file can have, is left out when it plays
fn discover_file(path: &Path, covers: bool) -> Option<Discovered> {
    crate::audio::init_gstreamer().ok()?;
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
    let video = !info.video_streams().is_empty();
    // The size of a file with a video counts the video too
    let size_bitrate = || {
        if video {
            return None;
        }
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
        // A lossy codec's depth is only that of what its decoder gives,
        // like the 32 bits of a WMA decoder's floats, and lofty gives none
        // for MP3, Opus or Vorbis files
        bit_depth: u8::try_from(audio.depth())
            .ok()
            .filter(|d| *d > 0 && is_lossless(&caps)),
        channels: u8::try_from(audio.channels()).ok().filter(|c| *c > 0),
        format: format_name(&info, &caps),
        video,
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

// Whether a codec keeps every bit of the sound, so that its depth means
// something
fn is_lossless(caps: &gst::CapsRef) -> bool {
    let Some(structure) = caps.structure(0) else {
        return false;
    };
    match structure.name().as_str() {
        "audio/x-raw" | "audio/x-flac" | "audio/x-alac" | "audio/x-tta" | "audio/x-wavpack"
        | "audio/x-true-hd" => true,
        "audio/x-wma" => wma_version(structure) == Some(4),
        _ => false,
    }
}

// The version of WMA a stream has: 3 is WMA Pro, and 4 WMA Lossless
fn wma_version(structure: &gst::StructureRef) -> Option<i32> {
    structure.get::<i32>("wmaversion").ok()
}

// The tags as Vorbis comments, under the keys GStreamer gives them, and the
// pictures
fn comments(
    tags: &gst::TagListRef,
    covers: bool,
) -> (Vec<(String, String)>, Vec<DiscoveredPicture>) {
    let mut comments = Vec::new();
    let mut images = Vec::new();
    // The date can be there twice, as a date and as a date and time
    let has_date_time = tags.get::<gst::tags::DateTime>().is_some();
    for (name, values) in tags.iter_generic() {
        match name.as_str() {
            "image" => {
                if covers {
                    images.extend(values.filter_map(|v| v.get::<gst::Sample>().ok()));
                }
            }
            // The comments GStreamer has no tag for, as "KEY=value", like
            // the Work Hierarchy plugin's
            "extended-comment" => {
                for value in values.filter_map(|v| v.get::<&str>().ok()) {
                    if let Ok(comment) = gst_tag::tag_parse_extended_comment(value, true)
                        && let Some(key) = comment.key
                    {
                        push_extended_comment(&mut comments, &key, &comment.value);
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
    // Two names can come to the same comment, like Matroska's CONDUCTOR,
    // which GStreamer takes for a performer, and LEAD_PERFORMER
    let mut unique = HashSet::new();
    comments.retain(|comment| unique.insert(comment.clone()));
    (comments, kept_pictures(images, MAX_PICTURES_SIZE))
}

// The pictures the parent gets: the front cover first, which covers are
// looked for first, then the others, as long as they come to at most
// `budget` bytes
fn kept_pictures(mut images: Vec<gst::Sample>, budget: usize) -> Vec<DiscoveredPicture> {
    images.sort_by_key(|sample| picture_kind(sample) != PictureType::CoverFront.as_u8());
    let mut left = budget;
    let mut kept = Vec::new();
    for sample in &images {
        let Some(size) = sample.buffer().map(gst::BufferRef::size) else {
            continue;
        };
        if size > left {
            continue;
        }
        if let Some(picture) = picture(sample) {
            left -= size;
            kept.push(picture);
        }
    }
    kept
}

// A comment that GStreamer passes on as it is, under the name lofty reads it
// by: ffmpeg writes some tags of Matroska files under names of its own, and
// the disc as a number and a total, like "1/2", which lofty only splits as
// it reads a file
fn push_extended_comment(comments: &mut Vec<(String, String)>, key: &str, value: &str) {
    let key = match key {
        "ALBUM_ARTIST" => "ALBUMARTIST",
        "LEAD_PERFORMER" => "PERFORMER",
        "DISC" => {
            let (number, total) = value.split_once('/').unwrap_or((value, ""));
            for (key, value) in [("DISCNUMBER", number), ("DISCTOTAL", total)] {
                if !value.trim().is_empty() {
                    comments.push((key.to_string(), value.trim().to_string()));
                }
            }
            return;
        }
        key => key,
    };
    comments.push((key.to_string(), value.to_string()));
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
    Some(DiscoveredPicture {
        kind: picture_kind(sample),
        mime_type,
        data: glib::base64_encode(&buffer).to_string(),
    })
}

// A picture's type as in ID3v2, e.g. 3 for the front cover
fn picture_kind(sample: &gst::Sample) -> u8 {
    let image_type = sample
        .info()
        .and_then(|info| info.get::<gst_tag::TagImageType>("image-type").ok())
        .map_or(0, IntoGlib::into_glib);
    // GStreamer's types are ID3v2's without its two icons, with -1 for none
    u8::try_from(image_type + 2)
        .ok()
        .filter(|_| image_type > 0)
        .unwrap_or(0)
}

// The name of the format: the codec's, after the container's when it isn't
// the codec's own
fn format_name(info: &DiscovererInfo, caps: &gst::CapsRef) -> Option<String> {
    let structure = caps.structure(0)?;
    let codec = match structure.name().as_str() {
        "audio/x-flac" => "FLAC".to_string(),
        "audio/x-opus" => "Opus".to_string(),
        "audio/x-vorbis" => "Vorbis".to_string(),
        "audio/x-wma" => match wma_version(structure) {
            Some(3) => "WMA Pro".to_string(),
            Some(4) => "WMA Lossless".to_string(),
            _ => "WMA".to_string(),
        },
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

    // Writes a file through a pipeline; false if an element is missing
    fn write_pipeline(description: &str) -> bool {
        gst::init().unwrap();
        let Ok(pipeline) = gst::parse::launch(description) else {
            return false;
        };
        pipeline.set_state(gst::State::Playing).unwrap();
        let done = pipeline.bus().unwrap().timed_pop_filtered(
            gst::ClockTime::from_seconds(10),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        pipeline.set_state(gst::State::Null).unwrap();
        done.is_some_and(|message| message.type_() == gst::MessageType::Eos)
    }

    #[test]
    fn webm_files_count_with_or_without_a_video() {
        let dir = test_dir("webm");
        let audio = dir.join("audio.webm");
        let video = dir.join("video.webm");
        // vp8enc and webmmux come with the good plugins, opusenc with base
        let sound = "audiotestsrc num-buffers=10 samplesperbuffer=4800 ! audioconvert ! opusenc";
        let wrote = write_pipeline(&format!(
            "{sound} ! webmmux ! filesink location=\"{}\"",
            audio.display()
        )) && write_pipeline(&format!(
            "{sound} ! mux. videotestsrc num-buffers=10 ! vp8enc ! webmmux name=mux \
             ! filesink location=\"{}\"",
            video.display()
        ));
        if wrote {
            let audio = discover_file(&audio, false).unwrap();
            assert!(!audio.video);
            // Opus is lossy, so its depth would only be its decoder's
            assert_eq!(audio.bit_depth, None);
            let video = discover_file(&video, false).unwrap();
            assert!(video.duration_ms > 0);
            assert!(video.video);
        }
        let _ = std::fs::remove_dir_all(&dir);
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
            video: true,
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
        assert!(tags.video);
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

    // A crafted file can have dozens of pictures of 16 MiB
    #[test]
    fn the_front_cover_is_kept_first_and_the_others_as_they_fit() {
        gst::init().unwrap();
        let sample = |image_type| gst::Sample::from_image_data(&PNG, image_type).unwrap();
        let images = vec![
            sample(gst_tag::TagImageType::BackCover),
            sample(gst_tag::TagImageType::Undefined),
            sample(gst_tag::TagImageType::FrontCover),
        ];
        let kinds = |budget| {
            kept_pictures(images.clone(), budget)
                .iter()
                .map(|picture| PictureType::from_u8(picture.kind))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(3 * PNG.len()),
            [
                PictureType::CoverFront,
                PictureType::CoverBack,
                PictureType::Other
            ]
        );
        assert_eq!(
            kinds(2 * PNG.len()),
            [PictureType::CoverFront, PictureType::CoverBack]
        );
        assert!(kinds(PNG.len() - 1).is_empty());
    }

    // What the reader writes is read into memory, so one that writes more
    // than a file's tags and pictures can come to is ended
    #[cfg(unix)]
    #[test]
    fn a_reader_that_writes_too_much_is_ended() {
        use std::os::unix::fs::PermissionsExt;

        let dir = test_dir("reader");
        let helper = dir.join("reader.sh");
        let script = format!("#!/bin/sh\nhead -c {} /dev/zero\n", MAX_REPORT_SIZE + 1);
        std::fs::write(&helper, script).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(run_child(&helper, Path::new("Nimrod.oga"), true), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // A reader that doesn't end, as one that blocked its alarm wouldn't, is
    // ended once its time is up
    #[cfg(unix)]
    #[test]
    fn a_reader_that_runs_too_long_is_ended() {
        use std::os::unix::fs::PermissionsExt;

        let dir = test_dir("late");
        let helper = dir.join("reader.sh");
        // The shell becomes sleep, which keeps the pipe open
        std::fs::write(&helper, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        let read = run_child_for(
            &helper,
            Path::new("Nimrod.oga"),
            true,
            Duration::from_millis(300),
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read, None);
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    // The variable that names the file `probe` reads
    const PROBE_FILE: &str = "DISMAS_SANDBOX_PROBE";

    // The sandbox, tried in a process of its own, as it can't be undone: the
    // test binary started again to run only `probe`, which confines itself
    // as the child does
    #[cfg(target_os = "linux")]
    #[test]
    fn the_sandbox_holds() {
        // Brings GStreamer's registry up to date, which `probe` can't write
        gst::init().unwrap();
        let dir = test_dir("sandbox");
        let path = dir.join("Nimrod.oga");
        let mut tags = gst::TagList::new();
        tags.get_mut()
            .unwrap()
            .add::<gst::tags::Title>(&"Nimrod", gst::TagMergeMode::Append);
        // flacenc and oggmux come with the good and base plugins
        if !write_with_gstreamer(&path, "flacenc name=tags ! oggmux", &tags) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        std::fs::write(dir.join("other.txt"), "Enigma").unwrap();

        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "audio::track::discovered::tests::probe",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PROBE_FILE, &path)
            .env("GST_REGISTRY_UPDATE", "no")
            .env("GST_REGISTRY_FORK", "no")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // What the probe found, which says how confined it was
        println!("{stdout}");
        assert!(output.status.success(), "{}: {stderr}", output.status);
        // A filter that matches no test succeeds too
        assert!(stdout.contains("test result: ok. 1 passed"), "{stderr}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "the_sandbox_holds runs it, in a process of its own"]
    fn probe() {
        use std::{net::TcpStream, os::unix::net::UnixStream};

        let Some(path) = std::env::var_os(PROBE_FILE).map(PathBuf::from) else {
            return;
        };
        let other = path.with_file_name("other.txt");
        let status = sandbox::restrict_to_reading(&path).unwrap();
        println!("Probing a sandbox of {status}");
        // The error of what failed, which says what refused it: Landlock
        // (EACCES) or seccomp (ENOSYS), rather than the call failing for a
        // reason of its own, like a socket that nothing listens on
        fn error<T>(result: std::io::Result<T>) -> Option<i32> {
            result.err().and_then(|e| e.raw_os_error())
        }

        // Its file, and no other file but the system's
        assert!(std::fs::read(&path).is_ok());
        if status.confines_files() {
            assert_eq!(error(std::fs::read(&other)), Some(libc::EACCES));
        } else {
            println!("Not checked: reading other files, as the kernel has no Landlock");
        }
        assert!(
            std::fs::read("/etc/ld.so.cache").is_ok() || !Path::new("/etc/ld.so.cache").exists()
        );
        // No writing, whether to a file that's there or a new one
        let append = std::fs::OpenOptions::new().append(true).open(&other);
        assert_eq!(error(append), Some(libc::ENOSYS));
        let create = std::fs::File::create(path.with_file_name("new.txt"));
        assert_eq!(error(create), Some(libc::ENOSYS));
        // No sockets, no programs
        let unix = UnixStream::connect("/run/dbus/system_bus_socket");
        assert_eq!(error(unix), Some(libc::ENOSYS));
        let tcp = TcpStream::connect(("127.0.0.1", 9));
        assert_eq!(error(tcp), Some(libc::ENOSYS));
        let program = Command::new("/bin/sh").arg("-c").arg("true").status();
        assert_eq!(error(program), Some(libc::ENOSYS));
        // And GStreamer still reads its file
        let discovered = discover_file(&path, false).unwrap();
        assert!(
            discovered
                .comments
                .contains(&("TITLE".to_string(), "Nimrod".to_string())),
            "{:?}",
            discovered.comments
        );
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
        // The library reads the tags without the covers and the properties
        let without_covers = without_covers.unwrap();
        assert_eq!(without_covers.tag.title().as_deref(), Some("Nimrod"));
        assert!(without_covers.tag.pictures().is_empty());
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
        assert!(read.properties.bit_depth().is_some());
    }

    #[test]
    fn ffmpegs_names_for_matroska_tags_are_read_like_vorbis_comments() {
        gst::init().unwrap();
        let mut tags = gst::TagList::new();
        {
            let tags = tags.get_mut().unwrap();
            // As GStreamer passes on the tags ffmpeg writes
            for comment in [
                "ALBUM_ARTIST=Edward Elgar",
                "DISC=1/2",
                "LEAD_PERFORMER=Jacqueline du Pré",
            ] {
                tags.add::<gst::tags::ExtendedComment>(&comment, gst::TagMergeMode::Append);
            }
            // As GStreamer reads a CONDUCTOR, which a file can have with the
            // same name
            tags.add::<gst::tags::Performer>(&"Jacqueline du Pré", gst::TagMergeMode::Append);
        }
        let (comments, _) = comments(&tags, false);
        let read = Discovered {
            comments,
            ..Default::default()
        }
        .into_file_tags();
        assert_eq!(
            read.tag.get_string(ItemKey::AlbumArtist),
            Some("Edward Elgar")
        );
        assert_eq!(read.tag.disk(), Some(1));
        assert_eq!(read.tag.disk_total(), Some(2));
        assert_eq!(
            read.tag.get_strings(ItemKey::Performer).collect::<Vec<_>>(),
            ["Jacqueline du Pré"]
        );

        // A disc without a total
        let mut comments = Vec::new();
        push_extended_comment(&mut comments, "DISC", "3");
        assert_eq!(comments, [("DISCNUMBER".to_string(), "3".to_string())]);
    }

    #[test]
    fn lyrics_come_through_as_the_lyrics_tag() {
        gst::init().unwrap();
        let lrc = "[00:01.00]Ave Maria!\n[00:04.00]Jungfrau mild,";
        let mut tags = gst::TagList::new();
        tags.get_mut()
            .unwrap()
            .add::<gst::tags::Lyrics>(&lrc, gst::TagMergeMode::Append);
        let (comments, _) = comments(&tags, false);
        let read = Discovered {
            comments,
            ..Default::default()
        }
        .into_file_tags();
        assert_eq!(read.tag.get_string(ItemKey::Lyrics), Some(lrc));
    }

    #[test]
    fn only_lossless_codecs_have_a_bit_depth() {
        gst::init().unwrap();
        let wma = |version: i32| {
            gst::Caps::builder("audio/x-wma")
                .field("wmaversion", version)
                .build()
        };
        assert!(!is_lossless(&wma(2)));
        // WMA Pro
        assert!(!is_lossless(&wma(3)));
        // WMA Lossless
        assert!(is_lossless(&wma(4)));
        assert!(is_lossless(&gst::Caps::new_empty_simple("audio/x-flac")));
        assert!(!is_lossless(&gst::Caps::new_empty_simple("audio/x-opus")));
        assert!(!is_lossless(&gst::Caps::new_empty_simple("audio/mpeg")));
    }
}
