// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use gtk::{gdk, gio, glib};
use lofty::prelude::TaggedFileExt;
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

#[derive(Clone, Debug)]
pub struct CoverArt {
    texture: gdk::Texture,
    row_texture: gdk::Texture,
    palette: Vec<gdk::RGBA>,
    accent: Option<gdk::RGBA>,
    cache: Option<PathBuf>,
}

impl CoverArt {
    pub fn texture(&self) -> &gdk::Texture {
        self.texture.as_ref()
    }

    pub fn row_texture(&self) -> &gdk::Texture {
        self.row_texture.as_ref()
    }

    pub fn palette(&self) -> &Vec<gdk::RGBA> {
        self.palette.as_ref()
    }

    // The color the accent color is made from, if the cover isn't grey
    pub fn accent(&self) -> Option<gdk::RGBA> {
        self.accent
    }

    pub fn cache(&self) -> Option<&PathBuf> {
        self.cache.as_ref()
    }
}

// The cover of an album, loaded once by the first of its tracks that asks
// for it; the others wait for it, rather than loading it again
type CoverSlot = Arc<OnceLock<Option<CoverArt>>>;

// The cover of a track, shared with the other tracks of its album, which
// keep it in memory: it goes once none of them is left
#[derive(Clone, Debug)]
pub struct AlbumCover {
    slot: CoverSlot,
    uuid: String,
}

impl AlbumCover {
    // The cover art, if the album has any
    pub fn art(&self) -> Option<&CoverArt> {
        self.slot.get()?.as_ref()
    }

    pub fn uuid(&self) -> &str {
        &self.uuid
    }
}

// The covers are shared by the tracks of an album, which are loaded in
// separate threads, so the cache is behind a mutex; it's only locked to
// find the slot of a cover, not while loading it
#[derive(Debug)]
pub struct CoverCache {
    // The albums without a cover are kept as well, so that we don't look
    // for it again for each of their tracks. The tracks hold the slots, so
    // the covers of the albums that are only browsed don't stay in memory
    entries: HashMap<String, Weak<OnceLock<Option<CoverArt>>>>,
}

impl CoverCache {
    pub fn global() -> &'static Mutex<CoverCache> {
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
    pub fn forget(&mut self, uuid: &str) {
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
    pub fn load_cover(
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
            .flat_map(Self::cover_files)
            .filter_map(|file| read_cover_file(&file));

        let cover = front
            .into_iter()
            .chain(fallbacks)
            .chain(files)
            .find_map(|data| CoverImage::decode(data, max_size));
        if cover.is_none() {
            debug!("No cover art");
        }
        cover
    }

    // The files that can have the cover of the tracks in a folder, in the
    // order they're looked for. The folder is read, as the case of their
    // names varies, and any image type is taken, since glycin goes by the
    // contents; the common types come first among files of the same name
    pub fn cover_files(dir: &Path) -> impl Iterator<Item = PathBuf> {
        const NAMES: [&str; 2] = ["cover", "folder"];

        let mut files: Vec<(usize, u8, PathBuf)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let stem = path.file_stem()?.to_str()?;
                let ext = path.extension()?.to_str()?;
                let name_rank = NAMES
                    .iter()
                    .position(|name| stem.eq_ignore_ascii_case(name))?;
                let (mime_type, _) = gio::content_type_guess(Some(&path), None);
                if !mime_type.starts_with("image/") {
                    return None;
                }
                let ext_rank =
                    if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") {
                        0
                    } else if ext.eq_ignore_ascii_case("png") {
                        1
                    } else {
                        2
                    };
                Some((name_rank, ext_rank, path))
            })
            .collect();
        files.sort();
        files.into_iter().map(|(_, _, path)| path)
    }

    // The cover of the album of a track, loaded the first time one of its
    // tracks asks for it; this blocks, so it must not be called on the main
    // thread
    pub fn cover_art(path: &Path, tag: &lofty::tag::Tag) -> AlbumCover {
        let mut album_artist = None;
        let mut track_artist = None;
        let mut album = None;

        fn get_text_value(value: &lofty::tag::ItemValue) -> Option<String> {
            match value {
                lofty::tag::ItemValue::Text(s) => Some(s.to_string()),
                _ => None,
            }
        }

        for item in tag.items() {
            match item.key() {
                lofty::prelude::ItemKey::AlbumTitle => album = get_text_value(item.value()),
                lofty::prelude::ItemKey::AlbumArtist => album_artist = get_text_value(item.value()),
                lofty::prelude::ItemKey::TrackArtist => track_artist = get_text_value(item.value()),
                _ => (),
            }
        }

        // We use the album and artist to ensure we share the
        // same cover data for every track in the album; if we
        // don't have an album, we use the file name. Without pictures in
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
            hasher.update(&album);

            if let Some(artist) = album_artist {
                hasher.update(&artist);
            } else if let Some(artist) = track_artist {
                hasher.update(&artist);
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
    pub fn load_full_cover(path: &Path) -> Option<gdk::Texture> {
        // The type of the file is guessed from its contents, as the
        // extension can be wrong, e.g. Opus in an ".ogg" file
        let tagged_file = lofty::probe::Probe::open(path)
            .ok()?
            .guess_file_type()
            .ok()?
            .read()
            .ok()?;
        // A file without tags can still have a cover file next to it
        let empty = lofty::tag::Tag::new(tagged_file.primary_tag_type());
        let tag = tagged_file
            .primary_tag()
            .or_else(|| tagged_file.first_tag())
            .unwrap_or(&empty);
        let image = Self::load_cover(tag, path.parent(), FULL_COVER_SIZE)?;

        Some(image.into_texture())
    }
}

// Reads a cover file next to the tracks, unless it's too large
fn read_cover_file(file: &Path) -> Option<glib::Bytes> {
    debug!("Looking for external cover file: {file:?}");

    let metadata = std::fs::metadata(file).ok()?;
    if !metadata.is_file() {
        return None;
    }
    if metadata.len() > MAX_COVER_FILE_SIZE {
        warn!("Skipping cover file {file:?}, which is too large");
        return None;
    }
    match std::fs::read(file) {
        Ok(data) => Some(glib::Bytes::from_owned(data)),
        Err(e) => {
            warn!("Unable to read cover file {file:?}: {e}");
            None
        }
    }
}

// Saves the cover in the cache directory, for the MPRIS controller, which
// can only pass the cover art to the desktop as a file
fn cache_cover_art(uuid: &str, image: &CoverImage) -> Option<PathBuf> {
    let mut dir = glib::user_cache_dir();
    dir.push("dismas");
    dir.push("covers");
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
    use lofty::prelude::TaggedFileExt;

    use super::{COVER_SIZE, CoverCache, write_cover_file};
    use crate::cover_image::{accent_color, oklch_chroma};

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

            let tagged_file = entries
                .iter()
                .filter(|path| {
                    gio::content_type_guess(Some(path), None)
                        .0
                        .starts_with("audio/")
                })
                .find_map(|path| {
                    lofty::probe::Probe::open(path)
                        .ok()?
                        .guess_file_type()
                        .ok()?
                        .read()
                        .ok()
                });
            let Some(tagged_file) = tagged_file else {
                continue;
            };
            let Some(tag) = tagged_file
                .primary_tag()
                .or_else(|| tagged_file.first_tag())
            else {
                continue;
            };
            let Some(image) = CoverCache::load_cover(tag, Some(&dir), COVER_SIZE) else {
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
