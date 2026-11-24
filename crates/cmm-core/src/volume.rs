//! Volume capacity, swap, Trash and the Full Disk Access probe (owned by S1).

use std::ffi::{CString, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::model::VolumeInfo;
use crate::size::Sizer;

/// Capacity of the volume holding `home`, swap usage and the size of `~/.Trash`.
/// Every part degrades on its own: total/free are 0 if `statfs` fails, the rest become `None`.
pub fn volume_info(home: &Path) -> VolumeInfo {
    let (total_bytes, free_bytes) = statfs_bytes(home).unwrap_or((0, 0));
    VolumeInfo {
        total_bytes,
        free_bytes,
        available_important_bytes: important_capacity(home),
        swap_used_bytes: swap_used_bytes(),
        trash_bytes: trash_bytes(home),
    }
}

/// Probe: the TCC database, `~/Library/Safari` or `~/.Trash` are readable only with Full Disk
/// Access.
pub fn has_full_disk_access(home: &Path) -> bool {
    std::fs::File::open(home.join("Library/Application Support/com.apple.TCC/TCC.db")).is_ok()
        || std::fs::read_dir(home.join("Library/Safari")).is_ok()
        || std::fs::read_dir(home.join(".Trash")).is_ok()
}

/// Allocated size of `~/.Trash`; `Some(0)` if it does not exist, `None` if it cannot be listed
/// (no Full Disk Access).
pub fn trash_bytes(home: &Path) -> Option<u64> {
    let trash = home.join(".Trash");
    match std::fs::read_dir(&trash) {
        Ok(_) => Some(Sizer::new().size_of(&trash).bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(0),
        Err(_) => None,
    }
}

/// `(total, available to unprivileged users)` bytes of the file system containing `path`.
fn statfs_bytes(path: &Path) -> Option<(u64, u64)> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: an all-zero `statfs` is a valid value of this plain C struct.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a valid NUL-terminated string and `stat` is a valid out-pointer.
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let block = u64::from(stat.f_bsize);
    Some((stat.f_blocks * block, stat.f_bavail * block))
}

/// `sysctl vm.swapusage` -> used bytes.
fn swap_used_bytes() -> Option<u64> {
    // SAFETY: an all-zero `xsw_usage` is a valid value of this plain C struct.
    let mut usage: libc::xsw_usage = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::xsw_usage>();
    // SAFETY: the name is NUL-terminated, `usage`/`len` describe a writable buffer of that size.
    let rc = unsafe {
        libc::sysctlbyname(
            c"vm.swapusage".as_ptr(),
            (&raw mut usage).cast::<c_void>(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len == std::mem::size_of::<libc::xsw_usage>()).then_some(usage.xsu_used)
}

/// `NSURLVolumeAvailableCapacityForImportantUsageKey` (free + purgeable space macOS would make
/// available for an important download), via its CoreFoundation twin. `None` on any failure.
fn important_capacity(path: &Path) -> Option<u64> {
    cf::volume_available_capacity_for_important_usage(path.as_os_str().as_bytes())
        .and_then(|bytes| u64::try_from(bytes).ok())
}

/// Minimal CoreFoundation FFI: `CFURLCopyResourcePropertyForKey` with one numeric key.
mod cf {
    use std::ffi::c_void;

    type CFTypeRef = *const c_void;
    type CFIndex = isize;
    type Boolean = u8;

    const K_CF_NUMBER_SINT64_TYPE: CFIndex = 4;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFURLVolumeAvailableCapacityForImportantUsageKey: CFTypeRef;
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: CFTypeRef,
            buffer: *const u8,
            buf_len: CFIndex,
            is_directory: Boolean,
        ) -> CFTypeRef;
        fn CFURLCopyResourcePropertyForKey(
            url: CFTypeRef,
            key: CFTypeRef,
            property_value: *mut CFTypeRef,
            error: *mut CFTypeRef,
        ) -> Boolean;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFNumberGetValue(number: CFTypeRef, the_type: CFIndex, value_ptr: *mut c_void) -> Boolean;
        fn CFRelease(cf: CFTypeRef);
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

    pub fn volume_available_capacity_for_important_usage(path: &[u8]) -> Option<i64> {
        let len = CFIndex::try_from(path.len()).ok()?;
        // SAFETY: `path` is valid for `len` bytes; a null allocator means the default allocator.
        let url = Owned(unsafe { CFURLCreateFromFileSystemRepresentation(std::ptr::null(), path.as_ptr(), len, 1) });
        if url.0.is_null() {
            return None;
        }
        let mut value: CFTypeRef = std::ptr::null();
        // SAFETY: `url` is a live CFURL, the key is a framework constant, `value` is an out-pointer;
        // a null error pointer is allowed.
        let ok = unsafe {
            CFURLCopyResourcePropertyForKey(
                url.0,
                kCFURLVolumeAvailableCapacityForImportantUsageKey,
                &mut value,
                std::ptr::null_mut(),
            )
        };
        let value = Owned(value);
        // SAFETY: `value` is a live CF object when non-null.
        if ok == 0 || value.0.is_null() || unsafe { CFGetTypeID(value.0) != CFNumberGetTypeID() } {
            return None;
        }
        let mut bytes: i64 = 0;
        // SAFETY: `value` is a CFNumber and `bytes` is a valid i64 out-pointer.
        let ok = unsafe { CFNumberGetValue(value.0, K_CF_NUMBER_SINT64_TYPE, (&raw mut bytes).cast()) };
        (ok != 0).then_some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statfs_reports_capacity_for_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let info = volume_info(dir.path());
        assert!(info.total_bytes > 0);
        assert!(info.free_bytes <= info.total_bytes);
    }

    #[test]
    fn important_capacity_is_at_least_free_space() {
        let dir = tempfile::tempdir().unwrap();
        let info = volume_info(dir.path());
        let important = info.available_important_bytes.expect("CoreFoundation answers for a local volume");
        assert!(important <= info.total_bytes);
        // Purgeable space only adds; allow some slack for files written meanwhile.
        assert!(important + 1_000_000_000 >= info.free_bytes, "{important} vs {}", info.free_bytes);
    }

    #[test]
    fn swap_usage_is_readable() {
        assert!(swap_used_bytes().is_some());
    }

    #[test]
    fn trash_size_in_fake_home() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(trash_bytes(home.path()), Some(0), "no Trash yet");
        std::fs::create_dir(home.path().join(".Trash")).unwrap();
        std::fs::write(home.path().join(".Trash/old.bin"), vec![0u8; 100_000]).unwrap();
        assert!(trash_bytes(home.path()).unwrap() >= 100_000);
        assert!(has_full_disk_access(home.path()), "a readable .Trash counts as access");
    }

    #[test]
    fn unreadable_trash_is_none() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let trash = home.path().join(".Trash");
        std::fs::create_dir(&trash).unwrap();
        std::fs::set_permissions(&trash, std::fs::Permissions::from_mode(0o000)).unwrap();
        let bytes = trash_bytes(home.path());
        std::fs::set_permissions(&trash, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(bytes, None);
    }

    #[test]
    fn full_disk_access_false_for_empty_home() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_full_disk_access(dir.path()));
    }
}
