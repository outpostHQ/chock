//! Which files below a target directory cargo compiles: the targets and the modules they declare.

use std::path::Path;

use crate::project;

/// The directories cargo compiles targets from; a `.rs` file outside them, `build.rs` aside, is
/// not compiled.
pub(crate) const TARGET_DIRS: [&str; 4] = ["src", "tests", "examples", "benches"];

/// Whether a target in the file's target directory declares it as a module, such as a module that
/// integration tests share.
pub(super) fn declared_by_a_target(root: &Path, shown: &str, crates: &[String]) -> bool {
    let entries: Vec<(String, String)> = crates
        .iter()
        .filter_map(|dir| Some((dir, target_directory(project::under(shown, dir)?)?)))
        .flat_map(|(dir, targets)| targets_in(root, &join(dir, targets)))
        .filter_map(|entry| {
            let src = std::fs::read_to_string(root.join(&entry)).ok()?;
            Some((entry, src))
        })
        .collect();
    super::declared_beyond(root, crates, &entries)
        .is_ok_and(|found| found.iter().any(|(module, _)| module == shown))
}

/// The target directory a nested file sits below. `src/bin` is absent: all of `src/` already counts
/// as compiled.
fn target_directory(rest: &str) -> Option<&'static str> {
    match rest.split('/').collect::<Vec<_>>().as_slice() {
        ["tests", _, _, ..] => Some("tests"),
        ["examples", _, _, ..] => Some("examples"),
        ["benches", _, _, ..] => Some("benches"),
        _ => None,
    }
}

/// A path under a crate directory, where `""` is the project root and adds no separator.
pub(crate) fn join(dir: &str, rest: &str) -> String {
    if dir.is_empty() {
        rest.to_string()
    } else {
        format!("{dir}/{rest}")
    }
}

/// Each target cargo finds in one directory: a `.rs` file, or a subdirectory's `main.rs`. Symlinks
/// are not followed.
fn targets_in(root: &Path, dir: &str) -> Vec<String> {
    let Ok(listed) = std::fs::read_dir(root.join(dir)) else {
        return Vec::new();
    };
    listed
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry.file_type().ok()?;
            let main = std::fs::symlink_metadata(entry.path().join("main.rs"));
            if !kind.is_symlink() && main.is_ok_and(|held| held.file_type().is_file()) {
                return Some(format!("{dir}/{name}/main.rs"));
            }
            (name.ends_with(".rs") && kind.is_file()).then(|| format!("{dir}/{name}"))
        })
        .collect()
}
