//! `installers` detector — `.dmg .pkg .iso .xip .mpkg` older than `days` (owned by S2).

use crate::detectors::Detector;
use crate::error::CoreError;
use crate::model::{Finding, RuleDef};
use crate::scan::ScanContext;

pub struct InstallersDetector;

impl Detector for InstallersDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let _ = (rule, ctx);
        Ok(vec![])
    }
}
