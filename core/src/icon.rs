//! Extracts game icons as PNG bytes with native Windows APIs: the Shell extracts the icon from an
//! `.ico`, `.exe` or `.dll` at the requested size, and WIC (Windows Imaging Component) encodes it.

use crate::IconRef;
use std::ffi::c_void;
use std::marker::PhantomData;
use windows::Win32::{
    CLSCTX_INPROC_SERVER, CLSID_WICImagingFactory, COINIT_MULTITHREADED, CoCreateInstance,
    CoInitializeEx, CoUninitialize, DestroyIcon, GUID_ContainerFormatPng, HICON, IPropertyBag2,
    IStream, IWICBitmapFrameEncode, IWICImagingFactory, LARGE_INTEGER, SHCreateMemStream,
    SHDefExtractIconW, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET, WICBitmapEncoderNoCache,
};
use windows::core::{HSTRING, Result};

/// Converts icons to PNG. Create one per thread and reuse it for a batch of icons.
pub struct IconExtractor {
    factory: IWICImagingFactory,
    _com: ComGuard,
}

impl IconExtractor {
    pub fn new() -> Result<Self> {
        let com = ComGuard::new();
        // SAFETY: COM is initialized on this thread for the lifetime of `_com`.
        let factory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
        Ok(Self { factory, _com: com })
    }

    /// The icon as a `size`×`size` PNG, or `None` if the file has no usable icon. PNG and JPEG
    /// files (e.g. Xbox package logos) are returned as they are; the UI scales them.
    pub fn png(&self, icon: &IconRef, size: u32) -> Option<Vec<u8>> {
        if is_image_file(&icon.path) {
            return std::fs::read(&icon.path).ok();
        }
        let hicon = extract(icon, size)?;
        let png = self.encode_png(hicon);
        // SAFETY: `hicon` was created by SHDefExtractIconW and is not used after this.
        unsafe {
            let _ = DestroyIcon(hicon);
        }
        png.ok()
    }

    fn encode_png(&self, hicon: HICON) -> Result<Vec<u8>> {
        // SAFETY: every interface comes from WIC/Shell calls that succeeded; the out-pointers are
        // valid locals, and `buffer` is sized from the stream's reported length.
        unsafe {
            let bitmap = self.factory.CreateBitmapFromHICON(hicon)?;
            let stream = SHCreateMemStream(None).ok_or_else(windows::core::Error::from_thread)?;

            let encoder = self
                .factory
                .CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache).ok()?;
            let mut frame: Option<IWICBitmapFrameEncode> = None;
            let mut options: Option<IPropertyBag2> = None;
            encoder.CreateNewFrame(&mut frame, &mut options).ok()?;
            let frame = frame.ok_or_else(windows::core::Error::empty)?;
            frame.Initialize(options.as_ref()).ok()?;
            frame.WriteSource(&bitmap, std::ptr::null()).ok()?;
            frame.Commit().ok()?;
            encoder.Commit().ok()?;

            read_all(&stream)
        }
    }
}

/// Extracts the icons of `games` as PNGs, keyed by game id. Games without a usable icon are left
/// out. Blocking: run it off the UI thread.
pub fn extract_all(games: &[crate::Game], size: u32) -> Vec<(String, Vec<u8>)> {
    let Ok(extractor) = IconExtractor::new() else {
        return Vec::new();
    };
    games
        .iter()
        .filter_map(|game| Some((game.id.clone(), extractor.png(game.icon.as_ref()?, size)?)))
        .collect()
}

fn is_image_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["png", "jpg", "jpeg"]
                .iter()
                .any(|e| ext.eq_ignore_ascii_case(e))
        })
}

/// Extracts the icon at `size` pixels, scaling from the closest size the file contains.
fn extract(icon: &IconRef, size: u32) -> Option<HICON> {
    let path = HSTRING::from(icon.path.as_os_str());
    let mut hicon: HICON = std::ptr::null_mut();
    // SAFETY: `path` is a valid wide string and `hicon` a valid out-pointer. The low word of the
    // last argument is the large icon's size; no small icon is requested.
    let hr = unsafe { SHDefExtractIconW(&path, icon.index, 0, Some(&mut hicon), None, size) };
    (hr.0 == 0 && !hicon.is_null()).then_some(hicon)
}

/// Reads a whole memory stream from the beginning.
unsafe fn read_all(stream: &IStream) -> Result<Vec<u8>> {
    // SAFETY: the caller passes a live stream; `stat` and `buffer` are valid for the calls.
    unsafe {
        let mut stat = STATSTG::default();
        stream.Stat(&mut stat, STATFLAG_NONAME as u32).ok()?;
        stream
            .Seek(LARGE_INTEGER { QuadPart: 0 }, STREAM_SEEK_SET as u32, None)
            .ok()?;
        let mut buffer = vec![0u8; stat.cbSize.QuadPart as usize];
        let mut read = 0u32;
        stream
            .Read(
                buffer.as_mut_ptr().cast::<c_void>(),
                buffer.len() as u32,
                Some(&mut read),
            )
            .ok()?;
        buffer.truncate(read as usize);
        Ok(buffer)
    }
}

/// Initializes COM for the current thread and balances it on drop. Not `Send`, so it is always
/// dropped on the thread that created it.
struct ComGuard {
    initialized: bool,
    _not_send: PhantomData<*const ()>,
}

impl ComGuard {
    fn new() -> Self {
        // SAFETY: plain COM initialization. If the thread already uses another apartment model
        // (RPC_E_CHANGED_MODE) COM is still usable, but must not be uninitialized by us.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED as u32) };
        Self {
            initialized: hr.is_ok(),
            _not_send: PhantomData,
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: balances the successful CoInitializeEx in `new`; the guard is not `Send`,
            // so this runs on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];

    fn system_file(name: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join(name)
    }

    #[test]
    fn extracts_exe_icon_as_png() {
        let extractor = IconExtractor::new().unwrap();
        let png = extractor
            .png(&IconRef::file(system_file("explorer.exe")), 64)
            .unwrap();
        assert_eq!(png[..8], PNG_SIGNATURE);
        // IHDR width and height (big-endian) follow the signature and chunk header.
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 64);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 64);
    }

    #[test]
    fn image_files_are_used_as_they_are() {
        let file =
            std::env::temp_dir().join(format!("gm-core-icon-test-{}.PNG", std::process::id()));
        let bytes = [PNG_SIGNATURE.as_slice(), b"rest of the file"].concat();
        std::fs::write(&file, &bytes).unwrap();
        let extractor = IconExtractor::new().unwrap();
        assert_eq!(extractor.png(&IconRef::file(&file), 64), Some(bytes));
        std::fs::remove_file(&file).unwrap();
    }

    #[test]
    fn missing_file_has_no_icon() {
        let extractor = IconExtractor::new().unwrap();
        let missing = IconRef::file(r"C:\does\not\exist\game.exe");
        assert!(extractor.png(&missing, 64).is_none());
    }
}
