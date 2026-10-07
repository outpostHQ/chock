//! Production lines: a Rust file's lines once its test code is taken out. Parsed rather than
//! matched, so a module gated from its parent's file is found.

use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::TokenTree;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{AttrStyle, Attribute, ItemMod, Meta};

use crate::project;
use crate::run::baseline::Series;

/// Whether a file name marks a whole-file test module. The walk drops these by name, since their
/// `#[cfg(test)]` sits on the parent's `mod` line.
#[must_use]
pub fn is_test_file(name: &str) -> bool {
    name == "tests.rs" || name.ends_with("_tests.rs")
}

/// The parsed file, or `line N: reason` for the caller to prefix with the path.
pub(crate) fn parse_rust(src: &str) -> Result<syn::File, String> {
    syn::parse_file(src).map_err(|e| format!("line {}: {e}", e.span().start().line))
}

/// Every production file with its line count. Files gated `#[cfg(test)]` from another file are
/// dropped once every file has been scanned.
pub fn measure(ctx: &crate::run::Ctx) -> Result<Vec<(PathBuf, usize)>, String> {
    let root = &ctx.root;
    let (paths, crates) = walked(ctx)?;
    let mut counted: Vec<(PathBuf, usize, Vec<PathBuf>)> = Vec::new();
    for path in paths {
        let shown = project::relative(root, &path);
        let src = fs::read_to_string(&path).map_err(|e| format!("{shown}: {e}"))?;
        let (lines, mods) = match scan(&src) {
            Ok(found) => found,
            Err(_)
                if !project::is_crate_code(&shown, &crates)
                    || project::only_included(root, &shown) =>
            {
                continue;
            }
            Err(why) => return Err(format!("{shown}: {why}")),
        };
        let gated = gated_paths(&path, &mods);
        counted.push((path, lines, gated));
    }
    let gated: Vec<PathBuf> = counted.iter().flat_map(|(_, _, g)| g.clone()).collect();
    Ok(counted
        .into_iter()
        .filter(|(path, _, _)| !gated.iter().any(|g| path.starts_with(g)))
        .map(|(path, lines, _)| (path, lines))
        .collect())
}

/// One source split into (production text, test text): `#[cfg(test)]` modules, or a whole file
/// gated test-only, go to the second.
pub fn split(src: &str) -> Result<(String, String), String> {
    let file = parse_rust(src)?;
    if file.attrs.iter().any(is_test_gate) {
        return Ok((String::new(), src.to_string()));
    }
    let mut mods = Mods::default();
    mods.visit_file(&file);
    let (mut production, mut tests) = (String::new(), String::new());
    for (index, line) in src.split('\n').enumerate() {
        let at = index + 1;
        let held = mods.found.iter().any(|m| (m.from..=m.to).contains(&at));
        let into = if held { &mut tests } else { &mut production };
        into.push_str(line);
        into.push('\n');
    }
    Ok((production, tests))
}

/// The production line count and the test modules, from one parse. Lines are `split('\n')`, so a
/// trailing newline adds one.
fn scan(src: &str) -> Result<(usize, Vec<TestMod>), String> {
    let file = parse_rust(src)?;
    let mut mods = Mods::default();
    mods.visit_file(&file);
    if file.attrs.iter().any(is_test_gate) {
        return Ok((0, mods.found));
    }
    let covered = merged(mods.found.iter().map(|m| (m.from, m.to)).collect());
    Ok((src.split('\n').count().saturating_sub(covered), mods.found))
}

/// A `#[cfg(test)]` module: the lines it occupies, and, when it is a `mod x;`, the file it names
/// and wherever `#[path]` sends that file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TestMod {
    from: usize,
    to: usize,
    declares: Option<String>,
    redirect: Option<String>,
}

#[derive(Debug, Default)]
struct Mods {
    found: Vec<TestMod>,
}

impl<'ast> Visit<'ast> for Mods {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        if !node.attrs.iter().any(is_test_gate) {
            syn::visit::visit_item_mod(self, node);
            return;
        }
        self.found.push(TestMod {
            from: first_line(node),
            to: last_line(node),
            declares: node.semi.is_some().then(|| node.ident.to_string()),
            redirect: path_attribute(&node.attrs),
        });
    }
}

/// The first line the module occupies, counting its outer attributes.
fn first_line(node: &ItemMod) -> usize {
    let declared = node.mod_token.span().start().line;
    node.attrs
        .iter()
        .filter(|attr| matches!(attr.style, AttrStyle::Outer))
        .map(|attr| attr.span().start().line)
        .chain(std::iter::once(declared))
        .min()
        .unwrap_or(declared)
}

/// The last line the module occupies: its closing brace, or the `;` ending a declaration.
fn last_line(node: &ItemMod) -> usize {
    if let Some((brace, _)) = &node.content {
        return brace.span.close().end().line;
    }
    node.semi.as_ref().map_or_else(
        || node.mod_token.span().end().line,
        |semi| semi.span().end().line,
    )
}

#[must_use]
pub fn is_test_gated(attrs: &[Attribute]) -> bool {
    attrs.iter().any(is_test_gate)
}

/// Whether a `cfg` turns its item on for tests: `#[cfg(test)]`, or `test` inside any `all`/`any`.
pub(crate) fn is_test_gate(attr: &Attribute) -> bool {
    let Meta::List(list) = &attr.meta else {
        return false;
    };
    list.path.is_ident("cfg") && names_test(&list.tokens)
}

/// Whether `test` appears as an identifier in the predicate, skipping anything under `not`.
fn names_test(tokens: &proc_macro2::TokenStream) -> bool {
    let mut rest = tokens.clone().into_iter();
    while let Some(tree) = rest.next() {
        match tree {
            TokenTree::Ident(id) if id == "not" => {
                let _ = rest.next();
            }
            TokenTree::Ident(id) => {
                if id == "test" {
                    return true;
                }
            }
            TokenTree::Group(group) => {
                if names_test(&group.stream()) {
                    return true;
                }
            }
            TokenTree::Punct(_) | TokenTree::Literal(_) => {}
        }
    }
    false
}

/// The path in the last non-empty `#[path = "..."]` attribute, verbatim.
fn path_attribute(attrs: &[Attribute]) -> Option<String> {
    attrs.iter().rev().find_map(|attr| {
        let Meta::NameValue(pair) = &attr.meta else {
            return None;
        };
        if !pair.path.is_ident("path") {
            return None;
        }
        match &pair.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(text),
                ..
            }) => {
                let value = text.value();
                (!value.is_empty()).then_some(value)
            }
            _ => None,
        }
    })
}

/// Lines the ranges cover, counting an overlap once.
fn merged(mut spans: Vec<(usize, usize)>) -> usize {
    spans.sort_unstable();
    let (mut covered, mut reach) = (0usize, 0usize);
    for (from, to) in spans {
        let start = from.max(reach + 1);
        if to >= start {
            covered += to - start + 1;
        }
        reach = reach.max(to);
    }
    covered
}

/// Where each `#[cfg(test)] mod x;` puts its code: its `#[path]` target, or both `x.rs` and `x/`.
fn gated_paths(file: &Path, mods: &[TestMod]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for gated in mods {
        let Some(name) = &gated.declares else {
            continue;
        };
        // `#[path]` is relative to the directory holding the file the attribute is written in.
        if let Some(rel) = &gated.redirect {
            out.extend(file.parent().map(|dir| dir.join(rel)));
            continue;
        }
        let Some(dir) = module_dir(file) else {
            continue;
        };
        out.push(dir.join(format!("{name}.rs")));
        out.push(dir.join(name));
    }
    out
}

/// Where `file`'s submodules live: the crate's `src/` for a root, the directory beside a `foo.rs`,
/// the parent of a `mod.rs`. A `foo.rs` with no `foo/` beside it owns nothing on disk.
pub(crate) fn module_dir(file: &Path) -> Option<PathBuf> {
    let stem = file.file_stem()?.to_str()?;
    let parent = file.parent()?;
    if stem == "mod" {
        return Some(parent.to_path_buf());
    }
    if stem == "lib" || stem == "main" {
        // A crate with both roots declares its modules in `lib.rs`; `main.rs` then reaches them
        // through the library rather than owning them.
        let root = stem == "lib" || !parent.join("lib.rs").is_file();
        let in_src = parent.file_name().is_some_and(|name| name == "src");
        return (root && in_src).then(|| parent.to_path_buf());
    }
    let beside = file.with_extension("");
    beside.is_dir().then_some(beside)
}

/// Directories holding no production Rust: the tree-wide skip and fixture lists, `tests`, and every
/// dot-directory.
pub(crate) fn skip_dir(name: &str) -> bool {
    project::SKIPPED.contains(&name)
        || project::FIXTURE_DIRS.contains(&name)
        || name == "tests"
        || name.starts_with('.')
}

/// Runs `of` over every source the walk keeps, including files gated test-only from a parent. An
/// error is fatal only for a file some crate compiles.
pub fn for_each_source<T>(
    ctx: &crate::run::Ctx,
    of: &dyn Fn(&str) -> Result<T, String>,
) -> Result<Vec<(String, T)>, String> {
    let root = &ctx.root;
    let (paths, crates) = walked(ctx)?;
    let mut out = Vec::new();
    for path in &paths {
        let shown = project::relative(root, path);
        let src = std::fs::read_to_string(path).map_err(|e| format!("{shown}: {e}"))?;
        match of(&src) {
            Ok(found) => out.push((shown, found)),
            Err(why)
                if project::is_crate_code(&shown, &crates)
                    && !project::only_included(root, &shown) =>
            {
                return Err(format!("{shown}: {why}"));
            }
            Err(_) => {}
        }
    }
    Ok(out)
}

/// One row per `file#key` from what `for_each_source` read. Never keyed by line, which moves with
/// every edit above it.
pub(crate) fn keyed<I, K>(read: Vec<(String, I)>) -> Series
where
    I: IntoIterator<Item = (K, u64)>,
    K: std::fmt::Display,
{
    let mut series = Series::new();
    for (shown, found) in read {
        for (key, count) in found {
            series.set(&format!("{shown}#{key}"), count);
        }
    }
    series
}

/// The production sources and every crate directory, from one walk; fixture crates are left out.
fn walked(ctx: &crate::run::Ctx) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let root = &ctx.root;
    let found = project::walked(
        &ctx.listing,
        root,
        &|name| !skip_dir(name),
        &|name, path| {
            name == "Cargo.toml"
                || (name.ends_with(".rs")
                    && !is_test_file(name)
                    && !project::is_compile_fail_fixture(path))
        },
    )?;
    let shown: Vec<String> = found
        .iter()
        .map(|path| project::relative(root, path))
        .collect();
    let dirs: Vec<String> = shown
        .iter()
        .filter_map(|path| project::crate_dir(path))
        .collect();
    let fixtures = project::fixture_crates(&dirs);
    let outside = |path: &str| {
        !fixtures
            .iter()
            .any(|dir| project::under(path, dir).is_some())
    };
    let sources = found
        .into_iter()
        .zip(&shown)
        .filter(|(_, shown)| shown.ends_with(".rs") && outside(shown))
        .map(|(path, _)| path)
        .collect();
    let crates = dirs.into_iter().filter(|dir| outside(dir)).collect();
    Ok((sources, crates))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::testdir::tree;

    fn count(src: &str) -> Result<usize, String> {
        Ok(scan(src)?.0)
    }

    fn lines(src: &str) -> usize {
        count(src).unwrap_or_else(|e| panic!("{src:?} did not parse: {e}"))
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_cargo_never_compiles_that_the_parser_rejects_is_passed_over() {
        let root = tree(
            "prodlines-corpus",
            &[
                ("Cargo.toml", ""),
                ("src/lib.rs", "pub fn f() {}\n"),
                ("examples/weird.rs", "fn f() { let _ = |&: |_| {}; }\n"),
            ],
        );
        assert_eq!(kept(&root), vec!["src/lib.rs".to_string()]);
    }

    fn ctx_at(root: &Path) -> crate::run::Ctx {
        crate::run::Ctx::for_root(
            root.to_path_buf(),
            crate::run::baseline::Baseline::empty("0.1.0"),
        )
    }

    fn kept(root: &Path) -> Vec<String> {
        measure(&ctx_at(root))
            .unwrap()
            .into_iter()
            .map(|(path, _)| project::relative(root, &path))
            .collect()
    }

    fn gated(declares: Option<&str>, redirect: Option<&str>) -> TestMod {
        TestMod {
            from: 0,
            to: 0,
            declares: declares.map(str::to_string),
            redirect: redirect.map(str::to_string),
        }
    }

    fn looked_for(file: &str, mods: &[TestMod]) -> Vec<String> {
        gated_paths(Path::new(file), mods)
            .iter()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn a_gated_module_naming_no_file_of_its_own_does_not_hide_the_one_behind_it() {
        let cases = [
            (
                "src/lib.rs",
                vec![gated(None, None), gated(Some("helpers"), None)],
                vec!["src/helpers.rs", "src/helpers"],
            ),
            (
                "src/lib.rs",
                vec![
                    gated(Some("drills"), Some("elsewhere/drills.rs")),
                    gated(Some("helpers"), None),
                ],
                vec!["src/elsewhere/drills.rs", "src/helpers.rs", "src/helpers"],
            ),
            (
                "/nowhere-chock-prodlines/thing.rs",
                vec![
                    gated(Some("helpers"), None),
                    gated(Some("drills"), Some("elsewhere/drills.rs")),
                ],
                vec!["/nowhere-chock-prodlines/elsewhere/drills.rs"],
            ),
        ];
        for (file, mods, expected) in cases {
            assert_eq!(looked_for(file, &mods), expected, "{file}");
        }
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_module_gated_in_its_parent_is_test_code_wherever_its_file_lives() {
        let root = tree(
            "prodlines-gated-parent",
            &[
                (
                    "src/lib.rs",
                    "#[cfg(any(test, feature = \"x\"))]\npub mod fixtures;\npub mod real;\n",
                ),
                ("src/real.rs", "pub fn a() {}\n"),
                ("src/fixtures.rs", "pub fn f() {}\n"),
                ("src/fixtures/deep.rs", "pub fn g() {}\n"),
            ],
        );
        assert_eq!(kept(&root), vec!["src/lib.rs", "src/real.rs"]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn an_ungated_module_is_production_wherever_its_file_lives() {
        let root = tree(
            "prodlines-ungated-parent",
            &[
                ("src/lib.rs", "pub mod fixtures;\n"),
                ("src/fixtures.rs", "pub fn f() {}\n"),
            ],
        );
        assert_eq!(kept(&root), vec!["src/fixtures.rs", "src/lib.rs"]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_redirected_gated_module_is_found_by_its_path_not_its_name() {
        let root = tree(
            "prodlines-redirected",
            &[
                (
                    "src/lib.rs",
                    "#[cfg(test)]\n#[path = \"elsewhere/drills.rs\"]\nmod named_otherwise;\n",
                ),
                ("src/elsewhere/drills.rs", "pub fn d() {}\n"),
            ],
        );
        assert_eq!(kept(&root), vec!["src/lib.rs"]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_last_path_attribute_is_the_one_that_redirects() {
        let root = tree(
            "prodlines-two-paths",
            &[
                (
                    "src/lib.rs",
                    "#[cfg(test)]\n#[path = \"first/drills.rs\"]\n#[path = \"second/drills.rs\"]\nmod named_otherwise;\n",
                ),
                ("src/first/drills.rs", "pub fn d() {}\n"),
                ("src/second/drills.rs", "pub fn d() {}\n"),
            ],
        );
        assert_eq!(kept(&root), vec!["src/first/drills.rs", "src/lib.rs"]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn an_integration_test_directory_is_not_production() {
        let root = tree(
            "prodlines-integration",
            &[
                ("src/lib.rs", "pub fn a() {}\n"),
                ("tests/end_to_end.rs", "pub fn t() {}\n"),
            ],
        );
        assert_eq!(kept(&root), vec!["src/lib.rs"]);
    }

    #[test]
    fn a_whole_file_test_module_is_skipped_by_name() {
        assert!(is_test_file("tests.rs"));
        assert!(is_test_file("local_tests.rs"));
        assert!(!is_test_file("local.rs"));
    }

    #[test]
    fn a_source_splits_at_its_test_module_line_for_line() {
        let src = "fn a() {}\n#[cfg(test)]\nmod tests {\n    use x;\n}\nfn b() {}";
        let (production, tests) = split(src).unwrap();
        assert_eq!(production, "fn a() {}\nfn b() {}\n");
        assert_eq!(tests, "#[cfg(test)]\nmod tests {\n    use x;\n}\n");
        let gated = "#![cfg(test)]\nfn t() {}\n";
        assert_eq!(split(gated).unwrap(), (String::new(), gated.to_string()));
        assert!(split("fn (").is_err());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_test_file_and_the_build_directory_are_left_out_of_the_walk() {
        let root = tree(
            "prodlines-walk",
            &[
                ("src/lib.rs", "pub fn a() {}\n"),
                ("src/tests.rs", "pub fn t() {}\n"),
                ("src/local_tests.rs", "pub fn t() {}\n"),
                ("target/debug/generated.rs", "pub fn g() {}\n"),
                (".hidden/tool.rs", "pub fn h() {}\n"),
            ],
        );
        assert_eq!(kept(&root), vec!["src/lib.rs"]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_mod_rs_gates_the_modules_of_its_own_directory() {
        let root = tree(
            "prodlines-modrs",
            &[
                ("src/lib.rs", "pub mod thing;\n"),
                ("src/thing/mod.rs", "#[cfg(test)]\nmod helpers;\n"),
                ("src/thing/helpers.rs", "pub fn h() {}\n"),
            ],
        );
        assert_eq!(kept(&root), vec!["src/lib.rs", "src/thing/mod.rs"]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn main_owns_the_source_directory_only_when_it_is_the_one_crate_root() {
        let alone = tree(
            "prodlines-main-alone",
            &[
                ("src/main.rs", "#[cfg(test)]\nmod fixtures;\n"),
                ("src/fixtures.rs", "pub fn f() {}\n"),
            ],
        );
        assert_eq!(kept(&alone), vec!["src/main.rs"]);

        let beside_lib = tree(
            "prodlines-main-beside-lib",
            &[
                ("src/lib.rs", "pub mod fixtures;\n"),
                ("src/main.rs", "#[cfg(test)]\nmod fixtures;\n"),
                ("src/fixtures.rs", "pub fn f() {}\n"),
            ],
        );
        assert_eq!(
            kept(&beside_lib),
            vec!["src/fixtures.rs", "src/lib.rs", "src/main.rs"]
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_the_parser_rejects_names_itself_rather_than_measuring_zero() {
        let root = tree("prodlines-unparsable", &crate::testdir::UNPARSABLE);
        let err = measure(&ctx_at(&root)).unwrap_err();
        assert!(err.starts_with("src/lib.rs: line 1:"), "{err}");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_root_that_is_not_there_is_a_failure_and_not_an_empty_tree() {
        let root = crate::testdir::make("prodlines-missing").join("gone");
        let err = measure(&ctx_at(&root)).unwrap_err();
        assert!(err.starts_with("cannot read "), "{err}");
        assert!(err.contains("gone"), "{err}");
    }

    #[test]
    fn a_whole_file_gated_by_an_inner_attribute_is_all_test_code() {
        for gate in [
            "#![cfg(test)]",
            "#![cfg(any(test, feature = \"test-utils\"))]",
            "#![cfg(all(test, unix))]",
        ] {
            let src = format!("//! doc\n\n{gate}\n\nfn helper() {{}}\nfn other() {{}}\n");
            assert_eq!(lines(&src), 0, "{gate} should make the file all test code");
        }
    }

    #[test]
    fn an_inner_attribute_below_real_code_is_not_rust_and_is_reported_as_such() {
        let err = count("fn a() {}\n\n#![cfg(test)]\n").unwrap_err();
        assert!(err.starts_with("line 3:"), "{err}");
    }

    #[test]
    fn a_file_with_no_test_module_counts_every_line() {
        assert_eq!(lines("fn a() {}\nfn b() {}\n"), 3);
    }

    #[test]
    fn a_file_that_does_not_end_in_a_newline_counts_what_is_there() {
        assert_eq!(lines("fn a() {}"), 1);
        assert_eq!(lines("fn a() {}\n#[cfg(test)]\nmod tests {}"), 1);
    }

    #[test]
    fn a_gate_paired_with_a_feature_still_excludes_its_module() {
        let src = "fn a() {}\n#[cfg(all(test, unix))]\nmod t {\n    fn b() {}\n}\n";
        assert_eq!(lines(src), 2);
    }

    #[test]
    fn a_gate_naming_test_after_a_feature_is_still_a_gate() {
        let src = "fn a() {}\n#[cfg(any(feature = \"x\", test))]\nmod tests {\nfn t() {}\n}\n";
        assert_eq!(lines(src), 2);
    }

    #[test]
    fn a_gate_written_with_spaces_inside_the_parentheses_is_still_a_gate() {
        let src = "fn a() {}\n#[cfg( test )]\nmod tests {\nfn t() {}\n}\n";
        assert_eq!(lines(src), 2);
    }

    #[test]
    fn a_cfg_that_does_not_switch_on_test_is_not_a_gate() {
        for attr in [
            "#[cfg(not(test))]",
            "#[cfg(all(unix, not(test)))]",
            "#[cfg(feature = \"test\")]",
            "#[cfg_attr(test, derive(Debug))]",
        ] {
            let src = format!("fn a() {{}}\n{attr}\nmod real {{\nfn r() {{}}\n}}\n");
            assert_eq!(lines(&src), 6, "{attr} gates no test module");
        }
    }

    #[test]
    fn a_gated_helper_before_the_test_module_is_still_production() {
        let src = "fn a() {}\n\
                   #[cfg(test)]\n\
                   fn helper() {}\n\
                   fn b() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                   fn t() {}\n\
                   }\n";
        assert_eq!(lines(src), 5);
    }

    #[test]
    fn a_gated_item_that_is_not_a_module_stays_production() {
        assert_eq!(lines("fn a() {}\n#[cfg(test)]\nuse std::fmt::Debug;\n"), 4);
    }

    #[test]
    fn both_test_modules_are_excluded_when_a_file_has_two() {
        let src = "fn a() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                   fn t() {}\n\
                   }\n\
                   fn b() {}\n\
                   #[cfg(test)]\n\
                   mod head_branch_tests {\n\
                   fn u() {}\n\
                   }\n";
        assert_eq!(lines(src), 3);
    }

    #[test]
    fn a_test_module_not_named_tests_is_still_excluded() {
        let src = "fn a() {}\n#[cfg(test)]\nmod status_code_tests {\nfn t() {}\n}\n";
        assert_eq!(lines(src), 2);
    }

    #[test]
    fn an_indented_test_module_is_still_excluded() {
        let src = "pub mod g {\n    pub fn f() {}\n\n    #[cfg(test)]\n    mod tests {\n        #[test]\n        fn t() {}\n    }\n}\n";
        assert_eq!(lines(src), 5);
    }

    #[test]
    fn a_test_module_declared_in_another_file_swallows_nothing() {
        let src = "//! doc\n\npub use apply::apply;\nmod apply;\n#[cfg(test)]\nmod tests;\nmod types;\n\npub use types::{\n    A,\n    B,\n};\n";
        assert_eq!(lines(src), 11);
    }

    #[test]
    fn every_visibility_and_indentation_of_a_test_module_is_excluded() {
        for form in [
            "mod tests",
            "pub mod tests",
            "pub(crate) mod tests",
            "pub(super) mod tests",
            "    mod tests",
        ] {
            let src = format!("fn a() {{}}\n#[cfg(test)]\n{form} {{\nfn t() {{}}\n}}\n");
            assert_eq!(lines(&src), 2, "{form} opens a test module");
        }
    }

    #[test]
    fn attributes_stacked_around_the_gate_belong_to_the_module() {
        let after = "fn a() {}\n#[cfg(test)]\n#[allow(\n    clippy::unwrap_used,\n)]\nmod tests {\nfn t() {}\n}\n";
        let before = "fn a() {}\n#[allow(\n    clippy::unwrap_used,\n)]\n#[cfg(test)]\nmod tests {\nfn t() {}\n}\n";
        assert_eq!(lines(after), 2);
        assert_eq!(lines(before), 2);
    }

    #[test]
    fn a_doc_comment_on_a_test_module_is_test_code_too() {
        let src =
            "fn a() {}\n/// What this module proves.\n#[cfg(test)]\nmod tests {\nfn t() {}\n}\n";
        assert_eq!(lines(src), 2);
    }

    #[test]
    fn a_brace_inside_a_multi_line_string_does_not_close_the_module() {
        let src = "fn a() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                   fn t() { let s = \"open\n\
                   } still inside\n\
                   \"; }\n\
                   }\n";
        assert_eq!(lines(src), 2);
    }

    #[test]
    fn a_raw_string_holding_a_brace_does_not_close_the_module() {
        for literal in [
            "r#\"a } brace\"#",
            "r##\"a \"# } brace\"##",
            "b\"a } brace\"",
        ] {
            let src = format!(
                "fn a() {{}}\n#[cfg(test)]\nmod tests {{\nfn t() {{ let s = {literal}; }}\n}}\n"
            );
            assert_eq!(lines(&src), 2, "{literal} holds no block delimiter");
        }
    }

    #[test]
    fn a_lifetime_is_not_a_char_literal() {
        let src = "fn a() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                   fn f(c: &Ctx<'_>) -> &'static str { \"x\" }\n\
                   }\n";
        assert_eq!(lines(src), 2);
    }

    #[test]
    fn a_brace_in_a_line_comment_is_not_a_brace() {
        let src = "fn a() {}\n#[cfg(test)]\nmod tests {\n// } not a brace\n}\n";
        assert_eq!(lines(src), 2);
    }

    #[test]
    fn a_block_comment_is_not_read_as_code() {
        for comment in ["/* } */", "/* \" */", "/* /* } */ } */"] {
            let src = format!(
                "fn a() {{}}\n#[cfg(test)]\nmod tests {{\n{comment}\nfn t() {{}}\n}}\nfn b() {{}}\n"
            );
            assert_eq!(lines(&src), 3, "{comment} is a comment, not code");
        }
    }

    #[test]
    fn a_test_module_written_on_one_line_takes_out_that_line_only() {
        let src = "fn a() {}\n#[cfg(test)] mod tests { fn t() {} }\nfn b() {}\n";
        assert_eq!(lines(src), 3);
    }

    #[test]
    fn two_test_modules_sharing_a_line_take_that_line_out_once() {
        let src = "#[cfg(test)] mod a { fn x() {} } #[cfg(test)] mod b { fn y() {} }\nfn c() {}\n";
        assert_eq!(lines(src), 2);
    }

    #[test]
    fn overlapping_excluded_ranges_are_counted_once() {
        assert_eq!(merged(vec![]), 0);
        assert_eq!(merged(vec![(1, 4), (2, 3)]), 4);
        assert_eq!(merged(vec![(3, 5), (1, 2)]), 5);
        assert_eq!(merged(vec![(1, 2), (4, 5)]), 4);
        assert_eq!(merged(vec![(1, 3), (2, 6)]), 6);
    }

    #[test]
    fn a_source_the_parser_rejects_is_a_failure_and_never_a_zero() {
        let err = count("fn broken( {\n").unwrap_err();
        assert!(err.starts_with("line 1:"), "{err}");
    }
}
