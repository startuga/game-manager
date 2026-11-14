//! A folder of pictures downloaded from the stores, shared by the covers (`covers.rs`) and the
//! game page's art (`page_art.rs`), each in its own folder under `%LOCALAPPDATA%\Game Manager\cache`.
//!
//! - **Kept byte for byte**: a picture is written as the store sent it, under its format's
//!   extension, named after a hash of what was asked (an identity string the caller makes, holding
//!   its own format number). It is read back only when its content is the format its name says.
//! - **Misses**: a lookup that found nothing leaves an empty `.none` file and is not asked again
//!   for a week (`MISS_AGE`); a failed request leaves nothing and is tried again next time.
//! - **Whole files**: written through a temporary file of their own (`whole_file::write`).
//!   `prune` deletes what no game uses any more, and temporary files older than ten minutes (a
//!   younger one may be a download still being written).
//!
//! Every failure here only means "not cached".

use crate::picture::{self, Format};
use crate::text::fnv1a;
use crate::{json_file, whole_file};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long a lookup that found nothing is not asked again.
const MISS_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The extensions a picture is kept under, one per format.
pub(crate) const EXTENSIONS: [&str; 5] = ["jpg", "png", "webp", "bmp", "ico"];

/// One folder of downloaded pictures and the `.none` markers of lookups that found nothing.
pub(crate) struct PictureCache {
    pub(crate) dir: PathBuf,
}

impl PictureCache {
    /// `%LOCALAPPDATA%\Game Manager\cache\<name>`, created; `None` when it cannot be used.
    pub(crate) fn open(name: &str) -> Option<Self> {
        Self::at(Self::dir(name)?)
    }

    /// The cache as it is, for reading only: nothing is created.
    pub(crate) fn existing(name: &str) -> Option<Self> {
        Some(Self {
            dir: Self::dir(name)?,
        })
        .filter(|cache| cache.dir.is_dir())
    }

    fn dir(name: &str) -> Option<PathBuf> {
        Some(json_file::local_file("cache").ok()?.join(name))
    }

    pub(crate) fn at(dir: PathBuf) -> Option<Self> {
        fs::create_dir_all(&dir).ok()?;
        Some(Self { dir })
    }

    pub(crate) fn stem(&self, identity: &str) -> PathBuf {
        self.dir
            .join(format!("{:016x}", fnv1a(identity.as_bytes())))
    }

    /// Where a picture may be cached: under each format's extension.
    pub(crate) fn files(&self, identity: &str) -> Vec<PathBuf> {
        let stem = self.stem(identity);
        EXTENSIONS
            .into_iter()
            .map(|extension| stem.with_extension(extension))
            .collect()
    }

    pub(crate) fn existing_file(&self, identity: &str) -> Option<PathBuf> {
        self.files(identity).into_iter().find(|file| file.is_file())
    }

    pub(crate) fn miss_file(&self, identity: &str) -> PathBuf {
        self.stem(identity).with_extension("none")
    }

    /// The cached file that holds a picture of the format its name says.
    pub(crate) fn find(&self, identity: &str) -> Option<PathBuf> {
        self.files(identity)
            .into_iter()
            .find(|file| read_picture(file).is_some())
    }

    /// Kept byte for byte under its format's extension: the file, or `None` when it is no
    /// picture or can't be written. A found picture also ends the wait a miss started.
    pub(crate) fn write(&self, identity: &str, bytes: &[u8]) -> Option<PathBuf> {
        let format = picture::format(bytes)?;
        let file = self.stem(identity).with_extension(format.extension());
        for other in self
            .files(identity)
            .into_iter()
            .filter(|other| *other != file)
        {
            let _ = fs::remove_file(other);
        }
        let written = whole_file::write(&file, bytes).is_ok();
        let _ = fs::remove_file(self.miss_file(identity));
        written.then_some(file)
    }

    /// Whether this was looked up and not found less than `MISS_AGE` ago.
    pub(crate) fn missed(&self, identity: &str) -> bool {
        fs::metadata(self.miss_file(identity))
            .and_then(|marker| marker.modified())
            .ok()
            .and_then(|at| SystemTime::now().duration_since(at).ok())
            .is_some_and(|age| age < MISS_AGE)
    }

    pub(crate) fn mark_miss(&self, identity: &str) {
        let _ = fs::write(self.miss_file(identity), b"");
    }

    pub(crate) fn forget(&self, identity: &str) {
        for file in self.files(identity) {
            let _ = fs::remove_file(file);
        }
        let _ = fs::remove_file(self.miss_file(identity));
    }

    /// The files and the miss marker of each identity, for `prune`.
    pub(crate) fn used(&self, identities: impl IntoIterator<Item = String>) -> HashSet<PathBuf> {
        let mut used = HashSet::new();
        for identity in identities {
            used.extend(self.files(&identity));
            used.insert(self.miss_file(&identity));
        }
        used
    }

    /// Deletes cached pictures and markers that are not in `used`, and temporary files left by
    /// writes that ended long ago (a younger one may be a download being written).
    pub(crate) fn prune(&self, used: &HashSet<PathBuf>) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            let cached = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| EXTENSIONS.contains(&ext) || ext == "none");
            if (cached && !used.contains(&path)) || whole_file::is_stale_temp(&path) {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

/// A file's bytes; `None` when it can't be read or is empty.
pub(crate) fn read_file(file: &Path) -> Option<Vec<u8>> {
    fs::read(file).ok().filter(|bytes| !bytes.is_empty())
}

/// A picture file's format and size, for `scan --covers`.
pub(crate) fn describe_file(file: &Path) -> String {
    let Some(bytes) = read_file(file) else {
        return "unreadable".to_owned();
    };
    match (picture::format(&bytes), picture::size(&bytes)) {
        (Some(format), Some((width, height))) => format!("{format} {width} × {height}"),
        _ => "not a picture".to_owned(),
    }
}

/// A file that holds a picture of the format its extension says.
pub(crate) fn read_picture(file: &Path) -> Option<Vec<u8>> {
    let bytes = read_file(file)?;
    let extension = file.extension()?.to_str()?;
    (picture::format(&bytes).map(Format::extension) == Some(extension)).then_some(bytes)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::picture::Rgba;

    pub(crate) fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Pictures-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A PNG of one colour.
    pub(crate) fn png(width: u32, height: u32) -> Vec<u8> {
        picture::encode_png(&Rgba {
            width,
            height,
            pixels: [30, 60, 90, 255].repeat((width * height) as usize),
        })
        .unwrap()
    }

    /// An uncompressed 24-bit BMP: the left half red, the right half blue.
    pub(crate) fn bmp(width: u32, height: u32) -> Vec<u8> {
        let row = (width * 3).div_ceil(4) * 4;
        let size = 54 + row * height;
        let mut file = Vec::with_capacity(size as usize);
        file.extend_from_slice(b"BM");
        file.extend_from_slice(&size.to_le_bytes());
        file.extend_from_slice(&[0; 4]);
        file.extend_from_slice(&54u32.to_le_bytes());
        file.extend_from_slice(&40u32.to_le_bytes());
        file.extend_from_slice(&width.to_le_bytes());
        file.extend_from_slice(&height.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&24u16.to_le_bytes());
        file.extend_from_slice(&[0; 24]);
        for _ in 0..height {
            for x in 0..width {
                // Blue, green, red.
                file.extend_from_slice(if x < width / 2 {
                    &[0, 0, 255]
                } else {
                    &[255, 0, 0]
                });
            }
            file.extend_from_slice(&vec![0; (row - width * 3) as usize]);
        }
        file
    }

    /// What the cache holds for `identity`, read from its file.
    pub(crate) fn cached(cache: &PictureCache, identity: &str) -> Option<Vec<u8>> {
        cache.find(identity).map(|file| fs::read(file).unwrap())
    }

    #[test]
    fn the_cache_keeps_originals_by_format_and_week_long_misses() {
        let cache = PictureCache::at(temp_dir("cache")).unwrap();
        let (half_life, portal) = ("net|steam:220|2", "net|steam:400|2");
        assert_eq!(cached(&cache, half_life), None);
        assert!(!cache.missed(half_life));
        // Kept byte for byte, under the extension its format has.
        let original = png(6, 9);
        let file = cache.write(half_life, &original).unwrap();
        assert_eq!(cached(&cache, half_life), Some(original.clone()));
        assert_eq!(cache.existing_file(half_life), Some(file.clone()));
        assert_eq!(cache.find(half_life), Some(file.clone()));
        assert_eq!(file.extension().unwrap(), "png");
        assert_eq!(fs::read(&file).unwrap(), original);
        // A new original in another format replaces it; what is no picture is not written.
        let bitmap = bmp(4, 6);
        cache.write(half_life, &bitmap);
        assert_eq!(cached(&cache, half_life), Some(bitmap.clone()));
        assert!(!file.exists());
        assert_eq!(cache.write(portal, b"<html>"), None);
        assert_eq!(cache.existing_file(portal), None);
        // A file whose content isn't what its name says is not read.
        fs::write(cache.stem(portal).with_extension("jpg"), png(2, 3)).unwrap();
        assert_eq!(cached(&cache, portal), None);
        cache.forget(portal);

        // A miss holds for a week, then the lookup may run again.
        cache.mark_miss(portal);
        assert!(cache.missed(portal));
        assert!(!cache.missed(half_life));
        let marker = fs::File::options()
            .write(true)
            .open(cache.miss_file(portal))
            .unwrap();
        let eight_days = Duration::from_secs(8 * 24 * 60 * 60);
        marker.set_modified(SystemTime::now() - eight_days).unwrap();
        drop(marker);
        assert!(!cache.missed(portal));
        // A picture found later ends the miss; forgetting drops both.
        cache.mark_miss(portal);
        cache.write(portal, &original);
        assert!(!cache.missed(portal));
        cache.forget(portal);
        assert_eq!(cached(&cache, portal), None);

        // Pruning keeps what is in use, and files that are not the cache's.
        cache.mark_miss(portal);
        let stale = cache.dir.join("0123456789abcdef.jpg");
        let old_webp = cache.dir.join("0123456789abcdee.webp");
        let leftover = cache.dir.join("fedcba9876543210.jpg.1-0.tmp");
        let in_progress = cache.dir.join("fedcba9876543210.jpg.1-1.tmp");
        let unrelated = cache.dir.join("notes.txt");
        for file in [&stale, &old_webp, &leftover, &in_progress, &unrelated] {
            fs::write(file, b"x").unwrap();
        }
        let leftover_file = fs::File::options().write(true).open(&leftover).unwrap();
        leftover_file
            .set_modified(SystemTime::now() - Duration::from_secs(60 * 60))
            .unwrap();
        drop(leftover_file);
        let used = cache.used([half_life.to_owned()]);
        cache.prune(&used);
        assert_eq!(cached(&cache, half_life), Some(bitmap));
        assert!(!cache.miss_file(portal).exists());
        assert!(!stale.exists() && !old_webp.exists() && !leftover.exists());
        assert!(in_progress.exists() && unrelated.exists());
        fs::remove_dir_all(&cache.dir).unwrap();
    }
}
