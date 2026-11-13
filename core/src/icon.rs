//! Game icons as they are in their files, at their best: each icon's single best image, byte for
//! byte, as a file of its own (`alone`): a PNG-compressed image as the `.png` it is, a bitmap in a
//! one-image `.ico` file (a bitmap icon needs the `.ico` header for its transparency mask). The
//! app decodes it and scales it once, to the pixels each place takes (`app/src/ui/image.rs`), so
//! every size comes from the sharpest image there is.
//!
//! - **The best image** (`best`): the largest true-colour one (24 bits a pixel or more), the
//!   deepest among equals; with no true colour, the largest and deepest there is. An icon often
//!   holds the same picture many times: small sizes, and 16 and 256 colours for old displays
//!   (The Sims 4's has 12 images, three of them 256 px).
//! - **Why one image, and a PNG outside `.ico`**: so every reader shows that image. Windows' own
//!   icon decoder (Explorer, image viewers) lists PNG-compressed images after bitmap ones, so a
//!   file with a 256 px PNG and smaller bitmaps shows as its 48 px one; and it can't read a PNG
//!   inside an `.ico` unless it is 32-bit RGBA (The Outer Worlds' 256 px one is 24-bit RGB).
//! - **An `.ico` file**: its best image.
//! - **A program or DLL** (`.exe`, `.dll`), mapped as data (`DataModule`: none of its code runs):
//!   an icon group resource (`RT_GROUP_ICON`) lists its images, each an `RT_ICON` resource; the
//!   best one is kept. `IconRef::index` picks the group as
//!   Windows does: 0 and up counts groups in the file's resource order, below 0 is the group whose
//!   resource number is its opposite. Since Windows 10 1903 system DLLs keep their icons in
//!   `%SystemRoot%\SystemResources\<name>.mun`, which is read when such a file has none.
//! - **PNG and JPEG files** (Xbox package logos) are taken as they are.
//!
//! **An image's size** comes from its own header (a PNG's, or a bitmap's), as the directory can't
//! say more than 255 and can be wrong. **Its depth** is the lower of its header's and the one the
//! icon's directory lists for it (`Listed`, `listed_bits`), or its header's when the directory
//! lists none. Both count: a 16-colour picture can be stored as a 32-bit PNG, and only the
//! directory tells it apart. The Sims 4's program holds its 16-colour, 256-colour and true-colour
//! 256 px images all as 32-bit RGBA PNGs, listed as 4, 8 and 32 bits; Windows chooses an icon's
//! image by the directory too.
//!
//! `pick` chooses the image for a size in any `.ico` file (one holding more than the best image
//! too): among the true-colour ones when there are any, that size exactly, the deepest colours
//! among those; else the smallest larger one; else the largest.
//!
//! Icons read from programs are cached in `%LOCALAPPDATA%\Game Manager\cache\icons` as `.png`
//! or `.ico` files (`alone`'s), named after the program's path, the icon's index, and the
//! program's length and modified time, so a game's program is only read again when it changes.
//! They are written whole, through a temporary file of their own (`json_file::write_whole`).
//! `.ico` and picture files are read where they are. No icon handle, GDI or WIC is used here.

use crate::data_module::{DataModule, int_resource};
use crate::folder_key;
use crate::text::fnv1a;
use crate::{Game, IconRef, json_file};
use std::cmp::Reverse;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// `RT_ICON` and `RT_GROUP_ICON`, the resource types of an icon's images and of its directory.
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
/// The first bytes of every PNG.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
/// Larger icon images are not icons.
const MAX_IMAGE: u32 = 1024;
/// True colour: 24 bits a pixel or more, against 16 or 256 colours.
const TRUE_COLOUR: u16 = 24;

/// The depth an image must have to be used, among images as deep as `deepest`: true colour when
/// there is any.
fn depth_floor(deepest: u16) -> u16 {
    deepest.min(TRUE_COLOUR)
}

/// One image of an `.ico` file: its size from its own header, its depth also from the directory
/// (see the module's doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IconImage {
    /// Its place in the file's directory (`image_data`).
    pub index: usize,
    pub width: u32,
    pub height: u32,
    /// Bits a pixel: 32 for colour with alpha.
    pub bits: u16,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// An image's own width, height and depth: from a PNG's header, or a bitmap's (whose height
/// counts the transparency mask too). `None` for anything else.
pub fn describe(data: &[u8]) -> Option<(u32, u32, u16)> {
    let (width, height, bits) = if is_png(data) {
        let be = |at: usize| -> Option<u32> {
            Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
        };
        let depth = u16::from(*data.get(24)?);
        let channels = match data.get(25)? {
            6 => 4,
            2 => 3,
            4 => 2,
            _ => 1,
        };
        (be(16)?, be(20)?, depth * channels)
    } else {
        if u32_at(data, 0)? < 40 {
            return None;
        }
        let width = i32::from_le_bytes(data.get(4..8)?.try_into().ok()?);
        let height = i32::from_le_bytes(data.get(8..12)?.try_into().ok()?);
        (
            u32::try_from(width).ok()?,
            height.unsigned_abs() / 2,
            u16_at(data, 14)?,
        )
    };
    let fits = |side: u32| (1..=MAX_IMAGE).contains(&side);
    (fits(width) && fits(height)).then_some((width, height, bits))
}

/// An icon image's bytes, with the depth the icon's directory lists for it (0: none).
#[derive(Clone, Copy, Debug)]
struct Listed<'a> {
    data: &'a [u8],
    bits: u16,
}

impl<'a> Listed<'a> {
    /// An image whose directory lists no depth.
    #[cfg(test)]
    fn unlisted(data: &'a [u8]) -> Self {
        Self { data, bits: 0 }
    }

    /// Its width and height from its own header, and its depth: the lower of its header's and
    /// the directory's, or its header's when the directory lists none.
    fn describe(&self) -> Option<(u32, u32, u16)> {
        let (width, height, own) = describe(self.data)?;
        let bits = if self.bits == 0 {
            own
        } else {
            own.min(self.bits)
        };
        Some((width, height, bits))
    }
}

/// The depth an icon directory's entry at `entry` lists (an `.ico` file's entries and a program's
/// icon group's start alike): its bit count, else what its colour count takes (16 colours: 4
/// bits), else 0.
fn listed_bits(directory: &[u8], entry: usize) -> u16 {
    match u16_at(directory, entry + 6) {
        Some(bits) if bits > 0 => bits,
        _ => directory
            .get(entry + 2)
            .and_then(|colours| colours.checked_sub(1))
            .map_or(0, |highest| (u8::BITS - highest.leading_zeros()) as u16),
    }
}

/// Whether `bytes` are a PNG file.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&PNG_SIGNATURE)
}

/// Whether `bytes` are an `.ico` file: its header says so.
pub fn is_ico(bytes: &[u8]) -> bool {
    u16_at(bytes, 0) == Some(0) && u16_at(bytes, 2) == Some(1)
}

/// Each directory entry of an `.ico` file with its image, in the file's order.
fn entries(ico: &[u8]) -> Vec<(usize, Listed<'_>)> {
    if !is_ico(ico) {
        return Vec::new();
    }
    let count = usize::from(u16_at(ico, 4).unwrap_or(0));
    (0..count)
        .filter_map(|index| {
            let entry = 6 + 16 * index;
            let size = u32_at(ico, entry + 8)? as usize;
            let offset = u32_at(ico, entry + 12)? as usize;
            let data = ico.get(offset..offset.checked_add(size)?)?;
            let bits = listed_bits(ico, entry);
            Some((index, Listed { data, bits }))
        })
        .collect()
}

/// The images an `.ico` file holds that can be read, with their place in the file.
pub fn images(ico: &[u8]) -> Vec<IconImage> {
    entries(ico)
        .into_iter()
        .filter_map(|(index, image)| {
            let (width, height, bits) = image.describe()?;
            Some(IconImage {
                index,
                width,
                height,
                bits,
            })
        })
        .collect()
}

/// Image `index` of an `.ico` file as it is in the file: a PNG, or a bitmap (a DIB with its
/// transparency mask), for `picture::icon` to decode.
pub(crate) fn image_data(ico: &[u8], index: usize) -> Option<&[u8]> {
    let (_, image) = entries(ico).into_iter().find(|(at, _)| *at == index)?;
    Some(image.data)
}

/// The image to show `size` pixels wide, among the true-colour ones when there are any: that
/// size exactly (the deepest colours among those), else the smallest larger one, else the
/// largest.
pub fn pick(images: &[IconImage], size: u32) -> Option<IconImage> {
    let floor = depth_floor(images.iter().map(|image| image.bits).max()?);
    let usable = || images.iter().copied().filter(|image| image.bits >= floor);
    usable()
        .filter(|image| image.width >= size)
        .min_by_key(|image| (image.width, Reverse(image.bits)))
        .or_else(|| usable().max_by_key(|image| (image.width, image.bits)))
}

/// An image's bytes with its width, height and depth.
type Described<'a> = (&'a [u8], (u32, u32, u16));

/// The best of `images` (bitmaps or PNGs), as `Listed::describe` describes them: the largest
/// true-colour one, the deepest among equals (the first of those); with no true colour, the
/// largest and deepest there is.
fn best<'a>(images: &[Listed<'a>]) -> Option<&'a [u8]> {
    let described: Vec<Described> = images
        .iter()
        .filter_map(|image| Some((image.data, image.describe()?)))
        .collect();
    let floor = depth_floor(described.iter().map(|(_, (_, _, bits))| *bits).max()?);
    let (data, _) = described
        .into_iter()
        .filter(|(_, (_, _, bits))| *bits >= floor)
        .rev()
        .max_by_key(|(_, (width, height, bits))| (u64::from(*width) * u64::from(*height), *bits))?;
    Some(data)
}

/// An icon image as a file of its own, byte for byte: a PNG as it is, a bitmap in a one-image
/// `.ico` file. `None` when it is neither.
fn alone(data: &[u8]) -> Option<Vec<u8>> {
    let (width, height, bits) = describe(data)?;
    if is_png(data) {
        return Some(data.to_vec());
    }
    // A side of 256 or more is written as 0.
    let side = |value: u32| if value >= 256 { 0 } else { value as u8 };
    let mut file = Vec::with_capacity(22 + data.len());
    file.extend(0u16.to_le_bytes());
    file.extend(1u16.to_le_bytes());
    file.extend(1u16.to_le_bytes());
    file.extend([side(width), side(height), 0, 0]);
    file.extend(1u16.to_le_bytes());
    file.extend(bits.to_le_bytes());
    file.extend(u32::try_from(data.len()).ok()?.to_le_bytes());
    file.extend(22u32.to_le_bytes());
    file.extend(data);
    Some(file)
}

/// The best of `images` as a file of its own (`best`, `alone`).
fn best_of(images: &[Listed]) -> Option<Vec<u8>> {
    alone(best(images)?)
}

/// The icon group `index` picks in a program (see the module's doc).
fn group(module: &DataModule, index: i32) -> Option<&[u8]> {
    let kind = int_resource(RT_GROUP_ICON);
    if index < 0 {
        let id = u16::try_from(index.checked_neg()?).ok()?;
        return module.resource(int_resource(id), kind);
    }
    let names = module.names(kind);
    let name = names.get(usize::try_from(index).ok()?)?;
    module.resource(name.as_pcwstr(), kind)
}

/// The images an icon group lists: each one's resource number and the depth listed for it.
fn group_entries(group: &[u8]) -> Vec<(u16, u16)> {
    if !is_ico(group) {
        return Vec::new();
    }
    let count = usize::from(u16_at(group, 4).unwrap_or(0));
    (0..count)
        .filter_map(|index| {
            let entry = 6 + 14 * index;
            Some((u16_at(group, entry + 12)?, listed_bits(group, entry)))
        })
        .collect()
}

/// A program's icon `index`: its best image as a file of its own, from the program or its `.mun`.
fn program_icon(file: &Path, index: i32) -> Option<Vec<u8>> {
    program_files(file).into_iter().find_map(|file| {
        let module = DataModule::load(&file)?;
        let images: Vec<Listed> = group_entries(group(&module, index)?)
            .into_iter()
            .filter_map(|(id, bits)| {
                let data = module.resource(int_resource(id), int_resource(RT_ICON))?;
                Some(Listed { data, bits })
            })
            .collect();
        best_of(&images)
    })
}

/// Where a program's icons may be: the file, then, for one of Windows' own, its `.mun`.
fn program_files(file: &Path) -> Vec<PathBuf> {
    let mut files = vec![file.to_path_buf()];
    if let Some(root) = std::env::var_os("SystemRoot").map(PathBuf::from)
        && folder_key(file).starts_with(&format!("{}\\", folder_key(&root)))
        && let Some(name) = file.file_name()
    {
        let mut mun = name.to_os_string();
        mun.push(".mun");
        let mun = root.join("SystemResources").join(mun);
        if mun.is_file() {
            files.push(mun);
        }
    }
    files
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)))
}

/// A game's icon as the app gets it: its best image as it is in the game's files, as a `.png`
/// file or a one-image `.ico` file (`alone`), or a picture file's bytes (PNG, JPEG). `None`
/// when there is no usable icon. Blocking.
pub fn read(icon: &IconRef) -> Option<Vec<u8>> {
    if has_extension(&icon.path, &["png", "jpg", "jpeg"]) {
        return fs::read(&icon.path).ok();
    }
    if has_extension(&icon.path, &["ico"]) {
        let bytes = fs::read(&icon.path).ok()?;
        let images: Vec<Listed> = entries(&bytes)
            .into_iter()
            .map(|(_, image)| image)
            .collect();
        return best_of(&images);
    }
    program_icon(&icon.path, icon.index)
}

/// The icons of `games`, keyed by game id (see `read`), from the icon cache when present. Games
/// without a usable icon are left out. Blocking: run it off the UI thread.
pub fn extract_all(games: &[Game]) -> Vec<(String, Vec<u8>)> {
    extract_cached(games, IconCache::open().as_ref()).0
}

/// Like `extract_all`, then deletes cached icons no game uses any more. For a full library only:
/// pruning after a partial list would drop the other games' icons.
pub fn refresh_all(games: &[Game]) -> Vec<(String, Vec<u8>)> {
    let cache = IconCache::open();
    let (icons, used) = extract_cached(games, cache.as_ref());
    if let Some(cache) = &cache {
        cache.prune(&used);
    }
    icons
}

/// Icons from `cache` when present, else read and cached (programs' only: other files are read
/// where they are). Also returns the cache files used.
fn extract_cached(
    games: &[Game],
    cache: Option<&IconCache>,
) -> (Vec<(String, Vec<u8>)>, HashSet<PathBuf>) {
    let mut used = HashSet::new();
    let mut icons = Vec::new();
    for game in games {
        let Some(icon) = &game.icon else {
            continue;
        };
        let bytes = if has_extension(&icon.path, &["png", "jpg", "jpeg", "ico"]) {
            read(icon)
        } else {
            let stem = cache.and_then(|cache| cache.stem(icon));
            if let Some(stem) = &stem {
                used.extend(IconCache::EXTENSIONS.map(|ext| stem.with_extension(ext)));
            }
            stem.as_deref().and_then(IconCache::read).or_else(|| {
                let bytes = read(icon)?;
                if let Some(stem) = &stem {
                    IconCache::write(stem, &bytes);
                }
                Some(bytes)
            })
        };
        if let Some(bytes) = bytes {
            icons.push((game.id.clone(), bytes));
        }
    }
    (icons, used)
}

/// Bump when what is cached changes, so icons cached by an older version are not reused.
const CACHE_FORMAT: u32 = 7;

/// Programs' icons in one folder, each a `.png` or a one-image `.ico` file (`alone`'s). Every
/// failure only means "not cached".
struct IconCache {
    dir: PathBuf,
}

impl IconCache {
    /// What a cached icon can be.
    const EXTENSIONS: [&str; 2] = ["png", "ico"];

    /// `%LOCALAPPDATA%\Game Manager\cache\icons`; `None` when it cannot be used.
    fn open() -> Option<Self> {
        Self::at(json_file::local_file("cache").ok()?.join("icons"))
    }

    fn at(dir: PathBuf) -> Option<Self> {
        fs::create_dir_all(&dir).ok()?;
        Some(Self { dir })
    }

    /// An icon's cache file without its extension, which says what it holds (`write`). Its name
    /// changes with the file's length and modified time, so an updated game gets its icon read
    /// again.
    fn stem(&self, icon: &IconRef) -> Option<PathBuf> {
        let metadata = fs::metadata(&icon.path).ok()?;
        let modified = metadata
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos();
        let identity = format!(
            "{}|{}|{}|{modified}|{CACHE_FORMAT}",
            icon.path.to_string_lossy().to_lowercase(),
            icon.index,
            metadata.len()
        );
        Some(
            self.dir
                .join(format!("{:016x}", fnv1a(identity.as_bytes()))),
        )
    }

    /// The icon cached at `stem`, as a `.png` or `.ico` file that holds what its name says.
    fn read(stem: &Path) -> Option<Vec<u8>> {
        Self::EXTENSIONS.into_iter().find_map(|ext| {
            let bytes = fs::read(stem.with_extension(ext)).ok()?;
            let fits = if ext == "png" {
                is_png(&bytes)
            } else {
                is_ico(&bytes)
            };
            fits.then_some(bytes)
        })
    }

    /// Written as `.png` or `.ico` by what it holds, through a temporary file of its own
    /// (`json_file::write_whole`), so another reader never sees half an icon.
    fn write(stem: &Path, bytes: &[u8]) {
        let ext = if is_png(bytes) { "png" } else { "ico" };
        let _ = json_file::write_whole(&stem.with_extension(ext), bytes);
    }

    /// Deletes cached icons (older versions' too) that are not in `used`, and temporary files
    /// left by writes that ended long ago (a younger one may be another copy's write).
    fn prune(&self, used: &HashSet<PathBuf>) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            let cached = has_extension(&path, &["ico", "png"]) && !used.contains(&path);
            if cached || json_file::is_stale_temp(&path) {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system_file(name: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join(name)
    }

    /// A `side` × `side` 32-bit bitmap filled with `color` (B, G, R, A), with its mask.
    fn dib(side: u32, color: [u8; 4]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend(40u32.to_le_bytes());
        data.extend((side as i32).to_le_bytes());
        data.extend((side as i32 * 2).to_le_bytes());
        data.extend(1u16.to_le_bytes());
        data.extend(32u16.to_le_bytes());
        data.extend([0u8; 24]);
        for _ in 0..side * side {
            data.extend(color);
        }
        // The mask: one bit a pixel, rows padded to 4 bytes, all opaque.
        let row = side.div_ceil(32) * 4;
        data.extend(vec![0u8; (row * side) as usize]);
        data
    }

    /// An `.ico` file holding `images` as they are, in this order, each listed as 32 bits (its
    /// directory's sizes left at 0: the images' own headers count).
    fn ico_with(images: &[&[u8]]) -> Vec<u8> {
        let listed: Vec<(&[u8], u8, u16)> = images.iter().map(|data| (*data, 0, 32)).collect();
        ico_listing(&listed)
    }

    /// An `.ico` file holding `images` as they are, in this order, each with the colour count
    /// and bit count its directory entry lists.
    fn ico_listing(images: &[(&[u8], u8, u16)]) -> Vec<u8> {
        let mut file = Vec::new();
        file.extend(0u16.to_le_bytes());
        file.extend(1u16.to_le_bytes());
        file.extend((images.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * images.len();
        for (data, colours, bits) in images {
            file.extend([0, 0, *colours, 0, 1, 0]);
            file.extend(bits.to_le_bytes());
            file.extend((data.len() as u32).to_le_bytes());
            file.extend((offset as u32).to_le_bytes());
            offset += data.len();
        }
        for (data, _, _) in images {
            file.extend(*data);
        }
        file
    }

    /// Images whose directory lists no depth: their own headers count.
    fn own<'a>(images: &[&'a [u8]]) -> Vec<Listed<'a>> {
        images.iter().map(|data| Listed::unlisted(data)).collect()
    }

    /// The images of an `.ico` file's entries.
    fn listed(ico: &[u8]) -> Vec<Listed<'_>> {
        entries(ico).into_iter().map(|(_, image)| image).collect()
    }

    /// A tiny PNG header saying `side` × `side`, 8-bit RGBA.
    fn png_header(side: u32) -> Vec<u8> {
        let mut data = PNG_SIGNATURE.to_vec();
        data.extend([0, 0, 0, 13]);
        data.extend(b"IHDR");
        data.extend(side.to_be_bytes());
        data.extend(side.to_be_bytes());
        data.extend([8, 6, 0, 0, 0]);
        data
    }

    fn image(index: usize, width: u32, bits: u16) -> IconImage {
        IconImage {
            index,
            width,
            height: width,
            bits,
        }
    }

    #[test]
    fn reads_each_images_own_size_and_depth() {
        let small = dib(16, [0, 0, 255, 255]);
        let large = png_header(256);
        let ico = ico_with(&[&small, &large, b"neither"]);
        assert_eq!(images(&ico), [image(0, 16, 32), image(1, 256, 32)]);
        // Each image as it is in the file; on its own, the PNG as it is, a bitmap in a one-image
        // file, byte for byte.
        assert_eq!(image_data(&ico, 1), Some(large.as_slice()));
        assert_eq!(image_data(&ico, 0), Some(small.as_slice()));
        assert_eq!(alone(&large), Some(large.clone()));
        let one = alone(&small).unwrap();
        assert!(is_ico(&one));
        assert_eq!(images(&one), [image(0, 16, 32)]);
        assert_eq!(entries(&one)[0].1.data, small.as_slice());
        // Its directory entry says what the image's header does: 0 for 256.
        let entry = |file: &[u8]| file[6..10].to_vec();
        assert_eq!(entry(&one), [16, 16, 0, 0]);
        assert_eq!(entry(&alone(&dib(256, [0; 4])).unwrap()), [0, 0, 0, 0]);
        assert_eq!(image_data(&ico, 3), None);
        assert!(images(b"not an icon").is_empty());
        assert_eq!(best_of(&own(&[b"nothing readable"])), None);
    }

    /// A tiny PNG header saying `side` × `side` with a palette of `bits` bits (16 or 256 colours).
    fn palette_png_header(side: u32, bits: u8) -> Vec<u8> {
        let mut data = png_header(side);
        data[24] = bits;
        data[25] = 3;
        data
    }

    /// A `side` × `side` bitmap of `bits` bits a pixel (only its header matters here).
    fn shallow_dib(side: u32, bits: u16) -> Vec<u8> {
        let mut data = dib(side, [0; 4]);
        data[14..16].copy_from_slice(&bits.to_le_bytes());
        data
    }

    #[test]
    fn only_the_best_image_is_kept() {
        // As The Sims 4's program holds it: the same sizes in 16 colours, 256 colours and true
        // colour.
        let (low, mid, best) = (
            palette_png_header(256, 4),
            palette_png_header(256, 8),
            png_header(256),
        );
        let (low_48, best_48, low_16) = (shallow_dib(48, 4), dib(48, [0; 4]), shallow_dib(16, 8));
        // The best is a PNG: the PNG itself, not an `.ico` around it.
        let all = own(&[&low, &low_48, &low_16, &mid, &best, &best_48]);
        assert_eq!(super::best(&all), Some(best.as_slice()));
        assert_eq!(best_of(&all), Some(best.clone()));
        // A 24-bit PNG (no alpha) counts as true colour too.
        let mut rgb = png_header(256);
        rgb[25] = 2;
        assert_eq!(best_of(&own(&[&best_48, &rgb])), Some(rgb.clone()));
        // A larger image with fewer colours doesn't beat true colour.
        let ico = best_of(&own(&[&best_48, &palette_png_header(256, 8)])).unwrap();
        assert_eq!(entries(&ico)[0].1.data, best_48.as_slice());
        // With no true colour at all, the largest and deepest there is.
        let deepest = shallow_dib(48, 8);
        let ico = best_of(&own(&[&low_48, &low_16, &deepest])).unwrap();
        assert_eq!(images(&ico), [image(0, 48, 8)]);
        assert_eq!(entries(&ico)[0].1.data, deepest.as_slice());
        // Already the best: the same file again.
        assert_eq!(best_of(&listed(&ico)), Some(ico));
    }

    #[test]
    fn the_directorys_depth_counts_too() {
        // As The Sims 4's program holds them: three 256 px images, all 32-bit RGBA PNGs, listed as
        // 16 colours, 256 colours and true colour. Each is told apart by a byte after its header.
        let [low, mid, best] = [1u8, 2, 3].map(|tag| [png_header(256), vec![tag]].concat());
        let ico = ico_listing(&[(&low, 16, 4), (&mid, 0, 8), (&best, 0, 32)]);
        assert_eq!(
            images(&ico),
            [image(0, 256, 4), image(1, 256, 8), image(2, 256, 32)]
        );
        assert_eq!(best_of(&listed(&ico)), Some(best.clone()));
        assert_eq!(pick(&images(&ico), 64), Some(image(2, 256, 32)));
        // A colour count alone says it too: 16 colours are 4 bits, 2 colours 1.
        let ico = ico_listing(&[(&low, 16, 0), (&mid, 2, 0)]);
        assert_eq!(images(&ico), [image(0, 256, 4), image(1, 256, 1)]);
        // Nothing listed: the image's own header. Listed deeper than the image: the image's.
        let palette = palette_png_header(256, 8);
        let ico = ico_listing(&[(&best, 0, 0), (&palette, 0, 32)]);
        assert_eq!(images(&ico), [image(0, 256, 32), image(1, 256, 8)]);
    }

    #[test]
    fn picks_the_size_on_screen_else_the_next_larger_else_the_largest() {
        let images = [
            image(0, 16, 32),
            image(1, 32, 8),
            image(2, 32, 32),
            image(3, 256, 32),
        ];
        assert_eq!(pick(&images, 32), Some(image(2, 32, 32)));
        assert_eq!(pick(&images, 24), Some(image(2, 32, 32)));
        assert_eq!(pick(&images, 54), Some(image(3, 256, 32)));
        assert_eq!(pick(&images, 16), Some(image(0, 16, 32)));
        // Nothing large enough: the largest.
        assert_eq!(pick(&images[..3], 64), Some(image(2, 32, 32)));
        assert_eq!(pick(&[], 64), None);
        // A 16-colour image of the size on screen loses to a true-colour one to scale.
        let mixed = [image(0, 24, 4), image(1, 48, 32)];
        assert_eq!(pick(&mixed, 24), Some(image(1, 48, 32)));
        // Without true colour, the deepest there is.
        let old = [image(0, 24, 4), image(1, 32, 8)];
        assert_eq!(pick(&old, 24), Some(image(1, 32, 8)));
    }

    #[test]
    fn an_ico_file_gives_its_best_image_and_a_picture_is_taken_as_it_is() {
        let dir = std::env::temp_dir().join(format!("GM-Icon-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // A file with several images gives its best one alone, byte for byte.
        let (small, large) = (dib(16, [255, 0, 0, 255]), dib(48, [0, 0, 255, 255]));
        let file = dir.join("game.ICO");
        fs::write(&file, ico_with(&[&small, &large])).unwrap();
        let read_back = read(&IconRef::file(&file)).unwrap();
        assert_eq!(read_back, best_of(&own(&[&large])).unwrap());
        assert_eq!(entries(&read_back)[0].1.data, large.as_slice());
        // A PNG best image comes out as the PNG.
        let png = png_header(256);
        fs::write(&file, ico_with(&[&small, &png])).unwrap();
        assert_eq!(read(&IconRef::file(&file)), Some(png));
        let logo = dir.join("logo.png");
        let bytes = [PNG_SIGNATURE.as_slice(), b"rest of the file"].concat();
        fs::write(&logo, &bytes).unwrap();
        assert_eq!(read(&IconRef::file(&logo)), Some(bytes));
        // Not an icon, or no file: nothing.
        fs::write(&file, b"nope").unwrap();
        assert_eq!(read(&IconRef::file(&file)), None);
        assert_eq!(read(&IconRef::file(dir.join("missing.ico"))), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_programs_icon_is_its_best_resource() {
        let explorer = read(&IconRef::file(system_file("explorer.exe"))).unwrap();
        // Its best image only, as a file of its own: the largest, in true colour.
        let (width, _, bits) = only_image(&explorer);
        assert!(width >= 48 && bits >= 24, "{width} px, {bits} bits");
        // shell32.dll keeps its icons in SystemResources\shell32.dll.mun on current Windows; a
        // group by place or by resource number.
        let shell32 = system_file(r"System32\shell32.dll");
        let at = |index: i32| IconRef {
            path: shell32.clone(),
            index,
        };
        for index in [0, 3, -4] {
            let icon = read(&at(index)).unwrap_or_else(|| panic!("index {index}"));
            only_image(&icon);
        }
        for index in [100_000, -30_000, i32::MIN] {
            assert_eq!(read(&at(index)), None, "index {index}");
        }
        // A program with no icon, and one that isn't there.
        assert_eq!(
            read(&IconRef::file(system_file(r"System32\kernel32.dll"))),
            None
        );
        assert_eq!(read(&IconRef::file(r"C:\does\not\exist\game.exe")), None);
    }

    /// The one image of an icon as `read` gives it, a `.png` or a one-image `.ico`: its width,
    /// height and depth.
    fn only_image(icon: &[u8]) -> (u32, u32, u16) {
        if is_png(icon) {
            return describe(icon).unwrap();
        }
        let found = entries(icon);
        assert_eq!(found.len(), 1);
        assert!(!is_png(found[0].1.data), "a PNG is kept outside .ico");
        found[0].1.describe().unwrap()
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
            edits: None,
            pinned: None,
            hidden: false,
            installed: None,
        }
    }

    #[test]
    fn cache_file_follows_the_icon_file() {
        let cache = temp_cache("key");
        let explorer = IconRef::file(system_file("explorer.exe"));
        let key = cache.stem(&explorer).unwrap();
        assert_eq!(cache.stem(&explorer).as_ref(), Some(&key));
        // The path's letter case does not matter; the icon index does.
        let upper = IconRef::file(system_file("explorer.exe").to_string_lossy().to_uppercase());
        assert_eq!(cache.stem(&upper).as_ref(), Some(&key));
        let second_icon = IconRef {
            index: 1,
            ..explorer.clone()
        };
        assert_ne!(cache.stem(&second_icon).as_ref(), Some(&key));

        // A changed file (new length and modified time) gets a new cache file.
        let file = cache.dir.join("game.exe");
        fs::write(&file, b"first").unwrap();
        let before = cache.stem(&IconRef::file(&file));
        fs::write(&file, b"second version").unwrap();
        assert_ne!(cache.stem(&IconRef::file(&file)), before);
        assert_eq!(cache.stem(&IconRef::file(r"C:\does\not\exist.exe")), None);
        fs::remove_dir_all(&cache.dir).unwrap();
    }

    #[test]
    fn reads_once_then_from_the_cache() {
        let cache = temp_cache("hit");
        let games = [game_with_icon(
            "steam:1",
            IconRef::file(system_file("explorer.exe")),
        )];
        let (icons, used) = extract_cached(&games, Some(&cache));
        only_image(&icons[0].1);
        // Cached under the extension that says what it holds.
        let written: Vec<&PathBuf> = used.iter().filter(|file| file.is_file()).collect();
        assert_eq!(written.len(), 1, "{used:?}");
        let file = written[0].clone();
        let ext = if is_png(&icons[0].1) { "png" } else { "ico" };
        assert!(has_extension(&file, &[ext]), "{file:?}");
        assert_eq!(fs::read(&file).unwrap(), icons[0].1);

        // The second run returns what the cached file holds, so it did not read the program.
        let stem = file.with_extension("");
        fs::remove_file(&file).unwrap();
        let cached = best_of(&own(&[&dib(16, [1, 2, 3, 255])])).unwrap();
        fs::write(stem.with_extension("ico"), &cached).unwrap();
        let (icons, _) = extract_cached(&games, Some(&cache));
        assert_eq!(icons[0].1, cached);
        let png = png_header(32);
        fs::write(stem.with_extension("png"), &png).unwrap();
        let (icons, _) = extract_cached(&games, Some(&cache));
        assert_eq!(icons[0].1, png);
        // A cached file that doesn't hold what its name says is read again from the program.
        fs::write(stem.with_extension("png"), &cached).unwrap();
        fs::write(stem.with_extension("ico"), b"broken").unwrap();
        let (icons, _) = extract_cached(&games, Some(&cache));
        only_image(&icons[0].1);
        // Without a cache, icons are read as before.
        let (icons, used) = extract_cached(&games, None);
        only_image(&icons[0].1);
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
        let stale = cache.dir.join("0123456789abcdef.ico");
        let old_format = cache.dir.join("0123456789abcdef.png");
        let leftover = cache.dir.join("fedcba9876543210.ico.1-0.tmp");
        let leftover_png = cache.dir.join("fedcba9876543210.png.tmp");
        let in_progress = cache.dir.join("fedcba9876543210.png.1-1.tmp");
        let unrelated = cache.dir.join("notes.txt");
        for file in [
            &stale,
            &old_format,
            &leftover,
            &leftover_png,
            &in_progress,
            &unrelated,
        ] {
            fs::write(file, b"x").unwrap();
        }
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60);
        for file in [&leftover, &leftover_png] {
            let handle = fs::File::options().write(true).open(file).unwrap();
            handle.set_modified(hour_ago).unwrap();
        }
        let (_, used) = extract_cached(&games, Some(&cache));
        cache.prune(&used);
        assert!(used.iter().any(|file| file.is_file()));
        assert!(!stale.exists() && !old_format.exists());
        assert!(!leftover.exists() && !leftover_png.exists());
        assert!(in_progress.exists() && unrelated.exists());
        fs::remove_dir_all(&cache.dir).unwrap();
    }
}
