//! Path guard. See ARCHITECTURE.md §6.2.
//!
//! Every path the app is about to trash or delete goes through [`check_target`] right before the
//! action. The guard never follows the final path component: a symlink is judged (and later
//! trashed) as the link itself. Only the parent directory is canonicalized, so `..` tricks and
//! symlinked parents cannot smuggle a target out of `$HOME` or onto a protected location.

use std::ffi::OsStr;
use std::fs::{self, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use crate::error::CoreError;

/// Locations (relative to `$HOME`; `""` is `$HOME` itself) that may never be a target and never be
/// inside a target. Removing things *inside* them is fine. The last entry is the app's default
/// `data_dir`; callers with a different `data_dir` pass it to [`check_target_with`].
pub const PROTECTED_RELATIVE: &[&str] = &[
    "",
    "Documents",
    "Desktop",
    "Downloads",
    "Developer",
    "Library",
    "Library/Application Support",
    "Library/Keychains",
    "Library/Mobile Documents",
    ".ssh",
    ".gnupg",
    "Library/Application Support/clean-my-mac",
];

/// Returns the canonical target if it may be trashed/deleted, otherwise `CoreError::Guard`
/// (`CoreError::NotFound` if the target does not exist).
///
/// The returned path is the canonical parent joined with the original file name, i.e. it still
/// names the symlink itself when the target is a symlink.
pub fn check_target(path: &Path, home: &Path) -> Result<PathBuf, CoreError> {
    check_target_with(path, home, &[])
}

/// [`check_target`] with additional protected absolute paths (e.g. a non-default `data_dir`).
pub fn check_target_with(path: &Path, home: &Path, extra_protected: &[PathBuf]) -> Result<PathBuf, CoreError> {
    check(path, home, extra_protected, GitEntries::None)
}

/// [`check_target_with`] for a linked git worktree (a `git-worktree-remove` finding): its own
/// top-level `.git` **file** (the gitdir pointer) is expected; any other `.git` entry inside it
/// (a nested repository, a submodule's `.git` file) is still refused.
pub fn check_worktree_target_with(path: &Path, home: &Path, extra_protected: &[PathBuf]) -> Result<PathBuf, CoreError> {
    check(path, home, extra_protected, GitEntries::OwnWorktreeFile)
}

/// Which `.git` entries a target may contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitEntries {
    /// None at all (safeguard 2): a `.git` file or directory anywhere inside means a checkout.
    None,
    /// Only the target's own top-level `.git` regular file (a linked worktree).
    OwnWorktreeFile,
}

fn check(path: &Path, home: &Path, extra_protected: &[PathBuf], allowed: GitEntries) -> Result<PathBuf, CoreError> {
    let refuse = |why: &str| CoreError::Guard(format!("{}: {why}", path.display()));

    if !path.is_absolute() {
        return Err(refuse("not an absolute path"));
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(refuse("contains '..'"));
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(refuse("does not name a file or directory"));
    };
    if eq_component(name, OsStr::new(".git")) {
        return Err(refuse("is a .git entry"));
    }

    let canonical_parent = match fs::canonicalize(parent) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(CoreError::NotFound(path.display().to_string()));
        }
        Err(e) => return Err(refuse(&format!("cannot resolve parent directory: {e}"))),
    };
    // Rebuilt from components: no trailing slash, so the final symlink is never followed.
    let target = canonical_parent.join(name);
    let target_meta = match fs::symlink_metadata(&target) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(CoreError::NotFound(path.display().to_string()));
        }
        Err(e) => return Err(CoreError::Io(e)),
    };

    let home = fs::canonicalize(home).map_err(|e| refuse(&format!("cannot resolve home directory: {e}")))?;
    if same_path(&target, &home) {
        return Err(refuse("is the home directory"));
    }
    if !is_within(&target, &home) {
        return Err(refuse("outside the home directory"));
    }

    for protected in protected_paths(&home, extra_protected) {
        if same_path(&target, &protected) || is_within(&protected, &target) {
            return Err(refuse(&format!("is or contains the protected location {}", protected.display())));
        }
        if let Some(ancestor) = identity_ancestor(&target_meta, &protected) {
            return Err(refuse(&format!(
                "is the same directory as {} (protected {})",
                ancestor.display(),
                protected.display()
            )));
        }
    }

    if target_meta.is_dir() {
        find_git_entry(&target, allowed).map_err(|why| refuse(&why))?;
    }
    Ok(target)
}

/// `canonical parent + file name` for an existing or non-existing entry; `None` if the parent
/// cannot be resolved. Same normalization as [`check_target`], for comparing paths with its result.
pub fn canonical_entry(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let parent = fs::canonicalize(path.parent()?).ok()?;
    Some(parent.join(name))
}

/// Both the lexical (`home` + relative) and, when it exists, the resolved form of every protected
/// location, plus the extra paths. Resolving matters when e.g. `~/Documents` is a symlink.
fn protected_paths(home: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let lexical = PROTECTED_RELATIVE
        .iter()
        .map(|rel| if rel.is_empty() { home.to_path_buf() } else { home.join(rel) })
        .chain(extra.iter().cloned());
    for p in lexical {
        if let Ok(resolved) = fs::canonicalize(&p)
            && resolved != p
        {
            out.push(resolved);
        }
        out.push(p);
    }
    out
}

/// If the target (by device + inode, never following it) is the protected directory or one of its
/// ancestors, returns that path. Catches aliases that string comparison misses (firmlinks,
/// Unicode normalization). A symlink target has its own inode and never matches.
fn identity_ancestor(target_meta: &Metadata, protected: &Path) -> Option<PathBuf> {
    if target_meta.file_type().is_symlink() {
        return None;
    }
    let id = (target_meta.dev(), target_meta.ino());
    protected.ancestors().find_map(|a| {
        let m = fs::metadata(a).ok()?;
        ((m.dev(), m.ino()) == id).then(|| a.to_path_buf())
    })
}

/// Errors if `dir` contains a `.git` entry (directory, file or link) anywhere below it, except what
/// `allowed` permits (walk does not follow symlinks or cross devices). An unreadable subdirectory
/// is also an error: we cannot prove it holds no repository.
fn find_git_entry(dir: &Path, allowed: GitEntries) -> Result<(), String> {
    let walker = walkdir::WalkDir::new(dir).follow_links(false).same_file_system(true).min_depth(1);
    for entry in walker {
        let entry = entry.map_err(|e| format!("cannot inspect contents: {e}"))?;
        if !eq_component(entry.file_name(), OsStr::new(".git")) {
            continue;
        }
        let own_pointer = entry.depth() == 1 && entry.file_type().is_file();
        if allowed == GitEntries::OwnWorktreeFile && own_pointer {
            continue;
        }
        return Err(if entry.file_type().is_dir() {
            format!("contains a git repository ({})", entry.path().display())
        } else {
            format!("contains a .git file ({}): part of a git checkout", entry.path().display())
        });
    }
    Ok(())
}

/// APFS is case-insensitive by default: compare components ignoring case when both are valid
/// UTF-8, byte-exact otherwise.
fn eq_component(a: &OsStr, b: &OsStr) -> bool {
    if a == b {
        return true;
    }
    match (a.to_str(), b.to_str()) {
        (Some(a), Some(b)) => a.to_lowercase() == b.to_lowercase(),
        _ => false,
    }
}

fn normal_components(p: &Path) -> Vec<&OsStr> {
    p.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// Case-insensitive path equality (absolute paths).
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    let (a, b) = (normal_components(a), normal_components(b));
    a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| eq_component(x, y))
}

/// Case-insensitive "`child` is `ancestor` or below it" (absolute paths).
pub(crate) fn is_within(child: &Path, ancestor: &Path) -> bool {
    let (c, a) = (normal_components(child), normal_components(ancestor));
    c.len() >= a.len() && c.iter().zip(&a).all(|(x, y)| eq_component(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A fake home with every protected directory, plus some removable content inside them.
    fn fake_home() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        for rel in PROTECTED_RELATIVE {
            fs::create_dir_all(home.join(rel)).unwrap();
        }
        fs::create_dir_all(home.join("Library/Caches/com.example/data")).unwrap();
        fs::create_dir_all(home.join("Downloads/old")).unwrap();
        fs::write(home.join("Downloads/old.dmg"), b"x").unwrap();
        fs::create_dir_all(home.join("Developer/proj/node_modules/pkg")).unwrap();
        fs::create_dir_all(home.join("Developer/proj/.git")).unwrap();
        fs::create_dir_all(home.join("Developer/proj2/node_modules")).unwrap();
        fs::create_dir_all(home.join(".npm/_cacache")).unwrap();
        fs::create_dir_all(tmp.path().join("outside/data")).unwrap();
        (tmp, home)
    }

    fn check(home: &Path, rel: &str) -> Result<PathBuf, CoreError> {
        check_target(&PathBuf::from(format!("{}/{rel}", home.display())), home)
    }

    #[test]
    fn every_protected_location_is_refused() {
        let (_tmp, home) = fake_home();
        assert!(matches!(check_target(&home, &home), Err(CoreError::Guard(_))), "home itself");
        for rel in PROTECTED_RELATIVE.iter().filter(|r| !r.is_empty()) {
            let r = check(&home, rel);
            assert!(matches!(r, Err(CoreError::Guard(_))), "{rel} must be refused, got {r:?}");
            let r = check(&home, &format!("{rel}/"));
            assert!(matches!(r, Err(CoreError::Guard(_))), "{rel}/ must be refused, got {r:?}");
        }
    }

    #[test]
    fn ancestors_of_protected_locations_are_refused() {
        let (_tmp, home) = fake_home();
        // ~/Library contains ~/Library/Keychains etc.; the tempdir parent contains home.
        assert!(matches!(check(&home, "Library"), Err(CoreError::Guard(_))));
        assert!(matches!(check_target(home.parent().unwrap(), &home), Err(CoreError::Guard(_))));
        assert!(matches!(check_target(Path::new("/"), &home), Err(CoreError::Guard(_))));
    }

    #[test]
    fn content_inside_protected_locations_is_allowed() {
        let (_tmp, home) = fake_home();
        let canonical_home = fs::canonicalize(&home).unwrap();
        for rel in [
            "Library/Caches/com.example",
            "Library/Caches/com.example/data",
            "Downloads/old",
            "Downloads/old.dmg",
            "Developer/proj/node_modules",
            "Developer/proj2",
            ".npm/_cacache",
        ] {
            let got = check(&home, rel).unwrap_or_else(|e| panic!("{rel} should be allowed: {e}"));
            assert_eq!(got, canonical_home.join(rel));
        }
        assert_eq!(check(&home, "Downloads/old/").unwrap(), canonical_home.join("Downloads/old"), "trailing slash");
        assert_eq!(check(&home, "Downloads/./old").unwrap(), canonical_home.join("Downloads/old"), "dot component");
    }

    #[test]
    fn dot_dot_tricks_are_refused() {
        let (_tmp, home) = fake_home();
        for rel in [
            "Library/Caches/../../Documents",
            "Library/Caches/..",
            "Library/Caches/com.example/../../../Desktop/",
            "Developer/proj/node_modules/../../../../outside/data",
            "..",
        ] {
            let r = check(&home, rel);
            assert!(matches!(r, Err(CoreError::Guard(_))), "{rel} must be refused, got {r:?}");
        }
    }

    #[test]
    fn relative_and_missing_paths() {
        let (_tmp, home) = fake_home();
        assert!(matches!(check_target(Path::new("Library/Caches"), &home), Err(CoreError::Guard(_))));
        assert!(matches!(check_target(Path::new(""), &home), Err(CoreError::Guard(_))));
        assert!(matches!(check(&home, "Library/Caches/nope"), Err(CoreError::NotFound(_))));
        assert!(matches!(check(&home, "nope/deeper"), Err(CoreError::NotFound(_))));
    }

    #[test]
    fn outside_home_is_refused() {
        let (tmp, home) = fake_home();
        let r = check_target(&tmp.path().join("outside/data"), &home);
        assert!(matches!(r, Err(CoreError::Guard(ref m)) if m.contains("outside")), "{r:?}");
        // A sibling whose name merely starts with "home" is not inside home.
        fs::create_dir_all(tmp.path().join("home2/x")).unwrap();
        assert!(matches!(check_target(&tmp.path().join("home2/x"), &home), Err(CoreError::Guard(_))));
    }

    #[test]
    fn symlinks_are_judged_as_links_never_followed() {
        let (tmp, home) = fake_home();
        let canonical_home = fs::canonicalize(&home).unwrap();
        // A link inside home pointing outside home: the link itself may go, the target is untouched.
        symlink(tmp.path().join("outside/data"), home.join("Library/Caches/out-link")).unwrap();
        assert_eq!(check(&home, "Library/Caches/out-link").unwrap(), canonical_home.join("Library/Caches/out-link"));
        assert_eq!(
            check(&home, "Library/Caches/out-link/").unwrap(),
            canonical_home.join("Library/Caches/out-link"),
            "trailing slash must not resolve the link"
        );
        // A link pointing at a protected directory: removing the link is harmless.
        symlink(home.join("Documents"), home.join("Library/Caches/docs-link")).unwrap();
        assert!(check(&home, "Library/Caches/docs-link").is_ok());
        // But going *through* a symlinked parent resolves to the real location.
        symlink(tmp.path().join("outside"), home.join("Library/Caches/out-dir")).unwrap();
        let r = check(&home, "Library/Caches/out-dir/data");
        assert!(matches!(r, Err(CoreError::Guard(ref m)) if m.contains("outside")), "{r:?}");
        symlink(&home, home.join("Library/Caches/home-link")).unwrap();
        assert!(matches!(check(&home, "Library/Caches/home-link/Documents"), Err(CoreError::Guard(_))));
        assert!(matches!(check(&home, "Library/Caches/home-link/Library"), Err(CoreError::Guard(_))));
    }

    #[test]
    fn symlinked_home_is_resolved() {
        let (tmp, home) = fake_home();
        let link_home = tmp.path().join("link-home");
        symlink(&home, &link_home).unwrap();
        assert!(check(&link_home, "Library/Caches/com.example").is_ok());
        assert!(matches!(check(&link_home, "Documents"), Err(CoreError::Guard(_))));
        assert!(check_target(&home.join("Downloads/old"), &link_home).is_ok());
    }

    #[test]
    fn symlinked_protected_dir_is_refused_by_both_spellings() {
        let (tmp, home) = fake_home();
        // ~/Documents is a symlink to a directory outside home (e.g. iCloud Drive).
        fs::remove_dir(home.join("Documents")).unwrap();
        fs::create_dir_all(tmp.path().join("cloud/Documents")).unwrap();
        symlink(tmp.path().join("cloud/Documents"), home.join("Documents")).unwrap();
        assert!(matches!(check(&home, "Documents"), Err(CoreError::Guard(_))));
    }

    #[test]
    fn git_directories_are_refused() {
        let (_tmp, home) = fake_home();
        let r = check(&home, "Developer/proj/.git");
        assert!(matches!(r, Err(CoreError::Guard(ref m)) if m.contains(".git")), "{r:?}");
        let r = check(&home, "Developer/proj");
        assert!(matches!(r, Err(CoreError::Guard(ref m)) if m.contains("git repository")), "{r:?}");
        // Deep inside, too.
        fs::create_dir_all(home.join("Library/Caches/com.example/data/vendor/lib/.git")).unwrap();
        assert!(matches!(check(&home, "Library/Caches/com.example"), Err(CoreError::Guard(_))));
        // A `.git` *file* (linked worktree, submodule) also marks a checkout: refused by the plain
        // check, allowed only as a worktree target's own top-level pointer.
        fs::write(home.join("Developer/proj2/.git"), "gitdir: /elsewhere").unwrap();
        let r = check(&home, "Developer/proj2");
        assert!(matches!(r, Err(CoreError::Guard(ref m)) if m.contains("contains a .git file")), "{r:?}");
        assert!(check_worktree_target_with(&home.join("Developer/proj2"), &home, &[]).is_ok());
        // ...but the file itself is never a target.
        assert!(matches!(check(&home, "Developer/proj2/.git"), Err(CoreError::Guard(_))));
        assert!(check_worktree_target_with(&home.join("Developer/proj2/.git"), &home, &[]).is_err());
        // A nested `.git` file (a submodule inside the worktree) is refused even for worktrees.
        fs::create_dir_all(home.join("Developer/proj2/lib")).unwrap();
        fs::write(home.join("Developer/proj2/lib/.git"), "gitdir: ../.git/modules/lib").unwrap();
        let r = check_worktree_target_with(&home.join("Developer/proj2"), &home, &[]);
        assert!(matches!(r, Err(CoreError::Guard(ref m)) if m.contains("lib/.git")), "{r:?}");
        // A worktree target whose top-level `.git` is a directory is a full clone: refused.
        let r = check_worktree_target_with(&home.join("Developer/proj"), &home, &[]);
        assert!(matches!(r, Err(CoreError::Guard(ref m)) if m.contains("git repository")), "{r:?}");
        // A `.git` symlink is not a plain pointer file either.
        fs::create_dir_all(home.join("Library/Caches/linked")).unwrap();
        symlink(home.join("Developer/proj/.git"), home.join("Library/Caches/linked/.git")).unwrap();
        assert!(check(&home, "Library/Caches/linked").is_err());
        assert!(check_worktree_target_with(&home.join("Library/Caches/linked"), &home, &[]).is_err());
    }

    #[test]
    fn case_insensitive_names_match_protected_locations() {
        let (_tmp, home) = fake_home();
        if !home.join("downloads").exists() {
            return; // case-sensitive volume: different names are different directories
        }
        for rel in ["downloads", "DOWNLOADS", "library", "Library/application support", ".SSH", "developer/"] {
            let r = check(&home, rel);
            assert!(matches!(r, Err(CoreError::Guard(_))), "{rel} must be refused, got {r:?}");
        }
        assert!(matches!(check(&home, "developer/PROJ/.GIT"), Err(CoreError::Guard(_))));
        assert!(check(&home, "library/caches/com.example").is_ok());
    }

    #[test]
    fn extra_protected_paths() {
        let (_tmp, home) = fake_home();
        fs::create_dir_all(home.join(".config/cmm-data/sub")).unwrap();
        let data_dir = home.join(".config/cmm-data");
        let check_with = |rel: &str| check_target_with(&home.join(rel), &home, std::slice::from_ref(&data_dir));
        assert!(matches!(check_with(".config/cmm-data"), Err(CoreError::Guard(_))));
        assert!(matches!(check_with(".config"), Err(CoreError::Guard(_))));
        assert!(check_with(".config/cmm-data/sub").is_ok());
    }

    #[test]
    fn path_helpers() {
        assert!(same_path(Path::new("/a/B/c/"), Path::new("/A/b/c")));
        assert!(is_within(Path::new("/a/b/c"), Path::new("/A/B")));
        assert!(is_within(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!is_within(Path::new("/a/bc"), Path::new("/a/b")));
        assert!(!is_within(Path::new("/a"), Path::new("/a/b")));
    }
}
