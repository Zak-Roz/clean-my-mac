//! Path guard (owned by S3). See ARCHITECTURE.md §6.2.

use std::path::{Path, PathBuf};

use crate::error::CoreError;

/// Returns the canonical target if it may be trashed/deleted, otherwise `CoreError::Guard`.
///
/// Skeleton: always refuses with `CoreError::Unimplemented`.
pub fn check_target(path: &Path, home: &Path) -> Result<PathBuf, CoreError> {
    let _ = (path, home);
    Err(CoreError::Unimplemented("guard::check_target"))
}
