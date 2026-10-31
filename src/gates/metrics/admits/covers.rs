//! `// checked: <crate> <test>` right above an escape in Rust: a test of the workspace checks what
//! the escape takes on trust, so the escape is not counted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// What a cover starts with, after the `//`.
const MARK: &str = "checked:";

/// The tests each crate of the workspace defines, read once the first cover names one.
pub(super) struct Covers<'a> {
    root: &'a Path,
    files: Vec<PathBuf>,
    tests: Option<BTreeMap<String, BTreeSet<String>>>,
}

impl<'a> Covers<'a> {
    /// The covers of a tree whose Rust files are `files`.
    pub(super) fn new(root: &'a Path, files: &[PathBuf]) -> Self {
        let rust = files
            .iter()
            .filter(|it| it.extension().is_some_and(|ext| ext == "rs"));
        Self {
            root,
            files: rust.cloned().collect(),
            tests: None,
        }
    }

    /// Why the `nth` escape on `line` still counts, or `None` where a cover right above it names
    /// a test its crate defines. Each cover in the run above covers one escape on the line.
    pub(super) fn uncovered(
        &mut self,
        lines: &[&str],
        line: usize,
        nth: usize,
    ) -> Result<Option<String>, String> {
        let Some(said) = above(lines, line).get(nth).copied() else {
            return Ok(Some(
                "no `// checked: <crate> <test>` right above it".to_string(),
            ));
        };
        let cover = format!("`// {MARK} {said}`");
        let [krate, test] = said.split_whitespace().collect::<Vec<_>>()[..] else {
            return Ok(Some(format!(
                "{cover} names other than one crate and one test"
            )));
        };
        let crates = self.tests()?;
        let Some(defined) = crates.get(&krate.replace('-', "_")) else {
            return Ok(Some(format!(
                "{cover}: the workspace has no crate `{krate}`"
            )));
        };
        if defined.contains(test) {
            return Ok(None);
        }
        Ok(Some(format!(
            "{cover}: crate `{krate}` defines no test `{test}`"
        )))
    }

    /// Each workspace crate, `-` read as `_`, with the names of the tests its files define. A file
    /// belongs to the deepest crate directory that holds it.
    fn tests(&mut self) -> Result<&BTreeMap<String, BTreeSet<String>>, String> {
        if self.tests.is_none() {
            let metadata = crate::project::metadata(self.root)?;
            self.tests = Some(defined(self.root, &metadata, &self.files)?);
        }
        Ok(self.tests.get_or_insert_default())
    }
}

/// What follows `// checked:` on each line of the run of `//` lines right above `line`, which
/// counts from 1. The top line comes first.
fn above<'t>(lines: &[&'t str], line: usize) -> Vec<&'t str> {
    let run = lines.get(..line.saturating_sub(1)).unwrap_or_default();
    let comments = run
        .iter()
        .rev()
        .map_while(|it| it.trim_start().strip_prefix("//"));
    let mut said: Vec<&str> = comments
        .filter_map(|it| it.trim_start().strip_prefix(MARK))
        .map(str::trim)
        .collect();
    said.reverse();
    said
}

/// The tests each member of the workspace that `metadata` describes defines in `files`.
fn defined(
    root: &Path,
    metadata: &str,
    files: &[PathBuf],
) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let read: Value = serde_json::from_str(metadata)
        .map_err(|e| format!("cargo metadata produced something unreadable: {e}"))?;
    let members = read["workspace_members"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let packages = read["packages"].as_array().into_iter().flatten();
    let base = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    // cargo prints each path as given, such as `D:\…` or through a link, and `base` is canonical
    // (`\\?\D:\…` on Windows): a member is read under the canonical `workspace_root`.
    let printed = read.get("workspace_root").and_then(Value::as_str);
    let printed = PathBuf::from(printed.unwrap_or_default());
    let canonical = printed.canonicalize().unwrap_or_else(|_| printed.clone());
    let crates: Vec<(String, PathBuf)> = packages
        .filter(|package| members.contains(&package["id"]))
        .map(|package| {
            let manifest = Path::new(package["manifest_path"].as_str().unwrap_or_default());
            let dir = manifest.parent().unwrap_or(manifest);
            let name = package["name"]
                .as_str()
                .unwrap_or_default()
                .replace('-', "_");
            let under = dir.strip_prefix(&printed).map(|rel| canonical.join(rel));
            let dir = under.unwrap_or_else(|_| dir.to_path_buf());
            (name, dir.strip_prefix(&base).unwrap_or(&dir).to_path_buf())
        })
        .collect();
    let mut tests: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, _) in &crates {
        tests.entry(name.clone()).or_default();
    }
    for file in files {
        let shown = crate::project::relative(root, file);
        let holding = crates
            .iter()
            .filter(|(_, dir)| Path::new(&shown).starts_with(dir));
        let Some((name, _)) = holding.max_by_key(|(_, dir)| dir.components().count()) else {
            continue;
        };
        let text = std::fs::read_to_string(file).unwrap_or_default();
        let scanned = crate::gates::metrics::testlint::scan::scan_source(&shown, &text);
        let names = scanned.tests.into_iter().map(|test| test.name);
        tests.entry(name.clone()).or_default().extend(names);
    }
    Ok(tests)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A file outside every member defines no test of a member, wherever the list holds it.
    /// Mutant: `break` for `continue` loses the member listed after it.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_outside_every_member_defines_no_test_of_a_member() {
        let root = crate::testdir::make("covers-defined");
        let lib = checks_at(&root, "crates/a/src/lib.rs");
        let loose = checks_at(&root, "tools/loose.rs");
        let metadata = one_member(&root.canonicalize().unwrap());
        let alone = defined(&root, &metadata, std::slice::from_ref(&lib)).unwrap();
        assert_eq!(alone, my_a_checks());
        assert_eq!(defined(&root, &metadata, &[loose, lib]).unwrap(), alone);
    }

    /// A member is read under the root however cargo spells the root, here through a link.
    /// Mutant: keep the directory as cargo printed it, and the member defines no test.
    #[cfg(unix)]
    #[test]
    fn a_member_printed_through_a_link_still_defines_its_tests() {
        let scratch = crate::testdir::make("covers-linked");
        let (root, alias) = (scratch.join("real"), scratch.join("alias"));
        let lib = checks_at(&root, "crates/a/src/lib.rs");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let found = defined(&root, &one_member(&alias), &[lib]);
        assert_eq!(found, Ok(my_a_checks()));
    }

    /// A file at `path` under `root` that defines the test `checks`.
    fn checks_at(root: &Path, path: &str) -> PathBuf {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "#[test]\nfn checks() {\n    assert!(f());\n}\n").unwrap();
        file
    }

    /// cargo's metadata for one member, `my-a` at `crates/a`, where cargo spells the root `shown`.
    fn one_member(shown: &Path) -> String {
        let manifest = shown.join("crates/a/Cargo.toml");
        let package = json!({"id": "a", "name": "my-a", "manifest_path": manifest});
        json!({"workspace_root": shown, "workspace_members": ["a"], "packages": [package]})
            .to_string()
    }

    /// What `defined` finds for `one_member` with `checks_at` its lib.
    fn my_a_checks() -> BTreeMap<String, BTreeSet<String>> {
        BTreeMap::from([("my_a".to_string(), BTreeSet::from(["checks".to_string()]))])
    }

    /// Only a `// checked:` line in the run of `//` lines right above a line covers it, top first;
    /// a blank or a code line ends the run. Mutant: `filter_map` for `map_while` reads past them.
    #[test]
    fn only_the_comment_lines_right_above_a_line_cover_it() {
        let lines = [
            "// checked: a far",
            "",
            "  // checked: a one",
            "// a note",
            "//checked: b two",
            "x();",
            "fn f() {}",
            "/// checked: a doc",
            "y();",
        ];
        assert_eq!(above(&lines, 6), ["a one", "b two"]);
        assert_eq!(above(&lines, 3), Vec::<&str>::new());
        assert_eq!(above(&lines, 7), Vec::<&str>::new());
        assert_eq!(above(&lines, 9), Vec::<&str>::new());
        assert_eq!(above(&lines, 1), Vec::<&str>::new());
    }
}
