//! Allocated-size measurement.
//!
//! [`Sizer::size_of`] walks a tree in parallel (rayon), reading each directory's entries and their
//! attributes with one `getattrlistbulk(2)` call per batch (falling back to `read_dir` + `lstat`):
//! symlinks are never followed and directories on another device (mount points) are neither
//! counted nor entered. Sizes are *allocated* bytes (`st_blocks * 512`), so sparse files and APFS
//! clones count what they really occupy.
//!
//! Hardlinks: a non-directory inode with `st_nlink > 1` is counted once per [`Sizer`], across all
//! `size_of` calls (the pnpm store and every project's `node_modules/.pnpm` share inodes). Which
//! path "owns" such an inode is whichever was walked first. Inodes with a single link are not
//! remembered: two overlapping roots (a worktree and a `node_modules` inside it) are each sized
//! fully, and the scan's nested-findings rule removes the double count (ARCHITECTURE.md §10).
//!
//! Unreadable directories and entries are recorded as [`ScanError`]s (see [`Sizer::take_errors`])
//! instead of being silently skipped. Entries that vanish mid-walk are ignored.
//!
//! [`Sizer::size_tree`] is the same walk, but it also keeps the biggest descendants (for the
//! Storage view and `cmm du`) and says which parts could not be read.

use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::ffi::{OsStr, OsString};
use std::fs::Metadata;
use std::hash::{Hash, Hasher};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use rayon::prelude::*;

use crate::model::{ScanError, Unreadable};

/// At most this many errors are kept per [`Sizer`]; the rest are counted and summarised.
const MAX_ERRORS: usize = 200;
/// Number of independently locked shards of the hardlink set (keeps lock contention low).
const SHARDS: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SizeInfo {
    /// Allocated bytes (`st_blocks * 512`), hardlinked inodes counted once per [`Sizer`].
    pub bytes: u64,
    /// Sum of `st_size`.
    pub logical_bytes: u64,
    /// Number of non-directory entries (files, symlinks, ...).
    pub files: u64,
    /// Newest `mtime` of the root and everything below it.
    pub newest_mtime: Option<DateTime<Utc>>,
}

/// What [`Sizer::size_tree`] keeps of a walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeOptions {
    /// Children are kept down to this many levels below the root (`0`: the root alone).
    pub max_depth: u32,
    /// Entries allocating less than this are not kept one by one but summed into their parent's
    /// [`SizeTree::rest`].
    pub min_bytes: u64,
    /// At most this many children are kept per directory (the biggest); the others go to `rest`.
    pub max_children: usize,
}

/// A sized entry and its biggest descendants, as kept by [`Sizer::size_tree`].
///
/// For a directory: `info` = its own entry + every kept child's `info` + `rest`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SizeTree {
    /// File name (for the root of a walk: the last path component, or the whole path for `/`).
    pub name: OsString,
    /// A real directory (`symlink_metadata`: a symlink is never one).
    pub is_dir: bool,
    /// The entry and everything below it.
    pub info: SizeInfo,
    /// Number of direct entries (directories only; entries on another device are not counted).
    pub entries: u64,
    /// The directory was within `max_depth`, so its children were considered for keeping. When
    /// false, `children` and `rest` are empty: the entries were walked (sized) but not listed.
    pub expanded: bool,
    /// Kept children, biggest first (ties by name).
    pub children: Vec<SizeTree>,
    /// The direct entries that were not kept (too small, too many, or below `max_depth`).
    pub rest: Rest,
    /// This directory, or something below it, could not be (fully) read: `info` is a lower bound.
    pub unreadable: Option<Unreadable>,
}

/// Direct entries of a directory summed together instead of kept one by one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rest {
    pub entries: u64,
    pub bytes: u64,
    pub files: u64,
    pub newest_mtime: Option<DateTime<Utc>>,
}

impl Rest {
    fn add(&mut self, info: &SizeInfo) {
        self.entries += 1;
        self.bytes += info.bytes;
        self.files += info.files;
        self.newest_mtime = self.newest_mtime.max(info.newest_mtime);
    }
}

/// What could not be read below a directory, merged upwards. `EPERM` (privacy protection / SIP)
/// wins over `EACCES` when both occur: it is the one Full Disk Access can fix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Unread {
    protected: bool,
    denied: bool,
}

impl Unread {
    fn merge(self, other: Unread) -> Unread {
        Unread { protected: self.protected || other.protected, denied: self.denied || other.denied }
    }

    fn of(error: &io::Error) -> Unread {
        match error.kind() {
            io::ErrorKind::NotFound => Unread::default(), // vanished mid-walk
            _ if error.raw_os_error() == Some(libc::EPERM) => Unread { protected: true, denied: false },
            _ => Unread { protected: false, denied: true },
        }
    }

    fn flag(self) -> Option<Unreadable> {
        if self.protected {
            Some(Unreadable::Protected)
        } else if self.denied {
            Some(Unreadable::Denied)
        } else {
            None
        }
    }
}

/// A directory walked by [`Sizer::size_tree`], without its own entry.
#[derive(Default)]
struct DirTree {
    acc: Acc,
    entries: u64,
    expanded: bool,
    children: Vec<SizeTree>,
    rest: Rest,
    unread: Unread,
}

/// Shared, thread-safe sizer. One instance per scan: it remembers hardlinked inodes (so they are
/// counted once across all calls) and collects the errors met while walking.
#[derive(Debug)]
pub struct Sizer {
    hardlinks: Vec<Mutex<HashSet<(u64, u64)>>>,
    errors: Mutex<Vec<ScanError>>,
    dropped_errors: AtomicU64,
    /// Allocated bytes of every directory listed so far (for progress reporting).
    visited: AtomicU64,
}

impl Default for Sizer {
    fn default() -> Self {
        Self::new()
    }
}

/// Accumulator used during the walk (raw mtime avoids a chrono conversion per entry).
#[derive(Debug, Clone, Copy, Default)]
struct Acc {
    bytes: u64,
    logical_bytes: u64,
    files: u64,
    newest: Option<(i64, i64)>,
}

impl Acc {
    fn merge(mut self, other: Acc) -> Acc {
        self.bytes += other.bytes;
        self.logical_bytes += other.logical_bytes;
        self.files += other.files;
        self.newest = self.newest.max(other.newest);
        self
    }

    fn into_info(self) -> SizeInfo {
        SizeInfo {
            bytes: self.bytes,
            logical_bytes: self.logical_bytes,
            files: self.files,
            newest_mtime: self.newest.and_then(|(secs, nsec)| DateTime::from_timestamp(secs, nsec as u32)),
        }
    }
}

impl Sizer {
    pub fn new() -> Self {
        Self {
            hardlinks: (0..SHARDS).map(|_| Mutex::new(HashSet::new())).collect(),
            errors: Mutex::new(Vec::new()),
            dropped_errors: AtomicU64::new(0),
            visited: AtomicU64::new(0),
        }
    }

    /// Allocated bytes counted so far by all walks of this sizer (grows while a walk runs).
    pub fn visited_bytes(&self) -> u64 {
        self.visited.load(Ordering::Relaxed)
    }

    /// Like [`Sizer::size_of`], but keeps the biggest descendants (see [`TreeOptions`]) and flags
    /// what could not be read. Same rules: no symlinks followed, no other devices, hardlinks once.
    /// A missing path yields an empty, non-directory tree.
    pub fn size_tree(&self, path: &Path, opts: &TreeOptions) -> SizeTree {
        let name = path.file_name().map_or_else(|| path.as_os_str().to_owned(), OsStr::to_owned);
        let meta = match path.symlink_metadata() {
            Ok(meta) => meta,
            Err(e) => {
                self.record_io_error(path, &e);
                return SizeTree { name, unreadable: Unread::of(&e).flag(), ..SizeTree::default() };
            }
        };
        let mut own = Acc::default();
        self.account(&meta, &mut own);
        if !meta.is_dir() {
            return SizeTree { name, info: own.into_info(), ..SizeTree::default() };
        }
        let dir = self.tree_dir(path, meta.dev(), 0, opts);
        SizeTree {
            name,
            is_dir: true,
            info: own.merge(dir.acc).into_info(),
            entries: dir.entries,
            expanded: dir.expanded,
            children: dir.children,
            rest: dir.rest,
            unreadable: dir.unread.flag(),
        }
    }

    /// Allocated size of `path` and everything below it. Never follows symlinks (a symlink root
    /// is sized as the link itself), never crosses devices. A missing path yields zeros.
    pub fn size_of(&self, path: &Path) -> SizeInfo {
        let meta = match path.symlink_metadata() {
            Ok(meta) => meta,
            Err(e) => {
                self.record_io_error(path, &e);
                return SizeInfo::default();
            }
        };
        let mut acc = Acc::default();
        self.account(&meta, &mut acc);
        if meta.is_dir() {
            acc = acc.merge(self.walk_dir(path, meta.dev()));
        }
        acc.into_info()
    }

    /// Records a non-fatal problem (shown in `ScanReport::errors`). Detectors reach it through
    /// [`crate::scan::ScanContext::warn`].
    pub fn push_error(&self, path: impl Into<String>, message: impl Into<String>) {
        let mut errors = self.errors.lock().unwrap_or_else(|e| e.into_inner());
        if errors.len() < MAX_ERRORS {
            errors.push(ScanError { path: path.into(), message: message.into() });
        } else {
            self.dropped_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Drains the errors collected so far (permission denied, unreadable entries, ...). If more
    /// than the cap were recorded, a final summary entry says how many were dropped.
    pub fn take_errors(&self) -> Vec<ScanError> {
        let mut errors = std::mem::take(&mut *self.errors.lock().unwrap_or_else(|e| e.into_inner()));
        let dropped = self.dropped_errors.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            errors.push(ScanError { path: String::from("…"), message: format!("{dropped} more errors not shown") });
        }
        errors
    }

    /// Calls `on_entry(name, stat)` for every entry of `dir` on device `dev` (mount points and
    /// other devices are skipped: not ours to count). Unreadable directories and entries are
    /// recorded as errors; the result says what could not be read.
    fn list(&self, dir: &Path, dev: u64, mut on_entry: impl FnMut(&OsStr, &bulk::Stat)) -> Unread {
        let mut unread = Unread::default();
        let listed = bulk::list_dir(dir, |entry| match entry {
            Ok(entry) => {
                if entry.dev == dev && !entry.mount_point {
                    on_entry(entry.name, &entry.stat);
                }
            }
            Err((name, error)) => unread = unread.merge(self.record_unreadable(&dir.join(name), &error)),
        });
        match listed {
            Ok(()) => unread,
            Err(e) if bulk::unsupported(&e) => self.list_std(dir, dev, on_entry),
            Err(e) => unread.merge(self.record_unreadable(dir, &e)),
        }
    }

    /// Portable fallback for file systems without `getattrlistbulk` support.
    fn list_std(&self, dir: &Path, dev: u64, mut on_entry: impl FnMut(&OsStr, &bulk::Stat)) -> Unread {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => return self.record_unreadable(dir, &e),
        };
        let mut unread = Unread::default();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    unread = unread.merge(self.record_unreadable(dir, &e));
                    continue;
                }
            };
            // DirEntry::metadata is lstat: symlinks are reported as links, never followed.
            match entry.metadata() {
                Ok(meta) if meta.dev() == dev => on_entry(&entry.file_name(), &stat_of(&meta)),
                Ok(_) => {}
                Err(e) => unread = unread.merge(self.record_unreadable(&entry.path(), &e)),
            }
        }
        unread
    }

    fn walk_dir(&self, dir: &Path, dev: u64) -> Acc {
        let mut acc = Acc::default();
        let mut subdirs = Vec::new();
        self.list(dir, dev, |name, stat| {
            self.account_stat(dev, stat, &mut acc);
            if stat.is_dir {
                subdirs.push(dir.join(name));
            }
        });
        self.visited.fetch_add(acc.bytes, Ordering::Relaxed);
        self.walk_subdirs(acc, &subdirs, dev)
    }

    fn walk_subdirs(&self, acc: Acc, subdirs: &[PathBuf], dev: u64) -> Acc {
        match subdirs {
            [] => acc,
            [one] => acc.merge(self.walk_dir(one, dev)),
            many => acc.merge(many.par_iter().map(|sub| self.walk_dir(sub, dev)).reduce(Acc::default, Acc::merge)),
        }
    }

    /// The tree walk behind [`Sizer::size_tree`]: `dir` sits `depth` levels below the root, so its
    /// children are kept only while `depth < max_depth`.
    fn tree_dir(&self, dir: &Path, dev: u64, depth: u32, opts: &TreeOptions) -> DirTree {
        let keep = depth < opts.max_depth;
        let mut out = DirTree { expanded: keep, ..DirTree::default() };
        let mut candidates = Vec::new();
        // Subdirectories with their own entry's size; their contents are walked below.
        let mut subdirs: Vec<(OsString, Acc)> = Vec::new();
        out.unread = self.list(dir, dev, |name, stat| {
            out.entries += 1;
            let mut own = Acc::default();
            self.account_stat(dev, stat, &mut own);
            if stat.is_dir {
                subdirs.push((name.to_owned(), own));
                return;
            }
            out.acc = out.acc.merge(own);
            if keep && own.bytes >= opts.min_bytes {
                candidates.push(SizeTree { name: name.to_owned(), info: own.into_info(), ..SizeTree::default() });
            } else if keep {
                out.rest.add(&own.into_info());
            }
        });
        self.visited
            .fetch_add(out.acc.bytes + subdirs.iter().map(|(_, own)| own.bytes).sum::<u64>(), Ordering::Relaxed);

        let walk = |(name, own): (OsString, Acc)| {
            let sub = self.tree_dir(&dir.join(&name), dev, depth + 1, opts);
            (name, own, sub)
        };
        let walked: Vec<(OsString, Acc, DirTree)> = if subdirs.len() > 1 {
            subdirs.into_par_iter().map(walk).collect()
        } else {
            subdirs.into_iter().map(walk).collect()
        };
        for (name, own, sub) in walked {
            let info = own.merge(sub.acc);
            out.acc = out.acc.merge(info);
            out.unread = out.unread.merge(sub.unread);
            if keep && info.bytes >= opts.min_bytes {
                candidates.push(SizeTree {
                    name,
                    is_dir: true,
                    info: info.into_info(),
                    entries: sub.entries,
                    expanded: sub.expanded,
                    children: sub.children,
                    rest: sub.rest,
                    unreadable: sub.unread.flag(),
                });
            } else if keep {
                out.rest.add(&info.into_info());
            }
        }

        candidates.sort_by(|a, b| b.info.bytes.cmp(&a.info.bytes).then_with(|| a.name.cmp(&b.name)));
        if candidates.len() > opts.max_children {
            for dropped in candidates.drain(opts.max_children..) {
                out.rest.add(&dropped.info);
            }
        }
        out.children = candidates;
        out
    }

    fn account(&self, meta: &Metadata, acc: &mut Acc) {
        self.account_stat(meta.dev(), &stat_of(meta), acc);
    }

    fn account_stat(&self, dev: u64, stat: &bulk::Stat, acc: &mut Acc) {
        if !stat.is_dir && stat.nlink > 1 && !self.first_sighting(dev, stat.ino) {
            return;
        }
        acc.bytes += stat.alloc;
        acc.logical_bytes += stat.size;
        if !stat.is_dir {
            acc.files += 1;
        }
        acc.newest = acc.newest.max(Some(stat.mtime));
    }

    /// `true` the first time this hardlinked inode is seen by this sizer.
    fn first_sighting(&self, dev: u64, ino: u64) -> bool {
        let mut hasher = DefaultHasher::new();
        (dev, ino).hash(&mut hasher);
        let shard = &self.hardlinks[hasher.finish() as usize % SHARDS];
        shard.lock().unwrap_or_else(|e| e.into_inner()).insert((dev, ino))
    }

    fn record_io_error(&self, path: &Path, error: &io::Error) {
        if error.kind() == io::ErrorKind::NotFound {
            return; // vanished while walking (temp files, caches being rewritten)
        }
        self.push_error(path.to_string_lossy(), io_message(error));
    }

    fn record_unreadable(&self, path: &Path, error: &io::Error) -> Unread {
        self.record_io_error(path, error);
        Unread::of(error)
    }
}

fn stat_of(meta: &Metadata) -> bulk::Stat {
    bulk::Stat {
        is_dir: meta.is_dir(),
        ino: meta.ino(),
        nlink: meta.nlink(),
        size: meta.len(),
        alloc: meta.blocks() * 512,
        mtime: (meta.mtime(), meta.mtime_nsec()),
    }
}

/// `getattrlistbulk(2)`: a directory's entries together with their attributes, one syscall per
/// batch instead of one path lookup + `lstat` per entry (~2x faster than `read_dir` + `lstat` on APFS).
mod bulk {
    use std::cell::RefCell;
    use std::ffi::{CString, OsStr, c_void};
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// The subset of `stat` the sizer needs.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Stat {
        pub is_dir: bool,
        pub ino: u64,
        pub nlink: u64,
        /// Logical size (`st_size`).
        pub size: u64,
        /// Allocated bytes on disk (`st_blocks * 512`).
        pub alloc: u64,
        /// `(seconds, nanoseconds)` of `st_mtime`.
        pub mtime: (i64, i64),
    }

    pub struct Entry<'a> {
        pub name: &'a OsStr,
        pub dev: u64,
        /// Another file system is mounted on this directory.
        pub mount_point: bool,
        pub stat: Stat,
    }

    const VDIR: u32 = 2;
    /// Not exported by the libc crate (`<sys/attr.h>`).
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const BUF_LEN: usize = 128 * 1024;
    const COMMON: u32 = libc::ATTR_CMN_RETURNED_ATTRS
        | ATTR_CMN_ERROR
        | libc::ATTR_CMN_NAME
        | libc::ATTR_CMN_DEVID
        | libc::ATTR_CMN_OBJTYPE
        | libc::ATTR_CMN_MODTIME
        | libc::ATTR_CMN_FILEID;
    const DIR: u32 = libc::ATTR_DIR_MOUNTSTATUS | libc::ATTR_DIR_ALLOCSIZE | libc::ATTR_DIR_DATALENGTH;
    const FILE: u32 = libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_TOTALSIZE | libc::ATTR_FILE_ALLOCSIZE;

    thread_local! {
        static BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; BUF_LEN]);
    }

    /// The file system does not support `getattrlistbulk`: use the portable walker instead.
    pub fn unsupported(error: &io::Error) -> bool {
        matches!(error.raw_os_error(), Some(libc::ENOTSUP | libc::ENOSYS | libc::EINVAL))
    }

    /// Calls `on_entry` for every entry of `dir` (never `.`/`..`). Per-entry failures are passed as
    /// `Err((name, error))`; failing to read the directory itself is the returned error.
    pub fn list_dir(dir: &Path, mut on_entry: impl FnMut(Result<Entry<'_>, (&OsStr, io::Error)>)) -> io::Result<()> {
        let c_path = CString::new(dir.as_os_str().as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let fd = loop {
            // SAFETY: `c_path` is a valid NUL-terminated string.
            let fd = unsafe { libc::open(c_path.as_ptr(), flags) };
            if fd >= 0 {
                // SAFETY: `fd` is a freshly opened descriptor that nothing else owns.
                break unsafe { OwnedFd::from_raw_fd(fd) };
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        };
        let mut attrs = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: COMMON,
            volattr: 0,
            dirattr: DIR,
            fileattr: FILE,
            forkattr: 0,
        };
        BUF.with_borrow_mut(|buf| {
            loop {
                // SAFETY: `attrs` is a valid attrlist, `buf` is writable for `buf.len()` bytes.
                let count = unsafe {
                    libc::getattrlistbulk(
                        fd.as_raw_fd(),
                        (&raw mut attrs).cast::<c_void>(),
                        buf.as_mut_ptr().cast::<c_void>(),
                        buf.len(),
                        0,
                    )
                };
                if count < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if count == 0 {
                    return Ok(());
                }
                let mut offset = 0usize;
                for _ in 0..count {
                    let len = read_u32(buf, offset).ok_or_else(malformed)? as usize;
                    let record = buf.get(offset..offset + len).filter(|_| len >= 24).ok_or_else(malformed)?;
                    on_entry(parse(record).ok_or_else(malformed)?);
                    offset += len;
                }
            }
        })
    }

    fn malformed() -> io::Error {
        io::Error::other("malformed getattrlistbulk record")
    }

    /// Decodes one record. Attributes are packed in bit order per group (common, dir, file),
    /// 4-byte aligned, except that `ATTR_CMN_ERROR` directly follows the returned-attributes set.
    #[allow(clippy::type_complexity)]
    fn parse(record: &[u8]) -> Option<Result<Entry<'_>, (&OsStr, io::Error)>> {
        let common = read_u32(record, 4)?;
        let dir = read_u32(record, 12)?;
        let file = read_u32(record, 16)?;
        let mut p = 24; // u32 length + attribute_set_t (5 x u32)

        let mut error = 0;
        if common & ATTR_CMN_ERROR != 0 {
            error = read_u32(record, p)?;
            p += 4;
        }
        let mut name: &[u8] = b"";
        if common & libc::ATTR_CMN_NAME != 0 {
            let start = p.checked_add_signed(read_u32(record, p)? as i32 as isize)?;
            let len = read_u32(record, p + 4)? as usize;
            name = record.get(start..start + len.saturating_sub(1))?; // length includes the NUL
            p += 8;
        }
        let name = OsStr::from_bytes(name);
        if error != 0 {
            return Some(Err((name, io::Error::from_raw_os_error(error as i32))));
        }
        if common & libc::ATTR_CMN_NAME == 0 || common & libc::ATTR_CMN_OBJTYPE == 0 {
            return Some(Err((name, io::Error::other("attributes not returned"))));
        }

        let mut dev = 0;
        if common & libc::ATTR_CMN_DEVID != 0 {
            dev = read_u32(record, p)? as i32 as u64; // dev_t is i32; sign-extend like MetadataExt::dev
            p += 4;
        }
        let obj_type = read_u32(record, p)?;
        p += 4;
        let mut stat = Stat { is_dir: obj_type == VDIR, ..Stat::default() };
        if common & libc::ATTR_CMN_MODTIME != 0 {
            stat.mtime = (read_u64(record, p)? as i64, read_u64(record, p + 8)? as i64);
            p += 16;
        }
        if common & libc::ATTR_CMN_FILEID != 0 {
            stat.ino = read_u64(record, p)?;
            p += 8;
        }

        let mut mount_point = false;
        if dir & libc::ATTR_DIR_MOUNTSTATUS != 0 {
            mount_point = read_u32(record, p)? & libc::DIR_MNTSTATUS_MNTPOINT != 0;
            p += 4;
        }
        if dir & libc::ATTR_DIR_ALLOCSIZE != 0 {
            stat.alloc = read_u64(record, p)?;
            p += 8;
        }
        if dir & libc::ATTR_DIR_DATALENGTH != 0 {
            stat.size = read_u64(record, p)?;
            p += 8;
        }

        if file & libc::ATTR_FILE_LINKCOUNT != 0 {
            stat.nlink = u64::from(read_u32(record, p)?);
            p += 4;
        }
        if file & libc::ATTR_FILE_TOTALSIZE != 0 {
            stat.size = read_u64(record, p)?;
            p += 8;
        }
        if file & libc::ATTR_FILE_ALLOCSIZE != 0 {
            stat.alloc = read_u64(record, p)?;
        }
        Some(Ok(Entry { name, dev, mount_point, stat }))
    }

    fn read_u32(buf: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_ne_bytes(buf.get(at..at + 4)?.try_into().ok()?))
    }

    fn read_u64(buf: &[u8], at: usize) -> Option<u64> {
        Some(u64::from_ne_bytes(buf.get(at..at + 8)?.try_into().ok()?))
    }
}

/// Short, stable message for an I/O error ("permission denied", "operation not permitted", ...).
fn io_message(error: &io::Error) -> String {
    match error.raw_os_error() {
        Some(libc::EPERM) => String::from("operation not permitted (needs Full Disk Access?)"),
        Some(libc::EACCES) => String::from("permission denied"),
        _ => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn write(path: &Path, len: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, vec![7u8; len]).unwrap();
    }

    #[test]
    fn sums_nested_tree() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/b/c/one.bin"), 10_000);
        write(&dir.path().join("a/two.bin"), 20_000);
        write(&dir.path().join("three.bin"), 30_000);
        let info = Sizer::new().size_of(dir.path());
        assert_eq!(info.files, 3);
        assert!(info.logical_bytes >= 60_000);
        assert!(info.bytes >= 60_000, "allocated >= data for non-sparse files");
        assert!(info.newest_mtime.is_some());
    }

    #[test]
    fn hardlink_counted_once() {
        let dir = tempfile::tempdir().unwrap();
        let data = vec![7u8; 64 * 1024];
        fs::write(dir.path().join("a.bin"), &data).unwrap();
        let single = Sizer::new().size_of(&dir.path().join("a.bin"));
        assert!(single.bytes >= data.len() as u64);

        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::hard_link(dir.path().join("a.bin"), dir.path().join("b.bin")).unwrap();
        fs::hard_link(dir.path().join("a.bin"), dir.path().join("sub/c.bin")).unwrap();
        let total = Sizer::new().size_of(dir.path());
        assert_eq!(total.files, 1, "three names, one inode");
        assert!(total.bytes < 2 * single.bytes);
        assert!(total.logical_bytes < 2 * data.len() as u64);
    }

    #[test]
    fn hardlink_counted_once_across_calls_on_one_sizer() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("store/pkg.bin"), 64 * 1024);
        fs::create_dir(dir.path().join("project")).unwrap();
        fs::hard_link(dir.path().join("store/pkg.bin"), dir.path().join("project/pkg.bin")).unwrap();

        let sizer = Sizer::new();
        let store = sizer.size_of(&dir.path().join("store"));
        let project = sizer.size_of(&dir.path().join("project"));
        assert_eq!(store.files, 1);
        assert_eq!(project.files, 0, "already counted under store");

        // A fresh sizer counts it again.
        assert_eq!(Sizer::new().size_of(&dir.path().join("project")).files, 1);
    }

    #[test]
    fn overlapping_roots_are_each_sized_fully() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("worktree/node_modules/x.bin"), 64 * 1024);
        let sizer = Sizer::new();
        let parent = sizer.size_of(&dir.path().join("worktree"));
        let child = sizer.size_of(&dir.path().join("worktree/node_modules"));
        assert_eq!(parent.files, 1);
        assert_eq!(child.files, 1, "single-link inodes are not deduped across calls");
    }

    #[test]
    fn symlinks_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        write(&target.path().join("big.bin"), 256 * 1024);
        std::os::unix::fs::symlink(target.path(), dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink(target.path().join("big.bin"), dir.path().join("file-link")).unwrap();
        let info = Sizer::new().size_of(dir.path());
        assert_eq!(info.files, 2, "only the links themselves");
        assert!(info.logical_bytes < 256 * 1024);

        let root_link = Sizer::new().size_of(&dir.path().join("link"));
        assert_eq!(root_link.files, 1, "a symlink root is sized as the link");
        assert!(root_link.logical_bytes < 1024);
    }

    #[test]
    fn missing_path_is_zero_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let sizer = Sizer::new();
        assert_eq!(sizer.size_of(&dir.path().join("nope")), SizeInfo::default());
        assert!(sizer.take_errors().is_empty());
    }

    #[test]
    fn unreadable_directory_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        write(&locked.join("secret.bin"), 1000);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let sizer = Sizer::new();
        let info = sizer.size_of(dir.path());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(info.files, 0, "contents of the locked dir are not visible");
        let errors = sizer.take_errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].path, locked.to_string_lossy());
        assert!(errors[0].message.contains("permission denied"), "{}", errors[0].message);
        assert!(sizer.take_errors().is_empty(), "take drains");
    }

    #[test]
    fn error_list_is_capped() {
        let sizer = Sizer::new();
        for i in 0..MAX_ERRORS + 5 {
            sizer.push_error(format!("/p/{i}"), "x");
        }
        let errors = sizer.take_errors();
        assert_eq!(errors.len(), MAX_ERRORS + 1);
        assert_eq!(errors.last().unwrap().message, "5 more errors not shown");
    }

    const ALL: TreeOptions = TreeOptions { max_depth: u32::MAX, min_bytes: 0, max_children: usize::MAX };

    /// `info` = own entry + kept children + rest, at every level.
    fn assert_adds_up(tree: &SizeTree, own_max: u64) {
        if !tree.expanded {
            assert!(tree.children.is_empty() && tree.rest == Rest::default(), "{tree:?}");
            return;
        }
        let kept: u64 = tree.children.iter().map(|c| c.info.bytes).sum();
        let total = kept + tree.rest.bytes;
        assert!(total <= tree.info.bytes && tree.info.bytes - total <= own_max, "{tree:?}");
        assert_eq!(tree.children.len() as u64 + tree.rest.entries, tree.entries, "{:?}", tree.name);
        tree.children.iter().for_each(|c| assert_adds_up(c, own_max));
    }

    #[test]
    fn size_tree_matches_size_of_and_sorts_children() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("big/inner/a.bin"), 300_000);
        write(&dir.path().join("big/b.bin"), 100_000);
        write(&dir.path().join("mid.bin"), 200_000);
        write(&dir.path().join("small.bin"), 10_000);
        let tree = Sizer::new().size_tree(dir.path(), &ALL);
        assert_eq!(tree.info, Sizer::new().size_of(dir.path()));
        assert!(tree.is_dir);
        let names: Vec<_> = tree.children.iter().map(|c| c.name.to_str().unwrap()).collect();
        assert_eq!(names, vec!["big", "mid.bin", "small.bin"]);
        assert_eq!(tree.children[0].children[0].name, "inner");
        assert_eq!(tree.rest, Rest::default());
        assert_eq!(tree.unreadable, None);
        assert_adds_up(&tree, 64 * 1024);
    }

    #[test]
    fn size_tree_sums_small_deep_and_extra_entries_into_rest() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("big/inner/a.bin"), 300_000);
        write(&dir.path().join("x1.bin"), 150_000);
        write(&dir.path().join("x2.bin"), 140_000);
        write(&dir.path().join("tiny.bin"), 1_000);
        let opts = TreeOptions { max_depth: 1, min_bytes: 100_000, max_children: 2 };
        let tree = Sizer::new().size_tree(dir.path(), &opts);
        let names: Vec<_> = tree.children.iter().map(|c| c.name.to_str().unwrap()).collect();
        assert_eq!(names, vec!["big", "x1.bin"], "two biggest kept");
        assert_eq!(tree.rest.entries, 2, "x2.bin (over max_children) and tiny.bin (under min_bytes)");
        assert_eq!(tree.rest.files, 2);
        assert!(tree.rest.bytes >= 141_000);
        let big = &tree.children[0];
        assert!(!big.expanded && big.children.is_empty(), "below max_depth nothing is kept");
        assert_eq!(big.entries, 1, "but its entries are counted");
        assert_eq!(big.rest, Rest::default());
        assert_eq!(tree.info, Sizer::new().size_of(dir.path()), "sizes are complete whatever is kept");
        assert_adds_up(&tree, 64 * 1024);
    }

    #[test]
    fn size_tree_flags_unreadable_folders_upwards() {
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("a/locked");
        write(&locked.join("secret.bin"), 1000);
        write(&dir.path().join("b/ok.bin"), 1000);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let sizer = Sizer::new();
        let tree = sizer.size_tree(dir.path(), &ALL);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(tree.unreadable, Some(Unreadable::Denied));
        let a = tree.children.iter().find(|c| c.name == "a").unwrap();
        let b = tree.children.iter().find(|c| c.name == "b").unwrap();
        assert_eq!(a.unreadable, Some(Unreadable::Denied));
        assert_eq!(a.children[0].unreadable, Some(Unreadable::Denied));
        assert_eq!(a.children[0].entries, 0);
        assert_eq!(b.unreadable, None);
        assert_eq!(sizer.take_errors().len(), 1);
    }

    #[test]
    fn size_tree_counts_hardlinks_once_and_never_follows_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        write(&target.path().join("huge.bin"), 512 * 1024);
        write(&dir.path().join("a/data.bin"), 64 * 1024);
        fs::create_dir(dir.path().join("b")).unwrap();
        fs::hard_link(dir.path().join("a/data.bin"), dir.path().join("b/data.bin")).unwrap();
        std::os::unix::fs::symlink(target.path(), dir.path().join("link")).unwrap();
        let tree = Sizer::new().size_tree(dir.path(), &ALL);
        assert_eq!(tree.info.files, 2, "one inode for the two names + the link itself");
        assert!(tree.info.logical_bytes < 2 * 64 * 1024);
        let link = tree.children.iter().find(|c| c.name == "link").unwrap();
        assert!(!link.is_dir, "a symlink is never a folder");
        assert!(link.info.bytes < 64 * 1024);

        let root_link = Sizer::new().size_tree(&dir.path().join("link"), &ALL);
        assert!(!root_link.is_dir && root_link.children.is_empty());
        let missing = Sizer::new().size_tree(&dir.path().join("nope"), &ALL);
        assert_eq!(missing.info, SizeInfo::default());
        assert_eq!(missing.unreadable, None, "a missing path is not unreadable");
    }

    #[test]
    fn visited_bytes_grow_with_every_walk() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/b.bin"), 100_000);
        let sizer = Sizer::new();
        assert_eq!(sizer.visited_bytes(), 0);
        let info = sizer.size_of(dir.path());
        let after_one = sizer.visited_bytes();
        assert!(after_one >= 100_000 && after_one <= info.bytes);
        sizer.size_tree(dir.path(), &ALL);
        assert!(sizer.visited_bytes() > after_one);
    }

    #[test]
    fn newest_mtime_is_the_newest_entry() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("old/a.bin"), 10);
        write(&dir.path().join("new.bin"), 10);
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        let newest = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for p in ["old/a.bin", "old", ""] {
            fs::File::open(dir.path().join(p)).unwrap().set_modified(old).unwrap();
        }
        fs::File::options().write(true).open(dir.path().join("new.bin")).unwrap().set_modified(newest).unwrap();
        let info = Sizer::new().size_of(dir.path());
        assert_eq!(info.newest_mtime.unwrap().timestamp(), 1_700_000_000);
        let info = Sizer::new().size_of(&dir.path().join("old"));
        assert_eq!(info.newest_mtime.unwrap().timestamp(), 1_000_000_000);
    }
}
