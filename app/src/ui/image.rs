//! Pictures for the window: PNG and JPEG bytes (the core's game icons and covers) decoded with WIC
//! into premultiplied BGRA pixels, the form Direct2D bitmaps take.
//!
//! - **Icons** (`decode_all`) keep their size; the renderer scales them.
//! - **Covers** (`decode_covers`) are scaled here to the tile's exact pixel size, and their rounded
//!   corners are cut into the alpha (`round_corners`), so the renderer needs no clipping and a
//!   cover takes only the memory its tile shows.
//!
//! Decoding runs on the background thread that read the bytes, so the UI thread only uploads
//! ready pixels. The thread gets its own COM initialization for the call (the core's `ComGuard` is
//! private to it).

use std::marker::PhantomData;
use windows::Win32::{
    CLSCTX_INPROC_SERVER, CLSID_WICImagingFactory, COINIT_MULTITHREADED, CoCreateInstance,
    CoInitializeEx, CoUninitialize, GENERIC_WRITE, GUID_ContainerFormatPng,
    GUID_WICPixelFormat32bppPBGRA, IPropertyBag2, IWICBitmapFrameDecode, IWICBitmapFrameEncode,
    IWICBitmapSource, IWICImagingFactory, IWICPalette, SHCreateMemStream, WICBitmapDitherTypeNone,
    WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};

/// Decoded pixels: premultiplied BGRA, `width * 4` bytes a row.
#[derive(Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// COM for the current thread while it lives.
struct Com {
    initialized: bool,
    _not_send: PhantomData<*const ()>,
}

impl Com {
    fn new() -> Self {
        // SAFETY: plain COM initialization; a thread already in another apartment
        // (RPC_E_CHANGED_MODE) can still use COM but must not be uninitialized by us.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED as u32) };
        Self {
            initialized: hr.is_ok(),
            _not_send: PhantomData,
        }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: balances the successful CoInitializeEx in `new`, on the same thread (not
            // `Send`).
            unsafe { CoUninitialize() };
        }
    }
}

/// Decodes pictures keyed by id (the core's PNG icons), with one WIC factory for all of them.
/// Bytes that are not a picture are left out.
pub fn decode_all(items: Vec<(String, Vec<u8>)>) -> Vec<(String, Picture)> {
    let _com = Com::new();
    // SAFETY: COM is initialized on this thread while `_com` lives.
    let factory: windows::core::Result<IWICImagingFactory> =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER as u32) };
    let Ok(factory) = factory else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|(id, bytes)| {
            decode_with(&factory, &bytes)
                .ok()
                .map(|picture| (id, picture))
        })
        .collect()
}

/// Decodes cover art keyed by id (the core's JPEG thumbnails) for tiles `size` pixels big, with
/// one WIC factory for all of them: each is scaled to exactly that size (WIC's Fant filter, which
/// averages when it shrinks), and its corners are rounded by `radius` pixels in the alpha, so the
/// renderer draws it like any picture. Bytes that are not a picture are left out.
pub fn decode_covers(
    items: Vec<(String, Vec<u8>)>,
    size: (u32, u32),
    radius: u32,
) -> Vec<(String, Picture)> {
    if size.0 == 0 || size.1 == 0 {
        return Vec::new();
    }
    let _com = Com::new();
    // SAFETY: COM is initialized on this thread while `_com` lives.
    let factory: windows::core::Result<IWICImagingFactory> =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER as u32) };
    let Ok(factory) = factory else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|(id, bytes)| {
            let mut picture = decode_scaled(&factory, &bytes, size).ok()?;
            round_corners(&mut picture, radius);
            Some((id, picture))
        })
        .collect()
}

/// Cuts rounded corners of `radius` pixels into a picture: the pixels outside each corner's
/// quarter circle fade out by how much of them it covers (premultiplied, so every channel is
/// scaled), which smooths the edge.
pub fn round_corners(picture: &mut Picture, radius: u32) {
    let (width, height) = (picture.width, picture.height);
    let radius = radius.min(width / 2).min(height / 2);
    let centre = radius as f32;
    for y in 0..radius {
        for x in 0..radius {
            // How far this pixel's centre is from the circle's, in the top-left corner.
            let (dx, dy) = (centre - (x as f32 + 0.5), centre - (y as f32 + 0.5));
            let cover = (centre + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            if cover >= 1.0 {
                continue;
            }
            // The same pixel mirrored into each corner.
            for (px, py) in [
                (x, y),
                (width - 1 - x, y),
                (x, height - 1 - y),
                (width - 1 - x, height - 1 - y),
            ] {
                let at = ((py * width + px) * 4) as usize;
                for channel in &mut picture.pixels[at..at + 4] {
                    *channel = (f32::from(*channel) * cover).round() as u8;
                }
            }
        }
    }
}

/// Writes premultiplied BGRA pixels to `path` as a PNG (the window's frame capture for checks,
/// `shell::WM_CAPTURE`).
pub fn save_png(
    pixels: &[u8],
    width: u32,
    height: u32,
    path: &std::path::Path,
) -> windows::core::Result<()> {
    let _com = Com::new();
    // SAFETY: COM is initialized on this thread while `_com` lives.
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER as u32) }?;
    // Every interface used below comes from the WIC call just before it, which succeeded.
    // SAFETY: `pixels` holds `width * 4 * height` bytes, as the stride and size say.
    let bitmap = unsafe {
        factory.CreateBitmapFromMemory(
            width,
            height,
            &GUID_WICPixelFormat32bppPBGRA,
            width * 4,
            pixels.len() as u32,
            pixels.as_ptr(),
        )
    }?;
    // SAFETY: a plain factory call.
    let stream = unsafe { factory.CreateStream() }?;
    // SAFETY: the path is a valid wide string; the file is created or replaced for writing.
    unsafe {
        stream.InitializeFromFilename(
            &windows::core::HSTRING::from(path.as_os_str()),
            GENERIC_WRITE as u32,
        )
    }
    .ok()?;
    // SAFETY: no vendor means "any encoder".
    let encoder = unsafe { factory.CreateEncoder(&GUID_ContainerFormatPng, None) }?;
    // SAFETY: `stream` is the live stream created above.
    unsafe { encoder.Initialize(&stream, WICBitmapEncoderNoCache) }.ok()?;
    let mut frame: Option<IWICBitmapFrameEncode> = None;
    let mut options: Option<IPropertyBag2> = None;
    // SAFETY: both out-pointers are valid locals.
    unsafe { encoder.CreateNewFrame(&mut frame, Some(&raw mut options)) }.ok()?;
    let frame = frame.ok_or_else(windows::core::Error::empty)?;
    // SAFETY: `options` is the property bag CreateNewFrame returned with this frame.
    unsafe { frame.Initialize(options.as_ref()) }.ok()?;
    // SAFETY: `bitmap` is live; no rectangle means all of it.
    unsafe { frame.WriteSource(&bitmap, None) }.ok()?;
    // SAFETY: the frame was initialized and written.
    unsafe { frame.Commit() }.ok()?;
    // SAFETY: the encoder's only frame was committed.
    unsafe { encoder.Commit() }.ok()
}

/// The picture in `bytes` at its own size.
fn decode_with(factory: &IWICImagingFactory, bytes: &[u8]) -> windows::core::Result<Picture> {
    let frame = first_frame(factory, bytes)?;
    pixels_of(factory, &frame)
}

/// The picture in `bytes` scaled to exactly `size` pixels.
fn decode_scaled(
    factory: &IWICImagingFactory,
    bytes: &[u8],
    size: (u32, u32),
) -> windows::core::Result<Picture> {
    let frame = first_frame(factory, bytes)?;
    // SAFETY: a plain factory call.
    let scaler = unsafe { factory.CreateBitmapScaler() }?;
    // SAFETY: `frame` is live, and the size is not zero (`decode_covers`).
    unsafe { scaler.Initialize(&frame, size.0, size.1, WICBitmapInterpolationModeFant) }.ok()?;
    pixels_of(factory, &scaler)
}

/// The first frame of the picture in `bytes`. Every interface used comes from the WIC call just
/// before it, which succeeded.
fn first_frame(
    factory: &IWICImagingFactory,
    bytes: &[u8],
) -> windows::core::Result<IWICBitmapFrameDecode> {
    // SAFETY: the stream copies `bytes`, which are valid for their length.
    let stream = unsafe { SHCreateMemStream(Some(bytes.as_ptr()), bytes.len() as u32) }
        .ok_or_else(windows::core::Error::empty)?;
    // SAFETY: `stream` is live; no vendor preference.
    let decoder =
        unsafe { factory.CreateDecoderFromStream(&stream, None, WICDecodeMetadataCacheOnDemand) }?;
    // SAFETY: frame 0 exists in any picture that decoded.
    unsafe { decoder.GetFrame(0) }
}

/// A WIC source's pixels as premultiplied BGRA. Every interface used comes from the WIC call
/// just before it, which succeeded.
fn pixels_of<S>(factory: &IWICImagingFactory, source: &S) -> windows::core::Result<Picture>
where
    for<'a> &'a S: windows::core::Param<IWICBitmapSource>,
{
    // SAFETY: a plain factory call.
    let converter = unsafe { factory.CreateFormatConverter() }?;
    // SAFETY: `source` is live; no palette is needed for 32-bit output.
    unsafe {
        converter.Initialize(
            source,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
    }
    .ok()?;
    let mut width = 0;
    let mut height = 0;
    // SAFETY: both out-pointers are valid locals.
    unsafe { converter.GetSize(&mut width, &mut height) }.ok()?;
    let stride = width * 4;
    let mut pixels = vec![0u8; (stride * height) as usize];
    // SAFETY: `pixels` holds exactly `stride * height` bytes; no rectangle means all of it.
    unsafe { converter.CopyPixels(None, stride, pixels.len() as u32, pixels.as_mut_ptr()) }.ok()?;
    Ok(Picture {
        width,
        height,
        pixels,
    })
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

    fn alpha(picture: &Picture, x: u32, y: u32) -> u8 {
        picture.pixels[((y * picture.width + x) * 4 + 3) as usize]
    }

    #[test]
    fn rounds_the_four_corners_into_the_alpha() {
        let mut picture = solid(12, 16, [255; 4]);
        round_corners(&mut picture, 4);
        // The very corners are gone, the same in all four.
        for (x, y) in [(0, 0), (11, 0), (0, 15), (11, 15)] {
            assert_eq!(alpha(&picture, x, y), 0, "{x},{y}");
        }
        // Along the curve a pixel is partly covered, mirrored alike; every channel fades with it.
        let edge = alpha(&picture, 1, 1);
        assert!(edge > 0 && edge < 255, "{edge}");
        assert_eq!(alpha(&picture, 10, 14), edge);
        assert_eq!(picture.pixels[((12 + 1) * 4) as usize], edge);
        // Inside the curve, along the edges past it and in the middle nothing changes.
        for (x, y) in [(3, 3), (4, 0), (0, 4), (6, 8), (11, 4), (7, 15)] {
            assert_eq!(alpha(&picture, x, y), 255, "{x},{y}");
        }
        // No radius, or a radius larger than the picture: nothing to do, nothing out of bounds.
        let mut square = solid(4, 4, [255; 4]);
        round_corners(&mut square, 0);
        assert_eq!(square, solid(4, 4, [255; 4]));
        round_corners(&mut square, 100);
        assert!(alpha(&square, 0, 0) < 255);
        assert_eq!(alpha(&square, 1, 1), 255);
    }

    #[test]
    fn decodes_covers_at_the_tiles_size() {
        // A 40 × 60 picture through a PNG file, as the core hands covers over.
        let file = std::env::temp_dir().join(format!("gm-app-cover-{}.png", std::process::id()));
        let source = solid(40, 60, [30, 60, 200, 255]);
        save_png(&source.pixels, 40, 60, &file).unwrap();
        let bytes = std::fs::read(&file).unwrap();
        std::fs::remove_file(&file).unwrap();

        let items = vec![
            ("steam:220".to_owned(), bytes.clone()),
            ("ea:1".to_owned(), b"not a picture".to_vec()),
        ];
        let covers = decode_covers(items, (20, 30), 3);
        assert_eq!(covers.len(), 1);
        let (id, cover) = &covers[0];
        assert_eq!(id, "steam:220");
        assert_eq!((cover.width, cover.height), (20, 30));
        assert_eq!(cover.pixels.len(), 20 * 30 * 4);
        // The colour survives the scaling; the corners are rounded.
        assert_eq!(cover.pixels[(15 * 20 + 10) * 4..][..4], [30, 60, 200, 255]);
        assert_eq!(alpha(cover, 0, 0), 0);
        assert_eq!(alpha(cover, 19, 29), 0);
        // Larger than the picture works too; no size gives nothing.
        let larger = decode_covers(vec![("a".to_owned(), bytes.clone())], (80, 120), 0);
        assert_eq!((larger[0].1.width, larger[0].1.height), (80, 120));
        assert!(decode_covers(vec![("a".to_owned(), bytes)], (0, 30), 0).is_empty());
    }
}
