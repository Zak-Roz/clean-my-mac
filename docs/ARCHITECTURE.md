# clean-my-mac — architecture & contracts

A local, **deterministic** (rule-based, no AI at runtime) disk cleaner for macOS, aimed at a
developer machine where space goes to DB dumps, agent worktrees, `node_modules`, AI-tool VM
bundles and package-manager caches — not to classic "junk".

This file is the contract between parallel work streams. **Do not change the public types in
`crates/cmm-core/src/model.rs` or the trait/function signatures listed here without the
coordinator's approval** — propose the change in your report instead.

## 1. Layout

```
clean-my-mac/
  Cargo.toml                 # workspace: crates/*, apps/desktop/src-tauri
  rules/default.toml         # built-in rule catalog (embedded with include_str!)
  fixtures/sample-report.json# realistic ScanReport for UI mock mode and tests
  crates/
    cmm-core/                # library: model, rules, scan, detectors, actions, history
    cmm-cli/                 # binary `cmm`
  apps/desktop/              # Tauri 2 + React 19 + Vite + TypeScript
    src-tauri/               # thin layer: Tauri commands -> cmm-core
    src/bindings/            # TS types generated from cmm-core by ts-rs (do not hand-edit)
  package.json, pnpm-workspace.yaml, turbo.json, biome.json
```

Toolchain: Rust edition 2024 (rustc 1.97), Node 24, pnpm 9 (via corepack), Biome 2, Turbo 2.
Bundle identifier `com.zak-roz.clean-my-mac`, binary name `cmm`.

## 2. Core model (`cmm-core/src/model.rs`)

All types: `#[derive(Debug, Clone, Serialize, Deserialize, TS)]`, `#[serde(rename_all = "camelCase")]`,
`#[ts(export)]`; enums serialize as kebab-case strings. Timestamps are `chrono::DateTime<Utc>`.

```rust
pub enum Risk { Safe, Review, Manual }
// safe   = regenerates by itself, preselected            (caches)
// review = probably unneeded, user ticks it              (stale node_modules, clean worktrees, VM bundles, old installers)
// manual = shown, never preselected, never bulk-selected (DB dumps, media, documents)

pub enum Category { Downloads, DevArtifacts, Worktrees, PackageCaches, AiTools, Xcode, Containers, AppCaches, System, LargeFiles }

pub enum DetectorKind { Paths, OlderThan, StaleArtifacts, VersionedSiblings, GitWorktrees, Installers, RedundantArchives, SimulatorRuntimes, Docker }

pub enum ActionKind { Trash, Delete, GitWorktreeRemove, Command, InfoOnly }

pub struct RuleDef {
    pub id: String,                 // "ai.claude-desktop.vm-bundles"
    pub title: String,
    pub description: String,
    pub category: Category,
    pub risk: Risk,
    pub detector: DetectorKind,
    pub roots: Vec<String>,         // globs, "~" expanded: "~/Library/Application Support/ai-profiles/profiles/*/gui-data/vm_bundles"
    pub params: serde_json::Value,  // detector-specific, see §4 (TOML table -> JSON)
    pub action: ActionKind,
    pub requires_quit: Vec<String>, // process names that must NOT be running (e.g. "Claude", "Docker")
    pub regenerates: bool,          // the data comes back on its own (caches)
    pub enabled: bool,              // default true
}

pub struct Finding {
    pub id: String,                 // "{rule_id}:{path-or-key}" — stable across scans (history depends on it)
    pub rule_id: String,
    pub category: Category,
    pub risk: Risk,
    pub label: String,              // short human name
    pub path: Option<String>,       // absolute; None for command-only findings (docker prune)
    pub bytes: u64,                 // ALLOCATED bytes (st_blocks*512), hardlinks counted once
    pub logical_bytes: u64,         // st_size sum (sparse files: Docker.raw is 68.7 GB logical / 11.5 GB real)
    pub files: u64,
    pub last_used: Option<DateTime<Utc>>, // newest mtime inside (or git last commit for projects)
    pub reason: String,             // deterministic explanation: "Project untouched for 143 days"
    pub action: ActionKind,
    pub command: Option<Vec<String>>, // argv for ActionKind::Command, program must be allowlisted
    pub selected_by_default: bool,  // only Risk::Safe with no blockers
    pub blockers: Vec<String>,      // "Claude is running", "Uncommitted changes in worktree"
    pub group: Option<String>,      // set key for versioned siblings / worktrees of one repo
}

pub struct VolumeInfo {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub available_important_bytes: Option<u64>, // NSURLVolumeAvailableCapacityForImportantUsageKey (incl. purgeable)
    pub swap_used_bytes: Option<u64>,           // sysctl vm.swapusage
    pub trash_bytes: Option<u64>,               // None => no Full Disk Access
}

pub struct ScanError { pub path: String, pub message: String }

pub struct ScanReport {
    pub id: String,                 // RFC3339 timestamp of start
    pub started_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub volume: VolumeInfo,
    pub full_disk_access: bool,
    pub findings: Vec<Finding>,     // sorted by bytes desc
    pub errors: Vec<ScanError>,
}

pub struct ScanOptions { pub rule_ids: Option<Vec<String>>, pub categories: Option<Vec<Category>> }
pub enum ScanPhase { Volume, Rules, Done }
pub struct ScanProgress { pub phase: ScanPhase, pub rule_id: Option<String>, pub done_rules: u32, pub total_rules: u32, pub findings: u32 }

pub enum CleanMode { Trash, Delete }
pub struct CleanRequest { pub finding_ids: Vec<String>, pub mode: CleanMode, pub dry_run: bool }
pub struct CleanItemResult { pub finding_id: String, pub ok: bool, pub bytes: u64, pub message: Option<String> }
pub struct CleanResult { pub dry_run: bool, pub mode: CleanMode, pub items: Vec<CleanItemResult>, pub freed_bytes: u64, pub moved_to_trash_bytes: u64 }

pub struct GrowthItem { pub key: String, pub label: String, pub bytes_then: u64, pub bytes_now: u64, pub delta: i64 }
pub struct FreeSpacePoint { pub at: DateTime<Utc>, pub free_bytes: u64 }
```

`config.rs` (owned by coordinator):

```rust
pub struct Config {
    pub stale_days: u32,               // 60  — project artifacts
    pub downloads_older_than_days: u32,// 30
    pub installers_older_than_days: u32,// 7
    pub keep_versions: u32,            // 1   — versioned siblings: keep newest N
    pub min_finding_bytes: u64,        // 50 MiB — smaller findings are dropped
    pub data_dir: PathBuf,             // ~/Library/Application Support/clean-my-mac
}
// Config::load() reads data_dir/config.toml, falling back to defaults.
```

`error.rs`: `pub enum CoreError` (thiserror) with `Io`, `Rule`, `Guard`, `Command`, `History`, `NotFound`,
`Unimplemented(&'static str)` (skeleton stubs only — must be gone before release).

## 3. Core API (signatures are the contract)

```rust
// rules/mod.rs
pub fn builtin_rules() -> Result<Vec<RuleDef>, CoreError>;              // parses rules/default.toml
pub fn load_rules(user_dir: Option<&Path>) -> Result<Vec<RuleDef>, CoreError>; // builtin + data_dir/rules.d/*.toml, override by id
pub fn expand_roots(rule: &RuleDef, home: &Path) -> Vec<PathBuf>;        // "~" + glob expansion, existing paths only

// size.rs
pub struct SizeInfo { pub bytes: u64, pub logical_bytes: u64, pub files: u64, pub newest_mtime: Option<DateTime<Utc>> }
pub struct Sizer { /* shared, thread-safe; dedupes (dev, ino) */ }
impl Sizer { pub fn new() -> Self; pub fn size_of(&self, path: &Path) -> SizeInfo; } // never follows symlinks, never crosses devices

// process.rs
pub struct ProcessSnapshot { /* names of running processes */ }
impl ProcessSnapshot { pub fn capture() -> Self; pub fn is_running(&self, name: &str) -> bool; }

// volume.rs
pub fn volume_info(home: &Path) -> VolumeInfo;
pub fn has_full_disk_access(home: &Path) -> bool;  // probe: read_dir(~/Library/Safari) or ~/.Trash succeeds

// scan.rs
pub struct ScanContext<'a> { pub home: PathBuf, pub now: DateTime<Utc>, pub sizer: &'a Sizer, pub processes: &'a ProcessSnapshot, pub config: &'a Config }
pub fn scan(rules: &[RuleDef], opts: &ScanOptions, config: &Config, on_progress: &(dyn Fn(ScanProgress) + Sync)) -> ScanReport;
// rules run in parallel (rayon); a failing rule becomes ScanError, never aborts the scan.

// detectors/mod.rs
pub trait Detector: Send + Sync {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError>;
}
pub fn detector_for(kind: DetectorKind) -> &'static dyn Detector;
pub fn make_finding(rule: &RuleDef, key: &str, path: Option<&Path>, size: &SizeInfo, reason: String) -> Finding; // shared helper: id, risk, defaults

// guard.rs
pub fn check_target(path: &Path, home: &Path) -> Result<PathBuf, CoreError>; // see §6

// actions/mod.rs
pub struct CleanPlan { pub items: Vec<Finding>, pub total_bytes: u64, pub warnings: Vec<String> }
pub fn plan(report: &ScanReport, req: &CleanRequest) -> Result<CleanPlan, CoreError>;
pub fn execute(report: &ScanReport, req: &CleanRequest, history: Option<&History>, on_item: &dyn Fn(&CleanItemResult)) -> CleanResult;

// history/mod.rs
pub struct History { /* rusqlite connection */ }
impl History {
    pub fn open(path: &Path) -> Result<Self, CoreError>;
    pub fn record_scan(&self, report: &ScanReport) -> Result<(), CoreError>;
    pub fn record_action(&self, finding: &Finding, mode: CleanMode, result: &CleanItemResult) -> Result<(), CoreError>;
    pub fn growth(&self, days: u32) -> Result<Vec<GrowthItem>, CoreError>;      // by rule_id and by finding id, delta desc
    pub fn free_space(&self, days: u32) -> Result<Vec<FreeSpacePoint>, CoreError>;
}
```

## 4. Rule catalog format (`rules/default.toml`)

```toml
[[rule]]
id = "ai.claude-desktop.vm-bundles"
title = "Claude Desktop VM bundle"
description = "Linux VM used by Claude Desktop for code execution. Recreated on demand (~10 GB download)."
category = "ai-tools"
risk = "review"
detector = "paths"
roots = [
  "~/Library/Application Support/Claude/vm_bundles",
  "~/Library/Application Support/ai-profiles/profiles/*/gui-data/vm_bundles",
]
action = "trash"
requires_quit = ["Claude"]
regenerates = true

[rule.params]   # optional, detector-specific
```

Detector semantics:

| detector | roots | params | produces |
|---|---|---|---|
| `paths` | globs of dirs/files | — | one finding per existing match |
| `older-than` | dirs (e.g. `~/Downloads`) | `days` (default `downloads_older_than_days`), `extensions?` | one per direct child not used for `days` |
| `stale-artifacts` | dirs (e.g. `~/Developer`) | `names` (node_modules, target, .next, dist, build, DerivedData, .turbo, .venv, Pods, .gradle) | one per artifact dir whose project (nearest parent with package.json/Cargo.toml/pyproject.toml/…) has no source change and no git commit for `stale_days`; never descend into artifact dirs or `.git` |
| `versioned-siblings` | dirs, depth ≤ 3 | `keep` (default `keep_versions`) | groups siblings whose names differ only by date/version tokens (`28_01_2025`, `2025-01-28`, `v1.2.3`, ` (1)`, ` copy`); all but the newest `keep` → findings with `group`, risk manual |
| `git-worktrees` | dirs with repos | — | per linked worktree (`git worktree list --porcelain`): action `git-worktree-remove`; blockers if dirty or has unpushed commits; orphaned dirs under `.claude/worktrees` not registered in git → `trash` |
| `installers` | dirs | `days` (default `installers_older_than_days`) | `.dmg .pkg .iso .xip .mpkg` older than `days` |
| `redundant-archives` | dirs | — | `X.zip/.tar.gz/.rar/.7z` when sibling `X`/`X.app` exists |
| `simulator-runtimes` | — | — | `xcrun simctl runtime list -j`: unusable or duplicate runtimes → `command` |
| `docker` | — | — | `docker system df`: reclaimable space → `command ["docker","system","prune","-f"]`; info-only when Docker is not running |

## 5. CLI (`cmm`)

```
cmm scan   [--json] [--category <c>]... [--rule <id>]...   # table by default
cmm rules  [--json]
cmm clean  (--id <finding-id>... | --rule <id>... | --safe) [--delete] [--yes]   # dry-run unless --yes
cmm history [--days 30] [--json]                          # growth + free-space trend
cmm du <path> [--depth 1] [--top 20]                      # quick allocated-size breakdown
```

The CLI re-runs a scan (scoped to the requested rules) before `clean`, so it never acts on stale data.

## 6. Safety invariants (non-negotiable)

1. Default action is **Trash** (`trash` crate → NSFileManager). Permanent delete only with explicit `CleanMode::Delete`.
2. `guard::check_target` — the canonical path (`symlink_metadata`, never follow links) must be inside `$HOME`
   and must **not** be or contain any of: `$HOME`, `~/Documents`, `~/Desktop`, `~/Downloads`, `~/Developer`,
   `~/Library`, `~/Library/Application Support`, `~/Library/Keychains`, `~/Library/Mobile Documents`, `~/.ssh`,
   `~/.gnupg`, any `.git` directory, the app's own `data_dir`. (Deleting *inside* those is fine.)
3. Re-validate at execution: path exists, rule still matches, `requires_quit` processes not running, git worktree still clean.
4. `Command` actions only for allowlisted programs: `xcrun`, `docker`, `git`, `tmutil`, `brew`, `npm`, `pnpm`, `yarn`.
5. Dry-run everywhere; the CLI is dry-run unless `--yes`.
6. Every executed action is written to the history `actions` table (audit log).
7. Moving to Trash on the same volume frees **nothing** until the Trash is emptied — results report
   `moved_to_trash_bytes` separately from `freed_bytes`, and the UI says so.

## 7. Desktop app (Tauri 2)

Tauri commands (`src-tauri/src/commands.rs`), all async, errors as `String`:

```
get_volume() -> VolumeInfo
list_rules() -> Vec<RuleDef>
start_scan(opts: ScanOptions) -> ScanReport          // emits "scan://progress" with ScanProgress; keeps last report in state
last_report() -> Option<ScanReport>
plan_clean(req: CleanRequest) -> CleanPlan
execute_clean(req: CleanRequest) -> CleanResult      // emits "clean://item" with CleanItemResult
history_growth(days: u32) -> Vec<GrowthItem>
history_free_space(days: u32) -> Vec<FreeSpacePoint>
reveal_in_finder(path: String)
open_full_disk_access_settings()
```

Screens: **Overview** (free-space gauge, swap, Trash, last scan, top growth, free-space trend) →
**Findings** (grouped by category, risk badge, checkbox, size, reason, blockers, reveal in Finder) →
**Confirm** (summary, Trash vs Delete, "Trash doesn't free space until emptied") → **History**.
Mock mode (`VITE_MOCK=1`) serves `fixtures/sample-report.json` so the UI can be built without a real scan.

## 8. History DB (`data_dir/history.sqlite`)

```
scans(id TEXT PK, at TEXT, total_bytes INT, free_bytes INT, swap_bytes INT, duration_ms INT)
finding_sizes(scan_id TEXT, finding_id TEXT, rule_id TEXT, label TEXT, bytes INT, PRIMARY KEY(scan_id, finding_id))
actions(id INTEGER PK, at TEXT, finding_id TEXT, path TEXT, mode TEXT, bytes INT, ok INT, message TEXT)
```

## 9. Work streams and file ownership

| stream | owns |
|---|---|
| coordinator | `model.rs`, `config.rs`, `error.rs`, `lib.rs`, root configs, this doc |
| S1 scan engine | `size.rs`, `scan.rs`, `volume.rs`, `process.rs`, `rules/`, `detectors/{mod,paths,older_than}.rs`, CLI `scan`/`rules`/`du` |
| S2 detectors + catalog | `detectors/{stale_artifacts,versioned,git_worktrees,installers,archives,simulators,docker}.rs`, `rules/default.toml` (catalog content) |
| S3 actions + history | `guard.rs`, `actions/`, `history/`, CLI `clean`/`history` |
| S4 desktop | `apps/desktop/**`, `fixtures/sample-report.json` |

Anyone may append dependencies to a crate's `Cargo.toml`. Touching another stream's files is a merge
conflict waiting to happen — report the need instead.

## 10. Decisions log (accepted after the skeleton, 5ce98de)

- TOML catalog keys are snake_case (`requires_quit`); the model keeps camelCase via `#[serde(alias)]`.
- Unit enums derive `Copy, PartialEq, Eq, Hash`; `ScanOptions` is `Default` + `#[serde(default)]`.
- `Finding::add_blocker(msg)` pushes a blocker **and** clears `selected_by_default` — always use it.
- `CleanPlan` is a serialized/TS type (frontend uses it for `plan_clean`).
- `Config::load()` never fails (falls back to defaults); `Config::load_from(path)` is strict. Helpers `config::home_dir()`, `config::default_data_dir()`.
- `load_rules(rules_d_dir)` takes the `rules.d` directory itself: callers pass `config.data_dir.join("rules.d")`.
- `scan()` adds "X is running" blockers from `requires_quit`; a failing rule becomes `ScanError { path: rule_id }`.
- **S1 adds `scan_at(home: &Path, …)`** with the same signature as `scan` plus `home`; `scan()` delegates with the real home. All scan tests use `scan_at` on a tempdir.
- u64 is emitted as TS `number` (`TS_RS_LARGE_INT=number`); bindings regenerate on any `cargo test`. Never write a literal `*/` inside `///` comments in `model.rs` (breaks the generated JSDoc).
- Aerial wallpaper videos on this macOS version live **inside** home: `~/Library/Application Support/com.apple.wallpaper/aerials/videos` (often tens of GB) — a `paths` rule, review, trash.
- Run pnpm as `corepack pnpm …` (global pnpm shim is broken on this machine); `COREPACK_ENABLE_DOWNLOAD_PROMPT=0` for non-interactive runs.

Quality bar for every stream: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`; desktop also `pnpm -w lint` and `pnpm -w typecheck`. Filesystem tests use
`tempfile` fixtures — **never** touch the real home directory in tests.
