// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use gtk::{gdk, glib};
use lofty::prelude::TaggedFileExt;
use log::{debug, warn};
use sha2::{Digest, Sha256};

use crate::cover_image::CoverImage;

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

    pub fn load_cover_art(tag: &lofty::tag::Tag, path: Option<&Path>) -> Option<glib::Bytes> {
        if let Some(picture) = tag.get_picture_type(lofty::picture::PictureType::CoverFront) {
            debug!("Found CoverFront");
            return Some(glib::Bytes::from(picture.data()));
        }
        // If we don't have a CoverFront picture, we fall back to Other
        // and BandLogo types
        for picture in tag.pictures() {
            let cover_art = match picture.pic_type() {
                lofty::picture::PictureType::Other => Some(glib::Bytes::from(picture.data())),
                lofty::picture::PictureType::BandLogo => Some(glib::Bytes::from(picture.data())),
                _ => None,
            };

            if cover_art.is_some() {
                debug!("Found fallback");
                return cover_art;
            }
        }

        // We always favour the cover art in the track metadata because it's going
        // to be in a hot cache; looking for a separate file will blow a bunch of
        // caches out of the water, which will slow down loading the track into the
        // queue model
        if let Some(p) = path {
            for cover_file in Self::cover_files(p) {
                debug!("Looking for external cover file: {cover_file:?}");

                let Ok(metadata) = std::fs::metadata(&cover_file) else {
                    continue;
                };
                if !metadata.is_file() {
                    continue;
                }
                if metadata.len() > MAX_COVER_FILE_SIZE {
                    warn!("Skipping cover file {cover_file:?}, which is too large");
                    continue;
                }
                match std::fs::read(&cover_file) {
                    Ok(data) => {
                        debug!("Loading cover from external cover file");
                        return Some(glib::Bytes::from_owned(data));
                    }
                    Err(e) => warn!("Unable to read cover file {cover_file:?}: {e}"),
                }
            }
        }

        debug!("No cover art");

        None
    }

    // The files that can have the cover of the tracks in a folder, in the
    // order they're looked for
    pub fn cover_files(dir: &Path) -> impl Iterator<Item = PathBuf> {
        ["Cover", "cover", "Folder", "folder"]
            .into_iter()
            .flat_map(|name| ["jpg", "png"].map(|ext| dir.join(format!("{name}.{ext}"))))
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
        // don't have an album, we use the file name; paths are hashed as
        // bytes, as they are not always valid UTF-8
        let mut hasher = Sha256::new();
        if let Some(album) = album {
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

        let uuid = format!("{:x}", hasher.finalize());

        let slot = Self::global().lock().unwrap().slot(&uuid);
        slot.get_or_init(|| {
            debug!("Loading cover art for UUID: {uuid}");
            Self::load_cover_art(tag, path.parent())
                .and_then(|data| CoverImage::decode(data, COVER_SIZE))
                .and_then(|image| {
                    // The color palette we use for styling the UI; we want
                    // both the texture and the palette
                    let palette = image.palette()?;
                    // Cache the cover, so that the MPRIS controller can
                    // reference it later
                    let cache = cache_cover_art(&uuid, &image);
                    let row_texture = image.thumbnail(ROW_COVER_SIZE)?.into_texture();
                    Some(CoverArt {
                        texture: image.into_texture(),
                        row_texture,
                        palette,
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
        let tag = tagged_file
            .primary_tag()
            .or_else(|| tagged_file.first_tag())?;
        let bytes = Self::load_cover_art(tag, path.parent())?;
        let image = CoverImage::decode(bytes, FULL_COVER_SIZE)?;

        Some(image.into_texture())
    }
}

// Saves the cover in the cache directory, for the MPRIS controller, which
// can only pass the cover art to the desktop as a file
fn cache_cover_art(uuid: &str, image: &CoverImage) -> Option<PathBuf> {
    let mut dir = glib::user_cache_dir();
    dir.push("dismas");
    dir.push("covers");
    glib::mkdir_with_parents(&dir, 0o755);

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
    let hash = format!("{:x}", Sha256::digest(png));
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
    use std::sync::Arc;

    use gtk::glib;

    use super::{CoverCache, write_cover_file};

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
}
