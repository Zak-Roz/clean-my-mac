//! Storage breakdown tests: a temp dir plays the Data volume (with /Users/you as home) and the
//! system volumes are injected `Mount`s. Never touches the real home.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

use chrono::{DateTime, Utc};

use super::*;
use crate::model::{ActionKind, Category, Risk, VolumeInfo};

const KB: usize = 1024;
/// Small enough that the fixture's "big" entries (≥ 192 KiB) are kept and the small ones (≤ 16 KiB) are not.
const MIN: u64 = 100_000;
const OPTS: TreeOptions = TreeOptions { max_depth: u32::MAX, min_bytes: MIN, max_children: 50 };

fn write(path: &Path, len: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![7u8; len]).unwrap();
}

struct Fixture {
    _tmp: tempfile::TempDir,
    data: PathBuf,
    home: PathBuf,
}

impl Fixture {
    /// ```text
    /// data/Applications/Big.app/bin                  256 KiB
    /// data/Library/shared.bin                        192 KiB
    /// data/private/var/log/small.log                   8 KiB
    /// data/Users/Shared/shared.bin                   200 KiB
    /// data/Users/you/Library/Caches/Yarn/cache.bin   512 KiB
    /// data/Users/you/Library/Caches/tiny.bin           4 KiB
    /// data/Users/you/Library/Preferences/p.plist       4 KiB
    /// data/Users/you/Developer/app/node_modules/m.js 384 KiB
    /// data/Users/you/Developer/app/src/main.rs        16 KiB
    /// data/Users/you/.npm/_cacache/blob              320 KiB
    /// data/Users/you/.zshrc                            1 KiB
    /// data/Users/you/Music/                         (empty)
    /// ```
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().canonicalize().unwrap().join("data");
        let home = data.join("Users/you");
        write(&data.join("Applications/Big.app/bin"), 256 * KB);
        write(&data.join("Library/shared.bin"), 192 * KB);
        write(&data.join("private/var/log/small.log"), 8 * KB);
        write(&data.join("Users/Shared/shared.bin"), 200 * KB);
        write(&home.join("Library/Caches/Yarn/cache.bin"), 512 * KB);
        write(&home.join("Library/Caches/tiny.bin"), 4 * KB);
        write(&home.join("Library/Preferences/p.plist"), 4 * KB);
        write(&home.join("Developer/app/node_modules/m.js"), 384 * KB);
        write(&home.join("Developer/app/src/main.rs"), 16 * KB);
        write(&home.join(".npm/_cacache/blob"), 320 * KB);
        write(&home.join(".zshrc"), KB);
        fs::create_dir_all(home.join("Music")).unwrap();
        Fixture { _tmp: tmp, data, home }
    }

    /// The fake machine: a 100 GB container with 40 GB free, system volumes of 12 GB + 2 GB,
    /// and a Data volume `unmeasured` bytes bigger than what the walk will find.
    fn env(&self, unmeasured: u64) -> StorageEnv {
        let walked = Sizer::new().size_of(&self.data).bytes;
        let data_used = walked + unmeasured;
        let volumes = vec![mount("/", 12_000_000_000), mount("/System/Volumes/VM", 2_000_000_000)];
        let used = data_used + 14_000_000_000 + 500_000_000; // + container overhead
        StorageEnv {
            home: self.home.clone(),
            data_root: self.data.clone(),
            data_used_bytes: data_used,
            firmlinks: HashMap::new(),
            total_bytes: used + 40_000_000_000,
            free_bytes: 40_000_000_000,
            volumes,
            swap_used_bytes: Some(1_500_000_000),
            local_snapshots: vec![String::from("com.apple.TimeMachine.2026-11-25-101500.local")],
            full_disk_access: false,
            errors: Vec::new(),
        }
    }
}

fn mount(on: &str, used: u64) -> Mount {
    Mount {
        from: format!("/dev/disk3s{}", on.len()),
        on: PathBuf::from(on),
        fs_type: "apfs".into(),
        total_bytes: 0,
        free_bytes: 0,
        used_bytes: used,
    }
}

fn run(env: &StorageEnv, report: Option<&ScanReport>) -> StorageBreakdown {
    breakdown_with(env, report, &OPTS, &|_| {})
}

fn find<'a>(nodes: &'a [StorageNode], id: &str) -> &'a StorageNode {
    try_find(nodes, id).unwrap_or_else(|| panic!("no node {id}"))
}

fn try_find<'a>(nodes: &'a [StorageNode], id: &str) -> Option<&'a StorageNode> {
    nodes.iter().find_map(|n| if n.id == id { Some(n) } else { try_find(&n.children, id) })
}

fn ids(nodes: &[StorageNode]) -> Vec<&str> {
    nodes.iter().map(|n| n.id.as_str()).collect()
}

fn id(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Every node's size is the sum of its children's (when it has any) plus at most its own entry.
fn assert_consistent(node: &StorageNode) {
    if !node.children.is_empty() {
        let sum: u64 = node.children.iter().map(|c| c.bytes).sum();
        assert!(sum <= node.bytes, "{}: children {sum} > {}", node.id, node.bytes);
        assert!(node.bytes - sum <= 64 * KB as u64, "{}: {} not explained by children {sum}", node.id, node.bytes);
        let found: u64 = node.children.iter().map(|c| c.found_bytes).sum();
        assert!(found <= node.found_bytes, "{}: children found {found} > {}", node.id, node.found_bytes);
    }
    assert!(node.found_bytes <= node.bytes, "{}: found {} > {}", node.id, node.found_bytes, node.bytes);
    node.children.iter().for_each(assert_consistent);
}

#[test]
fn parts_add_up_to_the_used_space() {
    let fx = Fixture::new();
    let env = fx.env(3_000_000);
    let b = run(&env, None);

    assert_eq!(b.used_bytes, env.used_bytes());
    let sum: u64 = b.nodes.iter().map(|n| n.bytes).sum();
    assert_eq!(sum, b.used_bytes, "top-level nodes add up to used: {:#?}", ids(&b.nodes));
    assert_eq!(b.walked_bytes, Sizer::new().size_of(&fx.data).bytes, "the walk sees exactly the Data volume");
    assert_eq!(b.unmeasured_bytes, 3_000_000);
    assert_eq!(b.overcount_bytes, 0);
    assert_eq!(b.volumes_bytes, 14_500_000_000);
    assert_eq!(b.walked_bytes + b.volumes_bytes + b.unmeasured_bytes, b.used_bytes);
    b.nodes.iter().for_each(assert_consistent);

    let macos = find(&b.nodes, "@macos");
    assert_eq!(macos.kind, StorageKind::Group);
    assert_eq!(
        ids(&macos.children),
        vec!["@macos:/", "@macos:/System/Volumes/VM", "@macos/container"],
        "volumes biggest first, container overhead included"
    );
    assert_eq!(find(&b.nodes, "@macos/container").bytes, 500_000_000);
    assert!(find(&b.nodes, "@macos:/System/Volumes/VM").note.as_deref().unwrap().contains("Swap in use: 1.5 GB"));
    let unmeasured = find(&b.nodes, "@unmeasured");
    assert_eq!(unmeasured.bytes, 3_000_000);
    assert!(unmeasured.note.as_deref().unwrap().contains("1 local Time Machine snapshot"), "{:?}", unmeasured.note);
    assert_eq!(b.local_snapshots.len(), 1);
}

#[test]
fn top_level_splits_home_applications_users_and_system_data() {
    let fx = Fixture::new();
    let b = run(&fx.env(0), None);
    let top = ids(&b.nodes);

    let library = id(&fx.home.join("Library"));
    let developer = id(&fx.home.join("Developer"));
    let applications = id(&fx.data.join("Applications"));
    for expected in [&library, &developer, &applications] {
        assert!(top.contains(&expected.as_str()), "{expected} in {top:?}");
    }
    for group in ["@home-other", "@users", "@system-data", "@macos"] {
        assert!(top.contains(&group), "{group} in {top:?}");
    }
    assert!(!top.contains(&"@unmeasured"), "nothing unmeasured, no node");
    assert_eq!(find(&b.nodes, &library).label, "~/Library");

    // Hidden folders go to "Other in home"; the empty ~/Music and ~/.zshrc are smaller items there.
    let other = find(&b.nodes, "@home-other");
    assert_eq!(ids(&other.children)[0], id(&fx.home.join(".npm")));
    let rest = other.children.last().unwrap();
    assert_eq!(rest.kind, StorageKind::Rest);
    assert_eq!(rest.path.as_deref(), Some(id(&fx.home).as_str()));
    assert_eq!(rest.entries, 2, "~/.zshrc and ~/Music");
    assert!(!top.contains(&id(&fx.home.join("Music")).as_str()));

    // /Users/Shared under "Other users & Shared"; /Library under "System data", /private summed.
    assert_eq!(ids(&find(&b.nodes, "@users").children), vec![id(&fx.data.join("Users/Shared"))]);
    let system = find(&b.nodes, "@system-data");
    assert_eq!(ids(&system.children)[0], id(&fx.data.join("Library")));
    assert_eq!(system.children.last().unwrap().kind, StorageKind::Rest, "/private is small");

    // The top level is sorted biggest first.
    assert!(b.nodes.windows(2).all(|w| w[0].bytes >= w[1].bytes), "{top:?}");
}

#[test]
fn drill_down_keeps_big_children_biggest_first_and_sums_the_rest() {
    let fx = Fixture::new();
    let b = run(&fx.env(0), None);
    let library = find(&b.nodes, &id(&fx.home.join("Library")));
    // Caches (516 KiB) kept; Preferences (4 KiB) summed.
    assert_eq!(
        ids(&library.children),
        vec![id(&fx.home.join("Library/Caches")), format!("{}/@rest", id(&fx.home.join("Library")))]
    );
    let caches = &library.children[0];
    assert_eq!(caches.kind, StorageKind::Folder);
    assert_eq!(caches.entries, 2);
    assert_eq!(caches.children[0].label, "Yarn");
    assert_eq!(caches.children[1].label, "1 smaller item");
    let yarn = &caches.children[0];
    assert_eq!(yarn.children[0].kind, StorageKind::File);
    assert_eq!(yarn.children[0].label, "cache.bin");
    assert!(yarn.modified.is_some());

    let modified: DateTime<Utc> = library.modified.unwrap();
    assert!(modified >= yarn.modified.unwrap());
}

#[test]
fn walk_exceeding_the_volume_is_reported_as_overcount() {
    let fx = Fixture::new();
    let mut env = fx.env(0);
    env.data_used_bytes -= 100_000;
    let b = run(&env, None);
    assert_eq!(b.unmeasured_bytes, 0);
    assert_eq!(b.overcount_bytes, 100_000);
    assert!(try_find(&b.nodes, "@unmeasured").is_none_or(|n| n.bytes == 0));
}

#[test]
fn symlinks_are_not_followed() {
    let fx = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("huge.bin"), 2048 * KB);
    std::os::unix::fs::symlink(outside.path(), fx.home.join("Developer/linked")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("huge.bin"), fx.home.join("huge-link")).unwrap();
    let b = run(&fx.env(0), None);
    let developer = find(&b.nodes, &id(&fx.home.join("Developer")));
    assert!(developer.bytes < 1024 * KB as u64, "the 2 MiB target is not counted: {}", developer.bytes);
    assert!(b.walked_bytes < 3000 * KB as u64);
}

#[test]
fn unreadable_folders_are_flagged_up_to_the_top() {
    let fx = Fixture::new();
    let locked = fx.home.join("Developer/locked");
    write(&locked.join("secret.bin"), 300 * KB);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let b = run(&fx.env(0), None);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

    let developer = find(&b.nodes, &id(&fx.home.join("Developer")));
    assert_eq!(developer.unreadable, Some(Unreadable::Denied), "EACCES below");
    assert!(developer.bytes < 300 * KB as u64 + 400 * KB as u64, "the locked file is not counted");
    assert!(
        b.errors.iter().any(|e| e.path == id(&locked) && e.message.contains("permission denied")),
        "{:?}",
        b.errors
    );
    assert_eq!(find(&b.nodes, &id(&fx.home.join("Library"))).unreadable, None);

    // A small unreadable entry stays visible on its own instead of disappearing into "smaller items".
    let tiny = fx.data.join("private/locked");
    fs::create_dir_all(&tiny).unwrap();
    fs::set_permissions(&tiny, fs::Permissions::from_mode(0o000)).unwrap();
    let b = run(&fx.env(1000), None);
    fs::set_permissions(&tiny, fs::Permissions::from_mode(0o755)).unwrap();
    let private = find(&b.nodes, &id(&fx.data.join("private")));
    assert_eq!(private.unreadable, Some(Unreadable::Denied));
    assert_eq!(find(&b.nodes, "@system-data").unreadable, Some(Unreadable::Denied));
    assert!(find(&b.nodes, "@unmeasured").note.as_deref().unwrap().contains("only macOS or an administrator"));
}

fn finding(id: &str, path: Option<&Path>, bytes: u64) -> Finding {
    Finding {
        id: id.into(),
        rule_id: id.split(':').next().unwrap().into(),
        category: Category::AppCaches,
        risk: Risk::Safe,
        label: id.into(),
        path: path.map(|p| p.to_string_lossy().into_owned()),
        bytes,
        logical_bytes: bytes,
        files: 1,
        is_dir: true,
        last_used: None,
        reason: String::new(),
        action: if path.is_some() { ActionKind::Trash } else { ActionKind::Command },
        command: None,
        selected_by_default: true,
        blockers: Vec::new(),
        group: None,
        meta: None,
    }
}

fn report(findings: Vec<Finding>) -> ScanReport {
    ScanReport {
        id: String::from("2026-11-25T10:00:00+00:00"),
        started_at: Utc::now(),
        duration_ms: 1,
        volume: VolumeInfo {
            total_bytes: 0,
            free_bytes: 0,
            available_important_bytes: None,
            swap_used_bytes: None,
            trash_bytes: None,
        },
        full_disk_access: false,
        findings,
        errors: Vec::new(),
    }
}

#[test]
fn findings_are_marked_where_they_are_and_never_double_counted() {
    let fx = Fixture::new();
    let yarn = fx.home.join("Library/Caches/Yarn");
    let node_modules = fx.home.join("Developer/app/node_modules");
    let report = report(vec![
        finding("cache.yarn:yarn", Some(&yarn), 600_000),
        // Inside the Yarn finding: de-duplicated, like the Overview.
        finding("cache.inner:yarn/cache.bin", Some(&yarn.join("cache.bin")), 500_000),
        finding("dev.stale:node_modules", Some(&node_modules), 390_000),
        // In a folder below the kept tree (src is 16 KiB): placed in Developer/app's smaller items.
        finding("x.small:src", Some(&fx.home.join("Developer/app/src")), 16_000),
        // No path (docker prune): in the total, nowhere in the map.
        finding("containers.docker:prune", None, 1_000_000),
        // Outside every walked folder.
        finding("elsewhere:x", Some(Path::new("/Volumes/External/x")), 5_000),
    ]);
    let b = run(&fx.env(0), Some(&report));
    assert_eq!(b.scan_id.as_deref(), Some("2026-11-25T10:00:00+00:00"));
    assert_eq!(b.found_bytes, 600_000 + 390_000 + 16_000 + 1_000_000 + 5_000, "de-duplicated like the Overview");
    b.nodes.iter().for_each(assert_consistent);

    // An exact match is wholly found, with everything below it.
    let yarn_node = find(&b.nodes, &id(&yarn));
    assert_eq!(yarn_node.found_bytes, yarn_node.bytes);
    assert_eq!(yarn_node.children[0].found_bytes, yarn_node.children[0].bytes);
    let caches = find(&b.nodes, &id(&fx.home.join("Library/Caches")));
    assert_eq!(caches.found_bytes, yarn_node.bytes, "the 4 KiB tiny.bin is not found");
    let library = find(&b.nodes, &id(&fx.home.join("Library")));
    assert_eq!(library.found_bytes, yarn_node.bytes);

    let nm = find(&b.nodes, &id(&node_modules));
    assert_eq!(nm.found_bytes, nm.bytes);
    let app_rest = find(&b.nodes, &format!("{}/@rest", id(&fx.home.join("Developer/app"))));
    assert_eq!(app_rest.found_bytes, 16_000.min(app_rest.bytes), "placed in the smaller items of its folder");

    let located: u64 = b.nodes.iter().map(|n| n.found_bytes).sum();
    assert_eq!(b.found_located_bytes, located);
    assert!(located < b.found_bytes, "docker and the external path are not in the map");
    assert_eq!(find(&b.nodes, "@macos").found_bytes, 0, "volumes never hold findings");

    // Re-annotating without a report clears every mark; with it again restores them.
    let mut cleared = b.clone();
    annotate(&mut cleared, None);
    assert_eq!(cleared.found_bytes, 0);
    assert_eq!(cleared.scan_id, None);
    assert!(cleared.nodes.iter().all(|n| n.found_bytes == 0));
    annotate(&mut cleared, Some(&report));
    assert_eq!(serde_json::to_value(&cleared).unwrap(), serde_json::to_value(&b).unwrap());
}

#[test]
fn found_total_never_exceeds_used() {
    let fx = Fixture::new();
    let env = fx.env(0);
    let report = report(vec![finding("docker:x", None, env.used_bytes() * 2)]);
    let b = run(&env, Some(&report));
    assert_eq!(b.found_bytes, b.used_bytes);
    assert_eq!(b.found_located_bytes, 0);
}

#[test]
fn children_lists_every_entry_biggest_first() {
    let fx = Fixture::new();
    let dir = fx.home.join("Developer/app");
    write(&dir.join("a.txt"), 8 * KB);
    write(&dir.join("b.txt"), 8 * KB);
    let report = report(vec![finding("dev.stale:nm", Some(&dir.join("node_modules")), 390_000)]);
    let (node, errors) = children(&dir, Some(&report));
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(node.id, id(&dir));
    assert_eq!(node.label, "app");
    let labels: Vec<&str> = node.children.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["node_modules", "src", "a.txt", "b.txt"], "biggest first, ties by name, nothing summed");
    let nm = &node.children[0];
    assert_eq!(nm.kind, StorageKind::Folder);
    assert_eq!(nm.entries, 1);
    assert!(nm.children.is_empty(), "one level only: deeper folders load on demand");
    assert_eq!(nm.found_bytes, nm.bytes);
    assert_eq!(node.found_bytes, nm.bytes);
}

#[test]
fn progress_is_reported_until_done() {
    let fx = Fixture::new();
    let env = fx.env(0);
    let events = Mutex::new(Vec::new());
    let b = breakdown_with(&env, None, &OPTS, &|p| events.lock().unwrap().push(p));
    let events = events.into_inner().unwrap();
    assert_eq!(events.first().unwrap().phase, StoragePhase::Walking);
    let last = events.last().unwrap();
    assert_eq!(last.phase, StoragePhase::Done);
    assert_eq!(last.expected_bytes, env.data_used_bytes);
    assert!(last.walked_bytes >= b.walked_bytes - 64 * KB as u64, "{} vs {}", last.walked_bytes, b.walked_bytes);
}

#[test]
fn breakdown_round_trips_through_json() {
    let fx = Fixture::new();
    let b = run(&fx.env(1_000), None);
    let json = serde_json::to_value(&b).unwrap();
    assert!(json.get("unmeasuredBytes").is_some());
    assert_eq!(json["nodes"][0]["kind"].as_str().map(|k| ["group", "folder", "volume"].contains(&k)), Some(true));
    let back: StorageBreakdown = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(back).unwrap(), json);
}

#[test]
fn parses_firmlinks_and_snapshots() {
    let links = parse_firmlinks("/Applications\tApplications\n/Users\tUsers\n/usr/local\tusr/local\nbogus\n");
    assert_eq!(links.get(&OsString::from("Users")), Some(&PathBuf::from("/Users")));
    assert_eq!(links.get(&OsString::from("Applications")), Some(&PathBuf::from("/Applications")));
    assert_eq!(links.len(), 2, "nested firmlinks are reached through their top-level folder");

    let names = parse_snapshots(
        "Snapshots for disk /:\ncom.apple.TimeMachine.2026-11-24-090000.local\ncom.apple.os.update-ABC\n\n",
    );
    assert_eq!(names, vec!["com.apple.TimeMachine.2026-11-24-090000.local", "com.apple.os.update-ABC"]);
    assert!(parse_snapshots("Snapshots for disk /:\n").is_empty());
}

#[test]
fn volumes_are_described() {
    let (id, label, _) = describe_volume(&mount("/", 1), None);
    assert_eq!((id.as_str(), label.as_str()), ("@macos:/", "System"));
    let (_, label, note) = describe_volume(&mount("/System/Volumes/VM", 1), Some(26_600_000_000));
    assert_eq!(label, "Swap & sleep image");
    assert!(note.unwrap().contains("26.6 GB"));
    assert_eq!(describe_volume(&mount("/Volumes/Recovery", 1), None).1, "Recovery");
    let (id, label, _) = describe_volume(&mount("/Volumes/Backup", 1), None);
    assert_eq!((id.as_str(), label.as_str()), ("@volume:/Volumes/Backup", "Volume “Backup”"));
}
