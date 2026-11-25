//! The daily background scan: a user LaunchAgent (`~/Library/LaunchAgents/<LABEL>.plist`) that
//! runs `cmm scan --quiet --notify` once a day. The job only scans, records the scan to history and
//! may post a notification — it never cleans anything.
//!
//! Installed only on an explicit user action (`cmm agent install`, the app's Settings toggle),
//! never automatically. `launchctl` sits behind the [`Launchd`] trait so tests never touch the real
//! launchd or `~/Library/LaunchAgents`.

use std::ffi::OsStr;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::config::Config;
use crate::error::CoreError;
use crate::process::run_with_timeout;

/// launchd job label (also the plist file name).
pub const LABEL: &str = "com.zak-roz.clean-my-mac.scan";
/// What the job runs after the `cmm` path.
pub const SCAN_ARGS: [&str; 3] = ["scan", "--quiet", "--notify"];
/// The job's stdout/stderr, in `data_dir`.
pub const LOG_FILE: &str = "agent.log";
/// Default time of day of the scan (local time).
pub const DEFAULT_TIME: (u8, u8) = (10, 0);

const LAUNCHCTL: &str = "/bin/launchctl";
const LAUNCHCTL_TIMEOUT: Duration = Duration::from_secs(10);
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// `~/Library/LaunchAgents/com.zak-roz.clean-my-mac.scan.plist`
pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

/// What the LaunchAgent runs and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpec {
    /// Absolute path of the `cmm` binary.
    pub cmm: PathBuf,
    pub hour: u8,
    pub minute: u8,
    pub log_path: PathBuf,
}

impl AgentSpec {
    /// Daily at [`DEFAULT_TIME`], logging to `data_dir/agent.log`.
    pub fn new(cmm: PathBuf, config: &Config) -> Self {
        Self { cmm, hour: DEFAULT_TIME.0, minute: DEFAULT_TIME.1, log_path: config.data_dir.join(LOG_FILE) }
    }
}

/// Parses `HH:MM` (24-hour clock).
pub fn parse_time(text: &str) -> Result<(u8, u8), String> {
    let invalid = || format!("invalid time {text:?}: expected HH:MM, e.g. 10:00");
    let (h, m) = text.trim().split_once(':').ok_or_else(invalid)?;
    let hour: u8 = h.parse().map_err(|_| invalid())?;
    let minute: u8 = m.parse().map_err(|_| invalid())?;
    if hour > 23 || minute > 59 || m.len() != 2 {
        return Err(invalid());
    }
    Ok((hour, minute))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// The LaunchAgent property list. `ProcessType=Background` and low-priority I/O keep the scan out
/// of the user's way; `RunAtLoad` is off so installing or logging in does not start a scan.
pub fn plist_xml(spec: &AgentSpec) -> String {
    let string = |s: &str| format!("<string>{}</string>", xml_escape(s));
    let program = std::iter::once(spec.cmm.to_string_lossy().into_owned())
        .chain(SCAN_ARGS.iter().map(|a| a.to_string()))
        .map(|a| format!("\t\t{}\n", string(&a)))
        .collect::<String>();
    let log = string(&spec.log_path.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	{label}
	<key>ProgramArguments</key>
	<array>
{program}	</array>
	<key>StartCalendarInterval</key>
	<dict>
		<key>Hour</key>
		<integer>{hour}</integer>
		<key>Minute</key>
		<integer>{minute}</integer>
	</dict>
	<key>RunAtLoad</key>
	<false/>
	<key>ProcessType</key>
	<string>Background</string>
	<key>LowPriorityIO</key>
	<true/>
	<key>StandardOutPath</key>
	{log}
	<key>StandardErrorPath</key>
	{log}
</dict>
</plist>
"#,
        label = string(LABEL),
        hour = spec.hour,
        minute = spec.minute,
    )
}

/// The text of the first `<tag>…</tag>` after `<key>{key}</key>` in a plist we wrote.
fn value_after<'a>(xml: &'a str, key: &str, tag: &str) -> Option<&'a str> {
    let after_key = &xml[xml.find(&format!("<key>{key}</key>"))?..];
    let open = format!("<{tag}>");
    let start = after_key.find(&open)? + open.len();
    let len = after_key[start..].find(&format!("</{tag}>"))?;
    Some(&after_key[start..start + len])
}

/// `(program, hour, minute)` read back from an installed plist.
pub fn read_plist(xml: &str) -> (Option<String>, Option<u8>, Option<u8>) {
    let program = value_after(xml, "ProgramArguments", "string").map(xml_unescape);
    let hour = value_after(xml, "Hour", "integer").and_then(|v| v.trim().parse().ok());
    let minute = value_after(xml, "Minute", "integer").and_then(|v| v.trim().parse().ok());
    (program, hour, minute)
}

/// The launchd operations the agent needs, for the current user's GUI domain.
pub trait Launchd {
    /// Loads the job from `plist` (`launchctl bootstrap gui/<uid> <plist>`).
    fn bootstrap(&self, plist: &Path) -> Result<(), CoreError>;
    /// Unloads the job (`launchctl bootout gui/<uid>/<LABEL>`).
    fn bootout(&self) -> Result<(), CoreError>;
    /// `launchctl print gui/<uid>/<LABEL>` output if the job is loaded, `None` otherwise.
    fn print(&self) -> Option<String>;
}

/// The real `/bin/launchctl`.
pub struct SystemLaunchd;

impl SystemLaunchd {
    fn domain() -> String {
        // SAFETY: getuid has no preconditions and cannot fail.
        format!("gui/{}", unsafe { libc::getuid() })
    }

    fn run(args: &[&OsStr]) -> Result<std::process::Output, CoreError> {
        run_with_timeout(LAUNCHCTL, args, LAUNCHCTL_TIMEOUT)
    }

    fn check(out: std::process::Output, what: &str) -> Result<(), CoreError> {
        if out.status.success() {
            return Ok(());
        }
        let msg =
            String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr }).trim().to_string();
        Err(CoreError::Command(format!("launchctl {what} failed: {msg}")))
    }
}

impl Launchd for SystemLaunchd {
    fn bootstrap(&self, plist: &Path) -> Result<(), CoreError> {
        let domain = Self::domain();
        Self::check(Self::run(&["bootstrap".as_ref(), domain.as_ref(), plist.as_os_str()])?, "bootstrap")
    }

    fn bootout(&self) -> Result<(), CoreError> {
        let target = format!("{}/{LABEL}", Self::domain());
        Self::check(Self::run(&["bootout".as_ref(), target.as_ref()])?, "bootout")
    }

    fn print(&self) -> Option<String> {
        let target = format!("{}/{LABEL}", Self::domain());
        let out = Self::run(&["print".as_ref(), target.as_ref()]).ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Install state of the background scan, for `cmm agent status` and the app's Settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentStatus {
    /// The plist exists.
    pub installed: bool,
    /// launchd has the job loaded.
    pub loaded: bool,
    pub plist_path: String,
    /// The `cmm` binary the installed job runs.
    pub program: Option<String>,
    /// `program` exists and is executable (a moved or deleted binary breaks the job).
    pub program_ok: bool,
    pub hour: Option<u8>,
    pub minute: Option<u8>,
    pub log_path: String,
    /// From `launchctl print`, once the job has run.
    pub last_exit_code: Option<i32>,
    /// Last line of the job's log (the summary `cmm scan --quiet` prints).
    pub last_log_line: Option<String>,
    /// The `cmm` binary an install would use now (filled by callers that resolve one).
    pub resolved_cmm: Option<String>,
}

/// Current state; never fails (unreadable parts are `None`/`false`).
pub fn status(home: &Path, config: &Config, launchd: &dyn Launchd) -> AgentStatus {
    let plist = plist_path(home);
    let xml = std::fs::read_to_string(&plist).ok();
    let (program, hour, minute) = xml.as_deref().map(read_plist).unwrap_or_default();
    let printed = launchd.print();
    let log_path = config.data_dir.join(LOG_FILE);
    AgentStatus {
        installed: xml.is_some(),
        loaded: printed.is_some(),
        plist_path: plist.to_string_lossy().into_owned(),
        program_ok: program.as_deref().is_some_and(|p| is_executable(Path::new(p))),
        program,
        hour,
        minute,
        last_exit_code: printed.as_deref().and_then(last_exit_code),
        last_log_line: last_line(&log_path),
        log_path: log_path.to_string_lossy().into_owned(),
        resolved_cmm: None,
    }
}

/// `last exit code = 0` in `launchctl print` output.
fn last_exit_code(printed: &str) -> Option<i32> {
    printed.lines().find_map(|line| line.trim().strip_prefix("last exit code = ")?.trim().parse().ok())
}

/// Last non-empty line of a (small, append-only) log file; reads at most the last 4 KiB.
fn last_line(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(4096))).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    String::from_utf8_lossy(&tail).lines().rev().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

/// Writes the plist and loads it (replacing an existing job). The `cmm` path must be absolute and
/// executable. If launchd refuses the job, the plist is removed again so nothing half-installed
/// stays behind.
pub fn install(
    spec: &AgentSpec,
    home: &Path,
    config: &Config,
    launchd: &dyn Launchd,
) -> Result<AgentStatus, CoreError> {
    if !spec.cmm.is_absolute() || !is_executable(&spec.cmm) {
        return Err(CoreError::NotFound(format!("{} is not an executable cmm binary", spec.cmm.display())));
    }
    let plist = plist_path(home);
    let dir = plist.parent().ok_or_else(|| CoreError::NotFound(plist.display().to_string()))?;
    std::fs::create_dir_all(dir)?;
    if let Some(log_dir) = spec.log_path.parent() {
        std::fs::create_dir_all(log_dir)?;
    }
    if launchd.print().is_some() {
        launchd.bootout()?;
    }
    let tmp = dir.join(format!(".{LABEL}.plist.tmp"));
    std::fs::write(&tmp, plist_xml(spec))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
    std::fs::rename(&tmp, &plist)?;
    if let Err(e) = launchd.bootstrap(&plist) {
        let _ = std::fs::remove_file(&plist);
        return Err(e);
    }
    Ok(status(home, config, launchd))
}

/// Unloads the job and removes the plist (the log stays). `Ok(false)` when nothing was installed.
pub fn uninstall(home: &Path, launchd: &dyn Launchd) -> Result<bool, CoreError> {
    let loaded = launchd.print().is_some();
    if loaded {
        launchd.bootout()?;
    }
    let existed = match std::fs::remove_file(plist_path(home)) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
    };
    Ok(loaded || existed)
}

fn is_executable(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Install locations outside home, checked between the home ones (Homebrew on Apple silicon, then
/// Intel / manual installs).
const SYSTEM_BIN_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// Where a `cmm` binary may live, in order: next to the running executable (the CLI itself, or a
/// `cmm` bundled in the app's `Contents/MacOS`), the usual install locations, then `$PATH`.
/// A Finder-launched app has a minimal `PATH`, so the fixed locations matter.
pub fn cmm_candidates(home: &Path, current_exe: Option<&Path>, path_var: Option<&OsStr>) -> Vec<PathBuf> {
    candidates_with(home, current_exe, path_var, &SYSTEM_BIN_DIRS)
}

/// [`cmm_candidates`] with the install locations outside home given by the caller, so tests do not
/// depend on what is installed on the machine.
fn candidates_with(
    home: &Path,
    current_exe: Option<&Path>,
    path_var: Option<&OsStr>,
    system_dirs: &[&str],
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(exe) = current_exe {
        if exe.file_name() == Some(OsStr::new("cmm")) {
            out.push(exe.to_path_buf());
        }
        if let Some(dir) = exe.parent() {
            out.push(dir.join("cmm"));
        }
    }
    out.extend([home.join(".cargo/bin/cmm"), home.join(".local/bin/cmm")]);
    out.extend(system_dirs.iter().map(|d| Path::new(d).join("cmm")));
    out.push(home.join("bin/cmm"));
    if let Some(path_var) = path_var {
        out.extend(std::env::split_paths(path_var).filter(|d| d.is_absolute()).map(|d| d.join("cmm")));
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| p.is_absolute() && seen.insert(p.clone()));
    out
}

/// `true` if `path` is an executable whose `--version` says it is `cmm`.
pub fn is_cmm(path: &Path) -> bool {
    is_executable(path)
        && run_with_timeout(path.as_os_str(), &["--version"], VERSION_TIMEOUT)
            .is_ok_and(|out| out.status.success() && out.stdout.starts_with(b"cmm "))
}

/// The first candidate that really is `cmm` (see [`cmm_candidates`]).
pub fn find_cmm(home: &Path, current_exe: Option<&Path>, path_var: Option<&OsStr>) -> Option<PathBuf> {
    find_cmm_with(home, current_exe, path_var, &SYSTEM_BIN_DIRS)
}

fn find_cmm_with(
    home: &Path,
    current_exe: Option<&Path>,
    path_var: Option<&OsStr>,
    system_dirs: &[&str],
) -> Option<PathBuf> {
    candidates_with(home, current_exe, path_var, system_dirs).into_iter().find(|p| is_cmm(p))
}

/// A binary inside a Cargo `target/` directory disappears on `cargo clean` and changes on every
/// build — fine for trying things out, fragile for a daily job.
pub fn is_dev_build(path: &Path) -> bool {
    let components: Vec<&OsStr> = path.components().map(|c| c.as_os_str()).collect();
    components.windows(2).any(|w| w[0] == "target" && (w[1] == "debug" || w[1] == "release"))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// Records calls; `loaded` mirrors what launchd would know.
    #[derive(Default)]
    struct FakeLaunchd {
        loaded: RefCell<bool>,
        refuse_bootstrap: bool,
        calls: RefCell<Vec<String>>,
    }

    impl Launchd for FakeLaunchd {
        fn bootstrap(&self, plist: &Path) -> Result<(), CoreError> {
            self.calls.borrow_mut().push(format!("bootstrap {}", plist.display()));
            assert!(plist.exists(), "plist is written before bootstrap");
            if self.refuse_bootstrap {
                return Err(CoreError::Command("launchctl bootstrap failed: 5: Input/output error".into()));
            }
            *self.loaded.borrow_mut() = true;
            Ok(())
        }
        fn bootout(&self) -> Result<(), CoreError> {
            self.calls.borrow_mut().push("bootout".into());
            *self.loaded.borrow_mut() = false;
            Ok(())
        }
        fn print(&self) -> Option<String> {
            self.loaded.borrow().then(|| String::from("\tstate = not running\n\tlast exit code = 0\n"))
        }
    }

    fn fake_cmm(dir: &Path, name: &str, version_output: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho '{version_output}'\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn setup() -> (tempfile::TempDir, PathBuf, Config) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let config = Config { data_dir: home.join("Library/Application Support/clean-my-mac"), ..Config::default() };
        (dir, home, config)
    }

    #[test]
    fn plist_runs_a_quiet_notifying_scan_daily_and_escapes_paths() {
        let spec = AgentSpec {
            cmm: PathBuf::from("/Users/me/Tools & Bins/<cmm>"),
            hour: 9,
            minute: 5,
            log_path: PathBuf::from("/Users/me/Library/Application Support/clean-my-mac/agent.log"),
        };
        let xml = plist_xml(&spec);
        assert!(xml.contains("<string>com.zak-roz.clean-my-mac.scan</string>"));
        assert!(xml.contains("<string>/Users/me/Tools &amp; Bins/&lt;cmm&gt;</string>"));
        assert!(xml.contains(
            "\t\t<string>scan</string>\n\t\t<string>--quiet</string>\n\t\t<string>--notify</string>\n\t</array>"
        ));
        assert!(xml.contains("<key>Hour</key>\n\t\t<integer>9</integer>"));
        assert!(xml.contains("<key>Minute</key>\n\t\t<integer>5</integer>"));
        assert!(xml.contains("<key>RunAtLoad</key>\n\t<false/>"));
        assert!(xml.contains("<string>Background</string>"));
        assert_eq!(
            xml.matches("<string>/Users/me/Library/Application Support/clean-my-mac/agent.log</string>").count(),
            2
        );
        // Never anything that could clean.
        for word in ["clean", "--yes", "--delete"] {
            assert!(!xml.contains(&format!("<string>{word}</string>")), "{word}");
        }
        assert_eq!(read_plist(&xml), (Some("/Users/me/Tools & Bins/<cmm>".into()), Some(9), Some(5)));
    }

    #[test]
    fn plist_is_valid_for_plutil() {
        let plutil = Path::new("/usr/bin/plutil");
        if !plutil.exists() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let spec = AgentSpec {
            cmm: PathBuf::from("/opt/homebrew/bin/cmm"),
            hour: 10,
            minute: 0,
            log_path: dir.path().join("a & b/agent.log"),
        };
        let path = dir.path().join("agent.plist");
        std::fs::write(&path, plist_xml(&spec)).unwrap();
        let out = run_with_timeout(plutil.as_os_str(), &["-lint".as_ref(), path.as_os_str()], VERSION_TIMEOUT).unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    }

    #[test]
    fn parse_time_accepts_hh_mm_only() {
        assert_eq!(parse_time("10:00"), Ok((10, 0)));
        assert_eq!(parse_time("7:30"), Ok((7, 30)));
        assert_eq!(parse_time(" 23:59 "), Ok((23, 59)));
        for bad in ["24:00", "10:60", "10", "10:5", "ab:cd", "", "-1:00"] {
            assert!(parse_time(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn install_writes_plist_then_bootstraps_and_reports_status() {
        let (dir, home, config) = setup();
        let cmm = fake_cmm(dir.path(), "cmm", "cmm 0.1.0");
        let launchd = FakeLaunchd::default();
        let status = install(&AgentSpec::new(cmm.clone(), &config), &home, &config, &launchd).unwrap();
        let plist = plist_path(&home);
        assert!(plist.starts_with(home.join("Library/LaunchAgents")));
        assert_eq!(plist.metadata().unwrap().permissions().mode() & 0o777, 0o644);
        assert!(status.installed && status.loaded && status.program_ok);
        assert_eq!(status.program.as_deref(), cmm.to_str());
        assert_eq!((status.hour, status.minute), (Some(10), Some(0)));
        assert_eq!(status.last_exit_code, Some(0));
        assert!(config.data_dir.is_dir(), "log directory created");
        assert_eq!(launchd.calls.borrow().as_slice(), [format!("bootstrap {}", plist.display())]);

        // Reinstalling replaces the loaded job.
        install(&AgentSpec { hour: 7, ..AgentSpec::new(cmm, &config) }, &home, &config, &launchd).unwrap();
        assert_eq!(launchd.calls.borrow()[1], "bootout");
        assert_eq!(status_hour(&home, &config, &launchd), Some(7));
    }

    fn status_hour(home: &Path, config: &Config, launchd: &dyn Launchd) -> Option<u8> {
        status(home, config, launchd).hour
    }

    #[test]
    fn install_refuses_a_missing_binary_and_cleans_up_when_launchd_refuses() {
        let (dir, home, config) = setup();
        let launchd = FakeLaunchd::default();
        let missing = AgentSpec::new(dir.path().join("nope/cmm"), &config);
        assert!(install(&missing, &home, &config, &launchd).is_err());
        assert!(!plist_path(&home).exists());

        let relative = AgentSpec::new(PathBuf::from("cmm"), &config);
        assert!(install(&relative, &home, &config, &launchd).is_err());

        let cmm = fake_cmm(dir.path(), "cmm", "cmm 0.1.0");
        let refusing = FakeLaunchd { refuse_bootstrap: true, ..Default::default() };
        let err = install(&AgentSpec::new(cmm, &config), &home, &config, &refusing).unwrap_err();
        assert!(err.to_string().contains("bootstrap"), "{err}");
        assert!(!plist_path(&home).exists(), "no half-installed plist");
        assert!(!status(&home, &config, &refusing).installed);
    }

    #[test]
    fn uninstall_unloads_and_removes_and_is_idempotent() {
        let (dir, home, config) = setup();
        let cmm = fake_cmm(dir.path(), "cmm", "cmm 0.1.0");
        let launchd = FakeLaunchd::default();
        install(&AgentSpec::new(cmm.clone(), &config), &home, &config, &launchd).unwrap();
        std::fs::write(config.data_dir.join(LOG_FILE), "old\n2026-11-24 10:00 scan recorded: 41.6 GB free\n\n")
            .unwrap();
        assert_eq!(
            status(&home, &config, &launchd).last_log_line.as_deref(),
            Some("2026-11-24 10:00 scan recorded: 41.6 GB free")
        );

        assert!(uninstall(&home, &launchd).unwrap());
        assert!(!plist_path(&home).exists());
        assert!(config.data_dir.join(LOG_FILE).exists(), "the log is kept");
        let after = status(&home, &config, &launchd);
        assert!(!after.installed && !after.loaded && after.program.is_none());
        assert!(!uninstall(&home, &launchd).unwrap(), "nothing left to remove");

        // A deleted binary shows up in the status of an installed job.
        install(&AgentSpec::new(cmm.clone(), &config), &home, &config, &launchd).unwrap();
        std::fs::remove_file(&cmm).unwrap();
        assert!(!status(&home, &config, &launchd).program_ok);
    }

    #[test]
    fn candidates_cover_sidecar_install_dirs_and_path_without_duplicates() {
        let home = Path::new("/Users/me");
        let exe = Path::new("/Applications/clean-my-mac.app/Contents/MacOS/cmm-desktop");
        let path_var = std::ffi::OsString::from("/opt/homebrew/bin:relative/dir:/Users/me/.cargo/bin:/custom/bin");
        let list = cmm_candidates(home, Some(exe), Some(&path_var));
        assert_eq!(
            list,
            [
                "/Applications/clean-my-mac.app/Contents/MacOS/cmm",
                "/Users/me/.cargo/bin/cmm",
                "/Users/me/.local/bin/cmm",
                "/opt/homebrew/bin/cmm",
                "/usr/local/bin/cmm",
                "/Users/me/bin/cmm",
                "/custom/bin/cmm",
            ]
            .map(PathBuf::from)
        );
        // The CLI itself comes first.
        let list = cmm_candidates(home, Some(Path::new("/x/bin/cmm")), None);
        assert_eq!(list[0], Path::new("/x/bin/cmm"));
        assert_eq!(list.iter().filter(|p| *p == Path::new("/x/bin/cmm")).count(), 1);
    }

    #[test]
    fn find_cmm_checks_the_version_output() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let impostor_dir = dir.path().join("impostor");
        let real_dir = dir.path().join("real");
        std::fs::create_dir_all(&impostor_dir).unwrap();
        std::fs::create_dir_all(&real_dir).unwrap();
        fake_cmm(&impostor_dir, "cmm", "Some Other Tool 2.0");
        let real = fake_cmm(&real_dir, "cmm", "cmm 0.1.0");
        let path_var = std::env::join_paths([&impostor_dir, &real_dir]).unwrap();
        assert!(!is_cmm(&impostor_dir.join("cmm")));
        assert!(is_cmm(&real));
        // No install locations outside the temp home: a `cmm` installed on this machine (Homebrew,
        // /usr/local/bin) must not be found before, or instead of, the fixtures.
        assert_eq!(find_cmm_with(&home, None, Some(&path_var), &[]), Some(real));
        assert_eq!(find_cmm_with(&home, None, Some(impostor_dir.as_os_str()), &[]), None);
    }

    #[test]
    fn dev_builds_are_recognized() {
        assert!(is_dev_build(Path::new("/Users/me/clean-my-mac/target/debug/cmm")));
        assert!(is_dev_build(Path::new("/Users/me/clean-my-mac/target/release/cmm")));
        assert!(!is_dev_build(Path::new("/Users/me/.cargo/bin/cmm")));
        assert!(!is_dev_build(Path::new("/Users/me/target/cmm")));
    }
}
