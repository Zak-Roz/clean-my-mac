//! Scan/action history in `data_dir/history.sqlite` (owned by S3). See ARCHITECTURE.md §8.

use std::path::Path;

use crate::error::CoreError;
use crate::model::{CleanItemResult, CleanMode, Finding, FreeSpacePoint, GrowthItem, ScanReport};

pub struct History {
    #[allow(dead_code)] // used once S3 implements the queries
    conn: rusqlite::Connection,
}

impl History {
    /// Skeleton: not implemented.
    pub fn open(path: &Path) -> Result<Self, CoreError> {
        let _ = path;
        Err(CoreError::Unimplemented("History::open"))
    }

    pub fn record_scan(&self, report: &ScanReport) -> Result<(), CoreError> {
        let _ = report;
        Err(CoreError::Unimplemented("History::record_scan"))
    }

    pub fn record_action(&self, finding: &Finding, mode: CleanMode, result: &CleanItemResult) -> Result<(), CoreError> {
        let _ = (finding, mode, result);
        Err(CoreError::Unimplemented("History::record_action"))
    }

    /// By rule_id and by finding id, delta desc.
    pub fn growth(&self, days: u32) -> Result<Vec<GrowthItem>, CoreError> {
        let _ = days;
        Err(CoreError::Unimplemented("History::growth"))
    }

    pub fn free_space(&self, days: u32) -> Result<Vec<FreeSpacePoint>, CoreError> {
        let _ = days;
        Err(CoreError::Unimplemented("History::free_space"))
    }
}
