//! Allocated-size measurement (owned by S1).
//!
//! Skeleton: a simple, correct, sequential implementation. S1 may parallelise it.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use walkdir::WalkDir;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SizeInfo {
    /// Allocated bytes (`st_blocks * 512`), each inode counted once per [`Sizer`].
    pub bytes: u64,
    /// Sum of `st_size`.
    pub logical_bytes: u64,
    /// Number of non-directory entries.
    pub files: u64,
    pub newest_mtime: Option<DateTime<Utc>>,
}

/// Shared, thread-safe sizer. Remembers every `(dev, ino)` it has counted, so hardlinks and
/// overlapping roots are counted once across all `size_of` calls on the same `Sizer`.
#[derive(Debug, Default)]
pub struct Sizer {
    seen: Mutex<HashSet<(u64, u64)>>,
}

impl Sizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Never follows symlinks, never crosses devices. Unreadable entries are skipped.
    pub fn size_of(&self, path: &Path) -> SizeInfo {
        let mut info = SizeInfo::default();
        let walker = WalkDir::new(path).follow_links(false).same_file_system(true);
        for entry in walker.into_iter().filter_map(Result::ok) {
            // walkdir returns symlink_metadata for entries when not following links.
            let Ok(meta) = entry.metadata() else { continue };
            let first_time = self.seen.lock().unwrap_or_else(|e| e.into_inner()).insert((meta.dev(), meta.ino()));
            if !first_time {
                continue;
            }
            info.bytes += meta.blocks() * 512;
            info.logical_bytes += meta.len();
            if !meta.is_dir() {
                info.files += 1;
            }
            if let Some(mtime) = DateTime::<Utc>::from_timestamp(meta.mtime(), meta.mtime_nsec() as u32)
                && info.newest_mtime.is_none_or(|newest| mtime > newest)
            {
                info.newest_mtime = Some(mtime);
            }
        }
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardlink_counted_once() {
        let dir = tempfile::tempdir().unwrap();
        let data = vec![7u8; 64 * 1024];
        std::fs::write(dir.path().join("a.bin"), &data).unwrap();
        let single = Sizer::new().size_of(&dir.path().join("a.bin"));
        assert!(single.bytes >= data.len() as u64);

        std::fs::hard_link(dir.path().join("a.bin"), dir.path().join("b.bin")).unwrap();
        let total = Sizer::new().size_of(dir.path());
        assert_eq!(total.files, 1, "two names, one inode");
        assert_eq!(total.logical_bytes - data.len() as u64, std::fs::metadata(dir.path()).unwrap().len());
        assert!(total.bytes < 2 * single.bytes);
        assert!(total.newest_mtime.is_some());
    }

    #[test]
    fn symlinks_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::write(target.path().join("big.bin"), vec![1u8; 256 * 1024]).unwrap();
        std::os::unix::fs::symlink(target.path(), dir.path().join("link")).unwrap();
        let info = Sizer::new().size_of(dir.path());
        assert_eq!(info.files, 1, "only the link itself");
        assert!(info.logical_bytes < 256 * 1024);
    }
}
