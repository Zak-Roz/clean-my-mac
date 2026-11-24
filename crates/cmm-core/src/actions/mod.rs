//! Clean planning and execution (owned by S3). See ARCHITECTURE.md §6.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::CoreError;
use crate::history::History;
use crate::model::{CleanItemResult, CleanRequest, CleanResult, Finding, ScanReport};

/// What `execute` would do for a request. Returned to the desktop app by `plan_clean`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanPlan {
    pub items: Vec<Finding>,
    pub total_bytes: u64,
    pub warnings: Vec<String>,
}

/// Skeleton: not implemented.
pub fn plan(report: &ScanReport, req: &CleanRequest) -> Result<CleanPlan, CoreError> {
    let _ = (report, req);
    Err(CoreError::Unimplemented("actions::plan"))
}

/// Skeleton: does nothing and returns an empty result.
pub fn execute(
    report: &ScanReport,
    req: &CleanRequest,
    history: Option<&History>,
    on_item: &dyn Fn(&CleanItemResult),
) -> CleanResult {
    let _ = (report, history, on_item);
    CleanResult { dry_run: req.dry_run, mode: req.mode, items: vec![], freed_bytes: 0, moved_to_trash_bytes: 0 }
}
