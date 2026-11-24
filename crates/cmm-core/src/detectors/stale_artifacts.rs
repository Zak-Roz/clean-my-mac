//! `stale-artifacts` detector — artifact dirs (node_modules, target, ...) of projects untouched for `stale_days` (owned by S2).

use crate::detectors::Detector;
use crate::error::CoreError;
use crate::model::{Finding, RuleDef};
use crate::scan::ScanContext;

pub struct StaleArtifactsDetector;

impl Detector for StaleArtifactsDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let _ = (rule, ctx);
        Ok(vec![])
    }
}
