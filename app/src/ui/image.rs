//! Pictures for the window: PNG bytes (the core's game icons) decoded with WIC into premultiplied
//! BGRA pixels, the form Direct2D bitmaps take.
//!
//! Decoding runs on the background thread that extracted the icons, so the UI thread only uploads
//! ready pixels. The thread gets its own COM initialization for the call (the core's `ComGuard` is
//! private to it).

use std::marker::PhantomData;
use windows::Win32::{
    CLSCTX_INPROC_SERVER, CLSID_WICImagingFactory, COINIT_MULTITHREADED, CoCreateInstance,
    CoInitializeEx, CoUninitialize, GENERIC_WRITE, GUID_ContainerFormatPng,
    GUID_WICPixelFormat32bppPBGRA, IPropertyBag2, IWICBitmapFrameEncode, IWICImagingFactory,
    IWICPalette, SHCreateMemStream, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
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

/// Every interface used comes from the WIC call just before it, which succeeded.
fn decode_with(factory: &IWICImagingFactory, bytes: &[u8]) -> windows::core::Result<Picture> {
    // SAFETY: the stream copies `bytes`, which are valid for their length.
    let stream = unsafe { SHCreateMemStream(Some(bytes.as_ptr()), bytes.len() as u32) }
        .ok_or_else(windows::core::Error::empty)?;
    // SAFETY: `stream` is live; no vendor preference.
    let decoder =
        unsafe { factory.CreateDecoderFromStream(&stream, None, WICDecodeMetadataCacheOnDemand) }?;
    // SAFETY: frame 0 exists in any picture that decoded.
    let frame = unsafe { decoder.GetFrame(0) }?;
    // SAFETY: a plain factory call.
    let converter = unsafe { factory.CreateFormatConverter() }?;
    // SAFETY: `frame` is live; no palette is needed for 32-bit output.
    unsafe {
        converter.Initialize(
            &frame,
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
