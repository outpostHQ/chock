//! `.chock/mutest-kills.json`: each mutation one mutest run evaluated, the tests that killed it,
//! and a hash of the function that holds it. Each run writes the file anew, and git ignores it.

use std::collections::BTreeMap;
use std::path::Path;

use proc_macro2::LineColumn;
use serde::Serialize;
use syn::spanned::Spanned;
use syn::visit::Visit;

use super::results::Kill;

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

/// One function with a body: its name, where its item starts and ends, and the hash of its text.
struct Function {
    name: String,
    start: LineColumn,
    end: LineColumn,
    hash: String,
}

/// Every function with a body in the file at `shown`. None where the file cannot be read or parsed.
fn functions(root: &Path, shown: &str) -> Vec<Function> {
    let text = std::fs::read_to_string(root.join(shown)).unwrap_or_default();
    defined(shown, &text)
}

/// Every function with a body in `text`, the file at `shown`.
fn defined(shown: &str, text: &str) -> Vec<Function> {
    let Ok(file) = syn::parse_file(text) else {
        return Vec::new();
    };
    let mut named = Named {
        file: shown,
        text,
        path: Vec::new(),
        found: Vec::new(),
    };
    named.visit_file(&file);
    named.found
}

/// The SHA-256 of a function's text as the file holds it, from the start of the line of its `fn`
/// to its closing brace. Attributes and doc comments above that line are not in it.
fn body_hash(text: &str, fn_token: proc_macro2::Span, block: &syn::Block) -> String {
    let at = fn_token.byte_range().start;
    let first = text
        .get(..at)
        .and_then(|it| it.rfind('\n'))
        .map_or(0, |it| it + 1);
    let last = block.brace_token.span.close().byte_range().end;
    sha256(text.get(first..last).unwrap_or_default())
}

/// The SHA-256 of `text`, in 64 lowercase hex digits.
fn sha256(text: &str) -> String {
    crate::setup::sha256::of(text.as_bytes())
}

/// Collects each function under the modules, types and functions that hold it.
struct Named<'a> {
    file: &'a str,
    text: &'a str,
    path: Vec<String>,
    found: Vec<Function>,
}

impl Named<'_> {
    fn take(&mut self, item: proc_macro2::Span, sig: &syn::Signature, block: &syn::Block) {
        let parts = std::iter::once(self.file.to_string()).chain(self.path.iter().cloned());
        let name = parts.chain([sig.ident.to_string()]).collect::<Vec<_>>();
        self.found.push(Function {
            name: name.join("::"),
            start: item.start(),
            end: block.brace_token.span.close().end(),
            hash: body_hash(self.text, sig.fn_token.span, block),
        });
    }

    /// Visits what `visit` reaches under `name`.
    fn under(&mut self, name: String, visit: impl FnOnce(&mut Self)) {
        self.path.push(name);
        visit(self);
        self.path.pop();
    }

    /// The type an `impl` is for, by its last name: `Counter` for `impl<T> Show for a::Counter<T>`.
    fn type_name(&self, ty: &syn::Type) -> String {
        if let syn::Type::Path(path) = ty
            && let Some(last) = path.path.segments.last()
        {
            return last.ident.to_string();
        }
        let written = self.text.get(ty.span().byte_range()).unwrap_or_default();
        written.split_whitespace().collect()
    }
}

impl<'ast> Visit<'ast> for Named<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.under(node.ident.to_string(), |it| {
            syn::visit::visit_item_mod(it, node);
        });
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let name = self.type_name(&node.self_ty);
        self.under(name, |it| syn::visit::visit_item_impl(it, node));
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        self.under(node.ident.to_string(), |it| {
            syn::visit::visit_item_trait(it, node);
        });
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.take(node.span(), &node.sig, &node.block);
        self.under(node.sig.ident.to_string(), |it| {
            syn::visit::visit_item_fn(it, node);
        });
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.take(node.span(), &node.sig, &node.block);
        self.under(node.sig.ident.to_string(), |it| {
            syn::visit::visit_impl_item_fn(it, node);
        });
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        if let Some(block) = &node.default {
            self.take(node.span(), &node.sig, block);
        }
        self.under(node.sig.ident.to_string(), |it| {
            syn::visit::visit_trait_item_fn(it, node);
        });
    }
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

    /// A trait's default method is named under the trait, and an `impl` for a type that is no path
    /// by its text; a file that does not parse defines nothing. Mutant: skipping traits.
    #[test]
    fn a_trait_default_and_an_impl_for_a_written_type_are_named_too() {
        let text = "trait Show {\n    fn show(&self) -> bool { true }\n    fn bare(&self);\n}\n\n\
                    impl Show for [u8; 2] {\n    fn bare(&self) {}\n}\n";
        let found = defined("src/lib.rs", text).into_iter();
        let names: Vec<String> = found.map(|it| it.name).collect();
        assert_eq!(
            names,
            ["src/lib.rs::Show::show", "src/lib.rs::[u8;2]::bare"]
        );
        assert!(defined("src/lib.rs", "fn (").is_empty());
    }

    /// The hash is SHA-256 in 64 lowercase hex digits, as the standard's test vectors give it.
    /// Mutant: std's `DefaultHasher` gives 16 digits that no vector matches.
    #[test]
    fn a_body_hash_is_the_sha_256_of_the_text() {
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256("abc"), abc);
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(sha256(""), empty);
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
