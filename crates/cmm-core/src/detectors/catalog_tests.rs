//! Consistency checks for the built-in catalog `rules/default.toml` (owned by S2).

use std::collections::HashSet;

use crate::detectors::detector_for;
use crate::detectors::support::testutil::TestEnv;
use crate::model::{ActionKind, DetectorKind, Risk, RuleDef};
use crate::rules::builtin_rules;

fn rules() -> Vec<RuleDef> {
    builtin_rules().expect("rules/default.toml parses")
}

/// Allowed `params` keys per detector (ARCHITECTURE.md §4 plus the documented extras).
fn known_params(kind: DetectorKind) -> &'static [&'static str] {
    match kind {
        DetectorKind::Paths | DetectorKind::SimulatorRuntimes | DetectorKind::Docker => &[],
        DetectorKind::OlderThan => &["days", "extensions", "min_bytes", "exclude", "group_by_root"],
        DetectorKind::StaleArtifacts => &["names", "days"],
        DetectorKind::VersionedSiblings => &["keep", "depth"],
        DetectorKind::GitWorktrees => &["depth"],
        DetectorKind::Installers => &["days", "extensions", "depth"],
        DetectorKind::RedundantArchives => &["depth"],
    }
}

#[test]
fn ids_are_unique_and_well_formed() {
    let rules = rules();
    assert!(rules.len() >= 50, "catalog has {} rules", rules.len());
    let mut seen = HashSet::new();
    for r in &rules {
        assert!(seen.insert(r.id.as_str()), "duplicate id {}", r.id);
        let ok = r.id.split('.').count() >= 2
            && r.id.split('.').all(|part| {
                !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            });
        assert!(ok, "id {:?} must look like `area.thing[.detail]`", r.id);
        assert!(!r.title.trim().is_empty(), "{}: title", r.id);
        assert!(r.description.trim().ends_with('.'), "{}: description must be a sentence", r.id);
        assert!(r.enabled, "{}: built-in rules are enabled", r.id);
    }
}

#[test]
fn every_rule_maps_to_a_working_detector() {
    let home = tempfile::tempdir().unwrap();
    let env = TestEnv::new(home.path());
    for r in rules() {
        let detector = detector_for(r.detector);
        // External-tool detectors would query the real machine; their parsing is unit-tested.
        if matches!(r.detector, DetectorKind::SimulatorRuntimes | DetectorKind::Docker) {
            continue;
        }
        let found = detector.detect(&r, &env.ctx());
        assert!(found.is_ok(), "{}: {:?}", r.id, found.err());
        assert!(found.unwrap().is_empty(), "{}: empty home yields nothing", r.id);
    }
}

#[test]
fn actions_risks_and_params_are_consistent() {
    for r in rules() {
        let expected = match r.detector {
            DetectorKind::GitWorktrees => ActionKind::GitWorktreeRemove,
            DetectorKind::SimulatorRuntimes | DetectorKind::Docker => ActionKind::Command,
            _ => ActionKind::Trash,
        };
        assert_eq!(r.action, expected, "{}: action", r.id);
        if r.risk == Risk::Safe {
            assert!(r.regenerates, "{}: safe means it regenerates by itself", r.id);
        }
        let params = r.params.as_object().expect("params is a table");
        for key in params.keys() {
            assert!(known_params(r.detector).contains(&key.as_str()), "{}: unknown param {key}", r.id);
        }
        if r.detector != DetectorKind::SimulatorRuntimes {
            assert!(!r.roots.is_empty(), "{}: needs roots", r.id);
        }
    }
}

#[test]
fn roots_stay_inside_home_and_never_target_protected_dirs() {
    const PROTECTED: &[&str] = &[
        "~",
        "~/Documents",
        "~/Desktop",
        "~/Downloads",
        "~/Developer",
        "~/Library",
        "~/Library/Application Support",
        "~/Library/Caches",
        "~/Library/Keychains",
        "~/Library/Mobile Documents",
        "~/.ssh",
        "~/.gnupg",
        "~/Library/Application Support/clean-my-mac",
    ];
    for r in rules() {
        for root in &r.roots {
            assert!(root.starts_with("~/"), "{}: root {root} must be inside home", r.id);
            assert!(!root.contains("{") && !root.contains(".."), "{}: unsupported glob/path {root}", r.id);
            assert!(!root.ends_with("/.git") && !root.contains("/.git/"), "{}: {root} touches .git", r.id);
            // `paths` findings are the roots themselves, so a protected dir would be a finding.
            if r.detector == DetectorKind::Paths {
                assert!(!PROTECTED.contains(&root.trim_end_matches('/')), "{}: {root} is protected", r.id);
            }
        }
    }
}

#[test]
fn required_coverage() {
    let rules = rules();
    let ids: HashSet<&str> = rules.iter().map(|r| r.id.as_str()).collect();
    for id in [
        // ids used by tests elsewhere and by fixtures/sample-report.json
        "ai.claude-desktop.vm-bundles",
        "ai.ai-profiles.migration-backups",
        "cache.npm",
        "cache.yarn",
        "containers.docker",
        "dev.git-worktrees",
        "dev.stale-artifacts",
        "downloads.versioned-siblings",
        "system.aerial-wallpapers",
        "xcode.simulator-runtimes",
        // the task brief's must-haves
        "ai.claude-desktop.caches",
        "ai.claude-code.transcripts",
        "ai.claude-code.debug-logs",
        "ai.claude-code.old-versions",
        "ai.codex.sessions",
        "ai.codex.logs-db",
        "ai.codex.runtimes",
        "ai.gemini.tmp",
        "ai.cursor.caches",
        "ai.local-models",
        "cache.npx",
        "cache.pnpm",
        "cache.pnpm-store",
        "cache.pip",
        "cache.poetry",
        "cache.uv",
        "cache.cargo",
        "cache.homebrew",
        "cache.cypress",
        "cache.puppeteer",
        "cache.playwright",
        "cache.node-gyp",
        "cache.typescript",
        "cache.gradle",
        "cache.cocoapods",
        "cache.prisma",
        "cache.mongodb-binaries",
        "xcode.derived-data",
        "xcode.archives",
        "xcode.device-support",
        "xcode.coresimulator-caches",
        "downloads.installers",
        "downloads.redundant-archives",
        "downloads.old-files",
        "apps.updater-caches",
        "apps.spotify.cache",
        "apps.chrome.cache",
        "apps.vscode.caches",
    ] {
        assert!(ids.contains(id), "missing rule {id}");
    }
    let kinds: HashSet<DetectorKind> = rules.iter().map(|r| r.detector).collect();
    assert_eq!(kinds.len(), 9, "every detector kind is used by at least one rule");
}

/// Safety fix 4: the transcripts rule proposes old, large session logs one by one, grouped by
/// project folder, and never the project folder itself or its `memory/`.
#[test]
fn claude_code_memory_is_never_proposed() {
    use crate::config::Config;
    use crate::detectors::support::testutil::{age, file};
    use crate::model::ScanOptions;

    const MIB: usize = 1024 * 1024;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let projects = [
        home.join(".claude/projects/-Users-me-Developer-app"),
        home.join("Library/Application Support/ai-profiles/profiles/p1/cli-config/projects/-Users-me-x"),
    ];
    for project in &projects {
        // Everything is old and large, including memory/ (and a differently cased copy).
        file(&project.join("memory/MEMORY.md"), 6 * MIB, 90);
        file(&project.join("memory/notes.md"), 10, 90);
        age(&project.join("memory"), 90);
        file(&project.join("Memory/x.md"), 6 * MIB, 90);
        age(&project.join("Memory"), 90);
        file(&project.join("old-session.jsonl"), 6 * MIB, 90);
        file(&project.join("old-session/subagents/a.jsonl"), 6 * MIB, 90);
        for d in ["old-session/subagents", "old-session"] {
            age(&project.join(d), 90);
        }
        file(&project.join("small-session.jsonl"), MIB, 90);
        file(&project.join("new-session.jsonl"), 6 * MIB, 0);
        age(project, 90);
    }
    let rules = rules();
    let config = Config { min_finding_bytes: 50 * MIB as u64, data_dir: home.join("data"), ..Config::default() };
    let opts = ScanOptions { rule_ids: Some(vec!["ai.claude-code.transcripts".into()]), categories: None };
    let report = crate::scan::scan_at(&home, &rules, &opts, &config, &|_| {});
    assert!(report.errors.is_empty(), "{:?}", report.errors);

    let mut found: Vec<(String, String)> = report
        .findings
        .iter()
        .map(|f| {
            let path = std::path::Path::new(f.path.as_deref().unwrap());
            let project = path.parent().unwrap().to_string_lossy().into_owned();
            assert_eq!(f.group.as_deref(), Some(project.as_str()), "grouped by project folder");
            (project, path.file_name().unwrap().to_string_lossy().into_owned())
        })
        .collect();
    found.sort();
    let mut expected: Vec<(String, String)> = projects
        .iter()
        .flat_map(|p| {
            let p = p.to_string_lossy().into_owned();
            [(p.clone(), "old-session".to_string()), (p, "old-session.jsonl".to_string())]
        })
        .collect();
    expected.sort();
    assert_eq!(found, expected, "5 MiB rule minimum (not 50 MiB), no memory, no small or recent logs");
    for f in &report.findings {
        let path = f.path.as_deref().unwrap().to_lowercase();
        assert!(!path.contains("/memory"), "memory proposed: {path}");
    }
}
