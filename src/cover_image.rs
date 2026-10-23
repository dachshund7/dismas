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

    // How much of the cover each color of the palette covers: the share of
    // the pixels the palette was made from (the opaque ones that aren't
    // white) that are nearest to it, in RGB, where the palette was found
    pub fn shares(&self, palette: &[gdk::RGBA]) -> Vec<f32> {
        let colors: Vec<[f32; 3]> = palette
            .iter()
            .map(|c| [c.red() * 255.0, c.green() * 255.0, c.blue() * 255.0])
            .collect();
        let mut counts = vec![0_u32; colors.len()];

        let bytes_per_pixel = if self.has_alpha { 4 } else { 3 };
        for pixel in self.pixels.chunks_exact(bytes_per_pixel) {
            let (r, g, b) = (pixel[0], pixel[1], pixel[2]);
            if pixel.get(3).is_some_and(|&a| a < 125) || (r > 250 && g > 250 && b > 250) {
                continue;
            }
            let distance = |color: &[f32; 3]| {
                (f32::from(r) - color[0]).powi(2)
                    + (f32::from(g) - color[1]).powi(2)
                    + (f32::from(b) - color[2]).powi(2)
            };
            let nearest = colors
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)));
            if let Some((i, _)) = nearest {
                counts[i] += 1;
            }
        }

        let total = counts.iter().sum::<u32>().max(1) as f32;
        counts.iter().map(|&count| count as f32 / total).collect()
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

// Colors less chromatic than this, in OKLCH, look grey; a cover with only
// such colors keeps the system's accent color
const MIN_ACCENT_CHROMA: f32 = 0.04;

// How much the share of the cover counts against the chroma, in choosing
// the accent color: a color that covers a sixteenth as much of the cover as
// another has to be twice as chromatic to win over it. Tuned on the covers
// of a library with `print_palettes` (`audio/cover_cache.rs`): above 0.25,
// a main color that's nearly grey won over a vivid title
const ACCENT_SHARE_WEIGHT: f32 = 0.25;

// The color of the palette that the accent color is made from, if any isn't
// grey: the most chromatic one, with the chroma of each color counting
// less the less of the cover it covers (`shares`, from `CoverImage::shares`),
// so that a small detail, like a red streak on a purple painting, only wins
// when it's much more chromatic. The stylesheet only keeps its hue and some
// of its chroma, at a lightness that white text can be read on
pub fn accent_color(palette: &[gdk::RGBA], shares: &[f32]) -> Option<gdk::RGBA> {
    palette
        .iter()
        .zip(shares)
        .map(|(color, share)| (oklch_chroma(color), share, color))
        .filter(|(chroma, _, _)| *chroma >= MIN_ACCENT_CHROMA)
        .map(|(chroma, share, color)| (chroma * share.powf(ACCENT_SHARE_WEIGHT), color))
        .max_by(|(a, _), (b, _)| a.total_cmp(b))
        .map(|(_, color)| *color)
}

// The chroma of an sRGB color in OKLCH: its distance from grey in OKLab
pub fn oklch_chroma(color: &gdk::RGBA) -> f32 {
    fn linear(c: f32) -> f32 {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    let (r, g, b) = (
        linear(color.red()),
        linear(color.green()),
        linear(color.blue()),
    );

    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();

    let a = 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s;
    let b = 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s;
    a.hypot(b)
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
    use gtk::{gdk, glib, prelude::*};

    use super::{CoverImage, accent_color, oklch_chroma};

    fn rgb(hex: u32) -> gdk::RGBA {
        let [_, r, g, b] = hex.to_be_bytes();
        gdk::RGBA::new(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            1.0,
        )
    }

    // The color the accent color is made from, for a palette and the share
    // of the cover of each of its colors
    fn accent(colors: &[(u32, f32)]) -> Option<u32> {
        let palette: Vec<gdk::RGBA> = colors.iter().map(|&(hex, _)| rgb(hex)).collect();
        let shares: Vec<f32> = colors.iter().map(|&(_, share)| share).collect();
        let accent = accent_color(&palette, &shares)?;
        colors
            .iter()
            .find(|&&(hex, _)| rgb(hex) == accent)
            .map(|&(hex, _)| hex)
    }

    #[test]
    fn chroma_is_measured_in_oklch() {
        assert!(oklch_chroma(&rgb(0xffffff)) < 0.001);
        assert!(oklch_chroma(&rgb(0x000000)) < 0.001);
        assert!(oklch_chroma(&rgb(0x777777)) < 0.001);
        // oklch(0.628 0.2577 29.23)
        assert!((oklch_chroma(&rgb(0xff0000)) - 0.2577).abs() < 0.001);
        // oklch(0.452 0.3132 264.05)
        assert!((oklch_chroma(&rgb(0x0000ff)) - 0.3132).abs() < 0.001);
    }

    #[test]
    fn grey_covers_have_no_accent_color() {
        assert_eq!(
            accent(&[(0x202020, 0.6), (0xd8d8d8, 0.3), (0x777779, 0.1)]),
            None
        );
        assert_eq!(accent(&[]), None);
    }

    // The palettes below are those of real covers, with their shares, as
    // `print_palettes` (`audio/cover_cache.rs`) prints them

    #[test]
    fn vivid_colors_win_over_muted_ones() {
        // Bortnyansky's Sacred Concertos, Vol. 1 (Polyansky): the main
        // color is red
        let palette = [
            (0xc83d2c, 0.589),
            (0x0f578f, 0.249),
            (0xdac8a2, 0.122),
            (0x958da4, 0.040),
        ];
        assert_eq!(accent(&palette), Some(0xc83d2c));

        // Borodin's Requiem (Geoffrey Simon): red lettering on pale green
        let palette = [
            (0xd2e0b0, 0.935),
            (0xa51736, 0.027),
            (0x6d8680, 0.035),
            (0x647464, 0.003),
        ];
        assert_eq!(accent(&palette), Some(0xa51736));
    }

    // Colors that cover little of the cover only win when they're much more
    // chromatic than the main ones
    #[test]
    fn small_details_dont_make_the_accent_color() {
        // Brahms's Requiem (Bernius): a purple painting with red streaks
        let palette = [
            (0x6e3163, 0.913),
            (0xd5cfd0, 0.045),
            (0xcf5c64, 0.006),
            (0xac84ac, 0.036),
        ];
        assert_eq!(accent(&palette), Some(0x6e3163));

        // The Krasiński Codex: teal, with a narrow strip of deep red on the
        // side, more chromatic than the teal
        let palette = [
            (0x2fa686, 0.802),
            (0xae302f, 0.101),
            (0xd3ab60, 0.058),
            (0x94d491, 0.039),
        ];
        assert_eq!(accent(&palette), Some(0x2fa686));

        // Bach's Goldberg Variations (Pietro De Maria): a grey photo with a
        // gold title, and a speck of red, which comes before the gold in
        // the palette
        let palette = [
            (0xadadad, 0.737),
            (0x1e2429, 0.095),
            (0x923e41, 0.005),
            (0x897443, 0.163),
        ];
        assert_eq!(accent(&palette), Some(0x897443));
    }

    // color-thief orders its palette by the pixels of each color times the
    // range of colors it stands for, so a small patch of many colors can
    // come before a flat background that covers most of the cover
    #[test]
    fn shares_follow_the_pixels_rather_than_the_palette() {
        let (width, height) = (100, 100);
        let pixels = (0..width * height)
            .flat_map(|i| {
                let (x, y) = (i % width, i / width);
                if y < 85 {
                    // A teal background, with a little noise
                    let noise = ((x * 7 + y * 13) % 3) as u8;
                    [40 + noise, 150 + noise, 125 + noise]
                } else {
                    // A strip of many oranges and browns
                    [
                        150 + (x % 80) as u8,
                        80 + ((x * 3 + y * 5) % 70) as u8,
                        50 + ((x * 5) % 60) as u8,
                    ]
                }
            })
            .collect();
        let image = CoverImage {
            width,
            height,
            has_alpha: false,
            pixels,
        };

        // The strip comes first in the palette, and is about as chromatic
        // as the teal
        let palette = image.palette().unwrap();
        assert!(palette[0].red() > palette[0].green());

        // The teal covers most of the cover, and makes the accent color
        let shares = image.shares(&palette);
        let accent = accent_color(&palette, &shares).unwrap();
        assert!(accent.green() > accent.red());
        let teal = palette.iter().position(|color| *color == accent).unwrap();
        assert!(shares[teal] > 0.8);
        assert!((shares.iter().sum::<f32>() - 1.0).abs() < 0.001);
    }

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
