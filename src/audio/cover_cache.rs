// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use gtk::{gdk, gio, glib, prelude::*};
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
// scale it down the rest of the way
const FULL_COVER_SIZE: u32 = 1024;

#[derive(Clone, Debug)]
pub struct CoverArt {
    texture: gdk::Texture,
    palette: Vec<gdk::RGBA>,
    cache: Option<PathBuf>,
}

impl CoverArt {
    pub fn texture(&self) -> &gdk::Texture {
        self.texture.as_ref()
    }

    pub fn palette(&self) -> &Vec<gdk::RGBA> {
        self.palette.as_ref()
    }

    pub fn cache(&self) -> Option<&PathBuf> {
        self.cache.as_ref()
    }
}

// The covers are shared by the tracks of an album, which can be loaded in
// separate threads, so the cache is behind a mutex; it's only locked to
// look up and add covers, not while loading them
#[derive(Debug)]
pub struct CoverCache {
    // The albums without a cover are kept as well, so that we don't look
    // for it again for each of their tracks
    entries: HashMap<String, Option<CoverArt>>,
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

    // Another thread might have added the same cover in the meantime, in
    // which case its cover is kept
    fn add_entry(&mut self, uuid: &str, cover: Option<CoverArt>) -> Option<CoverArt> {
        self.entries
            .entry(uuid.to_string())
            .or_insert(cover)
            .clone()
    }

    fn lookup(&self, uuid: &str) -> Option<Option<CoverArt>> {
        self.entries.get(uuid).cloned()
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
            let ext_cover_basename = ["Cover", "cover", "Folder", "folder"];
            let ext_cover_ext = ["jpg", "png"];

            let ext_covers = ext_cover_basename
                .iter()
                .map(|&b| {
                    ext_cover_ext
                        .iter()
                        .map(move |&e| format!("{b}.{e}"))
                        .collect::<Vec<_>>()
                })
                .fold(vec![], |mut v, mut dat| {
                    v.append(&mut dat);
                    v
                });
            for name in ext_covers {
                let mut cover_file = PathBuf::from(p);
                cover_file.push(name);
                debug!("Looking for external cover file: {cover_file:?}");

                let f = gio::File::for_path(&cover_file);
                if let Ok((res, _)) = f.load_bytes(None::<&gio::Cancellable>) {
                    debug!("Loading cover from external cover file");
                    return Some(res);
                }
            }
        }

        debug!("No cover art");

        None
    }

    // The cover of the album of a track, loaded the first time one of its
    // tracks asks for it; this blocks, so it must not be called on the main
    // thread
    pub fn cover_art(path: &Path, tag: &lofty::tag::Tag) -> Option<(CoverArt, String)> {
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

        if let Some(cover) = Self::global().lock().unwrap().lookup(&uuid) {
            debug!("Found cover for UUID '{uuid}'");
            return cover.map(|c| (c, uuid));
        }

        debug!("Loading cover art for UUID: {uuid}");
        let cover = Self::load_cover_art(tag, path.parent())
            .and_then(|data| CoverImage::decode(data, COVER_SIZE))
            .and_then(|image| {
                // The color palette we use for styling the UI; we want both
                // the texture and the palette
                let palette = image.palette()?;
                // Cache the cover, so that the MPRIS controller can
                // reference it later
                let cache = cache_cover_art(&uuid, &image);
                Some(CoverArt {
                    texture: image.into_texture(),
                    palette,
                    cache,
                })
            });

        let cover = Self::global().lock().unwrap().add_entry(&uuid, cover);
        cover.map(|c| (c, uuid))
    }

    // Loads the cover art of a file again, at full resolution; this does not
    // use the cache, and it blocks, so it must not be called on the main
    // thread
    pub fn load_full_cover(path: &Path) -> Option<gdk::Texture> {
        let tagged_file = lofty::probe::Probe::open(path).ok()?.read().ok()?;
        let tag = tagged_file
            .primary_tag()
            .or_else(|| tagged_file.first_tag())?;
        let bytes = Self::load_cover_art(tag, path.parent())?;
        let image = CoverImage::decode(bytes, FULL_COVER_SIZE)?;

        Some(image.into_texture())
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

// Saves the cover in the cache directory, for the MPRIS controller, which
// can only pass the cover art to the desktop as a file
fn cache_cover_art(uuid: &str, image: &CoverImage) -> Option<PathBuf> {
    let mut path = glib::user_cache_dir();
    path.push("amberol");
    path.push("covers");
    glib::mkdir_with_parents(&path, 0o755);

    path.push(format!("{uuid}.png"));
    if path.exists() {
        return Some(path);
    }

    match image.save_png(&path) {
        Ok(()) => {
            debug!("Created cover data cache at {path:?}");
            Some(path)
        }
        Err(e) => {
            warn!("Unable to cache cover data: {e}");
            None
        }
    }
}
