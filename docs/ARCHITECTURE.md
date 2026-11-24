# clean-my-mac — architecture

A local, **deterministic** (rule-based, no AI at runtime) disk cleaner for macOS, aimed at a
developer machine where space goes to DB dumps, agent worktrees, `node_modules`, AI-tool VM
bundles and package-manager caches — not to classic "junk".

The public types in `crates/cmm-core/src/model.rs` and the functions listed in §3 are shared by
the CLI, the desktop app and the TypeScript bindings generated from the model, so a change to one
of them is a change to all three.

## 1. Layout

```
clean-my-mac/
  Cargo.toml                 # workspace: crates/*, apps/desktop/src-tauri
  rules/default.toml         # built-in rule catalog (embedded with include_str!)
  fixtures/sample-report.json# realistic ScanReport for UI mock mode and tests
  crates/
    cmm-core/                # library: model, rules, scan, detectors, actions, history, monitoring
    cmm-cli/                 # binary `cmm`
  apps/desktop/              # Tauri 2 + React 19 + Vite + TypeScript
    src-tauri/               # thin layer: Tauri commands -> cmm-core, menu bar item
    src/bindings/            # TS types generated from cmm-core by ts-rs (do not hand-edit)
  scripts/                   # install/uninstall, app build and signing, release helpers
  packaging/homebrew/        # Homebrew cask template
  .github/workflows/         # CI checks and the tag-triggered release
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
    pub logical_bytes: u64,         // st_size sum (sparse files: a Docker.raw image is far bigger logically than on disk)
    pub files: u64,
    pub is_dir: bool,               // path was a directory at scan time (symlink_metadata); serde default false
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
pub enum CleanOutcome { Done, Skipped, Failed }
pub struct CleanItemResult { pub finding_id: String, pub outcome: CleanOutcome, pub bytes: u64, pub message: Option<String> }
pub struct CleanResult { pub dry_run: bool, pub mode: CleanMode, pub items: Vec<CleanItemResult>, pub freed_bytes: u64, pub moved_to_trash_bytes: u64 }

pub enum GrowthKind { Rule, Item }               // a GrowthItem sums a whole rule, or tracks one finding
pub struct GrowthItem { pub kind: GrowthKind, pub key: String, pub label: String, pub bytes_then: u64, pub bytes_now: u64, pub delta: i64 }
pub struct FreeSpacePoint { pub at: DateTime<Utc>, pub free_bytes: u64 }
```

`config.rs`:

```rust
pub struct Config {
    pub stale_days: u32,                  // 60  — project artifacts
    pub downloads_older_than_days: u32,   // 30
    pub installers_older_than_days: u32,  // 7
    pub keep_versions: u32,               // 1   — versioned siblings: keep newest N
    pub min_finding_bytes: u64,           // 50 MiB — smaller findings are dropped
    pub low_space_threshold_bytes: u64,   // 50 GB — menu bar warning, background-scan alert
    pub growth_alert_bytes: u64,          // 5 GB  — one finding growing more than this alerts
    pub growth_alert_days: u32,           // 7     — window for growth alerts and the menu bar list
    pub notifications: bool,              // true  — background scans post macOS notifications
    pub data_dir: PathBuf,                // ~/Library/Application Support/clean-my-mac (not read from the file)
}
// Config::load() reads data_dir/config.toml, falling back to defaults.
```

`error.rs`: `pub enum CoreError` (thiserror) with `Io`, `Rule`, `Guard`, `Command`, `History`, `NotFound`.

## 3. Core API

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
pub fn run_with_timeout(program, args, timeout) -> Result<Output, CoreError>; // the one runner for external tools

// volume.rs
pub fn volume_info(home: &Path) -> VolumeInfo;
pub fn has_full_disk_access(home: &Path) -> bool;  // probe: read_dir(~/Library/Safari) or ~/.Trash succeeds

// scan.rs
pub struct ScanContext<'a> { pub home: PathBuf, pub now: DateTime<Utc>, pub sizer: &'a Sizer, pub processes: &'a ProcessSnapshot, pub config: &'a Config }
impl ScanContext<'_> { pub fn warn(&self, path, message); } // non-fatal problem -> ScanReport.errors
pub fn scan(rules: &[RuleDef], opts: &ScanOptions, config: &Config, on_progress: &(dyn Fn(ScanProgress) + Sync)) -> ScanReport;
pub fn scan_at(home: &Path, /* same as scan */) -> ScanReport; // scan() delegates with the real home
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
// The stored report (`selected_from`) is only used to know what the user selected; both functions re-scan the
// requested rules and act/plan on the fresh findings (§10, "Cleaning safeguards"). There is no public execute on a
// stored report. `_with` variants take an `ExecuteEnv` (temp home, fake trash hook) for tests.
pub fn plan_fresh(rules: &[RuleDef], selected_from: &ScanReport, req: &CleanRequest, config: &Config, home: &Path) -> Result<CleanPlan, CoreError>;
pub fn execute_fresh(rules: &[RuleDef], selected_from: &ScanReport, req: &CleanRequest, config: &Config, home: &Path,
                     history: Option<&History>, on_item: &dyn Fn(&CleanItemResult)) -> CleanResult;

// history/mod.rs
pub struct History { /* rusqlite connection */ }
impl History {
    pub fn open(path: &Path) -> Result<Self, CoreError>;
    pub fn record_scan(&self, report: &ScanReport) -> Result<(), CoreError>;
    pub fn record_action(&self, finding: &Finding, mode: CleanMode, result: &CleanItemResult) -> Result<(), CoreError>;
    pub fn growth(&self, days: u32) -> Result<Vec<GrowthItem>, CoreError>;      // by rule_id and by finding id, delta desc
    pub fn free_space(&self, days: u32) -> Result<Vec<FreeSpacePoint>, CoreError>;
}

// monitor.rs — passive monitoring; never cleans
pub fn alerts_after_scan(history: &History, report: &ScanReport, config: &Config, home: &Path) -> Result<Vec<Alert>, CoreError>;
pub fn compose(alerts: &[Alert], free_bytes: u64) -> Option<Notification>;  // pure: evaluate / filter_new / compose

// agent.rs — the daily background-scan LaunchAgent (`cmm scan --quiet --notify`); `launchctl` behind the `Launchd` trait
pub fn install(spec: &AgentSpec, home: &Path, config: &Config, launchd: &dyn Launchd) -> Result<AgentStatus, CoreError>;
pub fn uninstall(home: &Path, launchd: &dyn Launchd) -> Result<bool, CoreError>;
pub fn status(home: &Path, config: &Config, launchd: &dyn Launchd) -> AgentStatus;
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
| `older-than` | dirs (e.g. `~/Downloads`) | `days` (default `downloads_older_than_days`), `extensions?`, `exclude?`, `min_bytes?`, `group_by_root?` | one per direct child not used for `days` |
| `stale-artifacts` | dirs (e.g. `~/Developer`) | `names` (node_modules, target, .next, dist, build, DerivedData, .turbo, .venv, Pods, .gradle) | one per artifact dir whose project (nearest parent with package.json/Cargo.toml/pyproject.toml/…) has no source change and no git commit for `stale_days`; never descend into artifact dirs or `.git` |
| `versioned-siblings` | dirs, depth ≤ 3 | `keep` (default `keep_versions`) | groups siblings whose names differ only by date/version tokens (`28_01_2025`, `2025-01-28`, `v1.2.3`, ` (1)`, ` copy`); all but the newest `keep` → findings with `group`, risk manual |
| `git-worktrees` | dirs with repos | — | per linked worktree (`git worktree list --porcelain`): action `git-worktree-remove`; blockers if dirty or has unpushed commits; orphaned dirs under `.claude/worktrees` not registered in git → `trash`; stale entries → one `info-only` finding |
| `installers` | dirs | `days` (default `installers_older_than_days`) | `.dmg .pkg .iso .xip .mpkg` older than `days` |
| `redundant-archives` | dirs | — | `X.zip/.tar.gz/.rar/.7z` when sibling `X`/`X.app` exists |
| `simulator-runtimes` | — | — | `xcrun simctl runtime list -j`: unusable or duplicate runtimes → `command` |
| `docker` | — | — | `docker system df`: reclaimable space → `command ["docker","system","prune","-f"]`; info-only when Docker is not running |

## 5. CLI (`cmm`)

```
cmm scan   [--json] [--category <c>]... [--rule <id>]... [--no-record] [--quiet] [--notify]   # table by default
cmm rules  [--json]
cmm clean  (--id <finding-id>... | --rule <id>... | --safe) [--delete] [--yes]   # dry-run unless --yes
cmm history [--days 30] [--json]                          # growth + free-space trend
cmm du <path> [--depth 1] [--top 20]                      # quick allocated-size breakdown
cmm agent  install [--at 10:00] | uninstall | status      # daily background scan (LaunchAgent); never cleans
```

The CLI re-runs a scan (scoped to the requested rules) before `clean`, so it never acts on stale data.
`cmm scan` records the report to history unless `--no-record` (filtered scans are never recorded).

## 6. Safety invariants (non-negotiable)

1. Default action is **Trash** (`trash` crate → NSFileManager). Permanent delete only with explicit `CleanMode::Delete`.
2. `guard::check_target` — the canonical path (`symlink_metadata`, never follow links) must be inside `$HOME`
   and must **not** be or contain any of: `$HOME`, `~/Documents`, `~/Desktop`, `~/Downloads`, `~/Developer`,
   `~/Library`, `~/Library/Application Support`, `~/Library/Keychains`, `~/Library/Mobile Documents`, `~/.ssh`,
   `~/.gnupg`, the app's own `data_dir`. (Deleting *inside* those is fine.) A target that is or contains any `.git`
   entry (file, directory or link) is refused; only a worktree finding's own top-level `.git` file is allowed
   (`guard::check_worktree_target_with`).
3. Re-validate at execution via a fresh scan of the requested rules: act only on findings with identical id, action,
   path and argv and no blockers; worktrees additionally re-check lock, submodules, untracked/dirty state and commits
   unreachable from remotes right before acting.
4. `Command` actions only for allowlisted programs: `xcrun`, `docker`, `git`, `tmutil`, `brew`, `npm`, `pnpm`, `yarn`.
5. Dry-run everywhere; the CLI is dry-run unless `--yes`.
6. Every executed action is written to the history `actions` table (audit log).
7. Moving to Trash on the same volume frees **nothing** until the Trash is emptied — results report
   `moved_to_trash_bytes` separately from `freed_bytes`, and the UI says so.

§10 ("Cleaning safeguards") spells out how these are enforced.

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
reveal_trash()                                       // opens ~/.Trash in Finder
open_full_disk_access_settings()
monitor_settings() -> MonitorSettings                // the monitoring fields of config.toml
agent_status() / agent_install() / agent_uninstall() -> AgentStatus   // Settings toggle only
```

Scans started from the menu bar (`src-tauri/src/tray.rs`) also emit "scan://started", "scan://finished"
(ScanReport) or "scan://failed".

Screens: **Overview** (free-space gauge, swap, Trash, last scan, top growth, free-space trend) →
**Findings** (grouped by category, risk badge, checkbox, size, reason, blockers, reveal in Finder) →
**Confirm** (summary, Trash vs Delete, "Trash doesn't free space until emptied") → **History**, plus
**Settings** (monitoring thresholds, background-scan toggle). The menu bar item shows free space and
turns to a warning state below `low_space_threshold_bytes`.
Mock mode (`VITE_MOCK=1`) serves `fixtures/sample-report.json` so the UI can be built without a real scan.

## 8. Data files (`data_dir`)

`history.sqlite` (WAL mode; schema version in `PRAGMA user_version`, currently 1):

```
scans(id TEXT PK, at TEXT, total_bytes INT, free_bytes INT, swap_bytes INT, duration_ms INT)
finding_sizes(scan_id TEXT, finding_id TEXT, rule_id TEXT, label TEXT, bytes INT, PRIMARY KEY(scan_id, finding_id))
actions(id INTEGER PK, at TEXT, finding_id TEXT, path TEXT, mode TEXT, bytes INT, outcome TEXT, message TEXT)
```

Timestamps are fixed-width RFC3339 UTC strings, so they sort chronologically. Next to it: `config.toml`
(optional), `rules.d/*.toml` (user rule overrides), `last-report.json` (the app's last full scan) and
`monitor-state.json` (alerts already notified).

## 9. Module map

| module | responsibility |
|---|---|
| `cmm-core/src/model.rs` | serializable data model shared by the CLI, the app (ts-rs bindings) and history |
| `cmm-core/src/config.rs` | `config.toml` tunables and defaults; `home_dir()`, `default_data_dir()` |
| `cmm-core/src/error.rs` | `CoreError` |
| `cmm-core/src/rules/` | catalog loading (built-in + `rules.d` overrides), strict parsing, root expansion |
| `cmm-core/src/scan.rs` | scan orchestration: parallel rules, blockers, size threshold, nested findings, sorting |
| `cmm-core/src/size.rs` | allocated-size measurement (hardlinks once, no symlinks, one device) |
| `cmm-core/src/volume.rs` | volume capacity, swap, Trash size, Full Disk Access probe |
| `cmm-core/src/process.rs` | running-process snapshot for `requires_quit`; the timed runner and tool resolver for external programs |
| `cmm-core/src/detectors/` | `Detector` trait, registry and `make_finding`; one module per detector, plus `naming.rs` (version tokens), `support.rs` (shared helpers), `external.rs` (read-only tool calls) and `catalog_tests.rs` (catalog consistency) |
| `cmm-core/src/guard.rs` | path guard (§6.2) |
| `cmm-core/src/actions/` | clean planning and execution: fresh re-scan, re-validation, Trash/Delete, worktrees; `command.rs` holds the allowlisted argv patterns |
| `cmm-core/src/history/` | SQLite history: scans, finding sizes, audit log; growth and free-space queries |
| `cmm-core/src/monitor.rs` | alerts after a recorded scan (low space, fast growth), de-duplication, notification text |
| `cmm-core/src/agent.rs` | the background-scan LaunchAgent: install, uninstall, status |
| `cmm-cli/src/main.rs`, `cmd/` | argument parsing and dispatch; one module per subcommand, shared formatting in `cmd/mod.rs` |
| `apps/desktop/src-tauri/` | Tauri commands over cmm-core (§7), app state, last-report persistence, menu bar item |
| `apps/desktop/src/` | React UI: `api.ts` (and `mock.ts` for mock mode), `state.tsx`, pure logic in `lib/`, `screens/`, `components/`, generated `bindings/` |
| `rules/default.toml` | built-in rule catalog |
| `fixtures/sample-report.json` | sample `ScanReport` for mock mode, screenshots and the model round-trip test |
| `scripts/`, `packaging/`, `.github/workflows/` | install/uninstall, app build and signing, releases, Homebrew cask, CI |

## 10. Design decisions

### Model and serialization

- TOML catalog keys are snake_case (`requires_quit`); the model keeps camelCase via `#[serde(alias)]`.
- Unit enums derive `Copy, PartialEq, Eq, Hash`; `ScanOptions` is `Default` + `#[serde(default)]`.
- `Finding::add_blocker(msg)` pushes a blocker **and** clears `selected_by_default` — always use it.
- `CleanPlan` is a serialized/TS type (the frontend uses it for `plan_clean`).
- `CleanItemResult.outcome` is `Done`, `Skipped` (not acted on by design: info-only, blocked, changed since the scan)
  or `Failed` (tried and failed), not a boolean; the history `actions` table stores the same value.
- `GrowthItem.kind` says whether a row sums a rule or tracks one finding, so the UI never parses `:` in keys.
- `Finding.is_dir` (set by `make_finding` from `symlink_metadata`, so a symlink is never a folder; `false` for
  path-less findings) is `#[serde(default)]`: a `last-report.json` saved without it still loads, its folder findings
  read as files and are `Skipped` ("changed since the scan: a file → a folder of N files, rescan") until the next
  scan — it fails closed.
- u64 is emitted as TS `number` (`TS_RS_LARGE_INT=number`); bindings regenerate on any `cargo test`. Never write a
  literal `*/` inside `///` comments in `model.rs` (breaks the generated JSDoc).

### Configuration and rules

- `Config::load()` never fails (falls back to defaults); `Config::load_from(path)` is strict. Helpers
  `config::home_dir()`, `config::default_data_dir()`.
- `load_rules(rules_d_dir)` takes the `rules.d` directory itself: callers pass `config.data_dir.join("rules.d")`.
- Aerial wallpaper videos on current macOS versions live **inside** home:
  `~/Library/Application Support/com.apple.wallpaper/aerials/videos` (often tens of GB) — a `paths` rule, review, trash.

### Scanning

- `scan()` adds "X is running" blockers from `requires_quit`; a failing rule becomes `ScanError { path: rule_id }`.
- `scan_at(home: &Path, …)` has the same signature as `scan` plus `home`; `scan()` delegates with the real home and
  `lib.rs` re-exports both. All scan tests use `scan_at` on a tempdir.
- `min_finding_bytes` does not apply to `Command` and `InfoOnly` findings (the zero-byte list of stale worktree
  entries, blocked runtimes). A rule's own `params.min_bytes` replaces it for that rule.
- **Nested findings** (e.g. a stale `node_modules` inside a worktree that is itself a finding):
  `scan_at` post-processing drops a nested finding when an ancestor finding has **no** blockers (the parent covers it),
  and keeps it when the ancestor is blocked (so the child can still be cleaned alone). The executor skips a
  selected item whose path is inside another selected item ("covered by parent"). UI totals de-duplicate the same way.
- Detectors report partial failures (one root, one repository) through `ScanContext::warn` instead of swallowing
  them; a rule fails only when every root failed.
- History records only unfiltered scans (`cmm scan` without `--category`/`--rule`, and the app's full scan): growth
  compares whole scans, so a filtered one would make every other finding look like it disappeared.

### External tools

- One timed process runner, `process::run_with_timeout`, for detectors and the executor alike;
  `detectors::external::run` is a thin read-only wrapper (`GIT_OPTIONAL_LOCKS=0`, stdin closed).
- External tools (`git`, `xcrun`, `docker`, `mdls`) run with a timeout (≤ 20 s); a missing or failing tool yields
  no findings plus a `ScanError`, never a panic.
- Allowlisted programs are resolved to absolute paths (`/usr/bin`, `/usr/local/bin`, `/opt/homebrew/bin`,
  `/Applications/Docker.app/Contents/Resources/bin`), never through `$PATH`: a Finder-launched app has a minimal
  `PATH`, and a planted `~/bin/git` must not be picked up.

### Cleaning

- `Command` findings free space immediately whatever the mode. `Trash` frees nothing until the Trash is emptied.
- There is no "Empty Trash" action: it would also erase unrelated user files. The app offers `reveal_trash()`
  (opens `~/.Trash` in Finder) instead.
- The desktop app persists the last full scan to `data_dir/last-report.json` and loads it at startup — for display
  and selection only (safeguard 1).

### Cleaning safeguards

Code comments refer to these by number ("safeguard 1"); they take precedence over anything above that conflicts.

1. **Never act on a stored report.** `actions::plan_fresh` and `actions::execute_fresh` take the report the user
   selected from (`selected_from`), re-run `scan_at` scoped to the rule ids of the requested findings, and act only on
   findings that exist in the fresh report with the **same id, same action and same command argv, and no blockers**;
   anything else is `Skipped` ("no longer matches the scan"). A path finding must also still be what the user saw:
   fresh `bytes` within `max(10 %, 10 MiB)` of the stored ones and the same `is_dir`, otherwise it is `Skipped`
   ("changed since the scan: X → Y, rescan"); `Command` findings are exempt (they are sized from tool output).
   Both `cmm clean` and the app's `execute_clean` use it; `plan_clean` re-scans too, so the Confirm dialog shows the
   real state and lists the items that changed since the scan. The stored report (`last-report.json`, in-memory
   state) is for display and selection only.
2. **Guard: any `.git` entry blocks Trash/Delete.** A target that is or contains a `.git` file **or** directory is
   refused, except for `git-worktree-remove` findings, whose own top-level `.git` file is expected.
3. **Worktrees — no repo-wide prune, full re-check at execution.** Right before acting: clean status via
   `git status --porcelain --untracked-files=all --ignore-submodules=none` (overrides user config), not locked,
   no commits unreachable from remotes (`git rev-list HEAD --not --remotes` empty), and no `modules/` in its admin
   dir (submodules → refuse, like `git worktree remove`). Any failure → `Skipped` with the reason. The detector uses
   the same flags, so it blocks what the executor would refuse; worktrees of repos with **no remote** are therefore
   always blocked (safe default), and so are orphan worktree folders that still contain a `.git` file.
   - `Trash`: move the worktree folder **and** its own admin dir (`<repo>/.git/worktrees/<id>`, resolved from the
     worktree's `.git` file) to the Trash. Restoring both restores a working worktree. No `git worktree prune`.
   - `Delete`: run the finding's `command = ["git", "-C", <main repo>, "worktree", "remove", <worktree path>]`
     (no `--force`) after the allowlist check and the re-checks above.
   - Stale worktree entries (folder already gone) become one `info-only` finding that lists them and tells the user
     the command to run themselves. A repo-wide `git worktree prune` frees ~0 bytes and can destroy the admin data of
     worktrees on unmounted disks or folders deleted by hand, so it is not an executable argv pattern.
4. **Claude Code transcripts must never take `memory/` with them.** `older-than` has `params.exclude` (child names
   never proposed). The transcripts rules target the children of each project folder (`projects/*`), exclude
   `memory`, set `group` to the project folder, and use a rule-level `params.min_bytes` (5 MiB) that replaces the
   global `min_finding_bytes` for that rule.
5. **Audit log is mandatory.** The app and the CLI refuse to execute (non-dry-run) when the history DB can't be
   opened. A failed audit write after an action keeps the item `Done` but adds "audit log write failed" to its message.
6. **Command argv must match an exact per-detector pattern** (defense in depth on top of #1): `docker system prune -f`;
   `xcrun simctl delete unavailable`; `xcrun simctl runtime delete <UUID>`;
   `git -C <abs repo> worktree remove <abs path>`. Anything else is refused even if `argv[0]` is allowlisted.

### Known limitations

- Files hidden via `assume-unchanged`/`skip-worktree` are invisible to `git status`, so the worktree checks cannot
  see them.
- `cmm clean` scans twice (once for the plan, once inside `execute_fresh`).

### Development

- CLI layout: one file per subcommand in `crates/cmm-cli/src/cmd/<name>.rs`; `main.rs` only parses and dispatches.
- pnpm runs through corepack (`corepack pnpm …`, the version pinned in `package.json`);
  set `COREPACK_ENABLE_DOWNLOAD_PROMPT=0` for non-interactive runs.
- Checks: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`;
  desktop also `pnpm -w lint`, `pnpm -w typecheck` and the Vitest suites. Filesystem tests use `tempfile`
  fixtures — **never** touch the real home directory in tests.
