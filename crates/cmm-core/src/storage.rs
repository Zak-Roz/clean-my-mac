//! Storage breakdown (ARCHITECTURE.md §11): the startup disk's used space split into parts that add
//! up to the Overview's "used" (`total - free`), with drill-down into folders. Read-only: nothing
//! here writes, moves or deletes anything.
//!
//! On APFS the startup disk is a container shared by several volumes, and `statfs` gives each
//! mounted volume's own used space. So the system volumes (the sealed System volume, VM with the
//! swap files, Preboot, Recovery, Update) are sized exactly without walking them. The volume that
//! holds the home folder (the Data volume, mounted at /System/Volumes/Data) is walked from its
//! root with [`Sizer::size_tree`]; its top-level entries are reached through their firmlinks
//! (/Users, /Applications, /Library, …) so paths match what Finder and the scan's findings show.
//! What the walk cannot see (local snapshots, folders only macOS, an administrator or Full Disk
//! Access can read, file-system metadata) is the "Not measured" remainder: the Data volume's used
//! space minus what was walked.
//!
//! Top level: macOS (system volumes), /Applications, ~/Library and the other home folders, "Other
//! in home" (hidden folders, loose files, small folders), "Other users & Shared", "System data"
//! (the rest of the Data volume: /Library, /private, /opt, …) and "Not measured".
//!
//! Findings of the last scan are matched to the nodes ([`annotate`]): every finding (de-duplicated
//! like the Overview's "Found by last scan") is placed in the deepest node that contains it, and a
//! node whose path is exactly a finding's path is wholly found. `found_bytes` never exceeds a
//! node's own size, so the map never claims more than it shows.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use rayon::prelude::*;

use crate::detectors::external;
use crate::model::{
    Finding, ScanError, ScanReport, StorageBreakdown, StorageKind, StorageNode, StoragePhase, StorageProgress,
    Unreadable,
};
use crate::size::{Rest, SizeTree, Sizer, TreeOptions};
use crate::volume::{self, Mount};

/// Entries allocating less than this are summed into "N smaller items" in the breakdown.
pub const MIN_NODE_BYTES: u64 = 50_000_000;
/// At most this many children are kept per folder in the breakdown.
pub const MAX_CHILDREN: usize = 200;
/// [`children`] lists at most this many entries of one folder.
pub const LIST_LIMIT: usize = 1000;

/// What the breakdown keeps of the walk: every entry of at least [`MIN_NODE_BYTES`], at any depth.
pub const BREAKDOWN_TREE: TreeOptions =
    TreeOptions { max_depth: u32::MAX, min_bytes: MIN_NODE_BYTES, max_children: MAX_CHILDREN };

const FIRMLINKS: &str = "/usr/share/firmlinks";
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// Everything the breakdown needs to know about the machine, detected once ([`StorageEnv::detect`])
/// or built by hand in tests.
#[derive(Debug, Clone, Default)]
pub struct StorageEnv {
    pub home: PathBuf,
    /// Mount point of the volume holding `home` (the APFS Data volume: /System/Volumes/Data).
    pub data_root: PathBuf,
    /// That volume's own used space (on APFS: this volume alone, not the whole container).
    pub data_used_bytes: u64,
    /// Data-root entry name -> the path it is reached by ("Users" -> "/Users").
    pub firmlinks: HashMap<OsString, PathBuf>,
    /// Container size and free space: the same numbers as `VolumeInfo`.
    pub total_bytes: u64,
    pub free_bytes: u64,
    /// The other mounted volumes of the same APFS container (System, VM, Preboot, …).
    pub volumes: Vec<Mount>,
    pub swap_used_bytes: Option<u64>,
    pub local_snapshots: Vec<String>,
    pub full_disk_access: bool,
    /// Problems met while detecting the above (`tmutil` failing, …).
    pub errors: Vec<ScanError>,
}

impl StorageEnv {
    /// Reads the real machine: `statfs` of `home`, the mounted volumes of its APFS container,
    /// `/usr/share/firmlinks`, swap usage and `tmutil listlocalsnapshots /` (read-only).
    pub fn detect(home: &Path) -> Self {
        let mut env = StorageEnv {
            home: home.to_path_buf(),
            data_root: PathBuf::from("/"),
            swap_used_bytes: volume::swap_used_bytes(),
            full_disk_access: volume::has_full_disk_access(home),
            ..StorageEnv::default()
        };
        let Some(mount) = volume::mount_of(home) else {
            env.errors.push(ScanError { path: home.to_string_lossy().into_owned(), message: "statfs failed".into() });
            return env;
        };
        env.total_bytes = mount.total_bytes;
        env.free_bytes = mount.free_bytes;
        env.data_root = mount.on.clone();
        if let Some(container) = mount.apfs_container() {
            env.data_used_bytes = mount.used_bytes;
            let mut seen = HashSet::from([mount.from.clone()]);
            env.volumes = volume::mounts()
                .into_iter()
                .filter(|m| m.apfs_container() == Some(container) && seen.insert(m.from.clone()))
                .collect();
        } else {
            // Not APFS: one file system, everything used is ours to explain.
            env.data_used_bytes = mount.total_bytes.saturating_sub(mount.free_bytes);
        }
        if env.data_root != Path::new("/") {
            env.firmlinks = std::fs::read_to_string(FIRMLINKS).map(|text| parse_firmlinks(&text)).unwrap_or_default();
        }
        match local_snapshots() {
            Ok(names) => env.local_snapshots = names,
            Err(message) => env.errors.push(ScanError { path: "tmutil listlocalsnapshots /".into(), message }),
        }
        env
    }

    /// `total - free`: the Overview's used space.
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }
}

/// `/usr/share/firmlinks` lines ("/Users\tUsers") -> top-level Data-volume entry -> visible path.
fn parse_firmlinks(text: &str) -> HashMap<OsString, PathBuf> {
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(visible, data)| visible.starts_with('/') && !data.is_empty() && !data.contains('/'))
        .map(|(visible, data)| (OsString::from(data.trim()), PathBuf::from(visible.trim())))
        .collect()
}

/// Names of the local Time Machine snapshots of the startup disk.
fn local_snapshots() -> Result<Vec<String>, String> {
    let out = external::run("tmutil", &["listlocalsnapshots", "/"]).map_err(|e| e.to_string())?;
    if !out.success() {
        return Err(out.stderr.trim().to_string());
    }
    Ok(parse_snapshots(&out.stdout))
}

fn parse_snapshots(stdout: &str) -> Vec<String> {
    stdout.lines().map(str::trim).filter(|l| !l.is_empty() && !l.ends_with(':')).map(String::from).collect()
}

/// Breakdown of the real startup disk. Walks the whole Data volume (tens of seconds); reports
/// progress every 250 ms. `report` sets the found-by-scan marks.
pub fn breakdown(
    home: &Path,
    report: Option<&ScanReport>,
    on_progress: &(dyn Fn(StorageProgress) + Sync),
) -> StorageBreakdown {
    on_progress(StorageProgress { phase: StoragePhase::Volumes, walked_bytes: 0, expected_bytes: 0 });
    breakdown_with(&StorageEnv::detect(home), report, &BREAKDOWN_TREE, on_progress)
}

/// [`breakdown`] for a given environment (tests use a temp dir as the Data volume).
pub fn breakdown_with(
    env: &StorageEnv,
    report: Option<&ScanReport>,
    opts: &TreeOptions,
    on_progress: &(dyn Fn(StorageProgress) + Sync),
) -> StorageBreakdown {
    let started = Instant::now();
    let measured_at = Utc::now();
    let sizer = Sizer::new();
    let plan = Plan::new(env, &sizer);
    let expected = env.data_used_bytes;
    let trees: Vec<SizeTree> = with_ticker(&sizer, expected, on_progress, || {
        plan.parts.par_iter().map(|part| sizer.size_tree(&part.path, opts)).collect()
    });
    on_progress(StorageProgress {
        phase: StoragePhase::Done,
        walked_bytes: sizer.visited_bytes(),
        expected_bytes: expected,
    });

    let walked = trees.iter().map(|t| t.info.bytes).sum::<u64>() + plan.own_bytes();
    let mut nodes = plan.assemble(env, trees, opts);
    let mut volumes_bytes = 0;
    for node in volume_nodes(env) {
        volumes_bytes += node.bytes;
        nodes.push(node);
    }
    let unmeasured = env.data_used_bytes.saturating_sub(walked);
    if unmeasured > 0 {
        let node = unmeasured_node(env, unmeasured, &nodes);
        nodes.push(node);
    }
    // A Volume of the container that is not macOS stands on its own; the rest is grouped.
    let (system, mut top): (Vec<_>, Vec<_>) =
        nodes.into_iter().partition(|n| n.kind == StorageKind::Volume && n.id.starts_with("@macos"));
    if let Some(group) = group("@macos", "macOS", NOTE_MACOS, system) {
        top.push(group);
    }
    sort_nodes(&mut top);

    let mut errors = env.errors.clone();
    errors.extend(sizer.take_errors());
    let mut breakdown = StorageBreakdown {
        measured_at,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        scan_id: None,
        total_bytes: env.total_bytes,
        free_bytes: env.free_bytes,
        used_bytes: env.used_bytes(),
        walked_bytes: walked,
        volumes_bytes,
        unmeasured_bytes: unmeasured,
        overcount_bytes: walked.saturating_sub(env.data_used_bytes),
        full_disk_access: env.full_disk_access,
        local_snapshots: env.local_snapshots.clone(),
        found_bytes: 0,
        found_located_bytes: 0,
        nodes: top,
        errors,
    };
    annotate(&mut breakdown, report);
    breakdown
}

/// The entries of `path`, sized (all of them up to [`LIST_LIMIT`], biggest first), for drilling
/// below what the breakdown kept. Their own children are not loaded (`entries` says how many).
pub fn children(path: &Path, report: Option<&ScanReport>) -> (StorageNode, Vec<ScanError>) {
    let sizer = Sizer::new();
    let tree = sizer.size_tree(path, &TreeOptions { max_depth: 1, min_bytes: 0, max_children: LIST_LIMIT });
    let label = tree.name.to_string_lossy().into_owned();
    let mut node = tree_node(tree, path, label);
    annotate_nodes(std::slice::from_mut(&mut node), report.map_or(&[][..], |r| &r.findings));
    (node, sizer.take_errors())
}

/// Runs `work` while a ticker thread reports the walk's progress.
fn with_ticker<T>(
    sizer: &Sizer,
    expected: u64,
    on_progress: &(dyn Fn(StorageProgress) + Sync),
    work: impl FnOnce() -> T,
) -> T {
    let done = AtomicBool::new(false);
    let tick = || StorageProgress {
        phase: StoragePhase::Walking,
        walked_bytes: sizer.visited_bytes(),
        expected_bytes: expected,
    };
    on_progress(tick());
    std::thread::scope(|scope| {
        let ticker = scope.spawn(|| {
            loop {
                std::thread::park_timeout(PROGRESS_EVERY);
                if done.load(Ordering::Relaxed) {
                    break;
                }
                on_progress(tick());
            }
        });
        let out = work();
        done.store(true, Ordering::Relaxed);
        ticker.thread().unpark();
        out
    })
}

/// Where a walked part of the Data volume is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Applications,
    /// A visible folder of the home folder: its own top-level node.
    HomeFolder,
    /// Hidden entries and loose files of the home folder.
    HomeOther,
    /// Other accounts' home folders, /Users/Shared.
    OtherUsers,
    /// Everything else on the Data volume.
    SystemData,
}

#[derive(Debug)]
struct Part {
    path: PathBuf,
    place: Place,
}

/// The Data volume cut into parts that are walked separately (in parallel) and then regrouped.
#[derive(Debug, Default)]
struct Plan {
    parts: Vec<Part>,
    /// The directory entries themselves of the Data root, the folders above home and home.
    root_own: u64,
    users_own: u64,
    home_own: u64,
    /// The folder holding the other accounts (/Users), for their "smaller items".
    users_dir: Option<PathBuf>,
}

impl Plan {
    fn new(env: &StorageEnv, sizer: &Sizer) -> Plan {
        let mut plan = Plan::default();
        let Ok(meta) = env.data_root.symlink_metadata() else {
            sizer.push_error(env.data_root.to_string_lossy(), "cannot read the volume root");
            return plan;
        };
        plan.root_own = meta.blocks() * 512;
        for (name, is_dir) in list(&env.data_root, meta.dev(), sizer) {
            let path = env.firmlinks.get(&name).cloned().unwrap_or_else(|| env.data_root.join(&name));
            if path == env.home {
                plan.carve_home(&path, sizer);
            } else if is_dir && env.home.starts_with(&path) {
                plan.carve_above_home(&path, &env.home, sizer);
            } else if name == "Applications" {
                plan.parts.push(Part { path, place: Place::Applications });
            } else {
                plan.parts.push(Part { path, place: Place::SystemData });
            }
        }
        plan
    }

    /// `dir` is a strict ancestor of `home` (/Users): its other entries belong to other users.
    fn carve_above_home(&mut self, dir: &Path, home: &Path, sizer: &Sizer) {
        let Ok(meta) = dir.symlink_metadata() else { return };
        self.users_own += meta.blocks() * 512;
        self.users_dir.get_or_insert_with(|| dir.to_path_buf());
        for (name, is_dir) in list(dir, meta.dev(), sizer) {
            let path = dir.join(name);
            if path == home {
                self.carve_home(&path, sizer);
            } else if is_dir && home.starts_with(&path) {
                self.carve_above_home(&path, home, sizer);
            } else {
                self.parts.push(Part { path, place: Place::OtherUsers });
            }
        }
    }

    fn carve_home(&mut self, home: &Path, sizer: &Sizer) {
        let Ok(meta) = home.symlink_metadata() else { return };
        self.home_own = meta.blocks() * 512;
        for (name, is_dir) in list(home, meta.dev(), sizer) {
            let visible = is_dir && !name.to_string_lossy().starts_with('.');
            let place = if visible { Place::HomeFolder } else { Place::HomeOther };
            self.parts.push(Part { path: home.join(name), place });
        }
    }

    fn own_bytes(&self) -> u64 {
        self.root_own + self.users_own + self.home_own
    }

    /// Regroups the walked parts (`trees[i]` is `parts[i]`) into top-level nodes.
    fn assemble(&self, env: &StorageEnv, trees: Vec<SizeTree>, opts: &TreeOptions) -> Vec<StorageNode> {
        let mut top = Vec::new();
        let mut home_other = Grouping::new(env.home.clone(), self.home_own);
        let mut users =
            Grouping::new(self.users_dir.clone().unwrap_or_else(|| env.data_root.join("Users")), self.users_own);
        let mut system = Grouping::new(env.data_root.clone(), self.root_own);
        for (part, tree) in self.parts.iter().zip(trees) {
            let label = match part.place {
                Place::HomeFolder | Place::HomeOther => format!("~/{}", tree.name.to_string_lossy()),
                _ => part.path.to_string_lossy().into_owned(),
            };
            let node = tree_node(tree, &part.path, label);
            let keep = node.bytes >= opts.min_bytes || node.unreadable.is_some();
            match part.place {
                Place::Applications => top.push(node),
                Place::HomeFolder if keep => top.push(node),
                Place::HomeFolder | Place::HomeOther => home_other.add(node, keep),
                Place::OtherUsers => users.add(node, keep),
                Place::SystemData => system.add(node, keep),
            }
        }
        top.extend(home_other.finish("@home-other", "Other in home", NOTE_HOME_OTHER));
        top.extend(users.finish("@users", "Other users & Shared", NOTE_USERS));
        top.extend(system.finish("@system-data", "System data", NOTE_SYSTEM_DATA));
        top
    }
}

const NOTE_MACOS: &str = "The sealed, read-only macOS system, its boot and recovery volumes, swap and the sleep image. \
Sized per APFS volume; managed by macOS.";
const NOTE_HOME_OTHER: &str = "Hidden folders in your home (~/.npm, ~/.cargo, ~/.docker, the Trash, …), loose files and \
small folders.";
const NOTE_USERS: &str =
    "Other accounts' home folders and /Users/Shared. Other accounts can only be read by an administrator.";
const NOTE_SYSTEM_DATA: &str = "The rest of the Data volume: /Library (shared app data, simulators), /private (logs, \
temporary files, system databases), /opt (Homebrew) and macOS indexes.";

/// The entries of `dir` on device `dev`: `(name, is a real directory)`. Unreadable → error.
fn list(dir: &Path, dev: u64, sizer: &Sizer) -> Vec<(OsString, bool)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            sizer.push_error(dir.to_string_lossy(), e.to_string());
            return Vec::new();
        }
    };
    let mut out: Vec<(OsString, bool)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            // lstat: a symlink is never a directory, a mount point is another device.
            let meta = e.metadata().ok()?;
            (meta.dev() == dev).then(|| (e.file_name(), meta.is_dir()))
        })
        .collect();
    out.sort();
    out
}

/// Nodes collected into one group; small ones are summed into its "smaller items", whose path is
/// the folder they come from (so findings inside them are placed there).
struct Grouping {
    rest_path: PathBuf,
    children: Vec<StorageNode>,
    /// Summed small nodes; starts with the directory entries' own bytes (no entry of its own).
    rest: Rest,
}

impl Grouping {
    fn new(rest_path: PathBuf, own_bytes: u64) -> Self {
        Grouping { rest_path, children: Vec::new(), rest: Rest { bytes: own_bytes, ..Rest::default() } }
    }

    fn add(&mut self, node: StorageNode, keep: bool) {
        if keep {
            self.children.push(node);
        } else {
            self.rest.entries += 1;
            self.rest.bytes += node.bytes;
            self.rest.files += node.files;
            self.rest.newest_mtime = self.rest.newest_mtime.max(node.modified);
        }
    }

    fn finish(mut self, id: &str, label: &str, note: &str) -> Option<StorageNode> {
        if self.rest.entries > 0 || (self.rest.bytes > 0 && !self.children.is_empty()) {
            let mut rest = rest_node(&self.rest_path, &self.rest);
            rest.id = format!("{id}/@rest");
            self.children.push(rest);
        }
        group(id, label, note, self.children)
    }
}

/// A group node over `children` (sorted), or None when there is nothing in it.
fn group(id: &str, label: &str, note: &str, mut children: Vec<StorageNode>) -> Option<StorageNode> {
    if children.is_empty() {
        return None;
    }
    sort_nodes(&mut children);
    Some(StorageNode {
        id: id.to_string(),
        label: label.to_string(),
        path: None,
        kind: StorageKind::Group,
        bytes: children.iter().map(|c| c.bytes).sum(),
        files: children.iter().map(|c| c.files).sum(),
        entries: children.len() as u64,
        modified: children.iter().filter_map(|c| c.modified).max(),
        unreadable: merge_unreadable(children.iter().map(|c| c.unreadable)),
        found_bytes: 0,
        note: Some(note.to_string()),
        children,
    })
}

fn merge_unreadable(flags: impl Iterator<Item = Option<Unreadable>>) -> Option<Unreadable> {
    flags.flatten().max_by_key(|u| match u {
        Unreadable::Protected => 1,
        Unreadable::Denied => 0,
    })
}

/// Biggest first, ties by label; `rest` nodes always last.
fn sort_nodes(nodes: &mut [StorageNode]) {
    nodes.sort_by(|a, b| {
        (a.kind == StorageKind::Rest)
            .cmp(&(b.kind == StorageKind::Rest))
            .then(b.bytes.cmp(&a.bytes))
            .then_with(|| a.label.cmp(&b.label))
    });
}

/// A walked tree as storage nodes; `path` is where it was reached (firmlinks resolved).
fn tree_node(tree: SizeTree, path: &Path, label: String) -> StorageNode {
    let mut children: Vec<StorageNode> = tree
        .children
        .into_iter()
        .map(|child| {
            let child_path = path.join(&child.name);
            let child_label = child.name.to_string_lossy().into_owned();
            tree_node(child, &child_path, child_label)
        })
        .collect();
    if tree.rest.entries > 0 {
        children.push(rest_node(path, &tree.rest));
    }
    StorageNode {
        id: path.to_string_lossy().into_owned(),
        label,
        path: Some(path.to_string_lossy().into_owned()),
        kind: if tree.is_dir { StorageKind::Folder } else { StorageKind::File },
        bytes: tree.info.bytes,
        files: tree.info.files,
        entries: tree.entries,
        modified: tree.info.newest_mtime,
        unreadable: tree.unreadable,
        found_bytes: 0,
        note: None,
        children,
    }
}

fn rest_node(parent: &Path, rest: &Rest) -> StorageNode {
    let entries = rest.entries;
    StorageNode {
        id: format!("{}/@rest", parent.to_string_lossy()),
        label: match entries {
            0 => String::from("Folder entries"),
            1 => String::from("1 smaller item"),
            n => format!("{n} smaller items"),
        },
        path: Some(parent.to_string_lossy().into_owned()),
        kind: StorageKind::Rest,
        bytes: rest.bytes,
        files: rest.files,
        entries,
        modified: rest.newest_mtime,
        unreadable: None,
        found_bytes: 0,
        note: None,
        children: Vec::new(),
    }
}

/// The container's other volumes: macOS ones get an "@macos…" id (grouped by the caller), any
/// other volume stands on its own. Plus the container space no mounted volume accounts for.
fn volume_nodes(env: &StorageEnv) -> Vec<StorageNode> {
    let mut nodes: Vec<StorageNode> = env
        .volumes
        .iter()
        .map(|m| {
            let (id, label, note) = describe_volume(m, env.swap_used_bytes);
            StorageNode {
                id,
                label,
                path: Some(m.on.to_string_lossy().into_owned()),
                kind: StorageKind::Volume,
                bytes: m.used_bytes,
                files: 0,
                entries: 0,
                modified: None,
                unreadable: None,
                found_bytes: 0,
                note,
                children: Vec::new(),
            }
        })
        .collect();
    let accounted = env.data_used_bytes + nodes.iter().map(|n| n.bytes).sum::<u64>();
    let container = env.used_bytes().saturating_sub(accounted);
    if !env.volumes.is_empty() && container > 0 {
        nodes.push(StorageNode {
            id: String::from("@macos/container"),
            label: String::from("Other container space"),
            path: None,
            kind: StorageKind::Volume,
            bytes: container,
            files: 0,
            entries: 0,
            modified: None,
            unreadable: None,
            found_bytes: 0,
            note: Some(String::from("APFS volumes that are not mounted (such as Recovery) and container metadata.")),
            children: Vec::new(),
        });
    }
    nodes
}

fn describe_volume(m: &Mount, swap: Option<u64>) -> (String, String, Option<String>) {
    let name =
        m.on.file_name().map_or_else(|| m.on.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
    let id = format!("@macos:{}", m.on.to_string_lossy());
    let system = m.on.starts_with("/System/Volumes");
    match (m.on.to_str(), name.as_str()) {
        (Some("/"), _) => (id, "System".into(), Some("The sealed, read-only macOS system volume.".into())),
        (_, "VM") if system => {
            let swap = swap.map_or_else(String::new, |s| format!(" Swap in use: {}.", fmt_gb(s)));
            (
                id,
                "Swap & sleep image".into(),
                Some(format!("Swap files and the sleep image (VM volume).{swap} A restart shrinks it.")),
            )
        }
        (_, "Preboot") if system => {
            (id, "Preboot".into(), Some("Boot files and system extensions for this macOS.".into()))
        }
        (_, "Recovery") => (id, "Recovery".into(), Some("macOS Recovery.".into())),
        (_, "Update") if system => (id, "Update".into(), Some("Staged macOS updates.".into())),
        _ if system => (id, name, None),
        _ => (
            format!("@volume:{}", m.on.to_string_lossy()),
            format!("Volume “{name}”"),
            Some(String::from("Another volume in the same APFS container: it shares the disk's free space.")),
        ),
    }
}

fn fmt_gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn unmeasured_node(env: &StorageEnv, bytes: u64, nodes: &[StorageNode]) -> StorageNode {
    let mut causes = Vec::new();
    if !env.local_snapshots.is_empty() {
        let n = env.local_snapshots.len();
        causes.push(format!("{n} local Time Machine snapshot{}", if n == 1 { "" } else { "s" }));
    }
    let any = |flag| nodes.iter().any(|n| contains_flag(n, flag));
    if any(Unreadable::Protected) {
        causes.push(String::from(if env.full_disk_access {
            "folders protected by macOS (System Integrity Protection)"
        } else {
            "folders that need Full Disk Access"
        }));
    }
    if any(Unreadable::Denied) {
        causes.push(String::from("folders only macOS or an administrator can read"));
    }
    causes.push(String::from("file-system metadata"));
    StorageNode {
        id: String::from("@unmeasured"),
        label: String::from("Not measured"),
        path: None,
        kind: StorageKind::Unmeasured,
        bytes,
        files: 0,
        entries: 0,
        modified: None,
        unreadable: None,
        found_bytes: 0,
        note: Some(format!("Used space no readable file accounts for: {}.", causes.join(", "))),
        children: Vec::new(),
    }
}

fn contains_flag(node: &StorageNode, flag: Unreadable) -> bool {
    node.unreadable == Some(flag) || node.children.iter().any(|c| contains_flag(c, flag))
}

/// (Re)sets the found-by-scan marks for `report` (None clears them). Cheap: no walking.
pub fn annotate(breakdown: &mut StorageBreakdown, report: Option<&ScanReport>) {
    let findings = report.map_or(&[][..], |r| &r.findings);
    let (total, located) = annotate_nodes(&mut breakdown.nodes, findings);
    breakdown.scan_id = report.map(|r| r.id.clone());
    // Same number as the Overview's "Found by last scan".
    breakdown.found_bytes = total.min(breakdown.used_bytes);
    breakdown.found_located_bytes = located.min(breakdown.found_bytes);
}

/// Findings counted once: path-less ones always, a path nested in (or equal to) an earlier
/// one's never (the Overview's `dedupedTotal`). Returns the located items and the path-less bytes.
fn dedupe(findings: &[Finding]) -> (Vec<(&Path, u64)>, u64) {
    let paths: Vec<&Path> = findings.iter().filter_map(|f| f.path.as_deref().map(Path::new)).collect();
    let mut seen = HashSet::new();
    let mut located = Vec::new();
    let mut pathless = 0;
    for f in findings {
        match f.path.as_deref().map(Path::new) {
            None => pathless += f.bytes,
            Some(path) => {
                let nested = paths.iter().any(|p| path != *p && path.starts_with(p));
                if !nested && seen.insert(path) {
                    located.push((path, f.bytes));
                }
            }
        }
    }
    (located, pathless)
}

#[derive(Default)]
struct Marks {
    /// Finding bytes placed in a node (the deepest one containing the finding).
    assigned: HashMap<String, u64>,
    /// Nodes whose path is exactly a finding's path: wholly found, with everything below.
    covered: HashSet<String>,
}

/// Sets `found_bytes` on `nodes` and below; returns (de-duplicated findings total, located part).
fn annotate_nodes(nodes: &mut [StorageNode], findings: &[Finding]) -> (u64, u64) {
    let (items, pathless) = dedupe(findings);
    let mut marks = Marks::default();
    for (path, bytes) in &items {
        match locate(nodes, path) {
            Some((id, true)) => {
                marks.covered.insert(id);
            }
            Some((id, false)) => *marks.assigned.entry(id).or_default() += bytes,
            None => {}
        }
    }
    let located = nodes.iter_mut().map(|n| finalize(n, &marks)).sum();
    (items.iter().map(|(_, b)| b).sum::<u64>() + pathless, located)
}

/// The deepest node containing `target`: `(id, its path is exactly target)`.
fn locate(nodes: &[StorageNode], target: &Path) -> Option<(String, bool)> {
    let best = best_container(nodes, target)?;
    if best.kind == StorageKind::Rest {
        return Some((best.id.clone(), false));
    }
    if best.path.as_deref().map(Path::new) == Some(target) {
        return Some((best.id.clone(), true));
    }
    Some(locate(&best.children, target).unwrap_or_else(|| (best.id.clone(), false)))
}

/// Among `nodes` (looking through path-less groups), the one whose path contains `target` most
/// closely. Volumes are never candidates (they are sized, not walked). A `rest` node covers its
/// folder's smaller entries, so it only wins when no kept sibling contains the target.
fn best_container<'a>(nodes: &'a [StorageNode], target: &Path) -> Option<&'a StorageNode> {
    let mut best: Option<&StorageNode> = None;
    for node in nodes {
        let candidate = match (node.kind, node.path.as_deref()) {
            (StorageKind::Volume | StorageKind::Unmeasured, _) => None,
            (_, None) => best_container(&node.children, target),
            (_, Some(path)) if target.starts_with(path) => Some(node),
            _ => None,
        };
        let Some(candidate) = candidate else { continue };
        let rank = |n: &StorageNode| {
            let depth = n.path.as_deref().map_or(0, |p| Path::new(p).components().count());
            (depth, n.kind != StorageKind::Rest)
        };
        if best.is_none_or(|b| rank(candidate) > rank(b)) {
            best = Some(candidate);
        }
    }
    best
}

fn finalize(node: &mut StorageNode, marks: &Marks) -> u64 {
    if marks.covered.contains(&node.id) {
        cover(node);
        return node.bytes;
    }
    let below: u64 = node.children.iter_mut().map(|c| finalize(c, marks)).sum();
    let own = marks.assigned.get(&node.id).copied().unwrap_or(0);
    node.found_bytes = (below + own).min(node.bytes);
    node.found_bytes
}

fn cover(node: &mut StorageNode) {
    node.found_bytes = node.bytes;
    for child in &mut node.children {
        cover(child);
    }
}

#[cfg(test)]
mod tests;
