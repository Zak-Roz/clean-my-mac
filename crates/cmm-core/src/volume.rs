//! Volume capacity, swap, Trash and the Full Disk Access probe.

use std::ffi::{CString, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

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

/// `(total, free)` bytes of the volume holding `home`: one `statfs`, no Trash walk, cheap enough
/// for a menu bar refresh. `None` if `statfs` fails. Same numbers as [`volume_info`].
pub fn capacity(home: &Path) -> Option<(u64, u64)> {
    statfs_bytes(home)
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
    mount_of(path).map(|m| (m.total_bytes, m.free_bytes))
}

/// One mounted file system, as `statfs(2)` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// Device ("/dev/disk3s5") or source ("map auto_home").
    pub from: String,
    /// Mount point ("/System/Volumes/Data").
    pub on: PathBuf,
    /// "apfs", "hfs", "autofs", …
    pub fs_type: String,
    /// Size of the file system; for APFS, of the whole container.
    pub total_bytes: u64,
    /// Available to unprivileged users; for APFS, the container's free space.
    pub free_bytes: u64,
    /// The space this file system uses. For an APFS volume that is the volume alone
    /// (`ATTR_VOL_SPACEUSED`, as `df` shows it): `statfs` reports the container's free blocks for
    /// every volume of a container, so `f_blocks - f_bfree` would be the whole container's use.
    pub used_bytes: u64,
}

impl Mount {
    fn from_statfs(stat: &libc::statfs) -> Self {
        let block = u64::from(stat.f_bsize);
        let on = PathBuf::from(c_chars(&stat.f_mntonname));
        let fs_type = c_chars(&stat.f_fstypename);
        let statfs_used = stat.f_blocks.saturating_sub(stat.f_bfree) * block;
        let used_bytes = if fs_type == "apfs" { space_used(&on).unwrap_or(statfs_used) } else { statfs_used };
        Mount {
            from: c_chars(&stat.f_mntfromname),
            on,
            fs_type,
            total_bytes: stat.f_blocks * block,
            free_bytes: stat.f_bavail * block,
            used_bytes,
        }
    }

    /// The APFS container ("disk3") of a volume device ("/dev/disk3s5", snapshot "/dev/disk3s1s1").
    pub fn apfs_container(&self) -> Option<&str> {
        if self.fs_type != "apfs" {
            return None;
        }
        let dev = self.from.strip_prefix("/dev/")?;
        let digits = dev.strip_prefix("disk")?.bytes().take_while(u8::is_ascii_digit).count();
        (digits > 0).then(|| &dev[..4 + digits])
    }
}

/// `getattrlist(ATTR_VOL_SPACEUSED)` of the volume mounted at `mount_point`.
fn space_used(mount_point: &Path) -> Option<u64> {
    let c_path = CString::new(mount_point.as_os_str().as_bytes()).ok()?;
    let mut attrs = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_SPACEUSED,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    // u32 total length, then the off_t.
    let mut buf = [0u8; 16];
    // SAFETY: `c_path` is NUL-terminated, `attrs` is a valid attrlist, `buf` is writable for its length.
    let rc = unsafe {
        libc::getattrlist(
            c_path.as_ptr(),
            (&raw mut attrs).cast::<c_void>(),
            buf.as_mut_ptr().cast::<c_void>(),
            buf.len(),
            0,
        )
    };
    let len = u32::from_ne_bytes(buf[..4].try_into().ok()?);
    if rc != 0 || len < 12 {
        return None;
    }
    u64::try_from(i64::from_ne_bytes(buf[4..12].try_into().ok()?)).ok()
}

fn c_chars(chars: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = chars.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The file system containing `path` (`statfs`). `None` if `statfs` fails.
pub fn mount_of(path: &Path) -> Option<Mount> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: an all-zero `statfs` is a valid value of this plain C struct.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a valid NUL-terminated string and `stat` is a valid out-pointer.
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    Some(Mount::from_statfs(&stat))
}

/// Every mounted file system (`getfsstat(2)`, which unlike `getmntinfo` is thread-safe).
pub fn mounts() -> Vec<Mount> {
    // SAFETY: a null buffer asks for the number of mounted file systems.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    let Ok(count) = usize::try_from(count) else { return Vec::new() };
    // Room for a few mounts appearing meanwhile.
    // SAFETY: an all-zero `statfs` is a valid value of this plain C struct.
    let mut buf: Vec<libc::statfs> = (0..count + 8).map(|_| unsafe { std::mem::zeroed() }).collect();
    let Ok(size) = libc::c_int::try_from(buf.len() * std::mem::size_of::<libc::statfs>()) else { return Vec::new() };
    // SAFETY: `buf` is writable for `size` bytes of `statfs` records.
    let filled = unsafe { libc::getfsstat(buf.as_mut_ptr(), size, libc::MNT_NOWAIT) };
    let Ok(filled) = usize::try_from(filled) else { return Vec::new() };
    buf.iter().take(filled.min(buf.len())).map(Mount::from_statfs).collect()
}

/// `sysctl vm.swapusage` -> used bytes.
pub(crate) fn swap_used_bytes() -> Option<u64> {
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
    fn mounts_include_the_root_file_system() {
        let all = mounts();
        assert!(all.iter().any(|m| m.on == Path::new("/")), "{all:?}");
        let tmp = tempfile::tempdir().unwrap();
        let mount = mount_of(tmp.path()).expect("statfs answers for a tempdir");
        assert!(mount.used_bytes <= mount.total_bytes);
        assert_eq!(statfs_bytes(tmp.path()).map(|(total, _)| total), Some(mount.total_bytes));
    }

    #[test]
    fn apfs_volumes_report_their_own_used_space() {
        let all = mounts();
        let Some(data) = mount_of(&std::env::temp_dir()) else { return };
        let Some(container) = data.apfs_container() else { return }; // not APFS: nothing to check
        let siblings: Vec<&Mount> = all.iter().filter(|m| m.apfs_container() == Some(container)).collect();
        let used: u64 = siblings.iter().map(|m| m.used_bytes).sum();
        let container_used = data.total_bytes - data.free_bytes;
        // Each volume reports its share, so together they stay within the container's use
        // (unmounted volumes and metadata make up the difference; allow for writes meanwhile).
        assert!(used <= container_used + 2_000_000_000, "{used} vs {container_used}: {siblings:?}");
        if siblings.len() > 1 {
            assert!(siblings.iter().any(|m| m.used_bytes < container_used / 2), "{siblings:?}");
        }
    }

    #[test]
    fn apfs_container_of_device_names() {
        let mount = |from: &str, fs_type: &str| Mount {
            from: from.into(),
            on: PathBuf::from("/x"),
            fs_type: fs_type.into(),
            total_bytes: 0,
            free_bytes: 0,
            used_bytes: 0,
        };
        assert_eq!(mount("/dev/disk3s5", "apfs").apfs_container(), Some("disk3"));
        assert_eq!(mount("/dev/disk3s1s1", "apfs").apfs_container(), Some("disk3"));
        assert_eq!(mount("/dev/disk12s2", "apfs").apfs_container(), Some("disk12"));
        assert_eq!(mount("/dev/disk3s5", "hfs").apfs_container(), None);
        assert_eq!(mount("map auto_home", "apfs").apfs_container(), None);
        assert_eq!(mount("/dev/diskXs1", "apfs").apfs_container(), None);
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
