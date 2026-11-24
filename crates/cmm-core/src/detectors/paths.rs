//! `paths` detector — one finding per existing match of the rule's root globs (owned by S1).

use crate::detectors::Detector;
use crate::error::CoreError;
use crate::model::{Finding, RuleDef};
use crate::scan::ScanContext;

pub struct PathsDetector;

impl Detector for PathsDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let _ = (rule, ctx);
        Ok(vec![])
    }
}
