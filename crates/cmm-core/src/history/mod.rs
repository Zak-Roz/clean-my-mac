//! Scan/action history in `data_dir/history.sqlite` (owned by S3). See ARCHITECTURE.md §8.
//!
//! WAL mode with a busy timeout, so the CLI and the desktop app can have the database open at the
//! same time. Timestamps are stored as fixed-width RFC3339 UTC strings (`…T10:00:00.000Z`), which
//! sort lexically in chronological order. The schema version lives in `PRAGMA user_version`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::error::CoreError;
use crate::model::{
    ActionKind, CleanItemResult, CleanMode, CleanOutcome, Finding, FreeSpacePoint, GrowthItem, GrowthKind, ScanReport,
};

/// Current schema version (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i32 = 1;

/// Version 1: the §8 schema (contract v2: `actions.outcome` instead of `ok`) plus lookup indexes.
const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS scans (
    id TEXT PRIMARY KEY,
    at TEXT NOT NULL,
    total_bytes INTEGER NOT NULL,
    free_bytes INTEGER NOT NULL,
    swap_bytes INTEGER,
    duration_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS scans_at ON scans(at);
CREATE TABLE IF NOT EXISTS finding_sizes (
    scan_id TEXT NOT NULL,
    finding_id TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    label TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    PRIMARY KEY (scan_id, finding_id)
);
CREATE TABLE IF NOT EXISTS actions (
    id INTEGER PRIMARY KEY,
    at TEXT NOT NULL,
    finding_id TEXT NOT NULL,
    path TEXT,
    mode TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    outcome TEXT NOT NULL,
    message TEXT
);
CREATE INDEX IF NOT EXISTS actions_at ON actions(at);
";

/// Ordered migrations: entry `i` upgrades `user_version` `i` -> `i + 1`.
const MIGRATIONS: &[&str] = &[SCHEMA_V1];

/// `(baseline, latest)` scan times compared by [`History::growth`].
pub type ScanWindow = (DateTime<Utc>, DateTime<Utc>);

/// Handle to the history database. `Send` but not `Sync`: share it behind a `Mutex`, or open one per
/// thread (opening is cheap).
pub struct History {
    conn: Connection,
    path: PathBuf,
}

/// One row of the `actions` audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRecord {
    pub at: DateTime<Utc>,
    pub finding_id: String,
    pub path: Option<String>,
    /// `trash`, `delete`, `git-worktree-remove` or `command` — what was actually done.
    pub mode: String,
    pub bytes: u64,
    pub outcome: CleanOutcome,
    pub message: Option<String>,
}

fn db_err(e: rusqlite::Error) -> CoreError {
    CoreError::History(e.to_string())
}

fn ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>, CoreError> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| CoreError::History(format!("bad timestamp {s:?}: {e}")))
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// `done` / `skipped` / `failed`, as stored in `actions.outcome`.
fn outcome_text(outcome: CleanOutcome) -> &'static str {
    match outcome {
        CleanOutcome::Done => "done",
        CleanOutcome::Skipped => "skipped",
        CleanOutcome::Failed => "failed",
    }
}

fn parse_outcome(text: &str) -> Result<CleanOutcome, CoreError> {
    match text {
        "done" => Ok(CleanOutcome::Done),
        "skipped" => Ok(CleanOutcome::Skipped),
        "failed" => Ok(CleanOutcome::Failed),
        other => Err(CoreError::History(format!("bad action outcome {other:?}"))),
    }
}

/// What an executed item actually did: the finding's action combined with the clean mode
/// (`Trash`/`Delete` findings follow the mode; worktree removal and commands are what they are).
pub fn action_label(action: ActionKind, mode: CleanMode) -> &'static str {
    match (action, mode) {
        (ActionKind::Trash | ActionKind::Delete, CleanMode::Trash) => "trash",
        (ActionKind::Trash | ActionKind::Delete, CleanMode::Delete) => "delete",
        (ActionKind::GitWorktreeRemove, _) => "git-worktree-remove",
        (ActionKind::Command, _) => "command",
        (ActionKind::InfoOnly, _) => "info-only",
    }
}

impl History {
    /// Opens (creating the file and parent directory if needed) and migrates the database.
    pub fn open(path: &Path) -> Result<Self, CoreError> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let mut conn = Connection::open(path).map_err(db_err)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(db_err)?;
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0)).map_err(db_err)?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(CoreError::History(format!("cannot enable WAL mode (got {mode})")));
        }
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(db_err)?;
        migrate(&mut conn)?;
        Ok(Self { conn, path: path.to_path_buf() })
    }

    /// The database file this handle was opened from (its directory is the app's `data_dir`).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records a scan and the size of every finding. Re-recording the same scan id replaces it.
    ///
    /// Only record complete scans: `growth` compares whole scans, so a scan limited to a few rules
    /// would make every other finding look like it disappeared.
    pub fn record_scan(&self, report: &ScanReport) -> Result<(), CoreError> {
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        tx.execute(
            "INSERT OR REPLACE INTO scans (id, at, total_bytes, free_bytes, swap_bytes, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                report.id,
                ts(report.started_at),
                to_i64(report.volume.total_bytes),
                to_i64(report.volume.free_bytes),
                report.volume.swap_used_bytes.map(to_i64),
                to_i64(report.duration_ms),
            ],
        )
        .map_err(db_err)?;
        tx.execute("DELETE FROM finding_sizes WHERE scan_id = ?1", params![report.id]).map_err(db_err)?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT OR REPLACE INTO finding_sizes (scan_id, finding_id, rule_id, label, bytes)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(db_err)?;
            for f in &report.findings {
                insert.execute(params![report.id, f.id, f.rule_id, f.label, to_i64(f.bytes)]).map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)
    }

    /// Appends one executed item to the `actions` audit log.
    pub fn record_action(&self, finding: &Finding, mode: CleanMode, result: &CleanItemResult) -> Result<(), CoreError> {
        self.conn
            .execute(
                "INSERT INTO actions (at, finding_id, path, mode, bytes, outcome, message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    ts(Utc::now()),
                    finding.id,
                    finding.path,
                    action_label(finding.action, mode),
                    to_i64(result.bytes),
                    outcome_text(result.outcome),
                    result.message,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// The most recent `limit` audit-log rows, newest first.
    pub fn recent_actions(&self, limit: u32) -> Result<Vec<ActionRecord>, CoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT at, finding_id, path, mode, bytes, outcome, message FROM actions ORDER BY at DESC, id DESC LIMIT ?1",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get(6)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (at, finding_id, path, mode, bytes, outcome, message) = row.map_err(db_err)?;
            out.push(ActionRecord {
                at: parse_ts(&at)?,
                finding_id,
                path,
                mode,
                bytes: to_u64(bytes),
                outcome: parse_outcome(&outcome)?,
                message,
            });
        }
        Ok(out)
    }

    /// Growth between the latest scan and a baseline `days` back: one [`GrowthKind::Item`] per
    /// finding id (`key` = finding id) and one [`GrowthKind::Rule`] per rule (`key` = `label` =
    /// rule id), sorted by delta descending (ties: rules first, then by key). Findings missing from one side count as 0 bytes there.
    ///
    /// Baseline: the latest scan at or before `now - days`; if there is none, the oldest scan.
    /// The latest scan is never its own baseline. Fewer than two scans → empty.
    pub fn growth(&self, days: u32) -> Result<Vec<GrowthItem>, CoreError> {
        self.growth_at(days, Utc::now())
    }

    /// `(baseline, latest)` scan times that [`History::growth`] compares, if there are two scans.
    pub fn growth_window(&self, days: u32) -> Result<Option<ScanWindow>, CoreError> {
        self.growth_window_at(days, Utc::now())
    }

    /// Free space at every scan of the last `days` days, oldest first.
    pub fn free_space(&self, days: u32) -> Result<Vec<FreeSpacePoint>, CoreError> {
        self.free_space_at(days, Utc::now())
    }

    pub(crate) fn growth_window_at(&self, days: u32, now: DateTime<Utc>) -> Result<Option<ScanWindow>, CoreError> {
        let Some((baseline, latest)) = self.comparison_scans(days, now)? else { return Ok(None) };
        Ok(Some((parse_ts(&baseline.1)?, parse_ts(&latest.1)?)))
    }

    /// `((id, at) baseline, (id, at) latest)`.
    #[allow(clippy::type_complexity)]
    fn comparison_scans(
        &self,
        days: u32,
        now: DateTime<Utc>,
    ) -> Result<Option<((String, String), (String, String))>, CoreError> {
        let one = |sql: &str, args: &[&dyn rusqlite::ToSql]| -> Result<Option<(String, String)>, CoreError> {
            self.conn.query_row(sql, args, |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(db_err)
        };
        let Some(latest) = one("SELECT id, at FROM scans ORDER BY at DESC, id DESC LIMIT 1", &[])? else {
            return Ok(None);
        };
        let cutoff = ts(now - TimeDelta::days(i64::from(days)));
        let baseline = match one(
            "SELECT id, at FROM scans WHERE at <= ?1 AND id <> ?2 ORDER BY at DESC, id DESC LIMIT 1",
            &[&cutoff, &latest.0],
        )? {
            Some(b) => Some(b),
            None => one("SELECT id, at FROM scans WHERE id <> ?1 ORDER BY at ASC, id ASC LIMIT 1", &[&latest.0])?,
        };
        Ok(baseline.map(|b| (b, latest)))
    }

    pub(crate) fn growth_at(&self, days: u32, now: DateTime<Utc>) -> Result<Vec<GrowthItem>, CoreError> {
        let Some(((then_id, _), (now_id, _))) = self.comparison_scans(days, now)? else { return Ok(vec![]) };
        let then = self.sizes(&then_id)?;
        let latest = self.sizes(&now_id)?;

        // finding id -> (label, rule_id, bytes_then, bytes_now)
        let mut by_finding: BTreeMap<&str, (&str, &str, u64, u64)> = BTreeMap::new();
        for (id, s) in &then {
            by_finding.insert(id, (&s.label, &s.rule_id, s.bytes, 0));
        }
        for (id, s) in &latest {
            let e = by_finding.entry(id).or_insert((&s.label, &s.rule_id, 0, 0));
            e.0 = &s.label; // prefer the newest label
            e.3 = s.bytes;
        }
        let mut by_rule: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
        for (_, rule_id, then_b, now_b) in by_finding.values() {
            let e = by_rule.entry(rule_id).or_default();
            e.0 += then_b;
            e.1 += now_b;
        }

        let item = |kind: GrowthKind, key: String, label: String, bytes_then: u64, bytes_now: u64| GrowthItem {
            kind,
            key,
            label,
            bytes_then,
            bytes_now,
            delta: to_i64(bytes_now) - to_i64(bytes_then),
        };
        let mut out: Vec<GrowthItem> = by_rule
            .into_iter()
            .map(|(rule_id, (t, n))| item(GrowthKind::Rule, rule_id.to_string(), rule_id.to_string(), t, n))
            .collect();
        out.extend(
            by_finding
                .into_iter()
                .map(|(id, (label, _, t, n))| item(GrowthKind::Item, id.to_string(), label.to_string(), t, n)),
        );
        let rank = |k: GrowthKind| match k {
            GrowthKind::Rule => 0,
            GrowthKind::Item => 1,
        };
        out.sort_by(|a, b| {
            b.delta.cmp(&a.delta).then_with(|| rank(a.kind).cmp(&rank(b.kind))).then_with(|| a.key.cmp(&b.key))
        });
        Ok(out)
    }

    pub(crate) fn free_space_at(&self, days: u32, now: DateTime<Utc>) -> Result<Vec<FreeSpacePoint>, CoreError> {
        let cutoff = ts(now - TimeDelta::days(i64::from(days)));
        let mut stmt =
            self.conn.prepare("SELECT at, free_bytes FROM scans WHERE at >= ?1 ORDER BY at ASC").map_err(db_err)?;
        let rows =
            stmt.query_map(params![cutoff], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (at, free) = row.map_err(db_err)?;
            out.push(FreeSpacePoint { at: parse_ts(&at)?, free_bytes: to_u64(free) });
        }
        Ok(out)
    }

    fn sizes(&self, scan_id: &str) -> Result<HashMap<String, SizeRow>, CoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT finding_id, rule_id, label, bytes FROM finding_sizes WHERE scan_id = ?1")
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![scan_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    SizeRow { rule_id: r.get(1)?, label: r.get(2)?, bytes: to_u64(r.get::<_, i64>(3)?) },
                ))
            })
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)
    }

    #[cfg(test)]
    fn user_version(&self) -> i32 {
        self.conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap()
    }
}

struct SizeRow {
    rule_id: String,
    label: String,
    bytes: u64,
}

fn migrate(conn: &mut Connection) -> Result<(), CoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(db_err)?;
    let version: i32 = tx.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(db_err)?;
    if version > SCHEMA_VERSION {
        return Err(CoreError::History(format!(
            "history database has schema version {version}, newer than this build supports ({SCHEMA_VERSION})"
        )));
    }
    for (from, sql) in MIGRATIONS.iter().enumerate().skip(usize::try_from(version).unwrap_or(0)) {
        tx.execute_batch(sql).map_err(|e| CoreError::History(format!("migration {from} -> {}: {e}", from + 1)))?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION).map_err(db_err)?;
    tx.commit().map_err(db_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Category, Risk, VolumeInfo};

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn finding(rule_id: &str, key: &str, bytes: u64) -> Finding {
        Finding {
            id: format!("{rule_id}:{key}"),
            rule_id: rule_id.into(),
            category: Category::AppCaches,
            risk: Risk::Safe,
            label: key.rsplit('/').next().unwrap().into(),
            path: Some(key.into()),
            bytes,
            logical_bytes: bytes,
            files: 1,
            last_used: None,
            reason: "test".into(),
            action: ActionKind::Trash,
            command: None,
            selected_by_default: true,
            blockers: vec![],
            group: None,
        }
    }

    fn report(started: &str, free: u64, findings: Vec<Finding>) -> ScanReport {
        let started_at = at(started);
        ScanReport {
            id: ts(started_at),
            started_at,
            duration_ms: 100,
            volume: VolumeInfo {
                total_bytes: 1_000_000,
                free_bytes: free,
                available_important_bytes: None,
                swap_used_bytes: Some(5),
                trash_bytes: None,
            },
            full_disk_access: true,
            findings,
            errors: vec![],
        }
    }

    fn open_tmp() -> (tempfile::TempDir, History) {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(&dir.path().join("data/history.sqlite")).unwrap();
        (dir, h)
    }

    #[test]
    fn open_creates_schema_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/history.sqlite");
        let h = History::open(&path).unwrap();
        assert_eq!(h.user_version(), SCHEMA_VERSION);
        assert_eq!(h.path(), path);
        let mode: String = h.conn.pragma_query_value(None, "journal_mode", |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
        h.record_scan(&report("2026-11-01T00:00:00Z", 10, vec![finding("a", "/x", 1)])).unwrap();
        drop(h);
        let h = History::open(&path).unwrap();
        assert_eq!(h.user_version(), SCHEMA_VERSION);
        assert_eq!(h.free_space_at(365, at("2026-11-02T00:00:00Z")).unwrap().len(), 1, "data survives reopen");
    }

    #[test]
    fn migrates_an_empty_v0_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite");
        Connection::open(&path).unwrap().execute_batch("CREATE TABLE unrelated (x INT);").unwrap();
        let h = History::open(&path).unwrap();
        assert_eq!(h.user_version(), 1);
        let tables: Vec<String> = h
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(tables, ["actions", "finding_sizes", "scans", "unrelated"]);
    }

    #[test]
    fn refuses_a_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite");
        Connection::open(&path).unwrap().pragma_update(None, "user_version", 99).unwrap();
        let err = History::open(&path).err().expect("newer schema must be refused");
        assert!(matches!(err, CoreError::History(ref m) if m.contains("99")), "{err}");
    }

    #[test]
    fn record_scan_replaces_same_id() {
        let (_d, h) = open_tmp();
        h.record_scan(&report("2026-11-01T00:00:00Z", 10, vec![finding("a", "/x", 1), finding("a", "/y", 2)])).unwrap();
        h.record_scan(&report("2026-11-01T00:00:00Z", 11, vec![finding("a", "/x", 3)])).unwrap();
        let n: i64 = h.conn.query_row("SELECT COUNT(*) FROM finding_sizes", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let pts = h.free_space_at(365, at("2026-11-02T00:00:00Z")).unwrap();
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0].free_bytes, 11);
    }

    /// Three scans: 40 days ago, 10 days ago, now.
    fn three_scans(h: &History) {
        h.record_scan(&report(
            "2026-10-15T12:00:00Z",
            5_000,
            vec![finding("cache.npm", "/n", 100), finding("dev.nm", "/p1/node_modules", 300), finding("vm", "/vm", 50)],
        ))
        .unwrap();
        h.record_scan(&report(
            "2026-11-14T12:00:00Z",
            4_000,
            vec![
                finding("cache.npm", "/n", 400),
                finding("dev.nm", "/p1/node_modules", 300),
                finding("dev.nm", "/p2/node_modules", 200),
                finding("vm", "/vm", 1_000),
            ],
        ))
        .unwrap();
        h.record_scan(&report(
            "2026-11-24T12:00:00Z",
            3_500,
            vec![
                finding("cache.npm", "/n", 450),
                finding("dev.nm", "/p2/node_modules", 260),
                finding("vm", "/vm", 2_000),
            ],
        ))
        .unwrap();
    }

    fn by_key(items: &[GrowthItem]) -> HashMap<&str, (u64, u64, i64)> {
        items.iter().map(|g| (g.key.as_str(), (g.bytes_then, g.bytes_now, g.delta))).collect()
    }

    #[test]
    fn growth_compares_latest_with_scan_before_cutoff() {
        let (_d, h) = open_tmp();
        three_scans(&h);
        let now = at("2026-11-24T13:00:00Z");

        // 7 days: baseline = latest scan at or before Nov 17 = Nov 14.
        let g = h.growth_at(7, now).unwrap();
        let m = by_key(&g);
        assert_eq!(m["vm:/vm"], (1_000, 2_000, 1_000));
        assert_eq!(m["cache.npm:/n"], (400, 450, 50));
        assert_eq!(m["dev.nm:/p2/node_modules"], (200, 260, 60));
        assert_eq!(m["dev.nm:/p1/node_modules"], (300, 0, -300), "gone since baseline");
        assert_eq!(m["vm"], (1_000, 2_000, 1_000));
        assert_eq!(m["dev.nm"], (500, 260, -240));
        assert_eq!(m["cache.npm"], (400, 450, 50));
        assert!(g.windows(2).all(|w| w[0].delta >= w[1].delta), "sorted by delta desc");
        assert_eq!((g[0].kind, g[0].key.as_str()), (GrowthKind::Rule, "vm"), "ties: rules first");
        assert_eq!((g[1].kind, g[1].key.as_str()), (GrowthKind::Item, "vm:/vm"));
        assert_eq!(g.iter().find(|i| i.key == "vm").unwrap().label, "vm");
        let kinds = |kind| g.iter().filter(|i| i.kind == kind).count();
        assert_eq!((kinds(GrowthKind::Rule), kinds(GrowthKind::Item)), (3, 4));
        assert_eq!(h.growth_window_at(7, now).unwrap(), Some((at("2026-11-14T12:00:00Z"), at("2026-11-24T12:00:00Z"))));

        // 30 days: baseline = Oct 15 (the only scan before Oct 25).
        let g30 = h.growth_at(30, now).unwrap();
        let m30 = by_key(&g30);
        assert_eq!(m30["vm:/vm"], (50, 2_000, 1_950));
        assert_eq!(m30["dev.nm:/p2/node_modules"], (0, 260, 260), "new since baseline");
        assert_eq!(m30["dev.nm"], (300, 260, -40));

        // 365 days: nothing that old → fall back to the oldest scan.
        assert_eq!(by_key(&h.growth_at(365, now).unwrap())["vm:/vm"], (50, 2_000, 1_950));
        // 0 days: baseline is the scan before the latest, never the latest itself.
        assert_eq!(by_key(&h.growth_at(0, now).unwrap())["vm:/vm"], (1_000, 2_000, 1_000));
    }

    #[test]
    fn growth_needs_two_scans() {
        let (_d, h) = open_tmp();
        assert!(h.growth(30).unwrap().is_empty());
        assert!(h.growth_window(30).unwrap().is_none());
        h.record_scan(&report("2026-11-24T12:00:00Z", 1, vec![finding("a", "/x", 1)])).unwrap();
        assert!(h.growth(30).unwrap().is_empty());
    }

    #[test]
    fn free_space_series_within_window() {
        let (_d, h) = open_tmp();
        three_scans(&h);
        let now = at("2026-11-24T13:00:00Z");
        let pts = h.free_space_at(30, now).unwrap();
        assert_eq!(
            pts.iter().map(|p| (p.at, p.free_bytes)).collect::<Vec<_>>(),
            [(at("2026-11-14T12:00:00Z"), 4_000), (at("2026-11-24T12:00:00Z"), 3_500)]
        );
        assert_eq!(h.free_space_at(60, now).unwrap().len(), 3);
        assert!(h.free_space_at(0, now).unwrap().is_empty());
    }

    #[test]
    fn record_action_writes_audit_log() {
        let (_d, h) = open_tmp();
        let mut f = finding("dev.wt", "/repo/.claude/worktrees/x", 42);
        let done = CleanItemResult {
            finding_id: f.id.clone(),
            outcome: CleanOutcome::Done,
            bytes: 42,
            message: Some("moved to Trash".into()),
        };
        h.record_action(&f, CleanMode::Trash, &done).unwrap();
        f.action = ActionKind::GitWorktreeRemove;
        let failed = CleanItemResult {
            finding_id: f.id.clone(),
            outcome: CleanOutcome::Failed,
            bytes: 0,
            message: Some("dirty".into()),
        };
        h.record_action(&f, CleanMode::Delete, &failed).unwrap();

        let rows = h.recent_actions(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].mode, "git-worktree-remove");
        assert_eq!(rows[0].outcome, CleanOutcome::Failed);
        assert_eq!(rows[0].message.as_deref(), Some("dirty"));
        assert_eq!(rows[1].mode, "trash");
        assert_eq!((rows[1].bytes, rows[1].outcome), (42, CleanOutcome::Done));
        let stored: Vec<String> = h
            .conn
            .prepare("SELECT outcome FROM actions ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(stored, ["done", "failed"]);
        assert_eq!(rows[1].path.as_deref(), Some("/repo/.claude/worktrees/x"));
    }

    #[test]
    fn two_handles_share_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite");
        let a = History::open(&path).unwrap();
        let b = History::open(&path).unwrap();
        a.record_scan(&report("2026-11-23T12:00:00Z", 7, vec![finding("x", "/1", 1)])).unwrap();
        b.record_scan(&report("2026-11-24T12:00:00Z", 6, vec![finding("x", "/1", 9)])).unwrap();
        let now = at("2026-11-24T13:00:00Z");
        assert_eq!(a.free_space_at(30, now).unwrap().len(), 2);
        assert_eq!(by_key(&b.growth_at(30, now).unwrap())["x:/1"], (1, 9, 8));

        // Concurrent opens/writes from threads (the CLI and the app at the same time).
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let h = History::open(&path).unwrap();
                    for j in 0..10 {
                        let f = finding("t", &format!("/{i}/{j}"), 1);
                        let r = CleanItemResult {
                            finding_id: f.id.clone(),
                            outcome: CleanOutcome::Done,
                            bytes: 1,
                            message: None,
                        };
                        h.record_action(&f, CleanMode::Delete, &r).unwrap();
                    }
                })
            })
            .collect();
        for t in handles {
            t.join().unwrap();
        }
        assert_eq!(a.recent_actions(100).unwrap().len(), 40);
    }

    #[test]
    fn action_labels() {
        assert_eq!(action_label(ActionKind::Delete, CleanMode::Trash), "trash", "Trash mode always wins");
        assert_eq!(action_label(ActionKind::Trash, CleanMode::Delete), "delete");
        assert_eq!(action_label(ActionKind::Command, CleanMode::Trash), "command");
    }
}
