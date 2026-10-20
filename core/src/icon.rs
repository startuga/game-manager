//! Extracts game icons as PNG bytes with native Windows APIs: the Shell extracts the icon from an
//! `.ico`, `.exe` or `.dll` at the requested size, and WIC (Windows Imaging Component) encodes it.
//!
//! Extracted icons are cached as PNGs in `%LOCALAPPDATA%\Game Manager\cache\icons`, named after the
//! icon file's path, index, size, length and modified time, so a game's files are only read again
//! when they change.

use crate::{Game, IconRef, json_file};
use std::collections::HashSet;
use std::ffi::c_void;
use std::fs;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
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
        // SAFETY: `hicon` is a live icon handle owned by the caller for this call. Every interface
        // used comes from the WIC or Shell call just before it, which succeeded; the out-pointers
        // are valid locals, and the null vendor and rectangle mean "any encoder" and "the whole
        // bitmap", as WIC documents.
        let stream = unsafe {
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
            stream
        };
        read_all(&stream)
    }
}

/// The icons of `games` as PNGs, keyed by game id, from the icon cache when present. Games without
/// a usable icon are left out. Blocking: run it off the UI thread.
pub fn extract_all(games: &[Game], size: u32) -> Vec<(String, Vec<u8>)> {
    extract_cached(games, size, IconCache::open().as_ref()).0
}

/// Like `extract_all`, then deletes cached icons no game uses any more. For a full library only:
/// pruning after a partial list would drop the other games' icons.
pub fn refresh_all(games: &[Game], size: u32) -> Vec<(String, Vec<u8>)> {
    let cache = IconCache::open();
    let (icons, used) = extract_cached(games, size, cache.as_ref());
    if let Some(cache) = &cache {
        cache.prune(&used);
    }
    icons
}

/// Icons from `cache` when present, else extracted and cached. Also returns the cache files used.
fn extract_cached(
    games: &[Game],
    size: u32,
    cache: Option<&IconCache>,
) -> (Vec<(String, Vec<u8>)>, HashSet<PathBuf>) {
    // COM and WIC start only when an icon really has to be extracted.
    let mut extractor: Option<Option<IconExtractor>> = None;
    let mut used = HashSet::new();
    let mut icons = Vec::new();
    for game in games {
        let Some(icon) = &game.icon else {
            continue;
        };
        let png = if is_image_file(&icon.path) {
            fs::read(&icon.path).ok()
        } else {
            let file = cache.and_then(|cache| cache.file(icon, size));
            if let Some(file) = &file {
                used.insert(file.clone());
            }
            file.as_deref().and_then(IconCache::read).or_else(|| {
                let png = extractor
                    .get_or_insert_with(|| IconExtractor::new().ok())
                    .as_ref()?
                    .png(icon, size)?;
                if let Some(file) = &file {
                    IconCache::write(file, &png);
                }
                Some(png)
            })
        };
        if let Some(png) = png {
            icons.push((game.id.clone(), png));
        }
    }
    (icons, used)
}

/// Bump when extraction changes, so icons cached by an older version are not reused.
const CACHE_FORMAT: u32 = 1;

/// Extracted icons as PNG files in one folder. Every failure only means "not cached".
struct IconCache {
    dir: PathBuf,
}

impl IconCache {
    /// `%LOCALAPPDATA%\Game Manager\cache\icons`; `None` when it cannot be used.
    fn open() -> Option<Self> {
        Self::at(json_file::local_file("cache").ok()?.join("icons"))
    }

    fn at(dir: PathBuf) -> Option<Self> {
        fs::create_dir_all(&dir).ok()?;
        Some(Self { dir })
    }

    /// The cache file of an icon at a size. Its name changes with the icon file's length and
    /// modified time, so an updated game gets its icon extracted again.
    fn file(&self, icon: &IconRef, size: u32) -> Option<PathBuf> {
        let metadata = fs::metadata(&icon.path).ok()?;
        let modified = metadata
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos();
        let identity = format!(
            "{}|{}|{size}|{}|{modified}|{CACHE_FORMAT}",
            icon.path.to_string_lossy().to_lowercase(),
            icon.index,
            metadata.len()
        );
        Some(
            self.dir
                .join(format!("{:016x}.png", fnv1a(identity.as_bytes()))),
        )
    }

    fn read(file: &Path) -> Option<Vec<u8>> {
        fs::read(file).ok().filter(|png| !png.is_empty())
    }

    /// Written through a temporary file, so another reader never sees half a PNG.
    fn write(file: &Path, png: &[u8]) {
        let temp = file.with_extension("png.tmp");
        if fs::write(&temp, png).is_ok() {
            let _ = fs::rename(&temp, file);
        }
    }

    /// Deletes cached icons (and leftover temporary files) that are not in `used`.
    fn prune(&self, used: &HashSet<PathBuf>) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            let cached = path
                .extension()
                .is_some_and(|ext| ext == "png" || ext == "tmp");
            if cached && !used.contains(&path) {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

/// FNV-1a, 64-bit: a small hash that stays the same across Rust versions and computers.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
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
fn read_all(stream: &IStream) -> Result<Vec<u8>> {
    // SAFETY: the reference keeps `stream` a live COM object for the calls. `stat` and `read` are
    // valid out-pointers; `buffer` has exactly the length passed to `Read`, so the stream cannot
    // write past it; with STATFLAG_NONAME, Stat allocates no name that would need freeing.
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

    fn temp_cache(test: &str) -> IconCache {
        let dir = std::env::temp_dir().join(format!("GM-IconCache-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        IconCache::at(dir).unwrap()
    }

    fn game_with_icon(id: &str, icon: IconRef) -> Game {
        Game {
            id: id.into(),
            name: id.into(),
            store: crate::Store::Steam,
            install_dir: None,
            launch: crate::Launch::Uri(String::new()),
            alt_launches: Vec::new(),
            icon: Some(icon),
            program: None,
            arch: None,
        }
    }

    #[test]
    fn cache_file_follows_the_icon_file() {
        let cache = temp_cache("key");
        let explorer = IconRef::file(system_file("explorer.exe"));
        let key = cache.file(&explorer, 64).unwrap();
        assert_eq!(cache.file(&explorer, 64).as_ref(), Some(&key));
        // The path's letter case does not matter; the size and the icon index do.
        let upper = IconRef::file(system_file("explorer.exe").to_string_lossy().to_uppercase());
        assert_eq!(cache.file(&upper, 64).as_ref(), Some(&key));
        assert_ne!(cache.file(&explorer, 128).as_ref(), Some(&key));
        let second_icon = IconRef {
            index: 1,
            ..explorer.clone()
        };
        assert_ne!(cache.file(&second_icon, 64).as_ref(), Some(&key));

        // A changed file (new length and modified time) gets a new cache file.
        let file = cache.dir.join("game.ico");
        fs::write(&file, b"first").unwrap();
        let before = cache.file(&IconRef::file(&file), 64);
        fs::write(&file, b"second version").unwrap();
        assert_ne!(cache.file(&IconRef::file(&file), 64), before);
        assert_eq!(
            cache.file(&IconRef::file(r"C:\does\not\exist.exe"), 64),
            None
        );
        fs::remove_dir_all(&cache.dir).unwrap();
    }

    #[test]
    fn extracts_once_then_reads_the_cache() {
        let cache = temp_cache("hit");
        let games = [game_with_icon(
            "steam:1",
            IconRef::file(system_file("explorer.exe")),
        )];
        let (icons, used) = extract_cached(&games, 64, Some(&cache));
        assert_eq!(icons[0].1[..8], PNG_SIGNATURE);
        let file = used.iter().next().unwrap().clone();
        assert_eq!(fs::read(&file).unwrap(), icons[0].1);

        // The second run returns what the cached file holds, so it did not extract again.
        fs::write(&file, b"from the cache").unwrap();
        let (icons, _) = extract_cached(&games, 64, Some(&cache));
        assert_eq!(icons[0].1, b"from the cache");
        // Without a cache, icons are extracted as before.
        let (icons, used) = extract_cached(&games, 64, None);
        assert_eq!(icons[0].1[..8], PNG_SIGNATURE);
        assert!(used.is_empty());
        fs::remove_dir_all(&cache.dir).unwrap();
    }

    #[test]
    fn prune_keeps_only_the_icons_in_use() {
        let cache = temp_cache("prune");
        let games = [game_with_icon(
            "steam:1",
            IconRef::file(system_file("explorer.exe")),
        )];
        let stale = cache.dir.join("0123456789abcdef.png");
        let leftover = cache.dir.join("fedcba9876543210.png.tmp");
        let unrelated = cache.dir.join("notes.txt");
        for file in [&stale, &leftover, &unrelated] {
            fs::write(file, b"x").unwrap();
        }
        let (_, used) = extract_cached(&games, 64, Some(&cache));
        cache.prune(&used);
        assert!(used.iter().all(|file| file.is_file()));
        assert!(!stale.exists());
        assert!(!leftover.exists());
        assert!(unrelated.exists());
        fs::remove_dir_all(&cache.dir).unwrap();
    }
}
