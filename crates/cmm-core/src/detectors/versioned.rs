//! `versioned-siblings` detector — siblings differing only by date/version tokens; all but the newest `keep` (owned by S2).

use crate::detectors::Detector;
use crate::error::CoreError;
use crate::model::{Finding, RuleDef};
use crate::scan::ScanContext;

pub struct VersionedSiblingsDetector;

impl Detector for VersionedSiblingsDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let _ = (rule, ctx);
        Ok(vec![])
    }
}
