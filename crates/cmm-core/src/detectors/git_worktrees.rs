//! `git-worktrees` detector — linked and orphaned git worktrees (owned by S2).

use crate::detectors::Detector;
use crate::error::CoreError;
use crate::model::{Finding, RuleDef};
use crate::scan::ScanContext;

pub struct GitWorktreesDetector;

impl Detector for GitWorktreesDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let _ = (rule, ctx);
        Ok(vec![])
    }
}
