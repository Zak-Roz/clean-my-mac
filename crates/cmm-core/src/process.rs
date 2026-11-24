//! Running-process snapshot used for `requires_quit` blockers (owned by S1).

use std::collections::HashSet;

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind};

/// Names of the processes running when the snapshot was captured.
#[derive(Debug, Clone, Default)]
pub struct ProcessSnapshot {
    names: HashSet<String>,
}

impl ProcessSnapshot {
    /// Captures process names (and executable file names). Empty if the process list is unavailable.
    pub fn capture() -> Self {
        let mut names = HashSet::new();
        let refresh = ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet);
        if let Ok(mut system) = System::new_with_specifics(RefreshKind::nothing().with_processes(refresh)) {
            system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
            for process in system.processes().values() {
                names.insert(process.name().to_string_lossy().into_owned());
                if let Some(exe_name) = process.exe().and_then(|exe| exe.file_name()) {
                    names.insert(exe_name.to_string_lossy().into_owned());
                }
            }
        }
        Self { names }
    }

    /// Builds a snapshot from explicit names (tests, fixtures).
    pub fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self { names: names.into_iter().map(Into::into).collect() }
    }

    /// Exact (case-sensitive) match against process or executable name.
    pub fn is_running(&self, name: &str) -> bool {
        self.names.contains(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_names_matches_exactly() {
        let snapshot = ProcessSnapshot::from_names(["Claude", "Docker Desktop"]);
        assert!(snapshot.is_running("Claude"));
        assert!(!snapshot.is_running("claude"));
        assert!(!snapshot.is_running("Docker"));
    }
}
