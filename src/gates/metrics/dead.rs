//! Private functions nothing in the tree names, counted across every crate at once.

use std::collections::BTreeMap;

use proc_macro2::TokenTree;
use syn::visit::Visit;

use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

pub const GATE: Gate = Gate {
    name: "dead",
    about: "a function defined in this crate that nothing in it names",
    group: Group::OptIn,
    builds: false,
    reads: None,
    kind: Kind::Binary(check),
};

fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let (defined, named) = read_tree(ctx)?;
    if defined.is_empty() {
        return Err("no function was found, so nothing was measured".to_string());
    }
    let findings = unreached(&defined, &named);
    if findings.is_empty() {
        return Ok(Outcome::passed());
    }
    Ok(Outcome::failed(findings))
}

/// A definition is unreached when the tree names it no more often than it is defined, since each
/// definition names itself once.
fn unreached(
    defined: &BTreeMap<String, Vec<(String, u32)>>,
    named: &BTreeMap<String, u64>,
) -> Vec<Finding> {
    let mut found: Vec<Finding> = defined
        .iter()
        .filter(|(name, sites)| {
            named.get(name.as_str()).copied().unwrap_or_default()
                <= u64::try_from(sites.len()).unwrap_or(u64::MAX)
        })
        .flat_map(|(name, sites)| {
            sites.iter().map(move |(file, line)| {
                Finding::at(file, "nothing in this crate names this function")
                    .line(*line)
                    .item(name)
            })
        })
        .collect();
    found.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    found
}

/// Every identifier the tree mentions and how often, read from tokens so a comment is not a use.
/// The `unreferenced` gate uses it to veto its own false findings.
pub fn names_in_tree(ctx: &Ctx) -> Result<BTreeMap<String, u64>, String> {
    read_tree(ctx).map(|(_, named)| named)
}

/// Each private function the tree defines, and every name it mentions. Names in `*_tests.rs` files
/// count too, since `for_each_source` leaves those files out.
fn read_tree(ctx: &Ctx) -> Result<(Defined, BTreeMap<String, u64>), String> {
    let mut defined = Defined::new();
    let mut named = BTreeMap::new();
    for (shown, found) in crate::gates::metrics::prodlines::for_each_source(ctx, &read)? {
        for (name, line) in found.defined {
            defined.entry(name).or_default().push((shown.clone(), line));
        }
        tally(&mut named, found.named);
    }
    tally(&mut named, names_in_test_files(&ctx.root)?);
    Ok((defined, named))
}

/// Where each name is defined: the file and the line of every site.
type Defined = BTreeMap<String, Vec<(String, u32)>>;

fn tally(into: &mut BTreeMap<String, u64>, from: BTreeMap<String, u64>) {
    for (name, count) in from {
        *into.entry(name).or_default() += count;
    }
}

/// Identifiers in the test files `for_each_source` sets aside; their definitions are not judged.
fn names_in_test_files(root: &std::path::Path) -> Result<BTreeMap<String, u64>, String> {
    let mut named: BTreeMap<String, u64> = BTreeMap::new();
    let files = crate::project::walk(
        root,
        &|name| !crate::project::SKIPPED.contains(&name),
        &|name, path| {
            crate::gates::metrics::prodlines::is_test_file(name)
                && !crate::project::is_compile_fail_fixture(path)
        },
    )?;
    for path in files {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(found) = read(&src) else { continue };
        tally(&mut named, found.named);
    }
    Ok(named)
}

/// What one file defines, and how often each identifier appears in it. Counted from tokens rather
/// than call expressions, so a call inside a macro still counts.
#[derive(Debug, Default, PartialEq, Eq)]
struct Read {
    defined: Vec<(String, u32)>,
    named: BTreeMap<String, u64>,
}

fn read(src: &str) -> Result<Read, String> {
    let file = crate::gates::metrics::prodlines::parse_rust(src)?;
    let mut found = Read::default();
    let mut walk = Definitions {
        found: Vec::new(),
        exported: false,
    };
    walk.visit_file(&file);
    found.defined = walk.found;
    let stream: proc_macro2::TokenStream = src
        .parse()
        .map_err(|e| format!("cannot read the token stream: {e}"))?;
    idents(stream, &mut found.named);
    Ok(found)
}

/// Counts every identifier in the stream, definitions included.
fn idents(stream: proc_macro2::TokenStream, into: &mut BTreeMap<String, u64>) {
    for tree in stream {
        if let TokenTree::Group(group) = tree {
            idents(group.stream(), into);
        } else if let Some(name) = named(&tree) {
            *into.entry(name).or_default() += 1;
        }
    }
}

/// The name one token writes, if any. A string literal counts, since attributes such as
/// `#[serde(default = "f")]` name a function in a string.
fn named(tree: &TokenTree) -> Option<String> {
    match tree {
        TokenTree::Ident(ident) => Some(ident.to_string()),
        TokenTree::Literal(literal) => quoted_name(&literal.to_string()),
        TokenTree::Group(_) | TokenTree::Punct(_) => None,
    }
}

/// The last segment of a string literal that spells a path; any other string is prose.
fn quoted_name(literal: &str) -> Option<String> {
    let inner = literal.strip_prefix('"')?.strip_suffix('"')?;
    let last = inner.rsplit("::").next()?;
    let mut chars = last.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    chars
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
        .then(|| last.to_string())
}

/// Collects private functions only, since a `pub` one may be called from outside the crate.
struct Definitions {
    found: Vec<(String, u32)>,
    exported: bool,
}

/// Whether this source hands `name` to something outside the crate: `main`, a harness or binding
/// attribute, or a trait impl. A call graph cannot see those callers.
pub fn is_entry_point(src: &str, name: &str) -> Result<bool, String> {
    let file = crate::gates::metrics::prodlines::parse_rust(src)?;
    let mut look = Entry { name, found: false };
    look.visit_file(&file);
    Ok(look.found)
}

struct Entry<'a> {
    name: &'a str,
    found: bool,
}

impl<'ast> Visit<'ast> for Entry<'_> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if node.sig.ident == self.name && called_from_outside(self.name, &node.attrs) {
            self.found = true;
        }
        syn::visit::visit_item_fn(self, node);
    }

    /// A trait method is called through the trait, which no call graph or token scan sees.
    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if node.trait_.is_some() && names_a_method(node, self.name) {
            self.found = true;
        }
        syn::visit::visit_item_impl(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if node.sig.ident == self.name && called_from_outside(self.name, &node.attrs) {
            self.found = true;
        }
        syn::visit::visit_impl_item_fn(self, node);
    }
}

fn names_a_method(block: &syn::ItemImpl, name: &str) -> bool {
    block
        .items
        .iter()
        .any(|item| matches!(item, syn::ImplItem::Fn(method) if method.sig.ident == name))
}

/// Whether `name` is `main`, or its attributes export it, hand it to a harness or another language,
/// or allow `dead_code`.
fn called_from_outside(name: &str, attrs: &[syn::Attribute]) -> bool {
    name == "main" || attrs.iter().any(excuses)
}

/// Attributes that hand a function to something outside this crate: an exported symbol, a bench or
/// kani harness, or a binding another language calls.
const BY_NAME: [&str; 8] = [
    "no_mangle",
    "export_name",
    "bench",
    "proof",
    "pyfunction",
    "pymodule",
    "wasm_bindgen",
    "napi",
];

/// Attributes on an `impl` block that hand every method in it to another language.
const BY_BLOCK: [&str; 5] = [
    "pymethods",
    "pyclass",
    "wasm_bindgen",
    "napi",
    "napi_derive",
];

/// Whether a block's attributes bind its methods to another language, as `#[pymethods]` does.
fn bound_to_another_language(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|last| BY_BLOCK.contains(&last.ident.to_string().as_str()))
    })
}

fn excuses(attr: &syn::Attribute) -> bool {
    let Some(last) = attr.path().segments.last() else {
        return false;
    };
    let word = last.ident.to_string();
    if word == "allow" || word == "expect" {
        return attr
            .meta
            .require_list()
            .is_ok_and(|list| list.tokens.to_string().contains("dead_code"));
    }
    BY_NAME.contains(&word.as_str()) || word.ends_with("test") || word.starts_with("proc_macro")
}

impl Definitions {
    fn take(&mut self, vis: &syn::Visibility, sig: &syn::Signature, attrs: &[syn::Attribute]) {
        if self.exported || !matches!(vis, syn::Visibility::Inherited) {
            return;
        }
        let name = sig.ident.to_string();
        if called_from_outside(&name, attrs) {
            return;
        }
        let line = u32::try_from(sig.ident.span().start().line).unwrap_or(u32::MAX);
        self.found.push((name, line));
    }
}

impl<'ast> Visit<'ast> for Definitions {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if crate::gates::metrics::prodlines::is_test_gated(&node.attrs) {
            return;
        }
        let was = self.exported;
        self.exported = was || !matches!(node.vis, syn::Visibility::Inherited);
        syn::visit::visit_item_mod(self, node);
        self.exported = was;
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.take(&node.vis, &node.sig, &node.attrs);
        syn::visit::visit_item_fn(self, node);
    }

    /// Skips trait impls, reached through the trait, and blocks bound to another language.
    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if node.trait_.is_some() || bound_to_another_language(&node.attrs) {
            return;
        }
        syn::visit::visit_item_impl(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.take(&node.vis, &node.sig, &node.attrs);
        syn::visit::visit_impl_item_fn(self, node);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::testdir::Held;

    fn defined(src: &str) -> Vec<String> {
        read(src)
            .unwrap()
            .defined
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    #[test]
    fn a_method_bound_to_another_language_is_called_from_outside_this_crate() {
        for block in BY_BLOCK {
            let src = format!(
                "#[{block}]\nimpl Thing {{\n    fn to_arrow(&self) {{}}\n    fn schema(&self) {{}}\n}}\n"
            );
            assert_eq!(defined(&src), Vec::<String>::new(), "{block}");
        }
        // The control: an ordinary block is still judged.
        assert_eq!(
            defined("impl Thing {\n    fn to_arrow(&self) {}\n}\n"),
            vec!["to_arrow".to_string()]
        );
    }

    #[test]
    fn a_function_another_language_calls_is_not_dead() {
        for named in ["pyfunction", "pymodule", "wasm_bindgen", "napi"] {
            let src = format!("#[{named}]\nfn build(x: u8) -> u8 {{ x }}\n");
            assert_eq!(defined(&src), Vec::<String>::new(), "{named}");
        }
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_name_mentioned_in_both_a_source_and_a_test_file_is_counted_once_for_each() {
        let ctx = Held::tree(
            "dead-names-summed",
            &[
                ("Cargo.toml", "[package]\nname = \"p\"\n"),
                ("src/lib.rs", "fn helper() {}\n"),
                ("src/lib_tests.rs", "fn t() { helper(); }\n"),
            ],
        );
        let named = names_in_tree(&ctx).unwrap();
        assert_eq!(named.get("helper"), Some(&2));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_name_only_a_source_file_mentions_is_counted_from_that_file_alone() {
        let ctx = Held::tree(
            "dead-names-source-only",
            &[
                ("Cargo.toml", "[package]\nname = \"p\"\n"),
                ("src/lib.rs", "fn helper() {}\nfn caller() { helper(); }\n"),
            ],
        );
        let named = names_in_tree(&ctx).unwrap();
        assert_eq!(named.get("helper"), Some(&2));
        assert_eq!(named.get("caller"), Some(&1));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_with_no_source_at_all_mentions_nothing() {
        let ctx = Held::tree(
            "dead-names-empty",
            &[("Cargo.toml", "[package]\nname = \"p\"\n")],
        );
        assert_eq!(names_in_tree(&ctx).unwrap(), BTreeMap::new());
    }

    #[test]
    fn a_method_a_trait_impl_names_is_treated_as_reached() {
        let src = "struct S;\ntrait T { fn go(&self); }\nimpl T for S { fn go(&self) {} }\n";
        assert!(is_entry_point(src, "go").unwrap());
    }

    #[test]
    fn a_method_on_an_inherent_impl_is_not_vetoed_by_the_trait_rule() {
        let src = "struct S;\nimpl S { fn go(&self) {} }\n";
        assert!(!is_entry_point(src, "go").unwrap());
    }

    #[test]
    fn a_trait_impl_that_does_not_name_the_method_vetoes_nothing() {
        let src = "struct S;\ntrait T { fn other(&self); }\nimpl T for S { fn other(&self) {} }\n";
        assert!(!is_entry_point(src, "go").unwrap());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_gate_passes_a_function_only_a_sibling_test_file_calls() {
        let ctx = Held::tree(
            "dead-gate-sibling",
            &[
                ("Cargo.toml", "[package]\nname = \"p\"\n"),
                ("src/lib.rs", "fn helper() {}\nfn main() {}\n"),
                ("src/lib_tests.rs", "fn t() { helper(); }\n"),
            ],
        );
        assert_eq!(check(&ctx).unwrap(), crate::run::Outcome::passed());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_gate_reports_a_function_no_sibling_names_and_passes_one_that_is_called() {
        let ctx = Held::tree(
            "dead-gate",
            &[
                ("Cargo.toml", "[package]\nname = \"p\"\n"),
                (
                    "src/lib.rs",
                    "fn helper() {}\nfn used() {}\nfn caller() { used(); }\nfn main() { caller(); }\n",
                ),
            ],
        );
        let Ok(Outcome {
            passed, findings, ..
        }) = check(&ctx)
        else {
            panic!("the gate could not run over a tree it should read")
        };
        assert!(!passed);
        let rendered: Vec<String> = findings.iter().map(Finding::render).collect();
        assert_eq!(
            rendered,
            ["src/lib.rs:1: helper: nothing in this crate names this function"]
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_gate_passes_a_crate_that_names_everything_it_defines() {
        let ctx = Held::tree(
            "dead-gate-clean",
            &[
                ("Cargo.toml", "[package]\nname = \"p\"\n"),
                ("src/lib.rs", "fn used() {}\nfn main() { used(); }\n"),
            ],
        );
        assert!(check(&ctx).unwrap().passed);
    }

    /// An empty tree measured nothing, which is not the same as nothing being dead.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_with_no_function_in_it_could_not_run() {
        let ctx = Held::tree(
            "dead-gate-empty",
            &[
                ("Cargo.toml", "[package]\nname = \"p\"\n"),
                ("src/lib.rs", ""),
            ],
        );
        assert_eq!(
            check(&ctx).map(|outcome| outcome.passed),
            Err("no function was found, so nothing was measured".to_string())
        );
    }

    fn sites(of: &[(&str, &str, u32)]) -> BTreeMap<String, Vec<(String, u32)>> {
        let mut defined: BTreeMap<String, Vec<(String, u32)>> = BTreeMap::new();
        for (name, file, line) in of {
            defined
                .entry((*name).to_string())
                .or_default()
                .push(((*file).to_string(), *line));
        }
        defined
    }

    fn counts(of: &[(&str, u64)]) -> BTreeMap<String, u64> {
        of.iter().map(|(n, c)| ((*n).to_string(), *c)).collect()
    }

    #[test]
    fn a_name_used_no_more_often_than_it_is_defined_is_unreached() {
        let defined = sites(&[("helper", "src/a.rs", 4), ("used", "src/a.rs", 9)]);
        let named = counts(&[("helper", 1), ("used", 2)]);
        let found: Vec<String> = unreached(&defined, &named)
            .iter()
            .map(Finding::render)
            .collect();
        assert_eq!(
            found,
            ["src/a.rs:4: helper: nothing in this crate names this function"]
        );
    }

    #[test]
    fn a_name_defined_twice_is_reported_at_both_sites() {
        let defined = sites(&[("parse", "src/b.rs", 7), ("parse", "src/a.rs", 3)]);
        let found: Vec<String> = unreached(&defined, &counts(&[("parse", 2)]))
            .iter()
            .map(Finding::render)
            .collect();
        assert_eq!(
            found,
            [
                "src/a.rs:3: parse: nothing in this crate names this function",
                "src/b.rs:7: parse: nothing in this crate names this function"
            ]
        );
    }

    /// A crate `p` with an empty `src/`, in its own scratch directory.
    fn crate_p(name: &str) -> crate::testdir::Scratch {
        let dir = crate::testdir::make(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"p\"\n").unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn a_test_file_that_cannot_be_opened_is_stepped_over_rather_than_ending_the_read() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate_p("dead-unreadable-tests");
        let locked = dir.join("src/a_tests.rs");
        std::fs::write(&locked, "fn t() { hidden(); }\n").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::write(dir.join("src/z_tests.rs"), "fn t() { used(); }\n").unwrap();
        let refused = std::fs::read_to_string(&locked).is_err();
        let named = names_in_test_files(&dir);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(refused, "this proves nothing while the file still opens");
        let named = named.unwrap();
        assert_eq!(named.get("used"), Some(&1));
        assert_eq!(named.get("hidden"), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_test_file_that_does_not_parse_is_stepped_over_rather_than_ending_the_read() {
        let dir = crate_p("dead-unparsable-tests");
        std::fs::write(dir.join("src/a_tests.rs"), "fn broken( {\n").unwrap();
        std::fs::write(dir.join("src/z_tests.rs"), "fn t() { used(); }\n").unwrap();
        let named = names_in_test_files(&dir).unwrap();
        assert_eq!(named.get("used"), Some(&1));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_helper_called_only_from_a_sibling_test_file_is_reached() {
        let dir = crate_p("dead-sibling-tests");
        std::fs::write(
            dir.join("src/lib.rs"),
            "fn helper() {}\nfn used() { helper(); }\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/lib_tests.rs"), "fn t() { used(); }\n").unwrap();
        let named = names_in_test_files(&dir).unwrap();
        assert_eq!(named.get("used"), Some(&1));
    }

    #[test]
    fn a_private_function_is_a_definition_this_gate_judges() {
        assert_eq!(defined("fn f() {}\n"), ["f"]);
    }

    #[test]
    fn a_public_function_is_somebody_elses_to_call_and_is_not_judged() {
        assert_eq!(defined("pub fn f() {}\n"), Vec::<String>::new());
        assert_eq!(defined("pub(crate) fn f() {}\n"), Vec::<String>::new());
    }

    #[test]
    fn a_private_function_inside_a_public_module_is_still_reachable_from_outside() {
        assert_eq!(
            defined("pub mod m {\n    fn f() {}\n}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_private_module_hides_nothing_and_its_end_restores_the_outer_scope() {
        assert_eq!(
            defined("mod m {\n    fn f() {}\n}\nfn g() {}\n"),
            vec!["f".to_string(), "g".to_string()]
        );
        assert_eq!(
            defined("pub mod m {\n    fn f() {}\n}\nfn g() {}\n"),
            vec!["g".to_string()]
        );
    }

    #[test]
    fn a_trait_method_is_reached_through_the_trait_and_is_not_judged() {
        assert_eq!(
            defined("struct S;\nimpl Iterator for S {\n    fn next(&mut self) {}\n}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_inherent_method_is_judged() {
        assert_eq!(
            defined("struct S;\nimpl S {\n    fn f(&self) {}\n}\n"),
            ["f"]
        );
    }

    #[test]
    fn a_harness_is_an_entry_point_and_an_ordinary_function_is_not() {
        assert!(is_entry_point("#[kani::proof]\nfn p() {}\n", "p").unwrap());
        assert!(is_entry_point("#[test]\nfn t() {}\n", "t").unwrap());
        assert!(is_entry_point("fn main() {}\n", "main").unwrap());
        assert!(!is_entry_point("fn plain() {}\n", "plain").unwrap());
    }

    #[test]
    fn a_marked_function_does_not_excuse_the_one_beside_it() {
        let src = "#[kani::proof]\nfn proved() {}\nfn plain() {}\n";
        assert!(is_entry_point(src, "proved").unwrap());
        assert!(!is_entry_point(src, "plain").unwrap());
    }

    #[test]
    fn a_method_carrying_the_attribute_is_found_as_well_as_a_free_function() {
        let src = "impl S {\n    #[test]\n    fn m() {}\n    fn plain() {}\n}\n";
        assert!(is_entry_point(src, "m").unwrap());
        assert!(!is_entry_point(src, "plain").unwrap());
        assert!(!is_entry_point(src, "absent").unwrap());
    }

    /// Answering "no" for a file that cannot be parsed would record the entity as debt.
    #[test]
    fn a_source_that_does_not_parse_is_an_error_rather_than_an_answer() {
        assert!(is_entry_point("fn f( {\n", "f").is_err());
    }

    #[test]
    fn a_function_inside_a_test_module_is_the_harness_and_is_not_judged() {
        assert_eq!(
            defined("#[cfg(test)]\nmod tests {\n    fn helper() {}\n}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_name_is_counted_everywhere_it_appears_including_inside_a_macro() {
        let found = read("fn f() {}\nfn g() { println!(\"{}\", f()); }\n").unwrap();
        assert_eq!(found.named.get("f"), Some(&2));
        assert_eq!(found.named.get("g"), Some(&1));
    }

    #[test]
    fn a_function_something_outside_the_crate_calls_is_not_judged() {
        assert_eq!(defined("fn main() {}\n"), Vec::<String>::new());
        assert_eq!(
            defined("#[no_mangle]\nfn hook() {}\n"),
            Vec::<String>::new()
        );
        assert_eq!(defined("#[tokio::test]\nfn t() {}\n"), Vec::<String>::new());
        assert_eq!(defined("#[kani::proof]\nfn p() {}\n"), Vec::<String>::new());
        assert_eq!(
            defined("#[proc_macro_derive(D)]\nfn d() {}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_function_an_attribute_names_in_a_string_counts_as_called() {
        let src = "#[derive(Deserialize)]\nstruct S {\n    \
                   #[serde(default = \"default_true\")]\n    a: bool,\n}\n\
                   fn default_true() -> bool { true }\n";
        let found = read(src).unwrap();
        assert_eq!(found.named.get("default_true"), Some(&2));
    }

    #[test]
    fn a_path_in_a_string_names_its_last_segment() {
        assert_eq!(quoted_name("\"a::b::helper\""), Some("helper".to_string()));
        assert_eq!(quoted_name("\"helper\""), Some("helper".to_string()));
        assert_eq!(quoted_name("\"not a name\""), None);
        assert_eq!(quoted_name("\"9lives\""), None);
        assert_eq!(quoted_name("42"), None);
    }

    #[test]
    fn a_function_the_author_told_rustc_is_uncalled_is_not_reported() {
        assert_eq!(
            defined("#[expect(dead_code)]\nfn probe() {}\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            defined("#[allow(dead_code, unused)]\nfn probe() {}\n"),
            Vec::<String>::new()
        );
        assert_eq!(defined("#[allow(unused_mut)]\nfn probe() {}\n"), ["probe"]);
    }

    #[test]
    fn a_file_the_parser_rejects_stops_the_dead_gate() {
        assert!(read("fn f( {\n").is_err());
    }
}
