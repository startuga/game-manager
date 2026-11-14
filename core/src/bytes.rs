//! Little-endian numbers in file data (PE headers, icons, bitmaps, ZIP records), read at an
//! offset: `None` past the end, so a short or damaged file is never read beyond its bytes.

/// The `u16` at `at`.
pub(crate) fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

/// The `u32` at `at`.
pub(crate) fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_inside_the_bytes_only() {
        let bytes = [0x34, 0x12, 0x78, 0x56];
        assert_eq!(u16_at(&bytes, 0), Some(0x1234));
        assert_eq!(u16_at(&bytes, 2), Some(0x5678));
        assert_eq!(u32_at(&bytes, 0), Some(0x5678_1234));
        assert_eq!(u16_at(&bytes, 3), None);
        assert_eq!(u32_at(&bytes, 1), None);
        assert_eq!(u32_at(&bytes, usize::MAX), None);
    }
}
