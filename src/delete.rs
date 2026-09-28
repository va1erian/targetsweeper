//! Deleting verified target directories, and nothing else.
//!
//! Every path is verified again immediately before it is touched, and the
//! recursive removal never follows a symlink or junction: a reparse point is
//! removed as the link itself, so a `target` containing one can never reach
//! outside its own tree. `std::fs::remove_dir_all` is deliberately not used.

use std::fs;
use std::path::{Path, PathBuf};

use crate::format;
use crate::scan;

/// What one deletion run removed and what it could not.
#[derive(Debug, Default)]
pub struct DeleteReport {
    /// The target directories fully removed.
    pub removed: Vec<PathBuf>,
    /// Total bytes freed by the removals.
    pub freed: u64,
    /// The directories that could not be fully removed, with the reason.
    pub failures: Vec<(PathBuf, String)>,
}

impl DeleteReport {
    /// The freed bytes, human-readable.
    pub fn freed_text(&self) -> String {
        format::human_size(self.freed)
    }
}

/// Verifies and removes one target directory, returning the bytes freed.
pub fn delete_target(path: &Path) -> Result<u64, String> {
    scan::verify_target(path)?;
    let mut removal = Removal::default();
    remove_tree(path, &mut removal);
    if removal.errors.is_empty() {
        Ok(removal.freed)
    } else {
        Err(format!(
            "{} item(s) could not be removed (first: {})",
            removal.errors.len(),
            removal.errors[0]
        ))
    }
}

#[derive(Default)]
struct Removal {
    freed: u64,
    errors: Vec<String>,
}

/// Empties `dir` and removes it, collecting per-entry failures instead of
/// aborting, so one locked file does not stop the rest.
fn remove_tree(dir: &Path, out: &mut Removal) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            out.errors.push(format!("{}: {error}", dir.display()));
            return;
        }
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                out.errors.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        if scan::is_reparse(&metadata) {
            // Remove the link itself; its target is never touched. A
            // directory link (junction or directory symlink) must go through
            // `remove_dir`: `Metadata::is_dir` is false for reparse points.
            let result = if scan::is_directory(&metadata) {
                fs::remove_dir(&path)
            } else {
                fs::remove_file(&path)
            };
            if let Err(error) = result {
                out.errors.push(format!("{}: {error}", path.display()));
            }
        } else if metadata.is_dir() {
            remove_tree(&path, out);
        } else {
            match remove_file(path.as_path(), &metadata) {
                Ok(()) => out.freed += metadata.len(),
                Err(error) => out.errors.push(format!("{}: {error}", path.display())),
            }
        }
    }
    if let Err(error) = fs::remove_dir(dir) {
        out.errors.push(format!("{}: {error}", dir.display()));
    }
}

/// Removes a file, clearing a read-only attribute and retrying once (Cargo
/// leaves some artifacts read-only).
// The clippy lint is about Unix world-writability; this app is Windows-only,
// where `set_readonly(false)` just clears FILE_ATTRIBUTE_READONLY.
#[allow(clippy::permissions_set_readonly_false)]
fn remove_file(path: &Path, metadata: &fs::Metadata) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(first) => {
            let mut permissions = metadata.permissions();
            if !permissions.readonly() {
                return Err(first);
            }
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions)?;
            fs::remove_file(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::tests::{Scratch, cargo_target};

    #[test]
    fn deleting_a_target_removes_its_tree_and_keeps_the_project() {
        let scratch = Scratch::new("delete");
        let root = scratch.path();
        let target = cargo_target(root, "real");
        let freed = delete_target(&target).expect("delete");

        assert!(!target.exists(), "the target tree is gone");
        assert!(
            root.join("real").join("Cargo.toml").exists(),
            "the manifest stayed"
        );
        assert!(root.join("real").exists(), "the project directory stayed");
        assert!(freed >= 2048);
    }

    #[test]
    fn a_non_target_directory_is_refused() {
        let scratch = Scratch::new("refuse");
        let root = scratch.path();
        let plain = root.join("plain");
        fs::create_dir_all(&plain).unwrap();
        fs::write(plain.join("keep.txt"), "keep").unwrap();

        assert!(delete_target(&plain).is_err());
        assert!(plain.join("keep.txt").exists(), "nothing was removed");
    }

    #[test]
    fn a_reparse_point_inside_a_target_is_removed_as_a_link() {
        let scratch = Scratch::new("reparse");
        let root = scratch.path();
        let target = cargo_target(root, "real");
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("precious.txt"), "precious").unwrap();

        // A symbolic link needs Developer Mode or elevation; a junction does
        // not, and it is the more common escape vector on Windows.
        if !make_directory_link(&target.join("link"), &outside) {
            eprintln!("skipping: cannot create a directory link here");
            return;
        }

        let freed = delete_target(&target).expect("delete");
        assert!(freed >= 2048);
        assert!(!target.exists(), "the target tree is gone");
        assert!(
            outside.join("precious.txt").exists(),
            "the link's target was never followed"
        );
    }

    /// Creates a directory symlink, falling back to a junction, which needs no
    /// privilege. Returns whether a link was created.
    fn make_directory_link(link: &Path, target: &Path) -> bool {
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return true;
        }
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .is_ok_and(|status| status.success())
    }
}
