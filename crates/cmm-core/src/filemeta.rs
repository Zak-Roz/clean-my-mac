//! File metadata for browsing (the Downloads tab): Spotlight dates, type and origin, with a
//! file-system fallback. Read-only.
//!
//! Spotlight is read through `MDItemCreate` + `MDItemCopyAttribute` (CoreServices), in the same
//! minimal CoreFoundation FFI style as `volume.rs`. An item counts as **indexed** when Spotlight
//! reports its `kMDItemDateAdded`: for files outside the index (temp dirs, excluded volumes)
//! Spotlight still answers `kMDItemLastUsedDate`, but with a value made up from the file's own
//! dates, so that value is used only for indexed items. Hence:
//! - indexed, no `kMDItemLastUsedDate` → "never opened" (`indexed: true`, `last_opened: None`);
//! - not indexed → "unknown" (`indexed: false`, `last_opened: None`); `date_added` falls back to
//!   the birth time.
//!
//! Symlinks are never followed: a link gets its own `lstat` dates and no Spotlight data.

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::model::FileMeta;

/// Metadata of `path` (never fails: whatever cannot be read stays `None`).
pub fn read(path: &Path) -> FileMeta {
    let lstat = std::fs::symlink_metadata(path).ok();
    let created = lstat.as_ref().and_then(|m| m.created().ok()).map(DateTime::<Utc>::from);
    let modified = lstat.as_ref().and_then(|m| m.modified().ok()).map(DateTime::<Utc>::from);
    let is_link = lstat.as_ref().is_some_and(|m| m.file_type().is_symlink());
    let spotlight = if is_link || lstat.is_none() { Spotlight::default() } else { spotlight(path) };
    combine(spotlight, created, modified)
}

/// What Spotlight said about one item.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Spotlight {
    pub date_added: Option<DateTime<Utc>>,
    pub last_used: Option<DateTime<Utc>>,
    pub content_type: Option<String>,
    pub where_from: Option<String>,
}

/// Applies the indexed / fallback rules (module docs) to raw Spotlight values and `lstat` dates.
pub(crate) fn combine(
    spotlight: Spotlight,
    created: Option<DateTime<Utc>>,
    modified: Option<DateTime<Utc>>,
) -> FileMeta {
    let indexed = spotlight.date_added.is_some();
    FileMeta {
        date_added: spotlight.date_added.or(created),
        last_opened: if indexed { spotlight.last_used } else { None },
        indexed,
        created,
        modified,
        content_type: spotlight.content_type,
        where_from: spotlight.where_from,
    }
}

#[cfg(target_os = "macos")]
fn spotlight(path: &Path) -> Spotlight {
    let Some(path) = path.to_str() else { return Spotlight::default() };
    md::attributes(path).unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
fn spotlight(_path: &Path) -> Spotlight {
    Spotlight::default()
}

/// Minimal CoreFoundation / Metadata FFI: `MDItemCreate` and `MDItemCopyAttribute` with a few
/// attribute names, plus the CFDate / CFString / CFArray readers they need.
#[cfg(target_os = "macos")]
mod md {
    use std::ffi::{c_char, c_void};

    use chrono::{DateTime, Utc};

    use super::Spotlight;

    type CFTypeRef = *const c_void;
    type CFIndex = isize;
    type CFTypeID = usize;
    type Boolean = u8;

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    /// Seconds from the Unix epoch to the CoreFoundation epoch (2001-01-01T00:00:00Z).
    const CF_EPOCH_UNIX_SECONDS: f64 = 978_307_200.0;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            alloc: CFTypeRef,
            bytes: *const u8,
            num_bytes: CFIndex,
            encoding: u32,
            is_external_representation: Boolean,
        ) -> CFTypeRef;
        fn CFStringGetLength(string: CFTypeRef) -> CFIndex;
        fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
        fn CFStringGetCString(string: CFTypeRef, buffer: *mut c_char, size: CFIndex, encoding: u32) -> Boolean;
        fn CFStringGetTypeID() -> CFTypeID;
        fn CFDateGetAbsoluteTime(date: CFTypeRef) -> f64;
        fn CFDateGetTypeID() -> CFTypeID;
        fn CFArrayGetCount(array: CFTypeRef) -> CFIndex;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, index: CFIndex) -> CFTypeRef;
        fn CFArrayGetTypeID() -> CFTypeID;
        fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
        fn CFRelease(cf: CFTypeRef);
    }

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn MDItemCreate(allocator: CFTypeRef, path: CFTypeRef) -> CFTypeRef;
        fn MDItemCopyAttribute(item: CFTypeRef, name: CFTypeRef) -> CFTypeRef;
    }

    /// Releases a CF object on drop.
    struct Owned(CFTypeRef);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: we own one reference (Create/Copy rule) to a live CF object.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    fn cf_string(text: &str) -> Option<Owned> {
        let len = CFIndex::try_from(text.len()).ok()?;
        // SAFETY: `text` is valid UTF-8 for `len` bytes; a null allocator is the default one.
        let s = unsafe { CFStringCreateWithBytes(std::ptr::null(), text.as_ptr(), len, K_CF_STRING_ENCODING_UTF8, 0) };
        (!s.is_null()).then_some(Owned(s))
    }

    /// The value is a live CF object of `type_id`.
    fn is_type(value: CFTypeRef, type_id: CFTypeID) -> bool {
        // SAFETY: `value` is a live CF object when non-null.
        !value.is_null() && unsafe { CFGetTypeID(value) } == type_id
    }

    fn read_string(value: CFTypeRef) -> Option<String> {
        // SAFETY: plain type-id getter.
        if !is_type(value, unsafe { CFStringGetTypeID() }) {
            return None;
        }
        // SAFETY: `value` is a live CFString.
        let max = unsafe { CFStringGetMaximumSizeForEncoding(CFStringGetLength(value), K_CF_STRING_ENCODING_UTF8) };
        let size = usize::try_from(max).ok()?.checked_add(1)?;
        let mut buf = vec![0u8; size];
        // SAFETY: `buf` is writable for `size` bytes, NUL included.
        let ok =
            unsafe { CFStringGetCString(value, buf.as_mut_ptr().cast(), size as CFIndex, K_CF_STRING_ENCODING_UTF8) };
        if ok == 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        buf.truncate(end);
        String::from_utf8(buf).ok()
    }

    fn read_date(value: CFTypeRef) -> Option<DateTime<Utc>> {
        // SAFETY: plain type-id getter.
        if !is_type(value, unsafe { CFDateGetTypeID() }) {
            return None;
        }
        // SAFETY: `value` is a live CFDate.
        let seconds = unsafe { CFDateGetAbsoluteTime(value) } + CF_EPOCH_UNIX_SECONDS;
        if !seconds.is_finite() {
            return None;
        }
        let whole = seconds.floor();
        DateTime::from_timestamp(whole as i64, ((seconds - whole) * 1e9) as u32)
    }

    /// First string of a CFArray of CFStrings.
    fn read_first_string(value: CFTypeRef) -> Option<String> {
        // SAFETY: plain type-id getter.
        if !is_type(value, unsafe { CFArrayGetTypeID() }) {
            return None;
        }
        // SAFETY: `value` is a live CFArray; index 0 is checked against its count. The element is
        // borrowed (Get rule), not released.
        unsafe {
            if CFArrayGetCount(value) < 1 {
                return None;
            }
            read_string(CFArrayGetValueAtIndex(value, 0))
        }
    }

    pub fn attributes(path: &str) -> Option<Spotlight> {
        let path = cf_string(path)?;
        // SAFETY: `path` is a live CFString; a null allocator is the default one.
        let item = Owned(unsafe { MDItemCreate(std::ptr::null(), path.0) });
        if item.0.is_null() {
            return None;
        }
        let copy = |name: &str| -> Option<Owned> {
            let name = cf_string(name)?;
            // SAFETY: `item` is a live MDItem, `name` a live CFString; the result follows the Copy rule.
            let value = unsafe { MDItemCopyAttribute(item.0, name.0) };
            (!value.is_null()).then_some(Owned(value))
        };
        Some(Spotlight {
            date_added: copy("kMDItemDateAdded").and_then(|v| read_date(v.0)),
            last_used: copy("kMDItemLastUsedDate").and_then(|v| read_date(v.0)),
            content_type: copy("kMDItemContentType").and_then(|v| read_string(v.0)),
            where_from: copy("kMDItemWhereFroms").and_then(|v| read_first_string(v.0)),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339).unwrap().to_utc()
    }

    #[test]
    fn indexed_without_last_used_means_never_opened() {
        let added = at("2026-10-01T08:00:00Z");
        let spotlight = Spotlight { date_added: Some(added), ..Spotlight::default() };
        let meta = combine(spotlight, Some(at("2026-09-30T00:00:00Z")), None);
        assert!(meta.indexed);
        assert_eq!(meta.last_opened, None, "never opened");
        assert_eq!(meta.date_added, Some(added), "Spotlight's date wins over the birth time");
    }

    #[test]
    fn unindexed_last_used_is_ignored_and_date_added_falls_back() {
        let born = at("2026-09-30T00:00:00Z");
        // What Spotlight answers for a file outside the index: a made-up last-used date, no date added.
        let spotlight =
            Spotlight { last_used: Some(born), content_type: Some("public.plain-text".into()), ..Default::default() };
        let meta = combine(spotlight, Some(born), Some(born));
        assert!(!meta.indexed);
        assert_eq!(meta.last_opened, None, "unknown, not opened at the birth time");
        assert_eq!(meta.date_added, Some(born));
        assert_eq!(meta.content_type.as_deref(), Some("public.plain-text"));
    }

    #[test]
    fn indexed_last_used_is_kept() {
        let opened = at("2026-11-01T12:00:00Z");
        let spotlight =
            Spotlight { date_added: Some(at("2026-10-01T08:00:00Z")), last_used: Some(opened), ..Default::default() };
        assert_eq!(combine(spotlight, None, None).last_opened, Some(opened));
    }

    /// A temp dir is outside the Spotlight index: dates come from the file system and the last
    /// opened date is unknown (not "never").
    #[test]
    fn unindexed_temp_dir_falls_back_to_file_system_dates() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.pdf");
        fs::write(&file, b"%PDF-1.4").unwrap();
        let meta = read(&file);
        let lstat = fs::symlink_metadata(&file).unwrap();
        let born = DateTime::<Utc>::from(lstat.created().unwrap());
        assert!(!meta.indexed, "{meta:?}");
        assert_eq!(meta.last_opened, None);
        assert_eq!(meta.date_added, Some(born));
        assert_eq!(meta.created, Some(born));
        assert_eq!(meta.modified, Some(DateTime::<Utc>::from(lstat.modified().unwrap())));
        assert_eq!(meta.where_from, None);
    }

    #[test]
    fn symlinks_and_missing_paths_get_no_spotlight_data() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        fs::write(&target, b"x").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let meta = read(&link);
        assert!(!meta.indexed && meta.content_type.is_none());
        assert!(meta.created.is_some(), "the link's own lstat dates");

        let missing = read(&dir.path().join("missing"));
        assert_eq!(missing, FileMeta::default());
    }
}
