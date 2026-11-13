//! Pictures in pure Rust: game icons, cover art and the window's capture, read and written with no
//! Windows API, so the same code reads them on any system.
//!
//! - **Formats** (`format`): JPEG (zune-jpeg), PNG (png), WebP (image-webp, lossy and lossless),
//!   BMP, and `.ico` files, whose images are PNGs or bitmaps. Bitmaps (DIBs) are read here
//!   (`decode_dib`): BI_RGB at 1, 4, 8, 16, 24 and 32 bits, and BI_BITFIELDS; compressed ones (RLE)
//!   are refused.
//! - **Sizes** (`size`) come from the headers alone, so a cover's size is known without decoding
//!   it.
//! - **Decoding** (`decode`) gives straight (not premultiplied) RGBA, 8 bits a channel, rows top to
//!   bottom. A picture over `MAX_SIDE` a side or `MAX_PIXELS` in all is refused before anything is
//!   allocated for it.
//! - **Transparency in bitmaps**: a 32-bit bitmap's fourth byte is its alpha when any pixel has
//!   some; when all are 0 it is unused, and the picture is opaque. An icon's bitmap is followed by
//!   its AND mask (one bit a pixel, set where it is transparent), which counts when the bitmap has
//!   no alpha, as Windows draws icons.
//! - **Icons** (`icon`): an `.ico` file gives the image `icon::pick` chooses for a size. `fit`
//!   then makes it that many pixels, premultiplied first so transparent edges stay clean, with
//!   Catmull-Rom (fast_image_resize), which rings less than Lanczos around an icon's hard edges.
//!   An image already that size is only premultiplied: pixel for pixel.
//! - **Writing**: `encode_png` only, for the window's capture. Nothing else is ever re-encoded:
//!   covers are kept as the stores' files are (`covers.rs`).

use crate::icon;
use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, MulDiv, PixelType, ResizeAlg, ResizeOptions, Resizer};
use std::fmt;
use std::io::Cursor;
use thiserror::Error;

/// Larger pictures are refused before they are decoded.
pub const MAX_SIDE: u32 = 16_384;
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// What a picture file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
    Bmp,
    Ico,
}

impl Format {
    /// The file extension a picture of this format is kept under.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Jpeg => "jpg",
            Format::Png => "png",
            Format::Webp => "webp",
            Format::Bmp => "bmp",
            Format::Ico => "ico",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Format::Jpeg => "JPEG",
            Format::Png => "PNG",
            Format::Webp => "WebP",
            Format::Bmp => "BMP",
            Format::Ico => "ICO",
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PictureError {
    /// None of the formats this reads.
    #[error("not a picture Game Manager reads (JPEG, PNG, WebP, BMP or ICO)")]
    UnknownFormat,
    #[error("a {width} × {height} picture is too large")]
    TooLarge { width: u32, height: u32 },
    /// Cut off, damaged, or a variant this doesn't read (a compressed bitmap).
    #[error("the {format} picture can't be read: {reason}")]
    Damaged { format: Format, reason: String },
    #[error("the picture could not be scaled: {0}")]
    Scale(String),
    #[error("the PNG could not be written: {0}")]
    Encode(String),
}

fn damaged(format: Format, reason: impl fmt::Display) -> PictureError {
    PictureError::Damaged {
        format,
        reason: reason.to_string(),
    }
}

/// Pixels, 8 bits a channel in R, G, B, A order, `width * 4` bytes a row, rows top to bottom.
/// Straight alpha from `decode` and `icon`; premultiplied from `premultiply` and `fit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Rgba {
    fn new(width: u32, height: u32, pixels: Vec<u8>, format: Format) -> Result<Self, PictureError> {
        if pixels.len() as u64 != u64::from(width) * u64::from(height) * 4 {
            return Err(damaged(format, "its pixels don't match its size"));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn i32_at(bytes: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The format `bytes` are in, from their first bytes.
pub fn format(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if icon::is_png(bytes) {
        Some(Format::Png)
    } else if bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(Format::Webp)
    } else if bytes.starts_with(b"BM") && u32_at(bytes, 14).is_some_and(|header| header >= 40) {
        Some(Format::Bmp)
    } else if icon::is_ico(bytes) && u16_at(bytes, 4).is_some_and(|count| count > 0) {
        Some(Format::Ico)
    } else {
        None
    }
}

/// A picture's width and height from its headers, without decoding it. For an `.ico` file, its
/// largest image's.
pub fn size(bytes: &[u8]) -> Option<(u32, u32)> {
    let size = match format(bytes)? {
        Format::Jpeg => jpeg_size(bytes),
        Format::Png => (bytes.get(12..16) == Some(b"IHDR")).then(|| {
            let be = |at: usize| {
                u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
            };
            (be(16), be(20))
        }),
        Format::Webp => image_webp::WebPDecoder::new(Cursor::new(bytes))
            .ok()
            .map(|decoder| decoder.dimensions()),
        Format::Bmp => dib_size(bytes.get(14..)?, false),
        Format::Ico => icon::images(bytes)
            .into_iter()
            .map(|image| (image.width, image.height))
            .max_by_key(|(width, height)| u64::from(*width) * u64::from(*height)),
    };
    size.filter(|(width, height)| *width > 0 && *height > 0)
}

/// The width and height in a JPEG's frame header (the first `SOFn` segment).
pub(crate) fn jpeg_size(jpeg: &[u8]) -> Option<(u32, u32)> {
    let mut rest = jpeg.strip_prefix(&[0xFF, 0xD8])?;
    loop {
        let [0xFF, marker, after @ ..] = rest else {
            return None;
        };
        match *marker {
            // A fill byte before a marker, then markers that stand alone.
            0xFF => {
                rest = &rest[1..];
                continue;
            }
            0x01 | 0xD0..=0xD9 => {
                rest = after;
                continue;
            }
            _ => {}
        }
        let length = usize::from(u16::from_be_bytes([*after.first()?, *after.get(1)?]));
        // Start of frame: every C0 to CF but the tables among them.
        if matches!(*marker, 0xC0..=0xCF) && !matches!(*marker, 0xC4 | 0xC8 | 0xCC) {
            let height = u16::from_be_bytes([*after.get(3)?, *after.get(4)?]);
            let width = u16::from_be_bytes([*after.get(5)?, *after.get(6)?]);
            return Some((u32::from(width), u32::from(height)));
        }
        rest = after.get(length..)?;
    }
}

fn check_size(width: u32, height: u32) -> Result<(), PictureError> {
    if width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(PictureError::TooLarge { width, height });
    }
    Ok(())
}

/// The picture in `bytes`, as straight RGBA. For an `.ico` file, its best image (`icon`).
pub fn decode(bytes: &[u8]) -> Result<Rgba, PictureError> {
    let format = format(bytes).ok_or(PictureError::UnknownFormat)?;
    if format == Format::Ico {
        return icon(bytes, u32::MAX);
    }
    let (width, height) = size(bytes).ok_or_else(|| damaged(format, "its header has no size"))?;
    check_size(width, height)?;
    match format {
        Format::Jpeg => decode_jpeg(bytes),
        Format::Png => decode_png(bytes),
        Format::Webp => decode_webp(bytes),
        Format::Bmp => {
            let offset =
                u32_at(bytes, 10).ok_or_else(|| damaged(format, "no file header"))? as usize;
            let dib = &bytes[14..];
            decode_dib(dib, Some(offset.saturating_sub(14)), false, format)
        }
        Format::Ico => icon(bytes, u32::MAX),
    }
}

/// An icon for `size` pixels: from an `.ico` file, the image `icon::pick` chooses (a PNG, or a
/// bitmap with its mask); any other picture as it is. Straight RGBA, not yet scaled (`fit`).
pub fn icon(bytes: &[u8], size: u32) -> Result<Rgba, PictureError> {
    if format(bytes) != Some(Format::Ico) {
        return decode(bytes);
    }
    let picked = icon::pick(&icon::images(bytes), size)
        .ok_or_else(|| damaged(Format::Ico, "it has no image this reads"))?;
    let data = icon::image_data(bytes, picked.index)
        .ok_or_else(|| damaged(Format::Ico, "its image is cut off"))?;
    check_size(picked.width, picked.height)?;
    if icon::is_png(data) {
        decode_png(data)
    } else {
        decode_dib(data, None, true, Format::Ico)
    }
}

fn decode_jpeg(bytes: &[u8]) -> Result<Rgba, PictureError> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(MAX_SIDE as usize)
        .set_max_height(MAX_SIDE as usize);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    let pixels = decoder
        .decode()
        .map_err(|error| damaged(Format::Jpeg, format!("{error:?}")))?;
    let (width, height) = decoder
        .dimensions()
        .ok_or_else(|| damaged(Format::Jpeg, "no size after decoding"))?;
    Rgba::new(width as u32, height as u32, pixels, Format::Jpeg)
}

fn decode_png(bytes: &[u8]) -> Result<Rgba, PictureError> {
    let fail = |error: png::DecodingError| damaged(Format::Png, error);
    // Enough for the largest picture allowed, at 16 bits a channel.
    let limits = png::Limits {
        bytes: (MAX_PIXELS * 8) as usize,
    };
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(bytes), limits);
    // Palettes expanded, 16 bits brought down to 8, alpha added where there is none.
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().map_err(fail)?;
    let (width, height) = (reader.info().width, reader.info().height);
    check_size(width, height)?;
    let length = reader
        .output_buffer_size()
        .ok_or_else(|| damaged(Format::Png, "too large"))?;
    let mut buffer = vec![0; length];
    let frame = reader.next_frame(&mut buffer).map_err(fail)?;
    buffer.truncate(frame.buffer_size());
    let pixels = match frame.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::GrayscaleAlpha => buffer
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
            .collect(),
        png::ColorType::Rgb => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
        png::ColorType::Grayscale => buffer
            .iter()
            .flat_map(|&grey| [grey, grey, grey, 255])
            .collect(),
        png::ColorType::Indexed => {
            return Err(damaged(Format::Png, "its palette was not expanded"));
        }
    };
    Rgba::new(frame.width, frame.height, pixels, Format::Png)
}

fn decode_webp(bytes: &[u8]) -> Result<Rgba, PictureError> {
    let fail = |error: image_webp::DecodingError| damaged(Format::Webp, error);
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(fail)?;
    let (width, height) = decoder.dimensions();
    check_size(width, height)?;
    let length = decoder
        .output_buffer_size()
        .ok_or_else(|| damaged(Format::Webp, "too large"))?;
    let mut buffer = vec![0; length];
    decoder.read_image(&mut buffer).map_err(fail)?;
    // Lossy pictures without alpha come out as RGB.
    let pixels = if decoder.has_alpha() {
        buffer
    } else {
        buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect()
    };
    Rgba::new(width, height, pixels, Format::Webp)
}

/// A bitmap's width and height from its info header; an icon's height counts its mask too.
fn dib_size(dib: &[u8], icon: bool) -> Option<(u32, u32)> {
    if u32_at(dib, 0)? < 40 {
        return None;
    }
    let width = u32::try_from(i32_at(dib, 4)?).ok()?;
    let height = i32_at(dib, 8)?.unsigned_abs();
    Some((width, if icon { height / 2 } else { height }))
}

/// One channel's value under `mask`, brought to 8 bits.
fn masked(value: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let top = u64::from(mask >> shift);
    let channel = u64::from((value & mask) >> shift);
    ((channel * 255 + top / 2) / top) as u8
}

/// A device-independent bitmap: a BMP file's after its 14-byte file header, or an icon's image.
/// `pixels_at` is where the pixel rows start, counted from the info header (a BMP file says so);
/// `None` when they follow the header, its colour masks and its palette, as in an icon. `icon`:
/// the height counts the AND mask too, which follows the rows (see the module's doc).
fn decode_dib(
    dib: &[u8],
    pixels_at: Option<usize>,
    icon: bool,
    format: Format,
) -> Result<Rgba, PictureError> {
    let bad = |reason: &str| damaged(format, reason);
    let header = u32_at(dib, 0).ok_or_else(|| bad("no bitmap header"))? as usize;
    let (width, height) =
        dib_size(dib, icon).ok_or_else(|| bad("its bitmap header is not one this reads"))?;
    if width == 0 || height == 0 {
        return Err(bad("it has no pixels"));
    }
    check_size(width, height)?;
    let top_down = i32_at(dib, 8).is_some_and(|height| height < 0);
    let bits = u16_at(dib, 14).ok_or_else(|| bad("no bit count"))?;
    let compression = u32_at(dib, 16).ok_or_else(|| bad("no compression"))?;
    let used = u32_at(dib, 32).unwrap_or(0);

    // Where the colour masks, palette and pixels are.
    let mut at = header;
    let masks: [u32; 4] = match (compression, bits) {
        (0, 1 | 4 | 8 | 24) => [0; 4],
        (0, 16) => [0x7C00, 0x03E0, 0x001F, 0],
        (0, 32) => [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000],
        // BI_BITFIELDS and BI_ALPHABITFIELDS: in the header from version 2 (52 bytes) on, else
        // just after it.
        (3 | 6, 16 | 32) => {
            let read = |at: usize| u32_at(dib, at).unwrap_or(0);
            if header >= 52 {
                [
                    read(40),
                    read(44),
                    read(48),
                    if header >= 56 { read(52) } else { 0 },
                ]
            } else {
                let count = if compression == 6 { 4 } else { 3 };
                let masks = [
                    read(at),
                    read(at + 4),
                    read(at + 8),
                    if count == 4 { read(at + 12) } else { 0 },
                ];
                at += 4 * count;
                masks
            }
        }
        _ => {
            return Err(bad(
                "compressed bitmaps (RLE, JPEG, PNG) and this bit count are not read",
            ));
        }
    };
    let palette: Vec<[u8; 3]> = if bits <= 8 {
        let full = 1usize << bits;
        let count = if used == 0 {
            full
        } else {
            (used as usize).min(full)
        };
        let table = dib
            .get(at..at + count * 4)
            .ok_or_else(|| bad("its palette is cut off"))?;
        at += count * 4;
        table
            .as_chunks::<4>()
            .0
            .iter()
            .map(|entry| [entry[2], entry[1], entry[0]])
            .collect()
    } else {
        Vec::new()
    };
    let start = pixels_at.unwrap_or(at);
    let (width, height) = (width as usize, height as usize);
    let stride = (width * usize::from(bits)).div_ceil(32) * 4;
    let rows = dib
        .get(start..start + stride * height)
        .ok_or_else(|| bad("its pixels are cut off"))?;

    let mut pixels = vec![0u8; width * height * 4];
    // Only 16- and 32-bit pixels can carry alpha, under its mask.
    let alpha_channel = matches!(bits, 16 | 32) && masks[3] != 0;
    let mut any_alpha = false;
    for y in 0..height {
        let row = &rows[y * stride..(y + 1) * stride];
        let out = if top_down { y } else { height - 1 - y };
        for x in 0..width {
            let rgba = match bits {
                1 | 4 | 8 => {
                    let bit = x * usize::from(bits);
                    let shift = 8 - usize::from(bits) - bit % 8;
                    let index = (row[bit / 8] >> shift) & ((1u16 << bits) - 1) as u8;
                    let [r, g, b] = palette.get(usize::from(index)).copied().unwrap_or([0; 3]);
                    [r, g, b, 255]
                }
                24 => {
                    let pixel = &row[x * 3..x * 3 + 3];
                    [pixel[2], pixel[1], pixel[0], 255]
                }
                16 => {
                    let value = u32::from(u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]));
                    masks.map(|mask| masked(value, mask))
                }
                _ => {
                    let value = u32::from_le_bytes([
                        row[x * 4],
                        row[x * 4 + 1],
                        row[x * 4 + 2],
                        row[x * 4 + 3],
                    ]);
                    masks.map(|mask| masked(value, mask))
                }
            };
            any_alpha |= alpha_channel && rgba[3] > 0;
            let at = (out * width + x) * 4;
            pixels[at..at + 4].copy_from_slice(&rgba);
        }
    }
    // A fourth byte that is 0 everywhere is unused: the picture is opaque.
    let has_alpha = any_alpha;
    if !has_alpha {
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
    }
    // An icon without alpha takes its transparency from its AND mask, when it has one.
    if icon && !has_alpha {
        let mask_stride = width.div_ceil(32) * 4;
        let mask_at = start + stride * height;
        if let Some(mask) = dib.get(mask_at..mask_at + mask_stride * height) {
            for y in 0..height {
                let row = &mask[y * mask_stride..(y + 1) * mask_stride];
                let out = if top_down { y } else { height - 1 - y };
                for x in 0..width {
                    if row[x / 8] & (0x80 >> (x % 8)) != 0 {
                        let at = (out * width + x) * 4;
                        pixels[at..at + 4].copy_from_slice(&[0; 4]);
                    }
                }
            }
        }
    }
    Rgba::new(width as u32, height as u32, pixels, format)
}

/// The size a `width` × `height` picture takes inside `size` × `size` pixels, its shape kept.
pub fn fitted(width: u32, height: u32, size: u32) -> (u32, u32) {
    let scale = f64::from(size) / f64::from(width.max(height).max(1));
    let side = |value: u32| ((f64::from(value) * scale).round() as u32).max(1);
    (side(width), side(height))
}

fn image(picture: Rgba) -> Result<Image<'static>, PictureError> {
    Image::from_vec_u8(
        picture.width,
        picture.height,
        picture.pixels,
        PixelType::U8x4,
    )
    .map_err(|error| PictureError::Scale(error.to_string()))
}

fn rgba(image: Image<'static>) -> Rgba {
    Rgba {
        width: image.width(),
        height: image.height(),
        pixels: image.into_vec(),
    }
}

/// Straight RGBA made premultiplied: each colour scaled by its alpha.
pub fn premultiply(picture: Rgba) -> Result<Rgba, PictureError> {
    let mut image = image(picture)?;
    MulDiv::new()
        .multiply_alpha_inplace(&mut image)
        .map_err(|error| PictureError::Scale(error.to_string()))?;
    Ok(rgba(image))
}

/// Premultiplied RGBA made straight again (for writing a PNG).
pub fn unpremultiply(picture: Rgba) -> Result<Rgba, PictureError> {
    let mut image = image(picture)?;
    MulDiv::new()
        .divide_alpha_inplace(&mut image)
        .map_err(|error| PictureError::Scale(error.to_string()))?;
    Ok(rgba(image))
}

/// A straight-RGBA picture inside `size` × `size` pixels, its shape kept, premultiplied: as it is
/// when it already has that size, else scaled with Catmull-Rom (see the module's doc).
pub fn fit(picture: Rgba, size: u32) -> Result<Rgba, PictureError> {
    let (width, height) = fitted(picture.width, picture.height, size);
    let premultiplied = premultiply(picture)?;
    if (width, height) == (premultiplied.width, premultiplied.height) {
        return Ok(premultiplied);
    }
    let source = image(premultiplied)?;
    let mut target = Image::new(width, height, PixelType::U8x4);
    let mut options =
        ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::CatmullRom));
    // Already premultiplied.
    options.mul_div_alpha = false;
    Resizer::new()
        .resize(&source, &mut target, &options)
        .map_err(|error| PictureError::Scale(error.to_string()))?;
    Ok(rgba(target))
}

/// Straight RGBA written as a PNG file's bytes.
pub fn encode_png(picture: &Rgba) -> Result<Vec<u8>, PictureError> {
    let fail = |error: png::EncodingError| PictureError::Encode(error.to_string());
    let mut file = Vec::new();
    let mut encoder = png::Encoder::new(&mut file, picture.width, picture.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(fail)?;
    writer.write_image_data(&picture.pixels).map_err(fail)?;
    writer.finish().map_err(fail)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Rgba {
        Rgba {
            width,
            height,
            pixels: rgba.repeat((width * height) as usize),
        }
    }

    fn pixel(picture: &Rgba, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * picture.width + x) * 4) as usize;
        picture.pixels[at..at + 4].try_into().unwrap()
    }

    /// A JPEG every Windows has.
    fn system_jpeg() -> Vec<u8> {
        let root = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        std::fs::read(root.join(r"Web\Screen\img100.jpg")).unwrap()
    }

    fn webp(picture: &Rgba, color: image_webp::ColorType) -> Vec<u8> {
        let data: Vec<u8> = match color {
            image_webp::ColorType::Rgb8 => picture
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect(),
            _ => picture.pixels.clone(),
        };
        let mut file = Vec::new();
        image_webp::WebPEncoder::new(&mut file)
            .encode(&data, picture.width, picture.height, color)
            .unwrap();
        file
    }

    /// A PNG of another colour type, with a palette and its transparency when given.
    fn png_of(
        width: u32,
        height: u32,
        color: png::ColorType,
        depth: png::BitDepth,
        data: &[u8],
        palette: Option<(&[u8], &[u8])>,
    ) -> Vec<u8> {
        let mut file = Vec::new();
        let mut encoder = png::Encoder::new(&mut file, width, height);
        encoder.set_color(color);
        encoder.set_depth(depth);
        if let Some((rgb, alpha)) = palette {
            encoder.set_palette(rgb.to_vec());
            encoder.set_trns(alpha.to_vec());
        }
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
        file
    }

    /// A bitmap's info header (40 bytes), its palette (B, G, R, 0 each) and `rows` as they are
    /// stored (bottom row first unless `height` is negative); `mask` follows for an icon.
    fn dib(
        width: i32,
        height: i32,
        bits: u16,
        compression: u32,
        palette: &[[u8; 3]],
        rows: &[u8],
        mask: &[u8],
    ) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend(40u32.to_le_bytes());
        data.extend(width.to_le_bytes());
        data.extend(height.to_le_bytes());
        data.extend(1u16.to_le_bytes());
        data.extend(bits.to_le_bytes());
        data.extend(compression.to_le_bytes());
        data.extend([0u8; 12]);
        data.extend((palette.len() as u32).to_le_bytes());
        data.extend([0u8; 4]);
        for [r, g, b] in palette {
            data.extend([*b, *g, *r, 0]);
        }
        data.extend(rows);
        data.extend(mask);
        data
    }

    /// A BMP file around a bitmap: the file header points at its pixels.
    fn bmp_file(dib: &[u8], pixels_at: usize) -> Vec<u8> {
        let mut file = b"BM".to_vec();
        file.extend(((14 + dib.len()) as u32).to_le_bytes());
        file.extend([0u8; 4]);
        file.extend(((14 + pixels_at) as u32).to_le_bytes());
        file.extend(dib);
        file
    }

    /// An `.ico` file holding `images` as they are.
    fn ico_of(images: &[&[u8]]) -> Vec<u8> {
        let mut file = vec![0, 0, 1, 0];
        file.extend((images.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * images.len();
        for data in images {
            file.extend([0, 0, 0, 0, 1, 0, 32, 0]);
            file.extend((data.len() as u32).to_le_bytes());
            file.extend((offset as u32).to_le_bytes());
            offset += data.len();
        }
        for data in images {
            file.extend(*data);
        }
        file
    }

    /// A `side` × `side` 32-bit icon bitmap of one colour, with an all-opaque mask.
    fn icon_dib(side: u32, rgba: [u8; 4]) -> Vec<u8> {
        let [r, g, b, a] = rgba;
        let rows = [b, g, r, a].repeat((side * side) as usize);
        let mask = vec![0u8; (side.div_ceil(32) * 4 * side) as usize];
        dib(side as i32, side as i32 * 2, 32, 0, &[], &rows, &mask)
    }

    #[test]
    fn formats_and_sizes_come_from_the_headers() {
        let png = encode_png(&solid(3, 2, RED)).unwrap();
        assert_eq!(
            (format(&png), size(&png)),
            (Some(Format::Png), Some((3, 2)))
        );
        let lossless = webp(&solid(4, 5, RED), image_webp::ColorType::Rgba8);
        assert_eq!(
            (format(&lossless), size(&lossless)),
            (Some(Format::Webp), Some((4, 5)))
        );
        let jpeg = system_jpeg();
        let (width, height) = size(&jpeg).unwrap();
        assert_eq!(format(&jpeg), Some(Format::Jpeg));
        let decoded = decode(&jpeg).unwrap();
        assert_eq!((decoded.width, decoded.height), (width, height));
        let bitmap = dib(2, 3, 24, 0, &[], &[0; 24], &[]);
        let bmp = bmp_file(&bitmap, 40);
        assert_eq!(
            (format(&bmp), size(&bmp)),
            (Some(Format::Bmp), Some((2, 3)))
        );
        let ico = ico_of(&[&icon_dib(16, RED), &icon_dib(48, BLUE)]);
        assert_eq!(
            (format(&ico), size(&ico)),
            (Some(Format::Ico), Some((48, 48)))
        );
        for junk in [
            &b"<html>Not found</html>"[..],
            b"",
            b"RIFF1234WAVE",
            &[0, 0, 1, 0, 0, 0],
        ] {
            assert_eq!((format(junk), size(junk)), (None, None), "{junk:?}");
        }
        assert_eq!(Format::Webp.extension(), "webp");
    }

    #[test]
    fn reads_a_jpegs_size() {
        // SOI, an APP0 segment, a quantization table, then a baseline frame of 300 × 450.
        let mut jpeg = vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 1, 2, 0xFF, 0xDB, 0x00, 0x03, 9,
        ];
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 8, 0x01, 0xC2, 0x01, 0x2C, 3]);
        assert_eq!(jpeg_size(&jpeg), Some((300, 450)));
        // A progressive frame after a fill byte.
        let progressive = [
            0xFF, 0xD8, 0xFF, 0xFF, 0xC2, 0x00, 0x11, 8, 0x02, 0x58, 0x01, 0x90,
        ];
        assert_eq!(jpeg_size(&progressive), Some((400, 600)));
        assert_eq!(jpeg_size(b"\x89PNG\r\n"), None);
        assert_eq!(jpeg_size(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x40, 1]), None);
        assert_eq!(jpeg_size(&[0xFF, 0xD8]), None);
        assert_eq!(jpeg_size(&[]), None);
    }

    #[test]
    fn every_png_colour_type_becomes_rgba() {
        let picture = Rgba {
            width: 2,
            height: 1,
            pixels: vec![255, 0, 0, 255, 0, 255, 0, 128],
        };
        assert_eq!(decode(&encode_png(&picture).unwrap()).unwrap(), picture);
        let eight = png::BitDepth::Eight;
        let grey = png_of(2, 1, png::ColorType::Grayscale, eight, &[0, 200], None);
        assert_eq!(
            decode(&grey).unwrap().pixels,
            [0, 0, 0, 255, 200, 200, 200, 255]
        );
        let grey_alpha = png_of(1, 1, png::ColorType::GrayscaleAlpha, eight, &[90, 40], None);
        assert_eq!(decode(&grey_alpha).unwrap().pixels, [90, 90, 90, 40]);
        let rgb = png_of(1, 1, png::ColorType::Rgb, eight, &[1, 2, 3], None);
        assert_eq!(decode(&rgb).unwrap().pixels, [1, 2, 3, 255]);
        // A palette with its transparency; 16 bits a channel come down to 8.
        let indexed = png_of(
            2,
            1,
            png::ColorType::Indexed,
            eight,
            &[1, 0],
            Some((&[10, 20, 30, 40, 50, 60], &[255, 0])),
        );
        assert_eq!(
            decode(&indexed).unwrap().pixels,
            [40, 50, 60, 0, 10, 20, 30, 255]
        );
        let deep = png_of(
            1,
            1,
            png::ColorType::Rgba,
            png::BitDepth::Sixteen,
            &[255, 0, 128, 0, 0, 0, 255, 255],
            None,
        );
        assert_eq!(decode(&deep).unwrap().pixels, [255, 128, 0, 255]);
    }

    #[test]
    fn webp_with_and_without_alpha() {
        let picture = Rgba {
            width: 2,
            height: 2,
            pixels: [RED, BLUE, [0, 255, 0, 100], [9, 9, 9, 0]].concat(),
        };
        let with_alpha = decode(&webp(&picture, image_webp::ColorType::Rgba8)).unwrap();
        // Lossless keeps the colours of the pixels it shows.
        assert_eq!(with_alpha.width, 2);
        assert_eq!(&with_alpha.pixels[..12], &picture.pixels[..12]);
        assert_eq!(with_alpha.pixels[15], 0);
        let opaque = decode(&webp(&solid(3, 1, BLUE), image_webp::ColorType::Rgb8)).unwrap();
        assert_eq!(opaque, solid(3, 1, BLUE));
    }

    #[test]
    fn bitmaps_at_every_depth() {
        // 2 × 2, stored bottom row first; the picture's top row is red, blue.
        let top_bottom = |picture: &Rgba| {
            [
                pixel(picture, 0, 0),
                pixel(picture, 1, 0),
                pixel(picture, 0, 1),
                pixel(picture, 1, 1),
            ]
        };
        let white = [255, 255, 255, 255];
        let black = [0, 0, 0, 255];
        // 24 bits: rows padded to 4 bytes.
        let rows24 = [255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 255, 255, 0, 0, 0, 0];
        let bmp = bmp_file(&dib(2, 2, 24, 0, &[], &rows24, &[]), 40);
        assert_eq!(
            top_bottom(&decode(&bmp).unwrap()),
            [RED, BLUE, white, black]
        );
        // Top-down rows.
        let rows_down = [0, 0, 255, 255, 0, 0, 0, 0, 255, 255, 255, 0, 0, 0, 0, 0];
        let bmp = bmp_file(&dib(2, -2, 24, 0, &[], &rows_down, &[]), 40);
        assert_eq!(
            top_bottom(&decode(&bmp).unwrap()),
            [RED, BLUE, white, black]
        );
        // 8, 4 and 1 bits through a palette.
        let palette = [[255, 255, 255], [0, 0, 0], [255, 0, 0], [0, 0, 255]];
        let rows8 = [0, 1, 0, 0, 2, 3, 0, 0];
        let bmp = bmp_file(&dib(2, 2, 8, 0, &palette, &rows8, &[]), 40 + 16);
        assert_eq!(
            top_bottom(&decode(&bmp).unwrap()),
            [RED, BLUE, white, black]
        );
        let rows4 = [0x01, 0, 0, 0, 0x23, 0, 0, 0];
        let bmp = bmp_file(&dib(2, 2, 4, 0, &palette, &rows4, &[]), 40 + 16);
        assert_eq!(
            top_bottom(&decode(&bmp).unwrap()),
            [RED, BLUE, white, black]
        );
        let rows1 = [0b0100_0000, 0, 0, 0, 0b1000_0000, 0, 0, 0];
        let bmp = bmp_file(&dib(2, 2, 1, 0, &palette[..2], &rows1, &[]), 40 + 8);
        assert_eq!(
            top_bottom(&decode(&bmp).unwrap()),
            [black, white, white, black]
        );
        // 16 bits, 5-5-5: pure red and blue.
        let rows16 = [0xFF, 0x7F, 0, 0, 0x00, 0x7C, 0x1F, 0x00];
        let bmp = bmp_file(&dib(2, 2, 16, 0, &[], &rows16, &[]), 40);
        assert_eq!(
            top_bottom(&decode(&bmp).unwrap()),
            [RED, BLUE, white, black]
        );
        // 32 bits: an alpha that is 0 everywhere is unused; some alpha is alpha.
        let rows32 = [255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 255, 0, 255, 0, 0, 0];
        let bmp = bmp_file(&dib(2, 2, 32, 0, &[], &rows32, &[]), 40);
        assert_eq!(
            top_bottom(&decode(&bmp).unwrap()),
            [RED, BLUE, white, black]
        );
        let mut with_alpha = rows32;
        with_alpha[11] = 128;
        let bmp = bmp_file(&dib(2, 2, 32, 0, &[], &with_alpha, &[]), 40);
        assert_eq!(pixel(&decode(&bmp).unwrap(), 0, 0), [255, 0, 0, 128]);
        assert_eq!(pixel(&decode(&bmp).unwrap(), 1, 0), [0, 0, 255, 0]);
        // BI_BITFIELDS: masks after the header, here R and B swapped from the usual.
        let mut fields = dib(1, 1, 32, 3, &[], &[], &[]);
        for mask in [0x0000_00FFu32, 0x0000_FF00, 0x00FF_0000] {
            fields.extend(mask.to_le_bytes());
        }
        fields.extend([255, 0, 0, 0]);
        assert_eq!(decode(&bmp_file(&fields, 52)).unwrap().pixels, RED);
        // RLE is not read; a file cut short is damaged.
        let rle = bmp_file(&dib(2, 2, 8, 1, &palette, &rows8, &[]), 56);
        assert!(matches!(
            decode(&rle),
            Err(PictureError::Damaged {
                format: Format::Bmp,
                ..
            })
        ));
        let cut = bmp_file(&dib(2, 2, 24, 0, &[], &rows24[..8], &[]), 40);
        assert!(matches!(decode(&cut), Err(PictureError::Damaged { .. })));
    }

    #[test]
    fn icon_bitmaps_take_their_transparency_from_the_mask_unless_they_have_alpha() {
        // 2 × 2 at 24 bits: all red, the mask's bottom-left bit set (the top-left pixel, as rows
        // are stored bottom first: the mask's second row is the picture's first).
        let rows = [0, 0, 255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255, 0, 0];
        let mask = [0, 0, 0, 0, 0b1000_0000, 0, 0, 0];
        let icon24 = dib(2, 4, 24, 0, &[], &rows, &mask);
        let decoded = icon(&ico_of(&[&icon24]), 2).unwrap();
        assert_eq!(pixel(&decoded, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&decoded, 1, 0), RED);
        assert_eq!(pixel(&decoded, 0, 1), RED);
        // 32 bits with alpha: the mask doesn't count. With alpha all 0: it does.
        let rows32 = [0, 0, 255, 200].repeat(4);
        let decoded = icon(&ico_of(&[&dib(2, 4, 32, 0, &[], &rows32, &mask)]), 2).unwrap();
        assert_eq!(pixel(&decoded, 0, 0), [255, 0, 0, 200]);
        let rows32 = [0, 0, 255, 0].repeat(4);
        let decoded = icon(&ico_of(&[&dib(2, 4, 32, 0, &[], &rows32, &mask)]), 2).unwrap();
        assert_eq!(pixel(&decoded, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&decoded, 1, 1), RED);
        // No mask at all: opaque.
        let decoded = icon(&ico_of(&[&dib(2, 4, 24, 0, &[], &rows, &[])]), 2).unwrap();
        assert_eq!(pixel(&decoded, 0, 0), RED);
    }

    #[test]
    fn an_icon_file_gives_the_image_for_the_size() {
        let png = encode_png(&solid(32, 32, [0, 255, 0, 255])).unwrap();
        let ico = ico_of(&[&icon_dib(16, RED), &icon_dib(48, BLUE), &png]);
        let at = |size| {
            let picture = icon(&ico, size).unwrap();
            (picture.width, pixel(&picture, 0, 0))
        };
        assert_eq!(at(16), (16, RED));
        assert_eq!(at(24), (32, [0, 255, 0, 255]));
        assert_eq!(at(40), (48, BLUE));
        assert_eq!(at(256), (48, BLUE));
        // Decoding the file gives its largest; any other picture is taken as it is.
        assert_eq!(decode(&ico).unwrap().width, 48);
        assert_eq!(icon(&png, 16).unwrap().width, 32);
    }

    #[test]
    fn too_large_damaged_or_unknown_pictures_are_refused() {
        // A PNG header claiming 20000 × 10: refused before anything is decoded.
        let mut huge = encode_png(&solid(1, 1, RED)).unwrap();
        huge[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        assert_eq!(
            decode(&huge),
            Err(PictureError::TooLarge {
                width: 20_000,
                height: 1
            })
        );
        let wide = bmp_file(&dib(9_000, 9_000, 24, 0, &[], &[], &[]), 40);
        assert_eq!(
            decode(&wide),
            Err(PictureError::TooLarge {
                width: 9_000,
                height: 9_000
            })
        );
        // Cut off, or not a picture at all.
        let png = encode_png(&solid(8, 8, RED)).unwrap();
        assert!(matches!(
            decode(&png[..40]),
            Err(PictureError::Damaged {
                format: Format::Png,
                ..
            })
        ));
        let jpeg = system_jpeg();
        assert!(matches!(
            decode(&jpeg[..200]),
            Err(PictureError::Damaged {
                format: Format::Jpeg,
                ..
            })
        ));
        assert_eq!(
            decode(b"<html>Not found</html>"),
            Err(PictureError::UnknownFormat)
        );
        assert_eq!(decode(b""), Err(PictureError::UnknownFormat));
    }

    #[test]
    fn fit_keeps_the_shape_and_an_image_of_that_size_as_it_is() {
        assert_eq!(fitted(64, 32, 32), (32, 16));
        assert_eq!(fitted(32, 64, 54), (27, 54));
        assert_eq!(fitted(256, 256, 54), (54, 54));
        assert_eq!(fitted(1, 300, 10), (1, 10));
        let wide = fit(solid(64, 32, RED), 32).unwrap();
        assert_eq!((wide.width, wide.height), (32, 16));
        assert_eq!(pixel(&wide, 16, 8), RED);
        // The size asked for: only premultiplied, pixel for pixel.
        let mut picture = solid(16, 16, BLUE);
        picture.pixels[3] = 128;
        picture.pixels[0] = 200;
        let same = fit(picture, 16).unwrap();
        assert_eq!(pixel(&same, 0, 0), [100, 0, 128, 128]);
        assert_eq!(pixel(&same, 1, 0), BLUE);
        // Scaled up too.
        let up = fit(solid(32, 32, RED), 54).unwrap();
        assert_eq!((up.width, up.height, pixel(&up, 27, 27)), (54, 54, RED));
    }

    #[test]
    fn transparent_pixels_lend_no_colour_to_their_neighbours() {
        // Left half opaque red, right half fully transparent green: scaled, no pixel turns green.
        let mut picture = solid(8, 8, RED);
        for y in 0..8 {
            for x in 4..8 {
                let at = ((y * 8 + x) * 4) as usize;
                picture.pixels[at..at + 4].copy_from_slice(&[0, 255, 0, 0]);
            }
        }
        for size in [3, 5, 20] {
            let scaled = fit(picture.clone(), size).unwrap();
            assert!(
                scaled.pixels.as_chunks::<4>().0.iter().all(|p| p[1] == 0),
                "size {size}"
            );
            // Premultiplied: no colour above its alpha.
            assert!(
                scaled
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| p[0] <= p[3]),
                "size {size}"
            );
        }
    }

    #[test]
    fn premultiplied_and_back() {
        let picture = Rgba {
            width: 3,
            height: 1,
            pixels: vec![200, 100, 50, 255, 200, 100, 50, 128, 200, 100, 50, 0],
        };
        let premultiplied = premultiply(picture.clone()).unwrap();
        assert_eq!(
            premultiplied.pixels,
            [200, 100, 50, 255, 100, 50, 25, 128, 0, 0, 0, 0]
        );
        let back = unpremultiply(premultiplied).unwrap();
        assert_eq!(&back.pixels[..4], &picture.pixels[..4]);
        assert!(
            back.pixels[4..8]
                .iter()
                .zip(&picture.pixels[4..8])
                .all(|(a, b)| a.abs_diff(*b) <= 2)
        );
        assert_eq!(&back.pixels[8..], [0, 0, 0, 0]);
    }
}
