//! Volume capacity and Full Disk Access probe (owned by S1).

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::model::VolumeInfo;

/// Capacity of the volume holding `home`. Skeleton: only total/free via `statvfs`;
/// `available_important_bytes`, `swap_used_bytes` and `trash_bytes` are `None` for now.
pub fn volume_info(home: &Path) -> VolumeInfo {
    let (total_bytes, free_bytes) = statvfs_bytes(home).unwrap_or((0, 0));
    VolumeInfo { total_bytes, free_bytes, available_important_bytes: None, swap_used_bytes: None, trash_bytes: None }
}

/// Probe: `read_dir(~/Library/Safari)` or `read_dir(~/.Trash)` succeeds only with Full Disk Access.
pub fn has_full_disk_access(home: &Path) -> bool {
    std::fs::read_dir(home.join("Library/Safari")).is_ok() || std::fs::read_dir(home.join(".Trash")).is_ok()
}

/// `(total, available)` bytes of the filesystem containing `path`.
fn statvfs_bytes(path: &Path) -> Option<(u64, u64)> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a valid NUL-terminated string and `stat` is a valid out-pointer.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }
    let frsize = stat.f_frsize as u64;
    Some((stat.f_blocks as u64 * frsize, stat.f_bavail as u64 * frsize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statvfs_reports_capacity_for_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let info = volume_info(dir.path());
        assert!(info.total_bytes > 0);
        assert!(info.free_bytes <= info.total_bytes);
    }

    #[test]
    fn full_disk_access_false_for_empty_home() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_full_disk_access(dir.path()));
    }
}
