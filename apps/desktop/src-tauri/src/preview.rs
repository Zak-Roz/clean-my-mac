//! Downloads tab previews (ARCHITECTURE.md §7): QuickLook thumbnails and the Quick Look panel.
//!
//! Both commands accept only paths whose **canonical** location (symlinks and `..` resolved) is
//! strictly inside `~/Downloads` ([`downloads_target`]): a link in Downloads that points at
//! `~/.ssh` is refused, so is `~/Downloads/../.ssh`. Neither touches the Tauri asset protocol: a
//! thumbnail travels as raw PNG bytes over IPC.
//!
//! Thumbnails come from `QLThumbnailGenerator` (the best representation: a real thumbnail when the
//! file type has one, its icon otherwise), are encoded as PNG and kept in a bounded in-memory
//! cache keyed by canonical path + modification time + size on disk + requested pixel size, so a
//! changed file gets a new thumbnail. Nothing is written to disk.
//!
//! The Quick Look panel is the system's own (`qlmanage -p`, run directly from `/usr/bin`): one at a
//! time; asking again for the same item while it is open closes it (the Space-bar toggle).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use cmm_core::config::home_dir;
use tauri::State;

/// Largest thumbnail edge the UI may ask for, in points (rendered at 2x).
const MAX_THUMBNAIL_POINTS: u32 = 512;
/// Upper bound for the cached PNG bytes.
const CACHE_BYTES: usize = 64 * 1024 * 1024;
const QLMANAGE: &str = "/usr/bin/qlmanage";

/// The canonical form of `path` if it is strictly inside `<home>/Downloads` (after resolving
/// symlinks and `..`), otherwise a readable refusal. `Downloads` itself is refused too.
pub fn downloads_target(path: &str, home: &Path) -> Result<PathBuf, String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!("not an absolute path: {path}"));
    }
    let downloads = std::fs::canonicalize(home.join("Downloads")).map_err(|e| format!("no Downloads folder: {e}"))?;
    let target = std::fs::canonicalize(p).map_err(|_| format!("{path} no longer exists"))?;
    if target != downloads && target.starts_with(&downloads) {
        Ok(target)
    } else {
        Err(format!("{path} is not inside Downloads"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    path: PathBuf,
    mtime: (i64, i64),
    bytes: u64,
    pixels: u32,
}

#[derive(Default)]
struct Cache {
    map: HashMap<Key, Arc<Vec<u8>>>,
    /// Insertion order, oldest first (evicted first).
    order: VecDeque<Key>,
    bytes: usize,
}

impl Cache {
    fn get(&self, key: &Key) -> Option<Arc<Vec<u8>>> {
        self.map.get(key).cloned()
    }

    fn insert(&mut self, key: Key, png: Arc<Vec<u8>>, cap: usize) {
        if png.len() > cap || self.map.contains_key(&key) {
            return;
        }
        // A file that changed gets a new key: drop its older thumbnails at the same size.
        let stale: Vec<Key> =
            self.order.iter().filter(|k| k.path == key.path && k.pixels == key.pixels).cloned().collect();
        for old in stale {
            self.remove(&old);
        }
        while self.bytes + png.len() > cap {
            let Some(oldest) = self.order.front().cloned() else { break };
            self.remove(&oldest);
        }
        self.bytes += png.len();
        self.order.push_back(key.clone());
        self.map.insert(key, png);
    }

    fn remove(&mut self, key: &Key) {
        if let Some(png) = self.map.remove(key) {
            self.bytes -= png.len();
        }
        self.order.retain(|k| k != key);
    }
}

/// Managed Tauri state for previews.
#[derive(Default)]
pub struct PreviewState {
    cache: Mutex<Cache>,
    /// The open Quick Look panel and the item it shows.
    quick_look: Mutex<Option<(PathBuf, Child)>>,
}

fn key_for(target: &Path, pixels: u32) -> Result<Key, String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(target).map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(Key { path: target.to_path_buf(), mtime: (meta.mtime(), meta.mtime_nsec()), bytes: meta.blocks() * 512, pixels })
}

/// PNG thumbnail of a Downloads item, `size` points square at 2x (so `size * 2` pixels at most).
#[tauri::command]
pub async fn thumbnail(
    state: State<'_, PreviewState>,
    path: String,
    size: u32,
) -> Result<tauri::ipc::Response, String> {
    let points = size.clamp(16, MAX_THUMBNAIL_POINTS);
    let target = downloads_target(&path, &home_dir())?;
    let key = key_for(&target, points * 2)?;
    if let Some(png) = state.cache.lock().map_err(|e| e.to_string())?.get(&key) {
        return Ok(tauri::ipc::Response::new(png.as_ref().clone()));
    }
    let png = tauri::async_runtime::spawn_blocking(move || ql::thumbnail_png(&target, f64::from(points), 2.0))
        .await
        .map_err(|e| format!("background task failed: {e}"))??;
    let png = Arc::new(png);
    state.cache.lock().map_err(|e| e.to_string())?.insert(key, png.clone(), CACHE_BYTES);
    Ok(tauri::ipc::Response::new(png.as_ref().clone()))
}

/// Opens the Quick Look panel for one Downloads item; asking again for the item that is already
/// showing closes it. Returns whether a panel is now open.
#[tauri::command]
pub async fn quick_look(state: State<'_, PreviewState>, path: String) -> Result<bool, String> {
    let target = downloads_target(&path, &home_dir())?;
    let mut slot = state.quick_look.lock().map_err(|e| e.to_string())?;
    if let Some((shown, mut child)) = slot.take() {
        let still_open = matches!(child.try_wait(), Ok(None));
        if still_open {
            let _ = child.kill();
        }
        let _ = child.wait();
        if still_open && shown == target {
            return Ok(false);
        }
    }
    let child = Command::new(QLMANAGE)
        .arg("-p")
        .arg(&target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Quick Look failed to start: {e}"))?;
    *slot = Some((target, child));
    Ok(true)
}

#[cfg(target_os = "macos")]
mod ql {
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
    use objc2_core_graphics::CGImage;
    use objc2_foundation::{NSDictionary, NSError, NSSize, NSString, NSURL};
    use objc2_quick_look_thumbnailing::{
        QLThumbnailGenerationRequest, QLThumbnailGenerationRequestRepresentationTypes, QLThumbnailGenerator,
        QLThumbnailRepresentation,
    };

    const TIMEOUT: Duration = Duration::from_secs(15);

    fn png_of(image: &CGImage) -> Result<Vec<u8>, String> {
        let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), image);
        // SAFETY: an empty properties dictionary is valid for every file type.
        let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) };
        data.map(|d| d.to_vec()).ok_or_else(|| "PNG encoding failed".to_string())
    }

    /// The best QuickLook representation of `path` (thumbnail, else icon) as PNG bytes.
    pub fn thumbnail_png(path: &Path, points: f64, scale: f64) -> Result<Vec<u8>, String> {
        let path = path.to_str().ok_or("path is not UTF-8")?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        // SAFETY: plain initializer with a file URL, a positive size and scale.
        let request = unsafe {
            QLThumbnailGenerationRequest::initWithFileAtURL_size_scale_representationTypes(
                QLThumbnailGenerationRequest::alloc(),
                &url,
                NSSize::new(points, points),
                scale,
                QLThumbnailGenerationRequestRepresentationTypes::All,
            )
        };
        let (tx, rx) = mpsc::sync_channel::<Result<Vec<u8>, String>>(1);
        let handler = RcBlock::new(move |rep: *mut QLThumbnailRepresentation, error: *mut NSError| {
            // SAFETY: QuickLook passes valid (or null) object pointers for the duration of the call.
            let result = match unsafe { rep.as_ref() } {
                // SAFETY: a representation always has a CGImage.
                Some(rep) => png_of(&*unsafe { rep.CGImage() }),
                None => Err(unsafe { error.as_ref() }
                    .map_or_else(|| "no thumbnail".to_string(), |e| e.localizedDescription().to_string())),
            };
            let _ = tx.try_send(result);
        });
        // SAFETY: the request and the handler are retained by the generator until it calls back.
        unsafe {
            QLThumbnailGenerator::sharedGenerator()
                .generateBestRepresentationForRequest_completionHandler(&request, &handler)
        };
        match rx.recv_timeout(TIMEOUT) {
            Ok(result) => result,
            Err(_) => {
                // SAFETY: cancelling a request we submitted.
                unsafe { QLThumbnailGenerator::sharedGenerator().cancelRequest(&request) };
                Err("thumbnail timed out".into())
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod ql {
    pub fn thumbnail_png(_path: &std::path::Path, _points: f64, _scale: f64) -> Result<Vec<u8>, String> {
        Err("thumbnails need macOS".into())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    struct Home {
        _tmp: tempfile::TempDir,
        home: PathBuf,
    }

    fn home() -> Home {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap().join("home");
        fs::create_dir_all(home.join("Downloads/folder")).unwrap();
        fs::create_dir_all(home.join(".ssh")).unwrap();
        fs::write(home.join("Downloads/photo.jpg"), b"jpg").unwrap();
        fs::write(home.join("Downloads/folder/inner.txt"), b"x").unwrap();
        fs::write(home.join(".ssh/id_ed25519"), b"secret").unwrap();
        fs::write(home.join("notes.txt"), b"n").unwrap();
        Home { _tmp: tmp, home }
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn items_inside_downloads_are_accepted() {
        let h = home();
        let dl = h.home.join("Downloads");
        assert_eq!(downloads_target(&s(&dl.join("photo.jpg")), &h.home).unwrap(), dl.join("photo.jpg"));
        assert_eq!(downloads_target(&s(&dl.join("folder")), &h.home).unwrap(), dl.join("folder"));
        // `..` that stays inside is fine; the canonical path is returned.
        let roundabout = dl.join("folder/../photo.jpg");
        assert_eq!(downloads_target(&s(&roundabout), &h.home).unwrap(), dl.join("photo.jpg"));
    }

    #[test]
    fn paths_outside_downloads_are_refused() {
        let h = home();
        let dl = h.home.join("Downloads");
        for path in [
            h.home.join("notes.txt"),
            h.home.join(".ssh/id_ed25519"),
            dl.join("../.ssh/id_ed25519"),
            dl.join("folder/../../notes.txt"),
            dl.clone(),
            dl.join("."),
            h.home.clone(),
        ] {
            let err = downloads_target(&s(&path), &h.home).unwrap_err();
            assert!(err.contains("not inside Downloads"), "{}: {err}", path.display());
        }
        assert!(downloads_target("relative/photo.jpg", &h.home).unwrap_err().contains("not an absolute path"));
        assert!(downloads_target(&s(&dl.join("missing.jpg")), &h.home).unwrap_err().contains("no longer exists"));
    }

    #[test]
    fn symlinks_are_judged_by_where_they_point() {
        let h = home();
        let dl = h.home.join("Downloads");
        symlink(h.home.join(".ssh/id_ed25519"), dl.join("key-link")).unwrap();
        symlink(h.home.join(".ssh"), dl.join("dir-link")).unwrap();
        symlink(dl.join("photo.jpg"), dl.join("photo-link")).unwrap();
        for escape in [dl.join("key-link"), dl.join("dir-link"), dl.join("dir-link/id_ed25519")] {
            let err = downloads_target(&s(&escape), &h.home).unwrap_err();
            assert!(err.contains("not inside Downloads"), "{}: {err}", escape.display());
        }
        assert_eq!(downloads_target(&s(&dl.join("photo-link")), &h.home).unwrap(), dl.join("photo.jpg"));

        // A Downloads that is itself a link is resolved first; its items are still accepted.
        let other = tempfile::tempdir().unwrap();
        let real = other.path().canonicalize().unwrap().join("RealDownloads");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("a.pdf"), b"a").unwrap();
        let linked_home = other.path().canonicalize().unwrap().join("home");
        fs::create_dir_all(&linked_home).unwrap();
        symlink(&real, linked_home.join("Downloads")).unwrap();
        let via_link = linked_home.join("Downloads/a.pdf");
        assert_eq!(downloads_target(&s(&via_link), &linked_home).unwrap(), real.join("a.pdf"));
    }

    #[test]
    fn cache_is_bounded_and_replaces_stale_thumbnails() {
        let key = |name: &str, mtime: i64| Key { path: PathBuf::from(name), mtime: (mtime, 0), bytes: 1, pixels: 128 };
        let mut cache = Cache::default();
        cache.insert(key("/d/a", 1), Arc::new(vec![0; 40]), 100);
        cache.insert(key("/d/b", 1), Arc::new(vec![0; 40]), 100);
        assert!(cache.get(&key("/d/a", 1)).is_some());
        cache.insert(key("/d/c", 1), Arc::new(vec![0; 40]), 100);
        assert!(cache.get(&key("/d/a", 1)).is_none(), "oldest evicted past the cap");
        assert_eq!(cache.bytes, 80);
        // The same file, modified: the old thumbnail goes, the new one is kept.
        cache.insert(key("/d/b", 2), Arc::new(vec![0; 10]), 100);
        assert!(cache.get(&key("/d/b", 1)).is_none() && cache.get(&key("/d/b", 2)).is_some());
        assert_eq!(cache.bytes, 50);
        cache.insert(key("/d/huge", 1), Arc::new(vec![0; 101]), 100);
        assert!(cache.get(&key("/d/huge", 1)).is_none(), "larger than the whole cache: not kept");
    }

    /// QuickLook renders a thumbnail for a real file (read-only; the file is in a temp dir).
    #[cfg(target_os = "macos")]
    #[test]
    fn quicklook_thumbnail_is_a_png() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("note.txt");
        fs::write(&file, "Hello from a test.\n").unwrap();
        let png = ql::thumbnail_png(&file, 64.0, 2.0).expect("QuickLook answers for a text file");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }
}
