//! `.chock/mutest-kills.json`: each mutation one mutest run evaluated, the tests that killed it,
//! and a hash of the function that holds it. Each run writes the file anew, and git ignores it.

use std::collections::BTreeMap;
use std::path::Path;

use proc_macro2::LineColumn;
use serde::Serialize;

use super::results::Kill;

mod defined;
use defined::{Function, defined};

/// The file, from the root of the project.
pub const FILE: &str = ".chock/mutest-kills.json";

#[derive(Serialize)]
struct Record {
    version: u32,
    chock: &'static str,
    /// HEAD in git, or `None` where git holds no commit.
    commit: Option<String>,
    /// Whether the tree differed from `commit` when the run began.
    dirty: bool,
    targets: Vec<Row>,
}

/// One mutation, its verdict, and the function that holds it.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct Row {
    file: String,
    /// `file.rs::Type::fn`, or `None` where no function in the file holds the mutation.
    function: Option<String>,
    line: u32,
    operator: String,
    mutation: String,
    verdict: &'static str,
    tests: Vec<String>,
    body_sha256: Option<String>,
}

/// Whether the tree a run measured held changes that its commit does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Worktree {
    /// The tree is the commit.
    Clean,
    /// The tree holds changes, or chock could not tell.
    Dirty,
}

impl Worktree {
    /// The tree whose `changed` files, if chock could read them, are the ones listed.
    pub(super) fn of(changed: Option<&BTreeMap<String, Option<u64>>>) -> Self {
        if changed.is_some_and(BTreeMap::is_empty) {
            Self::Clean
        } else {
            Self::Dirty
        }
    }
}

/// Writes the record of one run's `kills` under `root`, over the record of the run before.
pub(super) fn write(
    root: &Path,
    kills: Vec<Kill>,
    commit: Option<String>,
    tree: Worktree,
) -> Result<(), String> {
    let record = Record {
        version: 1,
        chock: env!("CARGO_PKG_VERSION"),
        commit,
        dirty: tree == Worktree::Dirty,
        targets: rows(root, kills),
    };
    let path = root.join(FILE);
    let cannot = |error: std::io::Error| format!("cannot write {FILE}: {error}");
    std::fs::create_dir_all(root.join(".chock")).map_err(cannot)?;
    let text = crate::project::document::render(&record);
    crate::project::document::write(&path, &text).map_err(cannot)
}

/// Each kill with the function that holds it. Each file is read and parsed once.
fn rows(root: &Path, kills: Vec<Kill>) -> Vec<Row> {
    let mut parsed: BTreeMap<String, Vec<Function>> = BTreeMap::new();
    let mut row = |kill: Kill| {
        let held = parsed
            .entry(kill.file.clone())
            .or_insert_with(|| functions(root, &kill.file));
        // mutest counts columns from 1, and the parser from 0.
        let at = LineColumn {
            line: usize::try_from(kill.line).unwrap_or(usize::MAX),
            column: usize::try_from(kill.column.saturating_sub(1)).unwrap_or(usize::MAX),
        };
        let holding = held.iter().filter(|it| it.start <= at && at <= it.end);
        let innermost = holding.max_by_key(|it| it.start);
        Row {
            function: innermost.map(|it| it.name.clone()),
            body_sha256: innermost.map(|it| it.hash.clone()),
            file: kill.file,
            line: kill.line,
            operator: kill.operator,
            mutation: kill.mutation,
            verdict: kill.verdict,
            tests: kill.tests,
        }
    };
    kills.into_iter().map(&mut row).collect()
}

/// Every function with a body in the file at `shown`. None where the file cannot be read or parsed.
fn functions(root: &Path, shown: &str) -> Vec<Function> {
    let text = std::fs::read_to_string(root.join(shown)).unwrap_or_default();
    defined(shown, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const LIB: &str = "pub struct Counter {\n    n: u32,\n}\n\nimpl Counter {\n    /// Adds.\n    \
                       pub fn add(&mut self, by: u32) -> u32 {\n        self.n += by;\n        self.n\n    \
                       }\n}\n\npub fn is_big(n: u32) -> bool {\n    n > 10\n}\n\nmod inner {\n    \
                       pub fn outer() -> bool {\n        fn nested() -> bool { true }\n        nested()\n    \
                       }\n}\n";

    #[test]
    fn a_tree_is_clean_only_when_chock_read_it_and_found_no_change() {
        let changed = |files: &[&str]| {
            files
                .iter()
                .map(|file| ((*file).to_string(), None))
                .collect()
        };
        assert_eq!(Worktree::of(Some(&BTreeMap::new())), Worktree::Clean);
        assert_eq!(Worktree::of(Some(&changed(&["src/a.rs"]))), Worktree::Dirty);
        assert_eq!(Worktree::of(None), Worktree::Dirty);
    }

    fn kill(line: u32, column: u32, verdict: &'static str) -> Kill {
        Kill {
            file: "src/lib.rs".to_string(),
            line,
            column,
            operator: "op".to_string(),
            mutation: format!("change at {line}"),
            verdict,
            tests: vec![format!("tests::at_{line}")],
        }
    }

    /// Each kill is one row under the innermost function that holds it, survivors too. Mutant:
    /// `kills.into_iter().filter(|it| it.verdict != "survived")` drops the survivors' rows.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn each_kill_is_one_row_under_the_innermost_function_that_holds_it() {
        let root = crate::testdir::tree("mutest-kills", &[("src/lib.rs", LIB)]);
        let kills = vec![
            kill(8, 16, "killed"),
            kill(14, 7, "survived"),
            kill(19, 10, "timed_out"),
            kill(2, 5, "survived"),
        ];
        write(&root, kills, Some("abc".to_string()), Worktree::Dirty).unwrap();
        let text = std::fs::read_to_string(root.join(FILE)).unwrap();
        let record: serde_json::Value = serde_json::from_str(&text).unwrap();
        let head = (&record["version"], &record["commit"], &record["dirty"]);
        assert_eq!(head, (&json!(1), &json!("abc"), &json!(true)));
        assert_eq!(record["chock"], env!("CARGO_PKG_VERSION"));
        let rows = record["targets"].as_array().unwrap();
        let cells = |row: &serde_json::Value| {
            let named = row["function"].as_str().unwrap_or("none").to_string();
            (named, row["verdict"].to_string(), row["tests"].to_string())
        };
        let said: Vec<_> = rows.iter().map(cells).collect();
        let row = |named: &str, verdict: &str, line: u32| {
            let tests = format!(r#"["tests::at_{line}"]"#);
            (named.to_string(), format!("\"{verdict}\""), tests)
        };
        let expected = [
            row("src/lib.rs::Counter::add", "killed", 8),
            row("src/lib.rs::is_big", "survived", 14),
            row("src/lib.rs::inner::outer::nested", "timed_out", 19),
            row("none", "survived", 2),
        ];
        assert_eq!(said, expected);
        assert!(rows[3]["body_sha256"].is_null());
    }

    /// A function's hash changes with its own text and not with another function's, even where that
    /// moves it down the file. Mutant: hashing the whole file gives every function a new hash.
    #[test]
    fn a_body_hash_changes_with_its_own_function_and_no_other() {
        let hashes = |text: &str| -> Vec<(String, String)> {
            let found = defined("src/lib.rs", text).into_iter();
            found.map(|it| (it.name, it.hash)).collect()
        };
        let before = hashes(LIB);
        let names: Vec<&str> = before.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "src/lib.rs::Counter::add",
                "src/lib.rs::is_big",
                "src/lib.rs::inner::outer",
                "src/lib.rs::inner::outer::nested"
            ]
        );
        let after = hashes(&LIB.replace("n > 10", "let big = 10;\n    n > big"));
        let changed = before.iter().zip(&after).filter(|(was, now)| was != now);
        let edited: Vec<&str> = changed.map(|((name, _), _)| name.as_str()).collect();
        assert_eq!(edited, ["src/lib.rs::is_big"]);
    }
}
