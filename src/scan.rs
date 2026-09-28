//! Finding Cargo `target` directories on the fixed drives.
//!
//! A directory qualifies only when every check holds: it is named `target`,
//! its parent has a `Cargo.toml`, it carries Cargo's cache marker
//! (`CACHEDIR.TAG` with the cache-directory signature, or `.rustc_info.json`),
//! and neither it nor anything on the way to it is a reparse point. The same
//! verification runs again immediately before a deletion, so a directory the
//! scanner missed can never be removed by a stale row.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use crate::format;

/// Cargo's cache-directory signature, the first line of `target/CACHEDIR.TAG`.
const CACHE_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";
/// `winnt.h`: `FILE_ATTRIBUTE_REPARSE_POINT` — a symlink, junction or other
/// reparse point. The walker never traverses one.
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
/// Directories that never hold a Cargo target and cost a lot to walk.
const SKIP: &[&str] = &[
    "$recycle.bin",
    "$winreagent",
    "config.msi",
    "msocache",
    "node_modules",
    "onedrivetemp",
    "perflogs",
    "program files",
    "program files (x86)",
    "programdata",
    "recovery",
    "system volume information",
    "windows",
    "windows.old",
    ".cargo",
    ".git",
    ".hg",
    ".rustup",
    ".svn",
];
/// How often the walker reports progress.
const PROGRESS_EVERY: Duration = Duration::from_millis(150);

/// One verified Cargo target directory, with the numbers the list shows.
#[derive(Clone, Debug)]
pub struct Target {
    /// The `…\project\target` directory.
    pub path: PathBuf,
    /// The project directory's name.
    pub project: String,
    /// Total logical bytes under the target directory.
    pub size: u64,
    /// The number of files under the target directory.
    pub files: u64,
    /// The newest file modification time seen, if any.
    pub modified: Option<SystemTime>,
    /// `size` preformatted for the list.
    pub size_text: String,
    /// `modified` preformatted for the list.
    pub modified_text: String,
}

/// What a scan found and what it could not read.
#[derive(Debug, Default)]
pub struct ScanOutcome {
    /// The verified target directories, in the order they were found.
    pub targets: Vec<Target>,
    /// The directories visited.
    pub dirs: u64,
    /// Entries that could not be read (permissions, locked files).
    pub errors: u64,
    /// Whether the scan stopped early because it was cancelled.
    pub cancelled: bool,
}

/// The fixed local drives to scan, as `C:\`-style roots.
#[cfg(windows)]
pub fn fixed_drives() -> Vec<PathBuf> {
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    use windows::Win32::System::WindowsProgramming::DRIVE_FIXED;
    use windows::core::PCWSTR;

    let mask = unsafe { GetLogicalDrives() };
    let mut roots = Vec::new();
    for index in 0..26u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let letter = (b'A' + index as u8) as char;
        let root = format!("{letter}:\\");
        let mut wide: Vec<u16> = root.encode_utf16().collect();
        wide.push(0);
        let kind = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
        if kind == DRIVE_FIXED {
            roots.push(PathBuf::from(root));
        }
    }
    roots
}

/// A non-Windows placeholder: the app is built for Windows, this keeps the
/// crate checkable elsewhere.
#[cfg(not(windows))]
pub fn fixed_drives() -> Vec<PathBuf> {
    vec![PathBuf::from("/")]
}

/// Walks `roots` and returns every verified target directory. `progress` is
/// called while walking; it receives the current directory, the number of
/// directories visited and the number of targets found.
pub fn scan(
    roots: &[PathBuf],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Path, u64, usize),
) -> ScanOutcome {
    let mut walker = Walker {
        cancel,
        progress,
        out: ScanOutcome::default(),
        last_report: Instant::now(),
    };
    for root in roots {
        walker.walk(root);
        if walker.cancelled() {
            break;
        }
    }
    walker.out.cancelled = walker.cancelled();
    walker.out
}

struct Walker<'a> {
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(&Path, u64, usize),
    out: ScanOutcome,
    last_report: Instant,
}

impl Walker<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn walk(&mut self, dir: &Path) {
        if self.cancelled() {
            return;
        }
        self.out.dirs += 1;
        if self.last_report.elapsed() >= PROGRESS_EVERY {
            self.last_report = Instant::now();
            (self.progress)(dir, self.out.dirs, self.out.targets.len());
        }

        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => {
                self.out.errors += 1;
                return;
            }
        };
        for entry in entries {
            if self.cancelled() {
                return;
            }
            let Ok(entry) = entry else {
                self.out.errors += 1;
                continue;
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if SKIP.contains(&name.to_ascii_lowercase().as_str()) {
                continue;
            }
            // `DirEntry::metadata` does not traverse a symlink.
            let Ok(metadata) = entry.metadata() else {
                self.out.errors += 1;
                continue;
            };
            if is_reparse(&metadata) || !metadata.is_dir() {
                continue;
            }
            let child = entry.path();
            if name.eq_ignore_ascii_case("target") && is_project_target(&child) {
                self.out.targets.push(measure(&child));
                // A verified target holds build artifacts, not projects.
                continue;
            }
            self.walk(&child);
        }
    }
}

/// Verifies that `path` is a Cargo `target` directory that may be deleted:
/// named `target`, not a reparse point itself, on a path with no reparse
/// point, and marked by Cargo (`Cargo.toml` next to it plus a cache marker).
pub fn verify_target(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("path is not absolute".into());
    }
    if !path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("target"))
    {
        return Err("the directory is not named `target`".into());
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("the path contains `..`".into());
    }

    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if !metadata.is_dir() {
        return Err("not a directory".into());
    }
    if is_reparse(&metadata) {
        return Err("the target directory is a reparse point (symlink or junction)".into());
    }

    // A symlinked project directory would make the path point outside where
    // the user believes it does, so refuse any reparse point on the way up.
    let mut ancestor = path.parent();
    while let Some(dir) = ancestor {
        let metadata = fs::symlink_metadata(dir)
            .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
        if is_reparse(&metadata) {
            return Err(format!(
                "{} is a reparse point (symlink or junction)",
                dir.display()
            ));
        }
        if !metadata.is_dir() {
            return Err(format!("{} is not a directory", dir.display()));
        }
        ancestor = dir.parent();
    }

    if !is_project_target(path) {
        return Err(
            "not a Cargo target directory (missing Cargo.toml or the Cargo cache marker)".into(),
        );
    }
    Ok(())
}

/// Whether `path` looks like a Cargo `target`: its parent has a regular
/// `Cargo.toml` and the directory carries a Cargo cache marker.
fn is_project_target(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if !is_plain_file(&parent.join("Cargo.toml")) {
        return false;
    }
    let tag = path.join("CACHEDIR.TAG");
    if is_plain_file(&tag) && fs::read(&tag).is_ok_and(|bytes| bytes.starts_with(CACHE_SIGNATURE)) {
        return true;
    }
    is_plain_file(&path.join(".rustc_info.json"))
}

/// Whether `path` is a regular file and not a reparse point.
fn is_plain_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file() && !is_reparse(&metadata))
}

/// Whether `metadata` describes a reparse point (symlink, junction or other
/// Windows reparse tag). Renamed for its use in the safety checks.
pub(crate) fn is_reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Sums the files under a verified target, never descending into a reparse
/// point.
fn measure(path: &Path) -> Target {
    let mut size = 0u64;
    let mut files = 0u64;
    let mut modified: Option<SystemTime> = None;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // `DirEntry::metadata` does not traverse a symlink.
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if is_reparse(&metadata) {
                continue;
            }
            if let Ok(time) = metadata.modified() {
                modified = Some(modified.map_or(time, |seen| seen.max(time)));
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                size += metadata.len();
                files += 1;
            }
        }
    }
    Target {
        project: path
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: path.to_path_buf(),
        size,
        files,
        modified,
        size_text: format::human_size(size),
        modified_text: modified
            .map(format::local_stamp)
            .unwrap_or_else(|| "unknown".into()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// A unique scratch directory that removes itself.
    pub(crate) struct Scratch(PathBuf);

    impl Scratch {
        pub(crate) fn new(label: &str) -> Scratch {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("targetsweeper-{label}-{}-{id}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create the scratch directory");
            Scratch(path)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Writes a project with a Cargo-marked target under `root`.
    pub(crate) fn cargo_target(root: &Path, project: &str) -> PathBuf {
        let project_dir = root.join(project);
        let target = project_dir.join("target");
        fs::create_dir_all(&target).expect("create the target directory");
        fs::write(project_dir.join("Cargo.toml"), "[package]\nname = \"x\"\n")
            .expect("write Cargo.toml");
        fs::write(
            target.join("CACHEDIR.TAG"),
            format!(
                "{}{}",
                String::from_utf8_lossy(CACHE_SIGNATURE),
                "\n# cache\n"
            ),
        )
        .expect("write the cache tag");
        fs::create_dir_all(target.join("debug")).expect("create a build directory");
        fs::write(target.join("debug").join("app.exe"), vec![0u8; 2048])
            .expect("write an artifact");
        target
    }

    #[test]
    fn a_scan_finds_only_verified_targets() {
        let scratch = Scratch::new("scan");
        let root = scratch.path();
        let real = cargo_target(root, "real");
        // A `target` without a Cargo.toml next to it is not a Cargo target.
        fs::create_dir_all(root.join("stray").join("target")).unwrap();
        // A project whose target lacks both markers is not verified.
        let no_marker = root.join("unmarked");
        fs::create_dir_all(no_marker.join("target")).unwrap();
        fs::write(no_marker.join("Cargo.toml"), "[package]\n").unwrap();
        // A directory that is skipped outright.
        fs::create_dir_all(root.join("node_modules").join("x").join("target")).unwrap();

        let cancel = AtomicBool::new(false);
        let outcome = scan(&[root.to_path_buf()], &cancel, &mut |_, _, _| {});
        let found: Vec<&Path> = outcome.targets.iter().map(|t| t.path.as_path()).collect();
        assert_eq!(found, vec![real.as_path()]);
        assert!(outcome.targets[0].size >= 2048);
        assert!(outcome.targets[0].files >= 1);
        assert!(!outcome.cancelled);
    }

    #[test]
    fn a_scan_stops_on_cancel_and_keeps_what_it_found() {
        let scratch = Scratch::new("cancel");
        let root = scratch.path();
        let real = cargo_target(root, "real");
        let cancel = AtomicBool::new(true);
        let outcome = scan(&[root.to_path_buf()], &cancel, &mut |_, _, _| {});
        assert!(outcome.cancelled);
        assert!(outcome.targets.is_empty(), "{real:?} was not reached");
    }

    #[test]
    fn verify_accepts_a_cargo_target_and_rejects_everything_else() {
        let scratch = Scratch::new("verify");
        let root = scratch.path();
        let target = cargo_target(root, "real");
        assert!(verify_target(&target).is_ok());

        assert!(
            verify_target(&root.join("real")).is_err(),
            "not named target"
        );
        assert!(verify_target(&root.join("missing").join("target")).is_err());
        fs::create_dir_all(root.join("stray").join("target")).unwrap();
        assert!(
            verify_target(&root.join("stray").join("target")).is_err(),
            "no Cargo.toml"
        );
        assert!(verify_target(Path::new("target")).is_err(), "not absolute");
    }
}
