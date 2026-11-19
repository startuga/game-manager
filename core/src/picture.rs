//! Pictures in pure Rust: game icons, cover art and the window's capture, read and written with no
//! Windows API, so the same code reads them on any system.
//!
//! - **Formats** (`format`): JPEG (zune-jpeg), PNG (png), WebP (image-webp, lossy and lossless),
//!   BMP, and `.ico` files, whose images are PNGs or bitmaps. Bitmaps (DIBs) are read here
//!   (`decode_dib`): BI_RGB at 1, 4, 8, 16, 24 and 32 bits, and BI_BITFIELDS; compressed ones (RLE)
//!   are refused. A BMP whose pixels are said to start inside its headers or palette is damaged,
//!   and every offset and length is added and multiplied checked, so no number in a file can wrap
//!   one round.
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
//!   then makes it that many pixels, premultiplied first so transparent edges stay clean, with our
//!   own `Q14` fixed-point Catmull-Rom resampler (streaming ring buffer, AVX2 `vpmaddwd` with a
//!   bit-identical portable scalar fallback), which rings less than Lanczos around an icon's hard
//!   edges. An image already that size is only premultiplied: pixel for pixel.
//! - **The game page's art**: `trimmed` cuts a logo to its visible pixels (Steam draws its logos
//!   on a 1280 × 720 transparent canvas); `blurred` makes a cover small (`fit`) and softens it
//!   with a box blur three times over, to be drawn stretched across the page when a game has no
//!   wide picture.
//! - **Capsules** (`capsule`): a game with no cover gets its icon made into one, the way Steam
//!   makes a portrait cover for a game that has only a wide picture: the picture across the
//!   cover's width in its upper part, and around it its own edges drawn out to the cover's and
//!   blurred. The icon is cut to what shows first and laid over its average colour for the blur,
//!   so its clear corners lend it no black.
//! - **Animations** (`Frames`): an animated WebP is read one frame at a time as it plays, never
//!   all at once: a 600 × 900 cover is 2 MB a frame, a four-second loop hundreds of MB. It is read
//!   from its file (`Frames::open`), so the file isn't held in memory either (a 600 × 900 one can
//!   be 32 MB), and its frame buffer is made only once it plays. `decode`
//!   gives its first frame, the picture shown while it doesn't play. Each frame is the whole
//!   picture (image-webp lays it over the ones before), shown for its own duration; 10 ms or
//!   less counts as 100 ms, as browsers show such GIFs. It plays as many times as the file says.
//!   `covering` then makes each frame premultiplied and no larger than it is drawn.
//! - **Writing**: `encode_png` only, for the window's capture. Nothing else is ever re-encoded:
//!   covers and page art are kept as the stores' files are (`covers.rs`, `page_art.rs`).

use crate::bytes::{u16_at, u32_at};
use crate::icon;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Seek};
use std::path::Path;
use std::time::Duration;

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

#[derive(Debug, PartialEq, Eq)]
pub enum PictureError {
    /// None of the formats this reads.
    UnknownFormat,
    TooLarge {
        width: u32,
        height: u32,
    },
    /// Cut off, damaged, or a variant this doesn't read (a compressed bitmap).
    Damaged {
        format: Format,
        reason: String,
    },
    Scale(String),
    Encode(String),
}

impl fmt::Display for PictureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFormat => {
                f.write_str("not a picture Game Manager reads (JPEG, PNG, WebP, BMP or ICO)")
            }
            Self::TooLarge { width, height } => {
                write!(f, "a {width} × {height} picture is too large")
            }
            Self::Damaged { format, reason } => {
                write!(f, "the {format} picture can't be read: {reason}")
            }
            Self::Scale(reason) => write!(f, "the picture could not be scaled: {reason}"),
            Self::Encode(reason) => write!(f, "the PNG could not be written: {reason}"),
        }
    }
}

impl std::error::Error for PictureError {}

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

fn i32_at(bytes: &[u8], at: usize) -> Option<i32> {
    u32_at(bytes, at).map(|value| value.cast_signed())
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
            // Counted from the file's start: the pixels can't begin inside its 14-byte header.
            let pixels_at = offset
                .checked_sub(14)
                .ok_or_else(|| damaged(format, "its pixels start inside its file header"))?;
            decode_dib(&bytes[14..], Some(pixels_at), false, format)
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

/// Where a 16- or 32-bit pixel keeps each channel (a mask of 0: none).
#[derive(Clone, Copy)]
struct Masks {
    red: u32,
    green: u32,
    blue: u32,
    alpha: u32,
}

impl Masks {
    /// Palette and 24-bit pixels have none.
    const NONE: Self = Self::of(0, 0, 0, 0);

    const fn of(red: u32, green: u32, blue: u32, alpha: u32) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }

    /// A pixel's value as R, G, B and A.
    fn rgba(self, value: u32) -> [u8; 4] {
        [self.red, self.green, self.blue, self.alpha].map(|mask| masked(value, mask))
    }
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
/// the height counts the AND mask too, which follows the rows (see the module's doc). Pixels
/// said to start inside the headers or the palette, and ends past what the numbers can hold, make
/// the bitmap damaged.
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
    let masks = match (compression, bits) {
        (0, 1 | 4 | 8 | 24) => Masks::NONE,
        (0, 16) => Masks::of(0x7C00, 0x03E0, 0x001F, 0),
        (0, 32) => Masks::of(0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000),
        // BI_BITFIELDS and BI_ALPHABITFIELDS: in the header from version 2 (52 bytes) on, else
        // just after it.
        (3 | 6, 16 | 32) => {
            let read = |at: usize| u32_at(dib, at).unwrap_or(0);
            if header >= 52 {
                let alpha = if header >= 56 { read(52) } else { 0 };
                Masks::of(read(40), read(44), read(48), alpha)
            } else {
                let count = if compression == 6 { 4 } else { 3 };
                let alpha = if count == 4 { read(at + 12) } else { 0 };
                let masks = Masks::of(read(at), read(at + 4), read(at + 8), alpha);
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
        let end = at
            .checked_add(count * 4)
            .ok_or_else(|| bad("its palette is cut off"))?;
        let table = dib
            .get(at..end)
            .ok_or_else(|| bad("its palette is cut off"))?;
        at = end;
        table
            .as_chunks::<4>()
            .0
            .iter()
            .map(|entry| [entry[2], entry[1], entry[0]])
            .collect()
    } else {
        Vec::new()
    };
    // The pixels come after the header, the masks and the palette, never inside them.
    let start = match pixels_at {
        Some(start) if start < at => return Err(bad("its pixels start inside its header")),
        Some(start) => start,
        None => at,
    };
    let (width, height) = (width as usize, height as usize);
    let stride = (width * usize::from(bits)).div_ceil(32) * 4;
    let rows_end = stride
        .checked_mul(height)
        .and_then(|length| start.checked_add(length))
        .ok_or_else(|| bad("its pixels are cut off"))?;
    let rows = dib
        .get(start..rows_end)
        .ok_or_else(|| bad("its pixels are cut off"))?;

    let mut pixels = vec![0u8; width * height * 4];
    // Only 16- and 32-bit pixels can carry alpha, under its mask.
    let alpha_channel = matches!(bits, 16 | 32) && masks.alpha != 0;
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
                16 => masks.rgba(u32::from(u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]))),
                _ => masks.rgba(u32::from_le_bytes([
                    row[x * 4],
                    row[x * 4 + 1],
                    row[x * 4 + 2],
                    row[x * 4 + 3],
                ])),
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
        let mask_end = mask_stride
            .checked_mul(height)
            .and_then(|length| rows_end.checked_add(length));
        if let Some(mask) = mask_end.and_then(|end| dib.get(rows_end..end)) {
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

/// Checks that a picture has non-zero dimensions and exactly `width * height * 4` pixel bytes.
fn check_rgba(picture: &Rgba) -> Result<(), PictureError> {
    if picture.width == 0
        || picture.height == 0
        || picture.pixels.len() as u64 != u64::from(picture.width) * u64::from(picture.height) * 4
    {
        return Err(PictureError::Scale(
            "its pixels don't match its size".to_owned(),
        ));
    }
    Ok(())
}

/// Straight RGBA made premultiplied: each colour scaled by its alpha.
pub fn premultiply(mut picture: Rgba) -> Result<Rgba, PictureError> {
    check_rgba(&picture)?;
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: `avx2` was just verified on this CPU.
        unsafe {
            avx2::premultiply(&mut picture.pixels);
        }
        return Ok(picture);
    }
    premultiply_scalar(&mut picture.pixels);
    Ok(picture)
}

/// Premultiplied RGBA made straight again (for writing a PNG).
pub fn unpremultiply(mut picture: Rgba) -> Result<Rgba, PictureError> {
    check_rgba(&picture)?;
    for px in picture.pixels.as_chunks_mut::<4>().0 {
        let a = u32::from(px[3]);
        if a == 0 {
            *px = [0, 0, 0, 0];
        } else if a < 255 {
            let half = a / 2;
            for channel in &mut px[..3] {
                *channel = ((u32::from(*channel) * 255 + half) / a).min(255) as u8;
            }
        }
    }
    Ok(picture)
}

/// A straight-RGBA picture inside `size` × `size` pixels, its shape kept, premultiplied: as it is
/// when it already has that size, else scaled with Catmull-Rom (see the module's doc).
pub fn fit(picture: Rgba, size: u32) -> Result<Rgba, PictureError> {
    let (width, height) = fitted(picture.width, picture.height, size);
    resized(premultiply(picture)?, width, height)
}

/// A premultiplied picture made `width` × `height` pixels with Catmull-Rom (see the module's doc),
/// or as it is when it already has that size.
fn resized(premultiplied: Rgba, width: u32, height: u32) -> Result<Rgba, PictureError> {
    check_rgba(&premultiplied)?;
    if width == 0 || height == 0 {
        return Err(PictureError::Scale("target size is empty".to_owned()));
    }
    if (width, height) == (premultiplied.width, premultiplied.height) {
        return Ok(premultiplied);
    }
    let (in_w, in_h) = (premultiplied.width as usize, premultiplied.height as usize);
    let (out_w, out_h) = (width as usize, height as usize);
    let mut pixels = vec![0u8; out_w * out_h * 4];
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: `avx2` was just verified on this CPU.
        unsafe {
            avx2::resize(&premultiplied.pixels, in_w, in_h, out_w, out_h, &mut pixels);
        }
        return Ok(Rgba {
            width,
            height,
            pixels,
        });
    }
    resize_scalar(&premultiplied.pixels, in_w, in_h, out_w, out_h, &mut pixels);
    Ok(Rgba {
        width,
        height,
        pixels,
    })
}

/// Fixed-point precision for Catmull-Rom filter weights: `1 << 14 = 16384`.
/// Catmull-Rom's largest normalized weight is at most ~1.15 (~18842 <= i16::MAX), and the sum of
/// `|w_i|` is at most ~1.35 * 16384 (~22118), so `255 * 22118 ≈ 5.64M << i32::MAX` never overflows
/// `i32` for any image dimensions.
const WEIGHT_BITS: i32 = 14;
const WEIGHT_SCALE: i32 = 1 << WEIGHT_BITS;
const WEIGHT_HALF: i32 = 1 << (WEIGHT_BITS - 1);

/// Catmull-Rom's cubic (B = 0, C = ½), 2 pixels to each side: sharp, with less ringing than
/// Lanczos around an icon's hard edges.
fn catmull_rom(x: f64) -> f64 {
    let x = x.abs();
    if x < 1.0 {
        (1.5 * x - 2.5) * x * x + 1.0
    } else if x < 2.0 {
        ((-0.5 * x + 2.5) * x - 4.0) * x + 2.0
    } else {
        0.0
    }
}

/// One output coordinate's slice of source coordinates and weights inside a flat `Filter`.
#[derive(Clone, Copy)]
struct Span {
    first: u32,
    offset: u32,
    len: u32,
}

/// Precomputed 1D Catmull-Rom filter along one axis: all `Q14` `i16` weights packed flat so
/// resampling never chases per-pixel heap allocations, padded to even tap counts whenever possible
/// for pairwise `vpmaddwd` accumulation.
struct Filter {
    spans: Vec<Span>,
    weights: Vec<i16>,
    max_taps: usize,
}

impl Filter {
    /// Weights for mapping `len` source coordinates to `out` output coordinates, summing to
    /// exactly `WEIGHT_SCALE`. Shrinking widens the filter with the scale so every source pixel
    /// counts; enlarging keeps it 2 pixels to each side. Leading and trailing zero weights are
    /// trimmed.
    fn new(len: usize, out: usize) -> Self {
        let scale = len as f64 / out as f64;
        let stretch = scale.max(1.0);
        let support = 2.0 * stretch;
        let approx_taps = (support * 2.0).ceil() as usize + 2;
        let mut spans = Vec::with_capacity(out);
        let mut weights = Vec::with_capacity(out * approx_taps);
        let mut raw = Vec::with_capacity(approx_taps);
        let mut max_taps = 0usize;

        for index in 0..out {
            let centre = (index as f64 + 0.5) * scale;
            let mut first = (centre - support).floor().max(0.0) as usize;
            let last = ((centre + support).ceil() as usize).clamp(first + 1, len);
            raw.clear();
            raw.extend((first..last).map(|x| catmull_rom((x as f64 + 0.5 - centre) / stretch)));
            let mut start = 0;
            let mut end = raw.len();
            while start + 1 < end && raw[start] == 0.0 {
                start += 1;
            }
            while end > start + 1 && raw[end - 1] == 0.0 {
                end -= 1;
            }
            first += start;
            let kept = &raw[start..end];
            let sum: f64 = kept.iter().sum();
            let inv = if sum == 0.0 {
                f64::from(WEIGHT_SCALE)
            } else {
                f64::from(WEIGHT_SCALE) / sum
            };
            let offset = weights.len();
            let mut total = 0i32;
            let mut peak_idx = 0usize;
            let mut peak_val = i16::MIN;
            for (i, &w) in kept.iter().enumerate() {
                let q = (w * inv)
                    .round()
                    .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
                if q > peak_val {
                    peak_val = q;
                    peak_idx = i;
                }
                total += i32::from(q);
                weights.push(q);
            }
            // Adjust the peak tap so weights sum to exactly `WEIGHT_SCALE` (solid colours stay
            // bit-exact).
            let diff = WEIGHT_SCALE - total;
            weights[offset + peak_idx] = (i32::from(weights[offset + peak_idx]) + diff)
                .clamp(i32::from(i16::MIN), i32::from(i16::MAX))
                as i16;

            let mut taps = kept.len();
            // Pad to an even tap count when room exists so pairwise SIMD skips the odd-tap tail.
            if taps % 2 == 1 {
                if first + taps < len {
                    weights.push(0);
                    taps += 1;
                } else if first > 0 {
                    weights.insert(offset, 0);
                    first -= 1;
                    taps += 1;
                }
            }
            max_taps = max_taps.max(taps);
            spans.push(Span {
                first: first as u32,
                offset: offset as u32,
                len: taps as u32,
            });
        }
        Self {
            spans,
            weights,
            max_taps,
        }
    }
}

/// Converts a `Q14` fixed-point RGBA sum into a clamped `u8` pixel; when `clamp_alpha` is true,
/// colour channels are also clamped to `<= alpha` (keeping premultiplied RGBA valid for Direct2D).
#[inline(always)]
fn finish_pixel(sums: [i32; 4], clamp_alpha: bool) -> [u8; 4] {
    let r = ((sums[0] + WEIGHT_HALF) >> WEIGHT_BITS).clamp(0, 255) as u8;
    let g = ((sums[1] + WEIGHT_HALF) >> WEIGHT_BITS).clamp(0, 255) as u8;
    let b = ((sums[2] + WEIGHT_HALF) >> WEIGHT_BITS).clamp(0, 255) as u8;
    let a = ((sums[3] + WEIGHT_HALF) >> WEIGHT_BITS).clamp(0, 255) as u8;
    if clamp_alpha {
        [r.min(a), g.min(a), b.min(a), a]
    } else {
        [r, g, b, a]
    }
}

/// Baseline horizontal pass for one RGBA row (`u8 -> u8`), bit-identical to the AVX2 kernel.
fn resample_row_scalar(src: &[[u8; 4]], dst: &mut [[u8; 4]], h: &Filter, clamp_alpha: bool) {
    for (span, out) in h.spans.iter().zip(dst.iter_mut()) {
        let first = span.first as usize;
        let offset = span.offset as usize;
        let len = span.len as usize;
        let pixels = &src[first..first + len];
        let ws = &h.weights[offset..offset + len];
        let mut acc = [0i32; 4];
        for (px, &w) in pixels.iter().zip(ws) {
            let w = i32::from(w);
            acc[0] += i32::from(px[0]) * w;
            acc[1] += i32::from(px[1]) * w;
            acc[2] += i32::from(px[2]) * w;
            acc[3] += i32::from(px[3]) * w;
        }
        *out = finish_pixel(acc, clamp_alpha);
    }
}

/// Baseline horizontal pass for four RGBA rows at once, sharing weight loads across all four.
fn resample_4_rows_scalar(
    src: [&[[u8; 4]]; 4],
    dst: [&mut [[u8; 4]]; 4],
    h: &Filter,
    clamp_alpha: bool,
) {
    let [s0, s1, s2, s3] = src;
    let [d0, d1, d2, d3] = dst;
    for ((((span, o0), o1), o2), o3) in h
        .spans
        .iter()
        .zip(d0.iter_mut())
        .zip(d1.iter_mut())
        .zip(d2.iter_mut())
        .zip(d3.iter_mut())
    {
        let first = span.first as usize;
        let offset = span.offset as usize;
        let len = span.len as usize;
        let p0 = &s0[first..first + len];
        let p1 = &s1[first..first + len];
        let p2 = &s2[first..first + len];
        let p3 = &s3[first..first + len];
        let ws = &h.weights[offset..offset + len];
        let mut a0 = [0i32; 4];
        let mut a1 = [0i32; 4];
        let mut a2 = [0i32; 4];
        let mut a3 = [0i32; 4];
        for ((((px0, px1), px2), px3), &w) in p0.iter().zip(p1).zip(p2).zip(p3).zip(ws) {
            let w = i32::from(w);
            for c in 0..4 {
                a0[c] += i32::from(px0[c]) * w;
                a1[c] += i32::from(px1[c]) * w;
                a2[c] += i32::from(px2[c]) * w;
                a3[c] += i32::from(px3[c]) * w;
            }
        }
        *o0 = finish_pixel(a0, clamp_alpha);
        *o1 = finish_pixel(a1, clamp_alpha);
        *o2 = finish_pixel(a2, clamp_alpha);
        *o3 = finish_pixel(a3, clamp_alpha);
    }
}

/// Chunk size (in channels, = 64 pixels = 1 KB of `i32` accumulators) for the scalar vertical
/// pass so the accumulator stays in L1.
const VERTICAL_CHUNK: usize = 256;

/// Baseline vertical pass combining one span of `u8` ring-buffer rows into a `u8` output row.
fn combine_vertical_scalar(
    ring: &[u8],
    ring_cap: usize,
    stride: usize,
    first: usize,
    ws: &[i16],
    dst_row: &mut [u8],
) {
    let mut chunk_start = 0;
    while chunk_start < stride {
        let chunk_len = (stride - chunk_start).min(VERTICAL_CHUNK);
        let mut acc = [0i32; VERTICAL_CHUNK];
        let acc_slice = &mut acc[..chunk_len];
        for (k, &w) in ws.iter().enumerate() {
            let w = i32::from(w);
            let slot = (first + k) % ring_cap;
            let row = &ring[slot * stride + chunk_start..slot * stride + chunk_start + chunk_len];
            for (a, &p) in acc_slice.iter_mut().zip(row) {
                *a += i32::from(p) * w;
            }
        }
        for (out, sums) in dst_row[chunk_start..chunk_start + chunk_len]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(acc_slice.as_chunks::<4>().0)
        {
            *out = finish_pixel(*sums, true);
        }
        chunk_start += chunk_len;
    }
}

/// Baseline streaming Catmull-Rom resampler (portable, 100% safe, bit-identical to the AVX2 path).
fn resize_scalar(src: &[u8], in_w: usize, in_h: usize, out_w: usize, out_h: usize, dst: &mut [u8]) {
    let in_stride = in_w * 4;
    let out_stride = out_w * 4;

    if out_h == in_h {
        let h = Filter::new(in_w, out_w);
        let mut y = 0;
        while y + 4 <= in_h {
            let s0 = src[y * in_stride..(y + 1) * in_stride].as_chunks::<4>().0;
            let s1 = src[(y + 1) * in_stride..(y + 2) * in_stride]
                .as_chunks::<4>()
                .0;
            let s2 = src[(y + 2) * in_stride..(y + 3) * in_stride]
                .as_chunks::<4>()
                .0;
            let s3 = src[(y + 3) * in_stride..(y + 4) * in_stride]
                .as_chunks::<4>()
                .0;
            let dst_quad = &mut dst[y * out_stride..(y + 4) * out_stride];
            let (d0, rest) = dst_quad.split_at_mut(out_stride);
            let (d1, rest) = rest.split_at_mut(out_stride);
            let (d2, d3) = rest.split_at_mut(out_stride);
            resample_4_rows_scalar(
                [s0, s1, s2, s3],
                [
                    d0.as_chunks_mut::<4>().0,
                    d1.as_chunks_mut::<4>().0,
                    d2.as_chunks_mut::<4>().0,
                    d3.as_chunks_mut::<4>().0,
                ],
                &h,
                true,
            );
            y += 4;
        }
        while y < in_h {
            resample_row_scalar(
                src[y * in_stride..(y + 1) * in_stride].as_chunks::<4>().0,
                dst[y * out_stride..(y + 1) * out_stride]
                    .as_chunks_mut::<4>()
                    .0,
                &h,
                true,
            );
            y += 1;
        }
        return;
    }

    let h = (out_w != in_w).then(|| Filter::new(in_w, out_w));
    let v = Filter::new(in_h, out_h);

    // Without a horizontal pass, read straight from `src` without copying into a ring buffer.
    let Some(h) = h else {
        for (v_span, dst_row) in v.spans.iter().zip(dst.chunks_exact_mut(out_stride)) {
            let offset = v_span.offset as usize;
            let len = v_span.len as usize;
            combine_vertical_scalar(
                src,
                in_h,
                out_stride,
                v_span.first as usize,
                &v.weights[offset..offset + len],
                dst_row,
            );
        }
        return;
    };

    // Sizing the ring to a power of two >= 4 that is at least `max_taps + 3` guarantees that
    // computing four source rows at `next_src_y` (a multiple of 4) neither wraps past the end of
    // `ring` nor overwrites any row still needed by the current vertical span, and turns `%` into
    // a single bitmask.
    let ring_cap = (v.max_taps + 3).next_power_of_two().max(4);
    let ring_mask = ring_cap - 1;
    let mut ring = vec![0u8; ring_cap * out_stride];
    let mut next_src_y = 0usize;

    for (v_span, dst_row) in v.spans.iter().zip(dst.chunks_exact_mut(out_stride)) {
        let first = v_span.first as usize;
        let len = v_span.len as usize;
        let need_end = first + len;

        while next_src_y < need_end {
            let slot = next_src_y & ring_mask;
            if next_src_y + 4 <= in_h {
                let s0 = src[next_src_y * in_stride..(next_src_y + 1) * in_stride]
                    .as_chunks::<4>()
                    .0;
                let s1 = src[(next_src_y + 1) * in_stride..(next_src_y + 2) * in_stride]
                    .as_chunks::<4>()
                    .0;
                let s2 = src[(next_src_y + 2) * in_stride..(next_src_y + 3) * in_stride]
                    .as_chunks::<4>()
                    .0;
                let s3 = src[(next_src_y + 3) * in_stride..(next_src_y + 4) * in_stride]
                    .as_chunks::<4>()
                    .0;
                let ring_quad = &mut ring[slot * out_stride..(slot + 4) * out_stride];
                let (d0, rest) = ring_quad.split_at_mut(out_stride);
                let (d1, rest) = rest.split_at_mut(out_stride);
                let (d2, d3) = rest.split_at_mut(out_stride);
                resample_4_rows_scalar(
                    [s0, s1, s2, s3],
                    [
                        d0.as_chunks_mut::<4>().0,
                        d1.as_chunks_mut::<4>().0,
                        d2.as_chunks_mut::<4>().0,
                        d3.as_chunks_mut::<4>().0,
                    ],
                    &h,
                    false,
                );
                next_src_y += 4;
            } else {
                resample_row_scalar(
                    src[next_src_y * in_stride..(next_src_y + 1) * in_stride]
                        .as_chunks::<4>()
                        .0,
                    ring[slot * out_stride..(slot + 1) * out_stride]
                        .as_chunks_mut::<4>()
                        .0,
                    &h,
                    false,
                );
                next_src_y += 1;
            }
        }

        let offset = v_span.offset as usize;
        combine_vertical_scalar(
            &ring,
            ring_cap,
            out_stride,
            first,
            &v.weights[offset..offset + len],
            dst_row,
        );
    }
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    //! AVX2 `vpmaddwd` Catmull-Rom resampler and `u16` premultiply, bit-identical to the scalar
    //! path. Only the fixed-size unaligned load/store wrappers touch raw pointers (validated by
    //! fixed-length array references); every arithmetic and shuffle intrinsic is safe.
    use super::{Filter, WEIGHT_BITS, WEIGHT_HALF, finish_pixel, resample_row_scalar};
    use std::arch::x86_64::*;

    #[inline]
    #[target_feature(enable = "avx2")]
    fn load_256(bytes: &[u8; 32]) -> __m256i {
        // SAFETY: `bytes` is a valid 32-byte array; `_mm256_loadu_si256` allows unaligned reads.
        unsafe { _mm256_loadu_si256(bytes.as_ptr().cast()) }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    fn store_256(v: __m256i, out: &mut [u8; 32]) {
        // SAFETY: `out` is a valid 32-byte array; `_mm256_storeu_si256` allows unaligned writes.
        unsafe { _mm256_storeu_si256(out.as_mut_ptr().cast(), v) }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    fn load_128(bytes: &[u8; 16]) -> __m128i {
        // SAFETY: `bytes` is a valid 16-byte array; `_mm_loadu_si128` allows unaligned reads.
        unsafe { _mm_loadu_si128(bytes.as_ptr().cast()) }
    }

    /// Loads four RGBA pixels from `lo` into lane 0 and four from `hi` into lane 1.
    #[inline]
    #[target_feature(enable = "avx2")]
    fn load_2x4px(lo: &[u8; 16], hi: &[u8; 16]) -> __m256i {
        _mm256_set_m128i(load_128(hi), load_128(lo))
    }

    /// Loads two RGBA pixels from `lo` into lane 0 (low 64 bits) and two from `hi` into lane 1.
    #[inline]
    #[target_feature(enable = "avx2")]
    fn load_2x2px(lo: &[u8; 8], hi: &[u8; 8]) -> __m256i {
        _mm256_set_epi64x(0, i64::from_ne_bytes(*hi), 0, i64::from_ne_bytes(*lo))
    }

    /// Packs two `i16` weights into one `i32` for broadcasting to `vpmaddwd`.
    #[inline(always)]
    fn pack_w2(w0: i16, w1: i16) -> i32 {
        (u32::from(w0 as u16) | (u32::from(w1 as u16) << 16)) as i32
    }

    /// Packs two pixels' `i32` RGBA sums (lane 0 and lane 1) into two `[u8; 4]` pixels.
    #[inline]
    #[target_feature(enable = "avx2")]
    fn pack_2px(acc: __m256i, clamp_alpha: bool, alpha_mask: __m256i) -> ([u8; 4], [u8; 4]) {
        let shifted = _mm256_srai_epi32::<WEIGHT_BITS>(acc);
        let p16 = _mm256_packs_epi32(shifted, shifted);
        let mut p8 = _mm256_packus_epi16(p16, p16);
        if clamp_alpha {
            let alphas = _mm256_shuffle_epi8(p8, alpha_mask);
            p8 = _mm256_min_epu8(p8, alphas);
        }
        (
            _mm256_extract_epi32::<0>(p8).to_ne_bytes(),
            _mm256_extract_epi32::<4>(p8).to_ne_bytes(),
        )
    }

    /// Horizontal pass for four RGBA rows at once: two YMM registers hold `[row0 | row1]` and
    /// `[row2 | row3]`, multiplying 4 taps per inner iteration via `vpmaddwd` with zero inner
    /// bounds checks.
    #[target_feature(enable = "avx2")]
    fn resample_4_rows(src: [&[u8]; 4], dst: [&mut [[u8; 4]]; 4], h: &Filter, clamp_alpha: bool) {
        let [s0, s1, s2, s3] = src;
        let [d0, d1, d2, d3] = dst;
        // Reorders `[R0 G0 B0 A0 R1 G1 B1 A1 ...]` into `[R0 0 R1 0 G0 0 G1 0 B0 0 B1 0 A0 0 A1 0]`
        // (zero-extended `i16` pairs ready for `vpmaddwd`).
        let shuf_01 = _mm256_setr_epi8(
            0, -1, 4, -1, 1, -1, 5, -1, 2, -1, 6, -1, 3, -1, 7, -1, 0, -1, 4, -1, 1, -1, 5, -1, 2,
            -1, 6, -1, 3, -1, 7, -1,
        );
        let shuf_23 = _mm256_setr_epi8(
            8, -1, 12, -1, 9, -1, 13, -1, 10, -1, 14, -1, 11, -1, 15, -1, 8, -1, 12, -1, 9, -1, 13,
            -1, 10, -1, 14, -1, 11, -1, 15, -1,
        );
        let alpha_mask = _mm256_setr_epi8(
            3, 3, 3, 3, 7, 7, 7, 7, 11, 11, 11, 11, 15, 15, 15, 15, 3, 3, 3, 3, 7, 7, 7, 7, 11, 11,
            11, 11, 15, 15, 15, 15,
        );
        let half = _mm256_set1_epi32(WEIGHT_HALF);

        for ((((span, o0), o1), o2), o3) in h
            .spans
            .iter()
            .zip(d0.iter_mut())
            .zip(d1.iter_mut())
            .zip(d2.iter_mut())
            .zip(d3.iter_mut())
        {
            let start = span.first as usize * 4;
            let bytes = span.len as usize * 4;
            let offset = span.offset as usize;
            let len = span.len as usize;

            let (q0, rem0) = s0[start..start + bytes].as_chunks::<16>();
            let (q1, rem1) = s1[start..start + bytes].as_chunks::<16>();
            let (q2, rem2) = s2[start..start + bytes].as_chunks::<16>();
            let (q3, rem3) = s3[start..start + bytes].as_chunks::<16>();
            let (wq, w_rem) = h.weights[offset..offset + len].as_chunks::<4>();

            let mut acc01 = half;
            let mut acc23 = half;
            for ((((c0, c1), c2), c3), &[w0, w1, w2, w3]) in
                q0.iter().zip(q1).zip(q2).zip(q3).zip(wq)
            {
                let w01 = _mm256_set1_epi32(pack_w2(w0, w1));
                let w23 = _mm256_set1_epi32(pack_w2(w2, w3));

                let v01 = load_2x4px(c0, c1);
                acc01 = _mm256_add_epi32(
                    acc01,
                    _mm256_madd_epi16(_mm256_shuffle_epi8(v01, shuf_01), w01),
                );
                acc01 = _mm256_add_epi32(
                    acc01,
                    _mm256_madd_epi16(_mm256_shuffle_epi8(v01, shuf_23), w23),
                );

                let v23 = load_2x4px(c2, c3);
                acc23 = _mm256_add_epi32(
                    acc23,
                    _mm256_madd_epi16(_mm256_shuffle_epi8(v23, shuf_01), w01),
                );
                acc23 = _mm256_add_epi32(
                    acc23,
                    _mm256_madd_epi16(_mm256_shuffle_epi8(v23, shuf_23), w23),
                );
            }
            let (p0, t0) = rem0.as_chunks::<8>();
            let (p1, t1) = rem1.as_chunks::<8>();
            let (p2, t2) = rem2.as_chunks::<8>();
            let (p3, t3) = rem3.as_chunks::<8>();
            let (wp, wt) = w_rem.as_chunks::<2>();
            for ((((c0, c1), c2), c3), &[w0, w1]) in p0.iter().zip(p1).zip(p2).zip(p3).zip(wp) {
                let w01 = _mm256_set1_epi32(pack_w2(w0, w1));
                let v01 = load_2x2px(c0, c1);
                acc01 = _mm256_add_epi32(
                    acc01,
                    _mm256_madd_epi16(_mm256_shuffle_epi8(v01, shuf_01), w01),
                );
                let v23 = load_2x2px(c2, c3);
                acc23 = _mm256_add_epi32(
                    acc23,
                    _mm256_madd_epi16(_mm256_shuffle_epi8(v23, shuf_01), w01),
                );
            }
            if let Some(&w0) = wt.first() {
                let w = _mm256_set1_epi32(pack_w2(w0, 0));
                let pad = |t: &[u8]| [t[0], t[1], t[2], t[3], 0, 0, 0, 0];
                let v01 = load_2x2px(&pad(t0), &pad(t1));
                acc01 = _mm256_add_epi32(
                    acc01,
                    _mm256_madd_epi16(_mm256_shuffle_epi8(v01, shuf_01), w),
                );
                let v23 = load_2x2px(&pad(t2), &pad(t3));
                acc23 = _mm256_add_epi32(
                    acc23,
                    _mm256_madd_epi16(_mm256_shuffle_epi8(v23, shuf_01), w),
                );
            }
            (*o0, *o1) = pack_2px(acc01, clamp_alpha, alpha_mask);
            (*o2, *o3) = pack_2px(acc23, clamp_alpha, alpha_mask);
        }
    }

    /// Vertical pass combining one span of `u8` rows into a `u8` output row: 8 RGBA pixels (32
    /// bytes) per chunk, accumulating two vertical rows per `vpmaddwd` step with precomputed row
    /// offsets and weight pairs (zero modulo or weight packing inside the pixel loop).
    #[target_feature(enable = "avx2")]
    fn combine_vertical(
        ring: &[u8],
        ring_cap: usize,
        stride: usize,
        first: usize,
        ws: &[i16],
        dst_row: &mut [u8],
        pairs: &mut Vec<(usize, usize, i32)>,
    ) {
        let zero = _mm256_setzero_si256();
        let half = _mm256_set1_epi32(WEIGHT_HALF);
        let alpha_mask = _mm256_setr_epi8(
            3, 3, 3, 3, 7, 7, 7, 7, 11, 11, 11, 11, 15, 15, 15, 15, 3, 3, 3, 3, 7, 7, 7, 7, 11, 11,
            11, 11, 15, 15, 15, 15,
        );
        // Precompute `(row0_byte_offset, row1_byte_offset, packed_weights)` once per output row.
        pairs.clear();
        let (w_pairs, w_odd) = ws.as_chunks::<2>();
        let pow2_mask = ring_cap.is_power_of_two().then_some(ring_cap - 1);
        let row_at = |y: usize| {
            let slot = match pow2_mask {
                Some(mask) => y & mask,
                None => y % ring_cap,
            };
            slot * stride
        };
        for (pair_idx, &[w0, w1]) in w_pairs.iter().enumerate() {
            let k = pair_idx * 2;
            pairs.push((row_at(first + k), row_at(first + k + 1), pack_w2(w0, w1)));
        }
        let odd = w_odd
            .first()
            .map(|&w| (row_at(first + w_pairs.len() * 2), pack_w2(w, 0)));

        let (dst_chunks, dst_tail) = dst_row.as_chunks_mut::<32>();
        let mut x = 0usize;
        for dst_chunk in dst_chunks {
            let mut acc0 = half;
            let mut acc1 = half;
            let mut acc2 = half;
            let mut acc3 = half;
            for &(r0, r1, w_packed) in pairs.iter() {
                let w01 = _mm256_set1_epi32(w_packed);
                let (c0, _) = ring[r0 + x..r0 + x + 32].as_chunks::<32>();
                let (c1, _) = ring[r1 + x..r1 + x + 32].as_chunks::<32>();
                let p0 = load_256(&c0[0]);
                let p1 = load_256(&c1[0]);
                let lo = _mm256_unpacklo_epi8(p0, p1);
                let hi = _mm256_unpackhi_epi8(p0, p1);
                acc0 =
                    _mm256_add_epi32(acc0, _mm256_madd_epi16(_mm256_unpacklo_epi8(lo, zero), w01));
                acc1 =
                    _mm256_add_epi32(acc1, _mm256_madd_epi16(_mm256_unpackhi_epi8(lo, zero), w01));
                acc2 =
                    _mm256_add_epi32(acc2, _mm256_madd_epi16(_mm256_unpacklo_epi8(hi, zero), w01));
                acc3 =
                    _mm256_add_epi32(acc3, _mm256_madd_epi16(_mm256_unpackhi_epi8(hi, zero), w01));
            }
            if let Some((r0, w_packed)) = odd {
                let w = _mm256_set1_epi32(w_packed);
                let (c0, _) = ring[r0 + x..r0 + x + 32].as_chunks::<32>();
                let p0 = load_256(&c0[0]);
                let lo = _mm256_unpacklo_epi8(p0, zero);
                let hi = _mm256_unpackhi_epi8(p0, zero);
                acc0 = _mm256_add_epi32(acc0, _mm256_madd_epi16(_mm256_unpacklo_epi8(lo, zero), w));
                acc1 = _mm256_add_epi32(acc1, _mm256_madd_epi16(_mm256_unpackhi_epi8(lo, zero), w));
                acc2 = _mm256_add_epi32(acc2, _mm256_madd_epi16(_mm256_unpacklo_epi8(hi, zero), w));
                acc3 = _mm256_add_epi32(acc3, _mm256_madd_epi16(_mm256_unpackhi_epi8(hi, zero), w));
            }
            let r0 = _mm256_srai_epi32::<WEIGHT_BITS>(acc0);
            let r1 = _mm256_srai_epi32::<WEIGHT_BITS>(acc1);
            let r2 = _mm256_srai_epi32::<WEIGHT_BITS>(acc2);
            let r3 = _mm256_srai_epi32::<WEIGHT_BITS>(acc3);
            let p01 = _mm256_packs_epi32(r0, r1);
            let p23 = _mm256_packs_epi32(r2, r3);
            let packed = _mm256_packus_epi16(p01, p23);
            let alphas = _mm256_shuffle_epi8(packed, alpha_mask);
            store_256(_mm256_min_epu8(packed, alphas), dst_chunk);
            x += 32;
        }
        // Remaining 0..7 pixels at the right edge.
        for out in dst_tail.as_chunks_mut::<4>().0 {
            let mut acc = [0i32; 4];
            for (k, &w) in ws.iter().enumerate() {
                let w = i32::from(w);
                let at = row_at(first + k) + x;
                for c in 0..4 {
                    acc[c] += i32::from(ring[at + c]) * w;
                }
            }
            *out = finish_pixel(acc, true);
            x += 4;
        }
    }

    #[target_feature(enable = "avx2")]
    pub(super) fn resize(
        src: &[u8],
        in_w: usize,
        in_h: usize,
        out_w: usize,
        out_h: usize,
        dst: &mut [u8],
    ) {
        let in_stride = in_w * 4;
        let out_stride = out_w * 4;

        if out_h == in_h {
            let h = Filter::new(in_w, out_w);
            let mut y = 0;
            while y + 4 <= in_h {
                let dst_quad = &mut dst[y * out_stride..(y + 4) * out_stride];
                let (d0, rest) = dst_quad.split_at_mut(out_stride);
                let (d1, rest) = rest.split_at_mut(out_stride);
                let (d2, d3) = rest.split_at_mut(out_stride);
                resample_4_rows(
                    [
                        &src[y * in_stride..(y + 1) * in_stride],
                        &src[(y + 1) * in_stride..(y + 2) * in_stride],
                        &src[(y + 2) * in_stride..(y + 3) * in_stride],
                        &src[(y + 3) * in_stride..(y + 4) * in_stride],
                    ],
                    [
                        d0.as_chunks_mut::<4>().0,
                        d1.as_chunks_mut::<4>().0,
                        d2.as_chunks_mut::<4>().0,
                        d3.as_chunks_mut::<4>().0,
                    ],
                    &h,
                    true,
                );
                y += 4;
            }
            while y < in_h {
                resample_row_scalar(
                    src[y * in_stride..(y + 1) * in_stride].as_chunks::<4>().0,
                    dst[y * out_stride..(y + 1) * out_stride]
                        .as_chunks_mut::<4>()
                        .0,
                    &h,
                    true,
                );
                y += 1;
            }
            return;
        }

        let h = (out_w != in_w).then(|| Filter::new(in_w, out_w));
        let v = Filter::new(in_h, out_h);
        let mut pairs = Vec::with_capacity(v.max_taps.div_ceil(2));

        let Some(h) = h else {
            for (v_span, dst_row) in v.spans.iter().zip(dst.chunks_exact_mut(out_stride)) {
                let offset = v_span.offset as usize;
                let len = v_span.len as usize;
                combine_vertical(
                    src,
                    in_h,
                    out_stride,
                    v_span.first as usize,
                    &v.weights[offset..offset + len],
                    dst_row,
                    &mut pairs,
                );
            }
            return;
        };

        let ring_cap = (v.max_taps + 3).next_power_of_two().max(4);
        let ring_mask = ring_cap - 1;
        let mut ring = vec![0u8; ring_cap * out_stride];
        let mut next_src_y = 0usize;

        for (v_span, dst_row) in v.spans.iter().zip(dst.chunks_exact_mut(out_stride)) {
            let first = v_span.first as usize;
            let len = v_span.len as usize;
            let need_end = first + len;

            while next_src_y < need_end {
                let slot = next_src_y & ring_mask;
                if next_src_y + 4 <= in_h {
                    let ring_quad = &mut ring[slot * out_stride..(slot + 4) * out_stride];
                    let (d0, rest) = ring_quad.split_at_mut(out_stride);
                    let (d1, rest) = rest.split_at_mut(out_stride);
                    let (d2, d3) = rest.split_at_mut(out_stride);
                    resample_4_rows(
                        [
                            &src[next_src_y * in_stride..(next_src_y + 1) * in_stride],
                            &src[(next_src_y + 1) * in_stride..(next_src_y + 2) * in_stride],
                            &src[(next_src_y + 2) * in_stride..(next_src_y + 3) * in_stride],
                            &src[(next_src_y + 3) * in_stride..(next_src_y + 4) * in_stride],
                        ],
                        [
                            d0.as_chunks_mut::<4>().0,
                            d1.as_chunks_mut::<4>().0,
                            d2.as_chunks_mut::<4>().0,
                            d3.as_chunks_mut::<4>().0,
                        ],
                        &h,
                        false,
                    );
                    next_src_y += 4;
                } else {
                    resample_row_scalar(
                        src[next_src_y * in_stride..(next_src_y + 1) * in_stride]
                            .as_chunks::<4>()
                            .0,
                        ring[slot * out_stride..(slot + 1) * out_stride]
                            .as_chunks_mut::<4>()
                            .0,
                        &h,
                        false,
                    );
                    next_src_y += 1;
                }
            }

            let offset = v_span.offset as usize;
            combine_vertical(
                &ring,
                ring_cap,
                out_stride,
                first,
                &v.weights[offset..offset + len],
                dst_row,
                &mut pairs,
            );
        }
    }

    /// Premultiplies 8 RGBA pixels per iteration in `u16` (`((c * a + 128) + ((c * a + 128) >> 8))
    /// >> 8`), skipping writes completely for opaque 8-pixel chunks (JPEGs and opaque covers).
    #[target_feature(enable = "avx2")]
    pub(super) fn premultiply(pixels: &mut [u8]) {
        let zero = _mm256_setzero_si256();
        let all_ff = _mm256_set1_epi8(-1);
        let c128 = _mm256_set1_epi16(128);
        let c255 = _mm256_set1_epi16(255);
        // Bit mask of the 8 alpha byte positions (bytes 3, 7, 11, 15, 19, 23, 27, 31).
        const ALPHA_BITS: i32 = 0x8888_8888u32.cast_signed();

        let (chunks, tail) = pixels.as_chunks_mut::<32>();
        for chunk in chunks {
            let v = load_256(chunk);
            // Opaque fast path: all 8 pixels have alpha 255, so nothing changes and no cache line
            // is dirtied.
            if (_mm256_movemask_epi8(_mm256_cmpeq_epi8(v, all_ff)) & ALPHA_BITS) == ALPHA_BITS {
                continue;
            }
            let lo = _mm256_unpacklo_epi8(v, zero);
            let hi = _mm256_unpackhi_epi8(v, zero);

            // Broadcast each pixel's alpha across its 4 `u16` lanes, keeping 255 in the alpha lane
            // so `mul(a, 255) == a`.
            let a_lo = _mm256_blend_epi16::<0x88>(
                _mm256_shufflelo_epi16::<0xFF>(_mm256_shufflehi_epi16::<0xFF>(lo)),
                c255,
            );
            let a_hi = _mm256_blend_epi16::<0x88>(
                _mm256_shufflelo_epi16::<0xFF>(_mm256_shufflehi_epi16::<0xFF>(hi)),
                c255,
            );

            let t_lo = _mm256_add_epi16(_mm256_mullo_epi16(lo, a_lo), c128);
            let r_lo = _mm256_srli_epi16::<8>(_mm256_add_epi16(t_lo, _mm256_srli_epi16::<8>(t_lo)));

            let t_hi = _mm256_add_epi16(_mm256_mullo_epi16(hi, a_hi), c128);
            let r_hi = _mm256_srli_epi16::<8>(_mm256_add_epi16(t_hi, _mm256_srli_epi16::<8>(t_hi)));

            store_256(_mm256_packus_epi16(r_lo, r_hi), chunk);
        }
        super::premultiply_scalar(tail);
    }
}

fn premultiply_scalar(pixels: &mut [u8]) {
    for px in pixels.as_chunks_mut::<4>().0 {
        let a = u16::from(px[3]);
        // Exact division-free `(c * a + 127) / 255` for all 0..=255 without overflowing `u16`
        // (max `t` is 65153, `t + (t >> 8)` is 65407 <= 65535).
        let mul = |c: u8| {
            let t = u16::from(c) * a + 128;
            ((t + (t >> 8)) >> 8) as u8
        };
        px[0] = mul(px[0]);
        px[1] = mul(px[1]);
        px[2] = mul(px[2]);
    }
}

/// A pixel with less alpha than this counts as empty for `trimmed`: a logo's faint glow at its
/// canvas's edges doesn't hold the trim back.
const TRIM_ALPHA: u8 = 8;

/// A picture cut to the smallest rectangle holding its visible pixels (alpha `TRIM_ALPHA` or more):
/// a logo drawn on a larger transparent canvas (Steam's are 1280 × 720) then lines up with the
/// text beside it and is sized by what shows. One with no visible pixel is left as it is.
pub fn trimmed(picture: Rgba) -> Rgba {
    let (width, height) = (picture.width as usize, picture.height as usize);
    let visible = |x: usize, y: usize| picture.pixels[(y * width + x) * 4 + 3] >= TRIM_ALPHA;
    let rows: Vec<usize> = (0..height)
        .filter(|&y| (0..width).any(|x| visible(x, y)))
        .collect();
    let columns: Vec<usize> = (0..width)
        .filter(|&x| rows.iter().any(|&y| visible(x, y)))
        .collect();
    let (Some(&top), Some(&bottom), Some(&left), Some(&right)) =
        (rows.first(), rows.last(), columns.first(), columns.last())
    else {
        return picture;
    };
    if (left, top, right, bottom) == (0, 0, width - 1, height - 1) {
        return picture;
    }
    let mut pixels = Vec::with_capacity((right - left + 1) * (bottom - top + 1) * 4);
    for y in top..=bottom {
        pixels.extend_from_slice(
            &picture.pixels[(y * width + left) * 4..(y * width + right + 1) * 4],
        );
    }
    Rgba {
        width: (right - left + 1) as u32,
        height: (bottom - top + 1) as u32,
        pixels,
    }
}

/// How many times `blurred` runs its box blur: three passes come close to a Gaussian.
const BLUR_PASSES: usize = 3;

/// A straight-RGBA picture made small (inside `size` × `size`, premultiplied, `fit`) and softened
/// with a 3 × 3 box blur run `BLUR_PASSES` times, its edges clamped. Drawn stretched over a large
/// area it is a blur of the picture: the game page's background from a cover when the game has no
/// wide picture.
pub fn blurred(picture: Rgba, size: u32) -> Result<Rgba, PictureError> {
    let mut small = fit(picture, size)?;
    let (width, height) = (small.width as usize, small.height as usize);
    for _ in 0..BLUR_PASSES {
        let across = box_blur(&small.pixels, width, height, (1, 0));
        small.pixels = box_blur(&across, width, height, (0, 1));
    }
    Ok(small)
}

/// A capsule's size in pixels: Steam's are 600 × 900.
pub const CAPSULE_WIDTH: u32 = 600;
pub const CAPSULE_HEIGHT: u32 = 900;
/// Of the height the icon leaves free, the share above it: Steam puts its 280-pixel wide picture
/// 132 pixels down a capsule, 488 above its bottom.
const CAPSULE_ABOVE: f64 = 132.0 / 620.0;
/// An icon is enlarged at most this many times: a 256-pixel one spans the capsule, a 48-pixel one
/// stays small rather than blurred.
const CAPSULE_ENLARGE: u32 = 3;
/// The width the capsule's backdrop is blurred at, before it is stretched over all of it.
const BACKDROP_WIDTH: u32 = 32;

/// A game's icon (straight RGBA) made into a cover as Steam makes one from a wide picture (see the
/// module's doc): `CAPSULE_WIDTH` × `CAPSULE_HEIGHT`, premultiplied and opaque.
pub fn capsule(icon: Rgba) -> Result<Rgba, PictureError> {
    let icon = trimmed(icon);
    let (width, height) = (CAPSULE_WIDTH, CAPSULE_HEIGHT);
    let size = width.min(icon.width.max(icon.height).saturating_mul(CAPSULE_ENLARGE));
    let (w, h) = fitted(icon.width, icon.height, size);
    let (x, y) = (
        (width - w) / 2,
        (f64::from(height - h) * CAPSULE_ABOVE).round() as u32,
    );

    // The backdrop, small: the icon over its average colour, its edge pixels drawn out to the
    // capsule's edges, blurred, then stretched over the capsule.
    let shrink = f64::from(BACKDROP_WIDTH) / f64::from(width);
    let small_size = ((f64::from(size) * shrink).round() as u32).max(1);
    let small = fit(flattened(&icon), small_size)?;
    let (back_width, back_height) = (
        BACKDROP_WIDTH as usize,
        (f64::from(height) * shrink).round() as usize,
    );
    let (left, top) = (
        (f64::from(x) * shrink).round() as usize,
        (f64::from(y) * shrink).round() as usize,
    );
    let (small_width, small_height) = (small.width as usize, small.height as usize);
    let mut pixels = Vec::with_capacity(back_width * back_height * 4);
    for row in 0..back_height {
        let from_y = row.saturating_sub(top).min(small_height - 1);
        for column in 0..back_width {
            let from_x = column.saturating_sub(left).min(small_width - 1);
            pixels.extend_from_slice(&small.pixels[(from_y * small_width + from_x) * 4..][..4]);
        }
    }
    for _ in 0..BLUR_PASSES {
        let across = box_blur(&pixels, back_width, back_height, (1, 0));
        pixels = box_blur(&across, back_width, back_height, (0, 1));
    }
    let backdrop = Rgba {
        width: back_width as u32,
        height: back_height as u32,
        pixels,
    };
    let mut capsule = resized(backdrop, width, height)?;

    // The icon over it, sharp.
    let sharp = fit(icon, size)?;
    let (w, x, y, width) = (w as usize, x as usize, y as usize, width as usize);
    for (row, line) in sharp.pixels.chunks_exact(w * 4).enumerate() {
        let start = ((y + row) * width + x) * 4;
        for (under, over) in capsule.pixels[start..start + w * 4]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(line.as_chunks::<4>().0)
        {
            let keep = 255 - u32::from(over[3]);
            for channel in 0..4 {
                let below = (u32::from(under[channel]) * keep + 127) / 255;
                under[channel] = (u32::from(over[channel]) + below).min(255) as u8;
            }
        }
    }
    Ok(capsule)
}

/// A straight-RGBA picture made opaque over its own average colour (weighted by alpha): blurred,
/// its clear parts then take its colour, not black.
fn flattened(picture: &Rgba) -> Rgba {
    let (mut sum, mut weight) = ([0u64; 3], 0u64);
    for pixel in picture.pixels.as_chunks::<4>().0 {
        let alpha = u64::from(pixel[3]);
        for channel in 0..3 {
            sum[channel] += u64::from(pixel[channel]) * alpha;
        }
        weight += alpha;
    }
    let average = sum.map(|total| (total / weight.max(1)) as u8);
    let pixels = picture
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| {
            let alpha = u32::from(pixel[3]);
            let mix = |channel: usize| {
                ((u32::from(pixel[channel]) * alpha
                    + u32::from(average[channel]) * (255 - alpha)
                    + 127)
                    / 255) as u8
            };
            [mix(0), mix(1), mix(2), 255]
        })
        .collect();
    Rgba {
        width: picture.width,
        height: picture.height,
        pixels,
    }
}

/// Each channel of each pixel averaged with its two neighbours one `step` (x, y) away on either
/// side, the edge pixels standing in for those beyond it.
fn box_blur(pixels: &[u8], width: usize, height: usize, step: (usize, usize)) -> Vec<u8> {
    let mut out = vec![0; pixels.len()];
    for y in 0..height {
        for x in 0..width {
            let before = ((x.saturating_sub(step.0)), y.saturating_sub(step.1));
            let after = ((x + step.0).min(width - 1), (y + step.1).min(height - 1));
            let at = |(x, y): (usize, usize)| (y * width + x) * 4;
            let (here, before, after) = (at((x, y)), at(before), at(after));
            for channel in 0..4 {
                let sum = u32::from(pixels[before + channel])
                    + u32::from(pixels[here + channel])
                    + u32::from(pixels[after + channel]);
                out[here + channel] = ((sum + 1) / 3) as u8;
            }
        }
    }
    out
}

/// A frame this short or shorter shows for `SLOW_FRAME_MS` (see the module's doc).
const SHORTEST_FRAME_MS: u32 = 10;
const SLOW_FRAME_MS: u32 = 100;

/// Whether `bytes` are an animated picture `Frames` can play.
pub fn is_animated(bytes: &[u8]) -> bool {
    format(bytes) == Some(Format::Webp)
        && image_webp::WebPDecoder::new(Cursor::new(bytes))
            .is_ok_and(|decoder| decoder.is_animated() && decoder.num_frames() > 1)
}

/// An animated picture's frames, read one at a time as it plays (see the module's doc), from a
/// file (`open`) or bytes (`new`).
pub struct Frames<R> {
    decoder: image_webp::WebPDecoder<R>,
    width: u32,
    height: u32,
    /// How many times it plays in all; `None`: forever.
    plays: Option<u32>,
    /// How many times it has started.
    started: u32,
    /// The decoder's output: RGBA, or RGB when the picture has no alpha; empty until the first
    /// frame.
    buffer: Vec<u8>,
    /// The buffer's length.
    length: usize,
}

impl Frames<Cursor<Vec<u8>>> {
    /// The frames of an animated WebP's bytes; `None` when they are no such picture, or one too
    /// large.
    pub fn new(bytes: Vec<u8>) -> Option<Self> {
        Self::read(Cursor::new(bytes))
    }
}

impl Frames<BufReader<File>> {
    /// The frames of an animated WebP file, read from it as they play; `None` when it is no such
    /// picture, one too large, or can't be opened. Blocking: its chunks are listed.
    pub fn open(path: &Path) -> Option<Self> {
        Self::read(BufReader::new(File::open(path).ok()?))
    }
}

impl<R: BufRead + Seek> Frames<R> {
    fn read(reader: R) -> Option<Self> {
        let decoder = image_webp::WebPDecoder::new(reader).ok()?;
        if !decoder.is_animated() || decoder.num_frames() < 2 {
            return None;
        }
        let (width, height) = decoder.dimensions();
        check_size(width, height).ok()?;
        let length = decoder.output_buffer_size()?;
        let plays = match decoder.loop_count() {
            image_webp::LoopCount::Forever => None,
            image_webp::LoopCount::Times(times) => Some(u32::from(times.get())),
        };
        Some(Self {
            decoder,
            width,
            height,
            plays,
            started: 1,
            buffer: Vec::new(),
            length,
        })
    }

    /// The picture's size in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The next frame (straight RGBA) and how long it shows. After the last it starts over, as
    /// many times as the file says; `None` once it has played them all (its last frame stays).
    pub fn next_frame(&mut self) -> Result<Option<(Rgba, Duration)>, PictureError> {
        let fail = |error: image_webp::DecodingError| damaged(Format::Webp, error);
        if self.buffer.is_empty() {
            self.buffer = vec![0; self.length];
        }
        let ms = match self.decoder.read_frame(&mut self.buffer) {
            Ok(ms) => ms,
            Err(image_webp::DecodingError::NoMoreFrames) => {
                if self.plays.is_some_and(|plays| self.started >= plays) {
                    return Ok(None);
                }
                self.started += 1;
                self.decoder.reset_animation();
                self.decoder.read_frame(&mut self.buffer).map_err(fail)?
            }
            Err(error) => return Err(fail(error)),
        };
        let ms = if ms <= SHORTEST_FRAME_MS {
            SLOW_FRAME_MS
        } else {
            ms
        };
        let pixels = if self.decoder.has_alpha() {
            self.buffer.clone()
        } else {
            self.buffer
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
                .collect()
        };
        let frame = Rgba::new(self.width, self.height, pixels, Format::Webp)?;
        Ok(Some((frame, Duration::from_millis(u64::from(ms)))))
    }

    /// Back to the first frame, its plays counted afresh.
    pub fn rewind(&mut self) {
        self.decoder.reset_animation();
        self.started = 1;
    }
}

/// A straight-RGBA picture made premultiplied and, when larger, shrunk with Catmull-Rom to the
/// smallest size that still covers `width` × `height`, its shape kept: an animation's frames are
/// then no larger than they are drawn.
pub fn covering(picture: Rgba, width: u32, height: u32) -> Result<Rgba, PictureError> {
    let scale = (f64::from(width) / f64::from(picture.width.max(1)))
        .max(f64::from(height) / f64::from(picture.height.max(1)));
    let premultiplied = premultiply(picture)?;
    if scale >= 1.0 {
        return Ok(premultiplied);
    }
    let side = |value: u32| ((f64::from(value) * scale).round() as u32).max(1);
    let (w, h) = (side(premultiplied.width), side(premultiplied.height));
    resized(premultiplied, w, h)
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
        // Pixels said to start inside the headers or the palette, or at an offset no file has.
        let at_offset = |offset: u32| {
            let mut file = bmp_file(&dib(2, 2, 24, 0, &[], &rows24, &[]), 40);
            file[10..14].copy_from_slice(&offset.to_le_bytes());
            decode(&file)
        };
        for offset in [0, 13, 14, 20, 53] {
            assert!(
                matches!(
                    at_offset(offset),
                    Err(PictureError::Damaged {
                        format: Format::Bmp,
                        ..
                    })
                ),
                "offset {offset}"
            );
        }
        assert_eq!(
            top_bottom(&at_offset(54).unwrap()),
            [RED, BLUE, white, black]
        );
        assert!(matches!(
            at_offset(u32::MAX),
            Err(PictureError::Damaged { .. })
        ));
        let in_palette = bmp_file(&dib(2, 2, 8, 0, &palette, &rows8, &[]), 40 + 8);
        assert!(matches!(
            decode(&in_palette),
            Err(PictureError::Damaged { .. })
        ));
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
    fn resampler_matches_across_paths_and_edge_shapes() {
        let pattern = |w: u32, h: u32| -> Rgba {
            let mut pixels = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h {
                for x in 0..w {
                    let a = ((x * 29 + y * 53 + 17) % 256) as u8;
                    let r = ((x * 73 + y * 19) % 256) as u8;
                    let g = ((x * 41 + y * 97) % 256) as u8;
                    let b = ((x * 13 + y * 61) % 256) as u8;
                    pixels.extend_from_slice(&[r, g, b, a]);
                }
            }
            premultiply(Rgba {
                width: w,
                height: h,
                pixels,
            })
            .unwrap()
        };

        // Exercise tiny, odd, horizontal-only, vertical-only, upscale, and downscale shapes
        // (including `in_w == 1`, unpadded `in_w == 3`, `in_h % 4 != 0`, and vertical tails).
        let cases = [
            (1, 1, 5, 5),
            (3, 3, 7, 7),
            (7, 5, 3, 2),
            (13, 9, 9, 13),
            (33, 17, 19, 11),
            (64, 16, 23, 16), // horizontal-only
            (23, 64, 23, 19), // vertical-only
            (64, 64, 24, 24),
            (64, 64, 54, 54),
            (64, 64, 150, 150),
        ];
        for (in_w, in_h, out_w, out_h) in cases {
            let src = pattern(in_w, in_h);
            let mut scalar = vec![0u8; (out_w * out_h * 4) as usize];
            resize_scalar(
                &src.pixels,
                in_w as usize,
                in_h as usize,
                out_w as usize,
                out_h as usize,
                &mut scalar,
            );
            // Premultiplied invariant: no channel above its alpha.
            assert!(
                scalar
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]),
                "{in_w}x{in_h} -> {out_w}x{out_h}"
            );
            #[cfg(target_arch = "x86_64")]
            if std::arch::is_x86_feature_detected!("avx2") {
                let mut simd = vec![0u8; (out_w * out_h * 4) as usize];
                // SAFETY: `avx2` was just verified above.
                unsafe {
                    avx2::resize(
                        &src.pixels,
                        in_w as usize,
                        in_h as usize,
                        out_w as usize,
                        out_h as usize,
                        &mut simd,
                    );
                }
                assert_eq!(scalar, simd, "{in_w}x{in_h} -> {out_w}x{out_h}");
            }
            // A solid premultiplied colour stays bit-exact through any resize.
            let flat = solid(in_w, in_h, [40, 80, 120, 200]);
            let flat_out = resized(flat, out_w, out_h).unwrap();
            assert!(
                flat_out
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|&p| p == [40, 80, 120, 200]),
                "solid drift on {in_w}x{in_h} -> {out_w}x{out_h}"
            );
        }

        // Invalid dimensions or mismatched pixel buffers fail cleanly rather than panicking.
        let bad = Rgba {
            width: 4,
            height: 4,
            pixels: vec![0; 12],
        };
        assert!(matches!(
            premultiply(bad.clone()),
            Err(PictureError::Scale(_))
        ));
        assert!(matches!(
            unpremultiply(bad.clone()),
            Err(PictureError::Scale(_))
        ));
        assert!(matches!(resized(bad, 2, 2), Err(PictureError::Scale(_))));
        assert!(matches!(
            resized(solid(2, 2, RED), 0, 2),
            Err(PictureError::Scale(_))
        ));
    }

    #[test]
    fn premultiply_is_exact_for_all_byte_pairs() {
        // Every (channel, alpha) pair in 0..=255, plus an odd tail (< 8 pixels) and opaque run.
        let mut pixels = Vec::with_capacity((256 * 256 + 11) * 4);
        for a in 0..=255u8 {
            for c in 0..=255u8 {
                pixels.extend_from_slice(&[c, 255 - c, c / 2, a]);
            }
        }
        // 8 opaque pixels (exercises the opaque skip) + 3 tail pixels.
        for i in 0..8u8 {
            pixels.extend_from_slice(&[i * 30, i * 20, i * 10, 255]);
        }
        pixels.extend_from_slice(&[200, 100, 50, 128, 255, 255, 255, 0, 10, 20, 30, 255]);

        let mut expected = pixels.clone();
        for px in expected.as_chunks_mut::<4>().0 {
            let a = u32::from(px[3]);
            for ch in &mut px[..3] {
                *ch = ((u32::from(*ch) * a + 127) / 255) as u8;
            }
        }

        let mut scalar = pixels.clone();
        premultiply_scalar(&mut scalar);
        assert_eq!(scalar, expected);

        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") {
            let mut simd = pixels;
            // SAFETY: `avx2` was just verified above.
            unsafe {
                avx2::premultiply(&mut simd);
            }
            assert_eq!(simd, expected);
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

    #[test]
    fn a_logo_is_trimmed_to_what_shows() {
        // 6 × 4, clear but for an opaque pixel at (1, 1), one at (3, 2) and a faint one at (5, 3).
        let mut pixels = vec![0; 6 * 4 * 4];
        for (x, y, alpha) in [(1, 1, 255), (3, 2, 200), (5, 3, 4)] {
            pixels[(y * 6 + x) * 4..][..4].copy_from_slice(&[10, 20, 30, alpha]);
        }
        let logo = trimmed(Rgba {
            width: 6,
            height: 4,
            pixels,
        });
        assert_eq!((logo.width, logo.height), (3, 2));
        assert_eq!(&logo.pixels[..4], [10, 20, 30, 255]);
        assert_eq!(&logo.pixels[logo.pixels.len() - 4..], [10, 20, 30, 200]);
        // All clear, or nothing to cut: as it was.
        let clear = Rgba {
            width: 2,
            height: 2,
            pixels: vec![0; 16],
        };
        assert_eq!(trimmed(clear.clone()), clear);
        let full = Rgba {
            width: 1,
            height: 1,
            pixels: vec![1, 2, 3, 255],
        };
        assert_eq!(trimmed(full.clone()), full);
    }

    #[test]
    fn a_blur_is_small_soft_and_keeps_the_colours_apart_at_its_edges() {
        // 40 × 20: the left half red, the right half blue.
        let pixels = (0..20 * 40)
            .flat_map(|i| {
                if i % 40 < 20 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                }
            })
            .collect();
        let blur = blurred(
            Rgba {
                width: 40,
                height: 20,
                pixels,
            },
            16,
        )
        .unwrap();
        assert_eq!((blur.width, blur.height), (16, 8));
        let pixel = |x: usize, y: usize| &blur.pixels[(y * 16 + x) * 4..][..4];
        // The edges keep their colour, the middle mixes both, and every pixel stays opaque.
        assert!(
            pixel(0, 4)[0] > 200 && pixel(0, 4)[2] < 40,
            "{:?}",
            pixel(0, 4)
        );
        assert!(
            pixel(15, 4)[2] > 200 && pixel(15, 4)[0] < 40,
            "{:?}",
            pixel(15, 4)
        );
        let middle = pixel(8, 4);
        assert!(middle[0] > 40 && middle[2] > 40, "{middle:?}");
        assert!(blur.pixels.chunks(4).all(|p| p[3] == 255));
        // Rows hold no stripes: a column is the same from top to bottom.
        assert_eq!(pixel(8, 0), pixel(8, 7));
    }

    #[test]
    fn an_icon_becomes_a_capsule_across_its_width_with_its_edges_drawn_out() {
        // A 256-pixel icon with a clear margin of 28 around a 200-pixel square: its left half red,
        // its right half blue.
        let pixels = (0..256 * 256)
            .flat_map(|i| {
                let (x, y) = (i % 256, i / 256);
                if !(28..228).contains(&x) || !(28..228).contains(&y) {
                    [0, 0, 0, 0]
                } else if x < 128 {
                    RED
                } else {
                    BLUE
                }
            })
            .collect();
        let capsule = capsule(Rgba {
            width: 256,
            height: 256,
            pixels,
        })
        .unwrap();
        assert_eq!(
            (capsule.width, capsule.height),
            (CAPSULE_WIDTH, CAPSULE_HEIGHT)
        );
        let pixel = |x: usize, y: usize| &capsule.pixels[(y * 600 + x) * 4..][..4];
        // Trimmed and enlarged three times, it spans the capsule, 64 pixels down (Steam's share
        // of the 300 left free above it): sharp, its halves still red and blue.
        let close = |p: &[u8], q: [u8; 4]| p.iter().zip(q).all(|(a, b)| a.abs_diff(b) <= 2);
        for (x, y, colour) in [
            (10, 64, RED),
            (589, 663, BLUE),
            (290, 364, RED),
            (310, 364, BLUE),
        ] {
            assert!(close(pixel(x, y), colour), "{x}, {y}: {:?}", pixel(x, y));
        }
        // Above and below, its edges drawn out and blurred: red on the left, blue on the
        // right, and opaque everywhere.
        for y in [5, 800] {
            assert!(
                pixel(20, y)[0] > 200 && pixel(20, y)[2] < 60,
                "{:?}",
                pixel(20, y)
            );
            assert!(
                pixel(580, y)[2] > 200 && pixel(580, y)[0] < 60,
                "{:?}",
                pixel(580, y)
            );
        }
        assert!(capsule.pixels.chunks(4).all(|p| p[3] == 255));
    }

    #[test]
    fn a_small_icon_stays_small_and_its_clear_parts_lend_no_black() {
        // A 48-pixel green dot on a clear ground.
        let pixels = (0..48 * 48)
            .flat_map(|i| {
                let (x, y) = (i % 48, i / 48);
                let (dx, dy) = (x - 24, y - 24);
                if dx * dx + dy * dy < 20 * 20 {
                    [0, 200, 0, 255]
                } else {
                    [0, 0, 0, 0]
                }
            })
            .collect();
        let capsule = capsule(Rgba {
            width: 48,
            height: 48,
            pixels,
        })
        .unwrap();
        let pixel = |x: usize, y: usize| &capsule.pixels[(y * 600 + x) * 4..][..4];
        // At most three times its size (the dot is 39 across once trimmed: 117 here), across the
        // middle.
        let middle = pixel(300, 225);
        assert!(
            middle[0] < 3 && middle[1].abs_diff(200) < 3 && middle[3] == 255,
            "{middle:?}"
        );
        // The corners of the backdrop are the dot's green, never black.
        for (x, y) in [(0, 0), (599, 0), (0, 899), (599, 899)] {
            assert!(pixel(x, y)[1] > 150, "{:?}", pixel(x, y));
        }
    }

    /// An animated WebP of whole-canvas `frames` (RGBA, `width` × `height`, each shown `ms`),
    /// played `plays` times (0: forever), as encoders write one: each frame a lossless still.
    fn animated_webp(width: u32, height: u32, frames: &[(Vec<u8>, u32)], plays: u16) -> Vec<u8> {
        let chunk = |name: &[u8; 4], data: &[u8]| {
            let mut chunk = name.to_vec();
            chunk.extend((data.len() as u32).to_le_bytes());
            chunk.extend(data);
            if data.len() % 2 == 1 {
                chunk.push(0);
            }
            chunk
        };
        let three = |value: u32| value.to_le_bytes()[..3].to_vec();
        let mut body = b"WEBP".to_vec();
        // Animation and alpha; the canvas size less one.
        let mut vp8x = vec![0x02 | 0x10, 0, 0, 0];
        vp8x.extend(three(width - 1));
        vp8x.extend(three(height - 1));
        body.extend(chunk(b"VP8X", &vp8x));
        let mut anim = vec![0; 4];
        anim.extend(plays.to_le_bytes());
        body.extend(chunk(b"ANIM", &anim));
        for (pixels, ms) in frames {
            let mut still = Vec::new();
            image_webp::WebPEncoder::new(&mut still)
                .encode(pixels, width, height, image_webp::ColorType::Rgba8)
                .unwrap();
            let at = still.windows(4).position(|name| name == b"VP8L").unwrap();
            let mut frame = [
                three(0),
                three(0),
                three(width - 1),
                three(height - 1),
                three(*ms),
            ]
            .concat();
            // Not blended: each frame replaces the canvas.
            frame.push(0x02);
            frame.extend(&still[at..]);
            body.extend(chunk(b"ANMF", &frame));
        }
        let mut file = b"RIFF".to_vec();
        file.extend((body.len() as u32).to_le_bytes());
        file.extend(body);
        file
    }

    #[test]
    fn an_animation_plays_its_frames_for_their_durations_as_many_times_as_it_says() {
        let (red, blue) = (RED.repeat(16), BLUE.repeat(16));
        let file = animated_webp(4, 4, &[(red.clone(), 50), (blue.clone(), 0)], 2);
        assert!(is_animated(&file));
        // Still, it is its first frame.
        assert_eq!(decode(&file).unwrap().pixels, red);
        let mut frames = Frames::new(file).unwrap();
        assert_eq!(frames.size(), (4, 4));
        let mut next = || {
            frames
                .next_frame()
                .unwrap()
                .map(|(frame, shows)| (frame.pixels, shows.as_millis()))
        };
        // A frame of 0 ms shows for 100, as browsers show it.
        for _ in 0..2 {
            assert_eq!(next(), Some((red.clone(), 50)));
            assert_eq!(next(), Some((blue.clone(), 100)));
        }
        // Played twice: done, the last frame stays.
        assert_eq!(next(), None);
        frames.rewind();
        assert_eq!(frames.next_frame().unwrap().unwrap().0.pixels, red);
        // Forever goes round and round.
        let mut forever =
            Frames::new(animated_webp(4, 4, &[(red.clone(), 50), (blue, 50)], 0)).unwrap();
        for _ in 0..7 {
            assert!(forever.next_frame().unwrap().is_some());
        }
        // From a file, read as it plays.
        let path = std::env::temp_dir().join(format!("gm-frames-{}.webp", std::process::id()));
        std::fs::write(
            &path,
            animated_webp(4, 4, &[(red.clone(), 50), (BLUE.repeat(16), 50)], 1),
        )
        .unwrap();
        let mut from_file = Frames::open(&path).unwrap();
        assert_eq!(from_file.next_frame().unwrap().unwrap().0.pixels, red);
        drop(from_file);
        std::fs::remove_file(&path).unwrap();
        assert!(Frames::open(&path).is_none());
        // A still picture is no animation.
        let mut still = Vec::new();
        image_webp::WebPEncoder::new(&mut still)
            .encode(&red, 4, 4, image_webp::ColorType::Rgba8)
            .unwrap();
        assert!(!is_animated(&still) && Frames::new(still).is_none());
        assert!(Frames::new(b"not a picture".to_vec()).is_none());
    }

    #[test]
    fn frames_are_made_no_larger_than_they_cover() {
        let size = |picture: Rgba, width, height| {
            let covered = covering(picture, width, height).unwrap();
            (covered.width, covered.height)
        };
        // A Steam cover on a 200 × 300 tile at 225%: 450 × 675.
        assert_eq!(size(solid(600, 900, RED), 450, 675), (450, 675));
        // Wider than the box: as tall as it, wider than it, as the cover is cut around its centre.
        assert_eq!(size(solid(1000, 500, RED), 200, 300), (600, 300));
        // Never enlarged.
        assert_eq!(size(solid(300, 200, RED), 450, 675), (300, 200));
        // Premultiplied.
        let half = covering(solid(2, 2, [200, 100, 0, 128]), 2, 2).unwrap();
        assert_eq!(half.pixels[..4], [100, 50, 0, 128]);
    }
}
