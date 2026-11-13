//! Pictures for the window: game icons and covers decoded in pure Rust (`gm_core::picture`) into
//! premultiplied BGRA pixels, the form Direct2D bitmaps take, and the window's capture written as
//! a PNG. No Windows API is used here.
//!
//! - **Icons** (`decode_icons`): the core gives each icon's best image, a PNG or a one-image
//!   `.ico` file (`gm_core::icon`). It is made here once for each pixel size it is drawn at
//!   (`picture::fit`): untouched when it is exactly that size, else scaled with Catmull-Rom,
//!   premultiplied first so transparent edges stay clean. A picture (an Xbox logo) is fitted too.
//! - **Covers** (`decode_covers`) are the stores' original files, decoded at their full size. The
//!   renderer scales each into its tile as it draws it (`el::El::fill`), so covers don't depend on
//!   the display's scaling.
//!
//! Decoding runs on the background thread that read the bytes, so the UI thread only uploads
//! ready pixels.

use std::path::Path;
use std::sync::Arc;

use gm_core::picture::{self, Rgba};

/// Decoded pixels: premultiplied BGRA, `width * 4` bytes a row.
#[derive(Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Picture {
    /// Premultiplied RGBA in the order Direct2D takes: blue and red swapped.
    fn from_premultiplied(picture: Rgba) -> Self {
        let mut pixels = picture.pixels;
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        Self {
            width: picture.width,
            height: picture.height,
            pixels,
        }
    }
}

/// Decodes the core's icons keyed by id (PNGs, `.ico` files or pictures) for both of `sizes`, in
/// pixels. Icons that can't be read are left out.
pub fn decode_icons(
    items: Vec<(String, Arc<Vec<u8>>)>,
    sizes: [u32; 2],
) -> Vec<(String, [Picture; 2])> {
    items
        .into_iter()
        .filter_map(|(id, bytes)| {
            let small = decode_icon(&bytes, sizes[0])?;
            let large = decode_icon(&bytes, sizes[1])?;
            Some((id, [small, large]))
        })
        .collect()
}

/// An icon `size` pixels big (see the module's doc).
fn decode_icon(bytes: &[u8], size: u32) -> Option<Picture> {
    let image = picture::icon(bytes, size).ok()?;
    let fitted = picture::fit(image, size).ok()?;
    Some(Picture::from_premultiplied(fitted))
}

/// Decodes cover art keyed by id (the stores' original files, or the user's own) at full size.
/// What is no picture is left out.
pub fn decode_covers(items: Vec<(String, Vec<u8>)>) -> Vec<(String, Picture)> {
    items
        .into_iter()
        .filter_map(|(id, bytes)| {
            let straight = picture::decode(&bytes).ok()?;
            let premultiplied = picture::premultiply(straight).ok()?;
            Some((id, Picture::from_premultiplied(premultiplied)))
        })
        .collect()
}

/// Writes premultiplied BGRA pixels to `path` as a PNG (the window's frame capture for checks,
/// `shell::WM_CAPTURE`).
pub fn save_png(pixels: &[u8], width: u32, height: u32, path: &Path) -> std::io::Result<()> {
    let mut rgba = pixels.to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let premultiplied = Rgba {
        width,
        height,
        pixels: rgba,
    };
    let straight = picture::unpremultiply(premultiplied).map_err(std::io::Error::other)?;
    let png = picture::encode_png(&straight).map_err(std::io::Error::other)?;
    std::fs::write(path, png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, bgra: [u8; 4]) -> Picture {
        Picture {
            width,
            height,
            pixels: bgra.repeat((width * height) as usize),
        }
    }

    /// A 32-bit bitmap `side` pixels square for an `.ico` file, with its (opaque) mask:
    /// vertical stripes of `colors`, one pixel wide each.
    fn stripes(side: u32, colors: &[[u8; 4]]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend(40u32.to_le_bytes());
        data.extend((side as i32).to_le_bytes());
        data.extend((side as i32 * 2).to_le_bytes());
        data.extend(1u16.to_le_bytes());
        data.extend(32u16.to_le_bytes());
        data.extend([0u8; 24]);
        for _ in 0..side {
            for x in 0..side {
                data.extend(colors[x as usize % colors.len()]);
            }
        }
        data.extend(vec![0u8; (side.div_ceil(32) * 4 * side) as usize]);
        data
    }

    /// An `.ico` file holding `images` (bitmaps `side` pixels square).
    fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut file = Vec::new();
        file.extend(0u16.to_le_bytes());
        file.extend(1u16.to_le_bytes());
        file.extend((images.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * images.len();
        for (side, data) in images {
            file.extend([*side as u8, *side as u8, 0, 0]);
            file.extend(1u16.to_le_bytes());
            file.extend(32u16.to_le_bytes());
            file.extend((data.len() as u32).to_le_bytes());
            file.extend((offset as u32).to_le_bytes());
            offset += data.len();
        }
        for (_, data) in images {
            file.extend(data);
        }
        file
    }

    #[test]
    fn an_icon_of_the_size_asked_for_is_used_pixel_for_pixel() {
        // Bitmaps store blue, green, red, alpha, as Direct2D takes them.
        let (red, blue) = ([0, 0, 255, 255], [255, 0, 0, 255]);
        let bytes = Arc::new(ico(&[
            (16, stripes(16, &[red, blue])),
            (48, stripes(48, &[blue])),
        ]));
        let items = vec![
            ("game".to_owned(), Arc::clone(&bytes)),
            ("broken".to_owned(), Arc::new(b"not an icon".to_vec())),
        ];
        let icons = decode_icons(items, [16, 32]);
        assert_eq!(icons.len(), 1);
        let [small, large] = &icons[0].1;
        // 16 px: the 16 px image untouched; any scaling would blend its one-pixel stripes.
        assert_eq!((small.width, small.height), (16, 16));
        for (index, pixel) in small.pixels.chunks(4).enumerate() {
            let expected = if index % 2 == 0 { red } else { blue };
            assert_eq!(pixel, expected, "pixel {index}");
        }
        // 32 px: scaled once from the next larger image, the 48 px one.
        assert_eq!((large.width, large.height), (32, 32));
        assert_eq!(large.pixels[(16 * 32 + 16) * 4..][..4], blue);
        // Larger than any image: the largest, scaled up.
        let up = decode_icons(vec![("game".to_owned(), bytes)], [64, 64]);
        assert_eq!((up[0].1[0].width, up[0].1[0].height), (64, 64));
    }

    #[test]
    fn covers_are_decoded_at_their_full_size_and_the_capture_reads_back() {
        // A 40 × 60 picture written as the capture writes, read back as a cover.
        let file = std::env::temp_dir().join(format!("gm-app-cover-{}.png", std::process::id()));
        let mut source = solid(40, 60, [30, 60, 200, 255]);
        // One half-transparent pixel, premultiplied: it comes back the same.
        source.pixels[..4].copy_from_slice(&[15, 30, 100, 128]);
        save_png(&source.pixels, 40, 60, &file).unwrap();
        let bytes = std::fs::read(&file).unwrap();
        std::fs::remove_file(&file).unwrap();

        let items = vec![
            ("steam:220".to_owned(), bytes),
            ("ea:1".to_owned(), b"not a picture".to_vec()),
        ];
        let covers = decode_covers(items);
        assert_eq!(covers.len(), 1);
        let (id, cover) = &covers[0];
        assert_eq!(id, "steam:220");
        assert_eq!((cover.width, cover.height), (40, 60));
        assert_eq!(cover.pixels[(15 * 40 + 10) * 4..][..4], [30, 60, 200, 255]);
        let corner = &cover.pixels[..4];
        assert!(
            corner
                .iter()
                .zip([15u8, 30, 100, 128])
                .all(|(a, b)| a.abs_diff(b) <= 1),
            "{corner:?}"
        );
    }
}
