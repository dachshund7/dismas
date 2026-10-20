// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! The cover art of the tracks
//!
//! Covers come from the tags of the tracks, or from image files next to
//! them, so anyone could have crafted them; they are decoded with glycin,
//! which does it in a sandboxed process. Only the decoded pixels are handled
//! here, to scale the covers down, pick their colors and save them as PNG.
//!
//! Decoding waits for the sandboxed loader, so none of this should be used
//! on the main thread.

use std::{io::Cursor, path::Path, time::Duration};

use color_thief::{ColorFormat, get_palette};
use gtk::{gdk, glib};
use image::{ImageBuffer, ImageFormat, Pixel, Rgb, Rgba, imageops::FilterType};
use log::warn;

// Larger images are refused rather than decoded; covers are usually no
// more than a few thousand pixels wide
const MAX_DIMENSIONS: (u32, u32) = (8192, 8192);
const TIMEOUT: Duration = Duration::from_secs(10);

// The pixels of a decoded cover, with no padding between the rows
#[derive(Clone)]
pub struct CoverImage {
    width: u32,
    height: u32,
    has_alpha: bool,
    pixels: Vec<u8>,
}

impl CoverImage {
    // Decodes the cover, scaled down to fit in `max_size`, keeping its
    // aspect ratio; smaller covers are not scaled up
    pub fn decode(data: glib::Bytes, max_size: u32) -> Option<Self> {
        let frame = match decode_frame(data) {
            Ok(frame) => frame,
            Err(e) => {
                warn!("Unable to load cover art: {e}");
                return None;
            }
        };

        let width = frame.width();
        let height = frame.height();
        let has_alpha = frame.memory_format().has_alpha();
        let row_len = width as usize * if has_alpha { 4 } else { 3 };
        let stride = frame.stride() as usize;
        if row_len == 0 || height == 0 || stride < row_len {
            warn!("Invalid cover art: {width} x {height}, stride {stride}");
            return None;
        }

        let mut pixels = Vec::with_capacity(row_len * height as usize);
        for row in frame.buf_slice().chunks(stride).take(height as usize) {
            pixels.extend_from_slice(row.get(..row_len)?);
        }
        if pixels.len() != row_len * height as usize {
            warn!("Incomplete cover art");
            return None;
        }

        let cover = CoverImage {
            width,
            height,
            has_alpha,
            pixels,
        };
        cover.scaled(max_size)
    }

    // A copy of the cover, scaled down to fit in `max_size`
    pub fn thumbnail(&self, max_size: u32) -> Option<Self> {
        self.clone().scaled(max_size)
    }

    fn scaled(self, max_size: u32) -> Option<Self> {
        if self.width <= max_size && self.height <= max_size {
            return Some(self);
        }

        let (width, height) = if self.width > self.height {
            (max_size, (max_size * self.height / self.width).max(1))
        } else {
            ((max_size * self.width / self.height).max(1), max_size)
        };

        let pixels = if self.has_alpha {
            resize::<Rgba<u8>>(self.width, self.height, self.pixels, width, height)?
        } else {
            resize::<Rgb<u8>>(self.width, self.height, self.pixels, width, height)?
        };

        Some(CoverImage {
            width,
            height,
            has_alpha: self.has_alpha,
            pixels,
        })
    }

    // The main colors of the cover, for styling the UI
    pub fn palette(&self) -> Option<Vec<gdk::RGBA>> {
        let format = if self.has_alpha {
            ColorFormat::Rgba
        } else {
            ColorFormat::Rgb
        };
        let palette = get_palette(&self.pixels, format, 5, 4).ok()?;

        Some(
            palette
                .iter()
                .map(|c| {
                    gdk::RGBA::new(
                        f32::from(c.r) / 255.0,
                        f32::from(c.g) / 255.0,
                        f32::from(c.b) / 255.0,
                        1.0,
                    )
                })
                .collect(),
        )
    }

    // The cover encoded as PNG
    pub fn png_bytes(&self) -> Result<Vec<u8>, String> {
        let color_type = if self.has_alpha {
            image::ExtendedColorType::Rgba8
        } else {
            image::ExtendedColorType::Rgb8
        };

        let mut data = Cursor::new(Vec::new());
        image::write_buffer_with_format(
            &mut data,
            &self.pixels,
            self.width,
            self.height,
            color_type,
            ImageFormat::Png,
        )
        .map_err(|e| e.to_string())?;

        Ok(data.into_inner())
    }

    // Saves the cover as PNG; the file is written under a temporary name,
    // and renamed once it's complete, so that an interrupted write doesn't
    // leave a partial cover behind
    pub fn save_png(&self, path: &Path) -> Result<(), String> {
        let data = self.png_bytes()?;
        glib::file_set_contents(path, &data).map_err(|e| e.to_string())
    }

    pub fn into_texture(self) -> gdk::Texture {
        let (format, bytes_per_pixel) = if self.has_alpha {
            (gdk::MemoryFormat::R8g8b8a8, 4)
        } else {
            (gdk::MemoryFormat::R8g8b8, 3)
        };
        let stride = self.width as usize * bytes_per_pixel;

        gdk::MemoryTexture::new(
            self.width as i32,
            self.height as i32,
            format,
            &glib::Bytes::from_owned(self.pixels),
            stride,
        )
        .into()
    }
}

// Decodes the first frame of the image in the sandbox, as 8-bit RGB, or
// RGBA if it has transparency
fn decode_frame(data: glib::Bytes) -> Result<glycin::Frame, glycin::Error> {
    let mut loader = glycin::Loader::new_bytes(data);
    loader
        .accepted_memory_formats(
            glycin::MemoryFormatSelection::R8g8b8 | glycin::MemoryFormatSelection::R8g8b8a8,
        )
        .limits(
            glycin::Limits::default()
                .max_dimensions(MAX_DIMENSIONS)
                .timeout(TIMEOUT),
        )
        // glycin's own main loop, rather than the one of the application,
        // which could be waiting for this thread
        .main_context_selector(glycin::MainContextSelector::Managed);

    futures::executor::block_on(async move {
        let mut image = loader.load().await?;
        image.next_frame().await
    })
}

fn resize<P>(
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    new_width: u32,
    new_height: u32,
) -> Option<Vec<u8>>
where
    P: Pixel<Subpixel = u8> + 'static,
{
    let image = ImageBuffer::<P, Vec<u8>>::from_raw(width, height, pixels)?;
    Some(image::imageops::resize(&image, new_width, new_height, FilterType::CatmullRom).into_raw())
}

#[cfg(test)]
mod tests {
    use gtk::{glib, prelude::*};

    use super::CoverImage;

    // A PNG with transparency, saved by us, and decoded again by the
    // glycin loaders, which have to be installed
    #[test]
    fn covers_are_decoded_and_scaled_down() {
        let (width, height) = (600, 300);
        let pixels = (0..width * height)
            .flat_map(|i| [(i % 256) as u8, 0, 255, 128])
            .collect();
        let image = CoverImage {
            width,
            height,
            has_alpha: true,
            pixels,
        };

        let dir = glib::mkdtemp("dismas-cover-XXXXXX").unwrap();
        let path = dir.join("cover.png");
        image.save_png(&path).unwrap();
        let data = glib::Bytes::from_owned(std::fs::read(&path).unwrap());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();

        let cover = CoverImage::decode(data, 384).unwrap();
        assert_eq!((cover.width, cover.height), (384, 192));
        assert!(cover.has_alpha);
        assert_eq!(cover.pixels.len(), 384 * 192 * 4);
        assert!(cover.palette().is_some());

        let texture = cover.into_texture();
        assert_eq!((texture.width(), texture.height()), (384, 192));
    }

    #[test]
    fn invalid_covers_are_not_decoded() {
        let data = glib::Bytes::from_static(b"not an image");
        assert!(CoverImage::decode(data, 384).is_none());
    }
}
