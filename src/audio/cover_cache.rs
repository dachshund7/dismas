// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};

use gtk::{gdk, gio, glib};
use log::{debug, warn};
use sha2::{Digest, Sha256};

use crate::{
    cover_image::{CoverImage, accent_color},
    utils,
};

// The base cover size is 192px, but we need to account for HiDPI;
// better to scale down when rendering on displays with a scaling
// factor of 1 than having to scale up on displays with a scaling
// factor of 2.
const COVER_SIZE: u32 = 192 * 2;

// The cover in the main view is drawn larger than the shared cover
// texture, so it is loaded separately for the current track; we limit its
// size, as the original artwork can be very large, and let the renderer
// scale it down the rest of the way. The cover grows with the window, up
// to about 1000 px in fullscreen, which is 2000 with a scaling factor of 2
const FULL_COVER_SIZE: u32 = 2048;

// The rows of the lists of tracks draw a small copy of the cover, at the
// size of the row's cover with a scaling factor of 2. Drawing the shared
// cover there would have the renderer scale it down with mipmaps in each
// row, which made dragging the scrollbar of a long album stall
const ROW_COVER_SIZE: u32 = 48 * 2;

// Cover files next to the tracks larger than this are not read; covers
// are rarely more than a few megabytes, and this one could be anything
const MAX_COVER_FILE_SIZE: u64 = 32 * 1024 * 1024;

// How long the pictures and the cover files of a track are tried for, as
// each can take the sandbox's whole timeout, and a crafted file can have a
// hundred pictures, which held up the queue and the scan for half an hour;
// a cover decodes in a fraction of a second
const COVER_SEARCH_TIME: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub(crate) struct CoverArt {
    texture: gdk::Texture,
    row_texture: gdk::Texture,
    palette: Vec<gdk::RGBA>,
    accent: Option<gdk::RGBA>,
    cache: Option<PathBuf>,
}

impl CoverArt {
    pub(crate) fn texture(&self) -> &gdk::Texture {
        self.texture.as_ref()
    }

    pub(crate) fn row_texture(&self) -> &gdk::Texture {
        self.row_texture.as_ref()
    }

    pub(crate) fn palette(&self) -> &Vec<gdk::RGBA> {
        self.palette.as_ref()
    }

    // The color the accent color is made from, if the cover isn't grey
    pub(crate) fn accent(&self) -> Option<gdk::RGBA> {
        self.accent
    }

    pub(crate) fn cache(&self) -> Option<&PathBuf> {
        self.cache.as_ref()
    }
}

// The cover of an album, loaded once by the first of its tracks that asks
// for it; the others wait for it, rather than loading it again
type CoverSlot = Arc<OnceLock<Option<CoverArt>>>;

// The cover of a track, shared with the other tracks of its album, which
// keep it in memory: it goes once none of them is left
#[derive(Clone, Debug)]
pub(crate) struct AlbumCover {
    slot: CoverSlot,
    uuid: String,
}

impl AlbumCover {
    // The cover art, if the album has any
    pub(crate) fn art(&self) -> Option<&CoverArt> {
        self.slot.get()?.as_ref()
    }

    pub(crate) fn uuid(&self) -> &str {
        &self.uuid
    }
}

// The covers are shared by the tracks of an album, which are loaded in
// separate threads, so the cache is behind a mutex; it's only locked to
// find the slot of a cover, not while loading it
#[derive(Debug)]
pub(crate) struct CoverCache {
    // The albums without a cover are kept as well, so that we don't look
    // for it again for each of their tracks. The tracks hold the slots, so
    // the covers of the albums that are only browsed don't stay in memory
    entries: HashMap<String, Weak<OnceLock<Option<CoverArt>>>>,
}

impl CoverCache {
    pub(crate) fn global() -> &'static Mutex<CoverCache> {
        static CACHE: OnceLock<Mutex<CoverCache>> = OnceLock::new();

        CACHE.get_or_init(|| {
            let c = CoverCache::new();
            Mutex::new(c)
        })
    }

    fn new() -> Self {
        CoverCache {
            entries: HashMap::new(),
        }
    }

    // Loads the cover of an album again the next time one of its tracks
    // asks for it, e.g. after its files changed; the tracks that have the
    // old cover keep it until they're read again
    pub(crate) fn forget(&mut self, uuid: &str) {
        self.entries.remove(uuid);
    }

    fn slot(&mut self, uuid: &str) -> CoverSlot {
        if let Some(slot) = self.entries.get(uuid).and_then(Weak::upgrade) {
            return slot;
        }

        // Forget the covers that no track has anymore
        self.entries.retain(|_, slot| slot.strong_count() > 0);
        let slot = CoverSlot::default();
        self.entries.insert(uuid.to_string(), Arc::downgrade(&slot));
        slot
    }

    // The cover of a track, scaled down to fit in `max_size`: the first of
    // the pictures in its tags and the cover files in its folder that
    // decodes, so that a broken or unsupported one doesn't hide the next
    pub(crate) fn load_cover(
        tag: &lofty::tag::Tag,
        dir: Option<&Path>,
        max_size: u32,
    ) -> Option<CoverImage> {
        use lofty::picture::PictureType;

        let front = tag
            .get_picture_type(PictureType::CoverFront)
            .map(|picture| glib::Bytes::from(picture.data()));
        // If we don't have a CoverFront picture, we fall back to Other
        // and BandLogo types
        let fallbacks = tag
            .pictures()
            .iter()
            .filter(|picture| {
                matches!(
                    picture.pic_type(),
                    PictureType::Other | PictureType::BandLogo
                )
            })
            .map(|picture| glib::Bytes::from(picture.data()));
        // We always favour the cover art in the track metadata because it's going
        // to be in a hot cache; looking for a separate file will blow a bunch of
        // caches out of the water, which will slow down loading the track into the
        // queue model. The files are only read if the pictures don't decode
        let files = dir
            .into_iter()
            .flat_map(Self::cover_files_for)
            .filter_map(|file| read_cover_file(&file));

        let cover = first_within(
            front.into_iter().chain(fallbacks).chain(files),
            COVER_SEARCH_TIME,
            |data| CoverImage::decode(data, max_size),
        );
        if cover.is_none() {
            debug!("No cover art");
        }
        cover
    }

    // The files that can have the cover of the tracks in a folder, in the
    // order they're looked for: the ones named as covers are in it, and
    // then in the folder of the album when it's in a disc folder, as in
    // Album/CD1, which often has no cover of its own. Failing those, the
    // only image in either folder, which can be named after the album
    pub(crate) fn cover_files_for(dir: &Path) -> impl Iterator<Item = PathBuf> {
        let album = crate::library::disc_album_folder(dir);
        let (named, only) = folder_covers(dir);
        let (album_named, album_only) = album.map(folder_covers).unwrap_or_default();
        named
            .into_iter()
            .chain(album_named)
            .chain(only)
            .chain(album_only)
    }

    // The cover files of a folder alone
    #[cfg(test)]
    pub(crate) fn cover_files(dir: &Path) -> impl Iterator<Item = PathBuf> {
        let (named, only) = folder_covers(dir);
        named.into_iter().chain(only)
    }

    // The cover of the album of a track, loaded the first time one of its
    // tracks asks for it; this blocks, so it must not be called on the main
    // thread
    pub(crate) fn cover_art(path: &Path, tag: &lofty::tag::Tag) -> AlbumCover {
        let mut album_artist = None;
        let mut album = None;

        for item in tag.items() {
            match item.key() {
                lofty::prelude::ItemKey::AlbumTitle => album = item.value().text(),
                lofty::prelude::ItemKey::AlbumArtist => album_artist = item.value().text(),
                _ => (),
            }
        }

        // We use the album, its artist and its folder to ensure we share
        // the same cover data for every track in the album; if we don't
        // have an album, we use the file name. Without an album artist, the
        // folder tells albums apart, rather than the artist of each track,
        // which gave each track of a compilation a cover of its own to
        // decode and keep. Without pictures in
        // the tags, the cover can only be a file in the folder, which the
        // tracks in it share, e.g. files without tags. Paths are hashed as
        // bytes, as they are not always valid UTF-8
        let mut hasher = Sha256::new();
        if tag.pictures().is_empty()
            && let Some(parent) = path.parent()
        {
            hasher.update([0]);
            hasher.update(parent.as_os_str().as_encoded_bytes());
        } else if let Some(album) = album {
            hasher.update(album);

            if let Some(artist) = album_artist {
                hasher.update(artist);
            }

            if let Some(parent) = path.parent() {
                hasher.update(parent.as_os_str().as_encoded_bytes());
            }
        } else {
            hasher.update(path.as_os_str().as_encoded_bytes());
        }

        let uuid = utils::hex(&hasher.finalize());

        let slot = Self::global().lock().unwrap().slot(&uuid);
        slot.get_or_init(|| {
            debug!("Loading cover art for UUID: {uuid}");
            Self::load_cover(tag, path.parent(), COVER_SIZE).and_then(|image| {
                // The color palette we use for styling the UI; we want
                // both the texture and the palette
                let palette = image.palette()?;
                let accent = accent_color(&palette, &image.shares(&palette));
                // Cache the cover, so that the MPRIS controller can
                // reference it later
                let cache = cache_cover_art(&uuid, &image);
                let row_texture = image.thumbnail(ROW_COVER_SIZE)?.into_texture();
                Some(CoverArt {
                    texture: image.into_texture(),
                    row_texture,
                    palette,
                    accent,
                    cache,
                })
            })
        });

        AlbumCover { slot, uuid }
    }

    // Loads the cover art of a file again, at full resolution; this does not
    // use the cache, and it blocks, so it must not be called on the main
    // thread
    pub(crate) fn load_full_cover(path: &Path) -> Option<gdk::Texture> {
        let tags = crate::audio::read_file_tags(path, crate::audio::ReadOptions::ALL)?;
        // A file without tags can still have a cover file next to it
        let image = Self::load_cover(&tags.tag, path.parent(), FULL_COVER_SIZE)?;

        Some(image.into_texture())
    }
}

// The images in a folder that are named as covers, in the order they're
// looked for, and the only image in it, if it has one image and it isn't
// named so. The folder is read, as the case of their names varies, and
// any image type is taken, since glycin goes by the contents; the common
// types come first among files of the same name
fn folder_covers(dir: &Path) -> (Vec<PathBuf>, Option<PathBuf>) {
    let mut named: Vec<(u8, u8, PathBuf)> = Vec::new();
    let mut others = Vec::new();
    // The type is guessed from the name, so once for each extension, as
    // a folder has many tracks
    let mut images: HashMap<String, bool> = HashMap::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let (Some(stem), Some(ext)) = (
            path.file_stem().and_then(|s| s.to_str()),
            path.extension().and_then(|s| s.to_str()),
        ) else {
            continue;
        };
        let ext = ext.to_lowercase();
        let ext_rank = match ext.as_str() {
            "jpg" | "jpeg" => 0,
            "png" => 1,
            _ => 2,
        };
        let image = *images.entry(ext).or_insert_with(|| {
            gio::content_type_guess(Some(&path), None)
                .0
                .starts_with("image/")
        });
        if !image {
            continue;
        }
        let stem = stem.to_lowercase();
        let name_rank = match stem.as_str() {
            "cover" => 0,
            "folder" => 1,
            "front" => 2,
            // Windows Media Player's, AlbumArt_{…}_Large.jpg and
            // AlbumArtSmall.jpg
            name if name.starts_with("albumart") && name.ends_with("large") => 3,
            name if name.starts_with("albumart") => 4,
            // A scan of the back cover isn't the cover, even alone
            name if name.starts_with("back") => continue,
            _ => {
                others.push(path);
                continue;
            }
        };
        named.push((name_rank, ext_rank, path));
    }
    named.sort();
    let only = (others.len() == 1).then(|| others.remove(0));
    (named.into_iter().map(|(_, _, path)| path).collect(), only)
}

// The first of the candidates that `decode` makes something of, trying no
// more of them once `limit` has passed
fn first_within<T, U>(
    candidates: impl IntoIterator<Item = T>,
    limit: Duration,
    decode: impl FnMut(T) -> Option<U>,
) -> Option<U> {
    let start = Instant::now();
    candidates
        .into_iter()
        .take_while(|_| start.elapsed() < limit)
        .find_map(decode)
}

// Reads a cover file next to the tracks, unless it's too large
fn read_cover_file(file: &Path) -> Option<glib::Bytes> {
    debug!("Looking for external cover file: {file:?}");

    // Not a named pipe, which would wait for something to write to it
    let opened = utils::open_regular_file(file).ok()?;
    if opened.metadata().ok()?.len() > MAX_COVER_FILE_SIZE {
        warn!("Skipping cover file {file:?}, which is too large");
        return None;
    }
    // The size that procfs and sysfs files report isn't what they hold:
    // /proc/self/pagemap reports 0 and holds hundreds of gigabytes, and a
    // symbolic link among the covers can lead to one, so the read itself
    // stops past the largest size
    let mut data = Vec::new();
    let read = opened.take(MAX_COVER_FILE_SIZE + 1).read_to_end(&mut data);
    match read {
        Ok(_) if data.len() as u64 > MAX_COVER_FILE_SIZE => {
            warn!("Skipping cover file {file:?}, which is too large");
            None
        }
        Ok(_) => Some(glib::Bytes::from_owned(data)),
        Err(e) => {
            warn!("Unable to read cover file {file:?}: {e}");
            None
        }
    }
}

// The folder of the covers saved for MPRIS
pub(crate) fn cache_dir() -> PathBuf {
    glib::user_cache_dir().join("dismas").join("covers")
}

// Saves the cover in the cache directory, for the MPRIS controller, which
// can only pass the cover art to the desktop as a file
fn cache_cover_art(uuid: &str, image: &CoverImage) -> Option<PathBuf> {
    let dir = cache_dir();
    glib::mkdir_with_parents(&dir, 0o700);

    let png = match image.png_bytes() {
        Ok(png) => png,
        Err(e) => {
            warn!("Unable to encode the cover: {e}");
            return None;
        }
    };
    match write_cover_file(&dir, uuid, &png) {
        Ok(path) => Some(path),
        Err(e) => {
            warn!("Unable to cache cover data: {e}");
            None
        }
    }
}

// Writes a cover to a file named after the album and a hash of the
// picture. A changed cover gets a new file, rather than replacing the old
// one, whose picture the desktop could have kept for the same file name;
// the files of the album's previous covers are removed
fn write_cover_file(dir: &Path, uuid: &str, png: &[u8]) -> Result<PathBuf, glib::Error> {
    let hash = utils::hex(&Sha256::digest(png));
    let name = format!("{uuid}-{}.png", &hash[..16]);
    let path = dir.join(&name);
    if path.exists() {
        // Used, so it's kept when old caches are cleaned up
        utils::touch(&path);
        return Ok(path);
    }

    glib::file_set_contents(&path, png)?;
    debug!("Created cover data cache at {path:?}");

    // Along with the file of the album's cover from before the files were
    // named after their picture
    let previous = |file: &str| {
        file != name
            && (file == format!("{uuid}.png")
                || file
                    .strip_prefix(uuid)
                    .is_some_and(|rest| rest.starts_with('-') && rest.ends_with(".png")))
    };
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_str().is_some_and(previous)
                && let Err(e) = std::fs::remove_file(entry.path())
            {
                warn!("Unable to remove the old cover {:?}: {e}", entry.path());
            }
        }
    }

    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    use gtk::{gdk, gio, glib, prelude::*};

    use super::{COVER_SIZE, CoverCache, first_within, read_cover_file, write_cover_file};
    use crate::cover_image::{accent_color, oklch_chroma};

    // A crafted file can have a hundred pictures, each taking the whole
    // timeout of the sandbox
    #[test]
    fn covers_are_looked_for_for_a_while_at_most() {
        use std::time::Duration;

        let mut tried = 0;
        let found: Option<u32> = first_within(0..100, Duration::from_millis(30), |_| {
            tried += 1;
            std::thread::sleep(Duration::from_millis(10));
            None
        });
        assert_eq!(found, None);
        assert!(tried < 20, "{tried} tried");
        let first = first_within(0..3, Duration::from_secs(10), |i| (i > 0).then_some(i));
        assert_eq!(first, Some(1));
    }

    #[test]
    fn changed_covers_get_a_new_file() {
        let dir = glib::mkdtemp("dismas-covers-XXXXXX").unwrap();
        let files = || {
            let mut names: Vec<String> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        };
        // From before the files were named after their picture, and the
        // cover of another album
        std::fs::write(dir.join("album.png"), b"old").unwrap();
        std::fs::write(dir.join("other-0123456789abcdef.png"), b"other").unwrap();

        let first = write_cover_file(&dir, "album", b"first").unwrap();
        assert_eq!(write_cover_file(&dir, "album", b"first").unwrap(), first);
        let second = write_cover_file(&dir, "album", b"second").unwrap();
        assert_ne!(first, second);

        let second_name = second.file_name().unwrap().to_str().unwrap().to_string();
        assert_eq!(
            files(),
            [second_name, "other-0123456789abcdef.png".to_string()]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cover_files_match_any_case_and_image_type() {
        let dir = glib::mkdtemp("dismas-cover-files-XXXXXX").unwrap();
        for name in [
            "Folder.JPEG",
            "COVER.png",
            "cover.JPG",
            "cover.webp",
            "cover.txt",
            "back.jpg",
            "notes.txt",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }

        let names: Vec<String> = CoverCache::cover_files(&dir)
            .map(|path| path.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["cover.JPG", "COVER.png", "cover.webp", "Folder.JPEG"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cover_files_have_other_names_and_places() {
        let dir = glib::mkdtemp("dismas-cover-files-XXXXXX").unwrap();
        let names = |folder: &str| -> Vec<String> {
            CoverCache::cover_files_for(&dir.join(folder))
                .map(|path| {
                    path.strip_prefix(&dir)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        };
        for file in [
            // Windows Media Player's, the large one first
            "WMP/AlbumArtSmall.jpg",
            "WMP/AlbumArt_{0A1B}_Large.jpg",
            "WMP/front.png",
            // The cover of an album whose discs are in their own folders,
            // after their own named covers, and before a lone image
            "Album/cover.jpg",
            "Album/CD1/disc.jpg",
            "Album/CD2/front.jpg",
            // The only image of a folder, unless it's the back cover
            "Named/Abbey Road.jpg",
            "Two/a.jpg",
            "Two/b.jpg",
            "Back/back.jpg",
        ] {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"").unwrap();
        }

        assert_eq!(
            names("WMP"),
            [
                "WMP/front.png",
                "WMP/AlbumArt_{0A1B}_Large.jpg",
                "WMP/AlbumArtSmall.jpg"
            ]
        );
        assert_eq!(
            names("Album/CD1"),
            ["Album/cover.jpg", "Album/CD1/disc.jpg"]
        );
        assert_eq!(
            names("Album/CD2"),
            ["Album/CD2/front.jpg", "Album/cover.jpg"]
        );
        assert_eq!(names("Named"), ["Named/Abbey Road.jpg"]);
        assert!(names("Two").is_empty());
        assert!(names("Back").is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn undecodable_cover_files_fall_through() {
        let dir = glib::mkdtemp("dismas-cover-files-XXXXXX").unwrap();
        std::fs::write(dir.join("cover.bmp"), b"not an image").unwrap();
        image::RgbImage::from_pixel(8, 4, image::Rgb([255, 0, 0]))
            .save(dir.join("folder.png"))
            .unwrap();

        let tag = lofty::tag::Tag::new(lofty::tag::TagType::Id3v2);
        let cover = CoverCache::load_cover(&tag, Some(&dir), 384).unwrap();
        let texture = cover.into_texture();
        assert_eq!((texture.width(), texture.height()), (8, 4));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn small_cover_files_are_read_whole() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-cover-files-XXXXXX")).unwrap();
        let file = dir.join("cover.jpg");
        std::fs::write(&file, b"a small cover").unwrap();

        let data = read_cover_file(&file).unwrap();
        assert_eq!(&*data, b"a small cover");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // A symbolic link among the covers can lead to a procfs file, which
    // says it's empty and holds hundreds of gigabytes; reading it whole
    // would run out of memory, so the read stops past the largest cover
    #[cfg(target_os = "linux")]
    #[test]
    fn cover_files_are_read_no_further_than_the_largest_size() {
        let dir = glib::mkdtemp(std::env::temp_dir().join("dismas-cover-files-XXXXXX")).unwrap();
        let link = dir.join("cover.jpg");
        std::os::unix::fs::symlink("/proc/self/pagemap", &link).unwrap();
        // So the size it gives doesn't keep it from being read
        let metadata = std::fs::metadata(&link).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.len(), 0);

        assert!(read_cover_file(&link).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn covers_go_once_no_track_has_them() {
        let mut cache = CoverCache::new();
        let first = cache.slot("first");
        assert!(Arc::ptr_eq(&first, &cache.slot("first")));

        // The cover is dropped with the last track that had it, and its
        // entry once another cover is looked up
        drop(first);
        let _second = cache.slot("second");
        assert!(!cache.entries.contains_key("first"));
        assert!(cache.entries.contains_key("second"));
    }

    // Prints the palette of the cover of each folder under DISMAS_COVERS_DIR,
    // loaded like the app does, with the chroma and share of each color, and
    // a star on the one the accent color is made from, to tune accent_color
    // on real covers rather than on palettes picked by eye:
    //   DISMAS_COVERS_DIR=~/Music cargo test print_palettes -- --ignored --nocapture
    #[test]
    #[ignore]
    fn print_palettes() {
        fn hex(color: &gdk::RGBA) -> String {
            let byte = |c: f32| (c * 255.0).round() as u8;
            format!(
                "#{:02x}{:02x}{:02x}",
                byte(color.red()),
                byte(color.green()),
                byte(color.blue())
            )
        }

        let root = std::env::var_os("DISMAS_COVERS_DIR").expect("DISMAS_COVERS_DIR is not set");
        let mut dirs = vec![PathBuf::from(root)];
        while let Some(dir) = dirs.pop() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .collect();
            entries.sort();
            dirs.extend(entries.iter().rev().filter(|path| path.is_dir()).cloned());

            let tags = entries
                .iter()
                .filter(|path| {
                    gio::content_type_guess(Some(path), None)
                        .0
                        .starts_with("audio/")
                })
                .find_map(|path| {
                    crate::audio::read_file_tags(path, crate::audio::ReadOptions::ALL)
                });
            let Some(tags) = tags else {
                continue;
            };
            let Some(image) = CoverCache::load_cover(&tags.tag, Some(&dir), COVER_SIZE) else {
                continue;
            };
            let Some(palette) = image.palette() else {
                continue;
            };

            let shares = image.shares(&palette);
            let accent = accent_color(&palette, &shares);
            let colors: Vec<String> = palette
                .iter()
                .zip(&shares)
                .map(|(color, share)| {
                    let star = if accent == Some(*color) { " *" } else { "" };
                    format!("{} {:.3} {share:.3}{star}", hex(color), oklch_chroma(color))
                })
                .collect();
            println!("{}\t{}", dir.display(), colors.join("\t"));
        }
    }
}
