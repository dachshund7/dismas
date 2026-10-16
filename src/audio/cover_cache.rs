// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use gtk::{gdk, gio, glib, prelude::*};
use lofty::prelude::TaggedFileExt;
use log::debug;
use sha2::{Digest, Sha256};

use crate::utils;

// The pixels of a full resolution cover; unlike GdkPixbuf, they can be
// sent from the thread loading them to the main thread
pub struct FullCover {
    width: i32,
    height: i32,
    stride: usize,
    has_alpha: bool,
    pixels: glib::Bytes,
}

impl FullCover {
    pub fn texture(&self) -> gdk::Texture {
        let format = if self.has_alpha {
            gdk::MemoryFormat::R8g8b8a8
        } else {
            gdk::MemoryFormat::R8g8b8
        };

        gdk::MemoryTexture::new(self.width, self.height, format, &self.pixels, self.stride).upcast()
    }
}

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

#[derive(Debug)]
pub struct CoverCache {
    entries: HashMap<String, CoverArt>,
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

    fn add_entry(&mut self, uuid: &str, cover: CoverArt) -> &CoverArt {
        self.entries.entry(uuid.to_string()).or_insert(cover)
    }

    fn lookup(&self, uuid: &String) -> Option<&CoverArt> {
        self.entries.get(uuid)
    }

    pub fn load_cover_art(tag: &lofty::tag::Tag, path: Option<&Path>) -> Option<glib::Bytes> {
        if let Some(picture) = tag.get_picture_type(lofty::picture::PictureType::CoverFront) {
            debug!("Found CoverFront");
            return Some(glib::Bytes::from(picture.data()));
        } else {
            // If we don't have a CoverFront picture, we fall back to Other
            // and BandLogo types
            for picture in tag.pictures() {
                let cover_art = match picture.pic_type() {
                    lofty::picture::PictureType::Other => Some(glib::Bytes::from(picture.data())),
                    lofty::picture::PictureType::BandLogo => {
                        Some(glib::Bytes::from(picture.data()))
                    }
                    _ => None,
                };

                if cover_art.is_some() {
                    debug!("Found fallback");
                    return cover_art;
                }
            }
        }

        // We always favour the cover art in the track metadata because it's going
        // to be in a hot cache; looking for a separate file will blow a bunch of
        // caches out of the water, which will slow down loading the track into the
        // playlist model
        if let Some(p) = path {
            let ext_cover_basename = ["Cover", "cover", "Folder", "folder"];
            let ext_cover_ext = ["jpg", "png"];

            let ext_covers = ext_cover_basename
                .iter()
                .map(|&b| {
                    ext_cover_ext
                        .iter()
                        .map(move |&e| format!("{}.{}", b, e))
                        .collect::<Vec<_>>()
                })
                .fold(vec![], |mut v, mut dat| {
                    v.append(&mut dat);
                    v
                });
            for name in ext_covers {
                let mut cover_file = PathBuf::from(p);
                cover_file.push(name);
                debug!("Looking for external cover file: {:?}", cover_file);

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

    pub fn cover_art(&mut self, path: &Path, tag: &lofty::tag::Tag) -> Option<(CoverArt, String)> {
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
            };
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

        match self.lookup(&uuid) {
            Some(c) => {
                debug!("Found cover for UUID '{}'", uuid);
                Some((c.clone(), uuid))
            }
            None => {
                debug!("Loading cover art for UUID: {}", uuid);

                let cover_art = Self::load_cover_art(tag, path.parent());

                // The pixel buffer for the cover art
                let cover_pixbuf = if let Some(ref cover_art) = cover_art {
                    utils::load_cover_texture(cover_art)
                } else {
                    None
                };

                // Cache the pixel buffer, so that the MPRIS controller can
                // reference it later
                let cache_path = if let Some(ref pixbuf) = cover_pixbuf {
                    utils::cache_cover_art(&uuid, pixbuf)
                } else {
                    None
                };

                // The texture we draw on screen
                let texture = cover_pixbuf.as_ref().map(gdk::Texture::for_pixbuf);

                // The color palette we use for styling the UI
                let palette = cover_pixbuf.as_ref().and_then(utils::load_palette);

                // We want both texture and palette
                if let (Some(texture), Some(palette)) = (texture, palette) {
                    let res = CoverArt {
                        texture,
                        palette,
                        cache: cache_path,
                    };

                    self.add_entry(&uuid, res.clone());

                    Some((res, uuid))
                } else {
                    None
                }
            }
        }
    }

    // Loads the cover art of a file again, at full resolution; this does not
    // use the cache, and it can be called from any thread
    pub fn load_full_cover(path: &Path) -> Option<FullCover> {
        let tagged_file = lofty::probe::Probe::open(path).ok()?.read().ok()?;
        let tag = tagged_file
            .primary_tag()
            .or_else(|| tagged_file.first_tag())?;
        let bytes = Self::load_cover_art(tag, path.parent())?;
        let pixbuf = utils::load_full_cover(&bytes)?;

        Some(FullCover {
            width: pixbuf.width(),
            height: pixbuf.height(),
            stride: pixbuf.rowstride() as usize,
            has_alpha: pixbuf.has_alpha(),
            pixels: pixbuf.read_pixel_bytes(),
        })
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
