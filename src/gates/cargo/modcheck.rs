//! The `modcheck` gate: every `mod x;` names a file, since a missing one aborts `cargo fmt --all`,
//! and every module file is declared, since rustc never compiles one that is not.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use syn::ext::IdentExt;
use syn::spanned::Spanned;

use crate::gates::source::targets::{TARGET_DIRS, join};
use crate::project;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Inspection, Kind};

pub const GATE: Gate = Gate {
    name: "modcheck",
    about: "every `mod` declaration names a file, and every module file is declared",
    group: Group::Gates,
    builds: false,
    reads: None,
    kind: Kind::Debt {
        inspect,
        unit: "orphan module file(s)",
    },
};

/// Marks a file whose modules come from `include!`, codegen or a macro; the directory it owns is
/// exempt in both directions.
const WAIVER: &str = "chock:modcheck-exempt";

fn inspect(ctx: &Ctx) -> Result<Inspection, String> {
    inspected(&Tree(read(&ctx.root)?))
}

/// Every file the check reads, keyed by its root-relative path with forward slashes.
#[derive(Debug, Default, Clone)]
struct Tree(BTreeMap<String, Option<String>>);

impl Tree {
    /// The file's text, or `None` when it is absent or not UTF-8; `has` still counts the latter.
    fn get(&self, path: &str) -> Option<&str> {
        self.0.get(path)?.as_deref()
    }

    fn has(&self, path: &str) -> bool {
        self.0.contains_key(path)
    }

    fn paths(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Crate directories, deepest first so a nested crate claims its own files. Fixture crates are
    /// left out, as cargo builds nothing there.
    fn crates(&self) -> Vec<String> {
        let mut dirs: Vec<String> = self.paths().filter_map(project::crate_dir).collect();
        let fixtures = project::fixture_crates(&dirs);
        dirs.retain(|dir| !fixtures.contains(dir));
        dirs.sort_by_key(|dir| std::cmp::Reverse(dir.len()));
        dirs
    }
}

/// Every `.rs` file and manifest under the root, `None` for one that is not UTF-8. Shared with
/// `features` so both gates judge the same tree.
pub(super) fn read(root: &Path) -> Result<BTreeMap<String, Option<String>>, String> {
    project::read_tree(
        root,
        &|name| !project::SKIPPED.contains(&name) && !project::FIXTURE_DIRS.contains(&name),
        &|name, path| {
            (name == "Cargo.toml" || name.ends_with(".rs"))
                && !project::is_compile_fail_fixture(path)
        },
    )
}

/// One `mod` that names a file of its own, and where that file is looked for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Decl {
    /// The module name without any `r#`, so `mod r#async;` is looked for in `async.rs`.
    name: String,
    /// The directory holding `<name>.rs` or `<name>/mod.rs`.
    dir: String,
    /// The files a `#[path]` names, replacing both candidates. Several when a `cfg_attr` sets one
    /// per target, since chock builds for none of them.
    redirects: Vec<String>,
    line: u32,
}

#[cfg(test)]
fn faults(tree: &Tree) -> Result<Vec<Finding>, String> {
    let found = inspected(tree)?;
    let mut findings: Vec<Finding> = found.blockers.into_iter().chain(found.debt).collect();
    findings.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    Ok(findings)
}

/// What the walk from every target root found.
#[derive(Default)]
pub(crate) struct Reach {
    /// Each file a root reaches by `mod`, `#[path]` or `include!`.
    reached: BTreeSet<String>,
    /// The directory of each file marked exempt.
    waived: Vec<String>,
    /// Each `mod` that names no file.
    absent: Vec<Finding>,
}

impl Reach {
    fn waives(&self, path: &str) -> bool {
        let holds = |dir: &String| project::under(path, dir).is_some();
        self.waived.iter().any(holds)
    }

    /// Whether a cargo target compiles `path`, as far as the tree says.
    pub(crate) fn compiles(&self, path: &str) -> bool {
        self.reached.contains(path) || self.waives(path)
    }
}

/// What the targets under `root` compile. A tree with no manifest compiles nothing.
pub(crate) fn reach(root: &Path) -> Result<Reach, String> {
    let tree = Tree(read(root)?);
    walked(&tree, &tree.crates())
}

fn inspected(tree: &Tree) -> Result<Inspection, String> {
    let crates = tree.crates();
    if crates.is_empty() {
        return Err("no Cargo.toml under the project root".to_string());
    }
    let reach = walked(tree, &crates)?;
    let debt = tree
        .paths()
        .filter(|path| path.ends_with(".rs"))
        .filter(|path| !reach.reached.contains(*path))
        .filter(|path| compiled(&crates, path))
        .filter(|path| project::is_crate_code(path, &crates))
        .filter(|path| !reach.waives(path))
        .map(undeclared)
        .collect();
    Ok(Inspection {
        debt,
        blockers: reach.absent,
    })
}

/// Follows each `mod`, `#[path]` and `include!` from every target root of `crates`.
fn walked(tree: &Tree, crates: &[String]) -> Result<Reach, String> {
    let mut reach = Reach::default();
    let mut queue: VecDeque<Site> = crates
        .iter()
        .flat_map(|dir| roots(tree, dir))
        .map(|path| Site {
            base: base_dir(&path, true),
            path,
        })
        .collect();

    while let Some(site) = queue.pop_front() {
        if !reach.reached.insert(site.path.clone()) {
            continue;
        }
        let Some(src) = tree.get(&site.path) else {
            continue;
        };
        if src.contains(WAIVER) {
            reach.waived.push(site.base);
            continue;
        }
        let found = match reaches(src, &site.base, &beside(&site.path)) {
            Ok(found) => found,
            // Outside `src/` a file may be test input that never parses, as may an `include!`d
            // fragment; under `src/` the build would fail too.
            Err(_)
                if !project::is_crate_code(&site.path, crates) || pulled_in(tree, &site.path) =>
            {
                continue;
            }
            Err(why) => return Err(format!("{}: {why}", site.path)),
        };
        for (decl, reportable) in found.declarations() {
            let named = sites(tree, decl);
            if named.is_empty() && reportable {
                reach.absent.push(absent(&site.path, decl));
            }
            queue.extend(named);
        }
        queue.extend(found.spliced);
    }
    Ok(reach)
}

/// A blocker: `cargo fmt --all` aborts on a `mod` with no file, breaking every commit hook.
fn absent(owner: &str, decl: &Decl) -> Finding {
    let looked_for = if decl.redirects.is_empty() {
        format!("{} or {}", flat(decl), nested(decl))
    } else {
        decl.redirects.join(" or ")
    };
    Finding::at(owner, &format!("names no file — looked for {looked_for}"))
        .line(decl.line)
        .item(&decl.name)
}

/// A file nothing declares is never compiled, so its errors surface only once someone declares it.
const ORPHAN: &str = "no `mod` declaration reaches this file — rustc never compiles it";

fn undeclared(path: &str) -> Finding {
    Finding::at(path, ORPHAN)
}

/// Whether cargo compiles this file at all. `crates` is deepest first, so a file inside a nested
/// crate is judged against that crate's layout and not the workspace's.
fn compiled(crates: &[String], path: &str) -> bool {
    let Some(rest) = crates.iter().find_map(|dir| project::under(path, dir)) else {
        return false;
    };
    rest == "build.rs"
        || TARGET_DIRS
            .iter()
            .any(|dir| project::under(rest, dir).is_some())
}

/// Whether another file in the tree `include!`s this one, matched by file name.
fn pulled_in(tree: &Tree, path: &str) -> bool {
    let read = tree
        .0
        .iter()
        .filter_map(|(at, src)| Some((at.as_str(), src.as_deref()?)));
    project::included_elsewhere(read, path)
}

/// Every file cargo compiles as a crate root of its own: the library, the build script, and each
/// binary, integration test, example and benchmark target.
fn roots(tree: &Tree, dir: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for name in ["src/lib.rs", "src/main.rs", "build.rs"] {
        let path = join(dir, name);
        if tree.has(&path) {
            out.insert(path);
        }
    }
    for holder in ["src/bin", "tests", "examples", "benches"] {
        let holder = join(dir, holder);
        out.extend(
            tree.paths()
                .filter_map(|path| Some((path, project::under(path, &holder)?)))
                .filter(|(_, rest)| is_target(rest))
                .map(|(path, _)| path.to_string()),
        );
    }
    out.extend(manifest_targets(tree, dir));
    out
}

/// A target directory holds one root per `x.rs` and one per `x/main.rs`; anything deeper is a
/// module of one of those, not a target of its own.
fn is_target(rest: &str) -> bool {
    match rest.split_once('/') {
        None => rest.ends_with(".rs"),
        Some((_, tail)) => tail == "main.rs",
    }
}

/// Every `path = "…rs"` a manifest names, in any table. Over-collecting can only excuse a file,
/// never accuse one.
fn manifest_targets(tree: &Tree, dir: &str) -> Vec<String> {
    let Some(manifest) = tree.get(&join(dir, "Cargo.toml")) else {
        return Vec::new();
    };
    manifest
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("path")?;
            rest.starts_with([' ', '=']).then_some(rest)
        })
        .filter_map(quoted)
        .filter(|value| value.ends_with(".rs"))
        .map(|value| normalise(&join(dir, &value)))
        .collect()
}

/// The first double-quoted run in a line.
fn quoted(text: &str) -> Option<String> {
    let start = text.find('"')? + 1;
    let end = text[start..].find('"')? + start;
    Some(text[start..end].to_string())
}

/// Where each of a declaration's files sends the walk next.
fn sites(tree: &Tree, decl: &Decl) -> Vec<Site> {
    resolve(tree, decl)
        .into_iter()
        .map(|path| Site {
            base: base_dir(&path, false),
            path,
        })
        .collect()
}

/// Every file a declaration names that the tree holds. `x.rs` is tried before `x/mod.rs`, the order
/// rustc resolves them in; a `cfg_attr` redirect names one file per target, so all of them count.
fn resolve(tree: &Tree, decl: &Decl) -> Vec<String> {
    if !decl.redirects.is_empty() {
        return decl
            .redirects
            .iter()
            .filter(|path| tree.has(path))
            .cloned()
            .collect();
    }
    [flat(decl), nested(decl)]
        .into_iter()
        .find(|path| tree.has(path))
        .into_iter()
        .collect()
}

fn flat(decl: &Decl) -> String {
    normalise(&join(&decl.dir, &format!("{}.rs", decl.name)))
}

fn nested(decl: &Decl) -> String {
    normalise(&join(&decl.dir, &format!("{}/mod.rs", decl.name)))
}

/// Where a file's children live. A crate root and a `mod.rs` share their own directory; every other
/// module file owns the directory named after it.
fn base_dir(path: &str, is_root: bool) -> String {
    if is_root || stem(path) == "mod" {
        return beside(path);
    }
    path.strip_suffix(".rs").unwrap_or(path).to_string()
}

/// The directory the file itself sits in, which is the anchor a top-level `#[path]` resolves from.
fn beside(path: &str) -> String {
    path.rsplit_once('/')
        .map_or(String::new(), |(dir, _)| dir.to_string())
}

fn stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_suffix(".rs").unwrap_or(name)
}

/// `.` and `..` resolved, so a `#[path]` reaching out of its own directory still matches a key.
fn normalise(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Everything one file adds to the module graph. Parsed rather than matched: a `mod` in a string
/// literal or a comment is not a declaration.
fn reaches(src: &str, base: &str, file_dir: &str) -> Result<Reaches, String> {
    let file = syn::parse_file(src).map_err(|e| format!("line {}: {e}", e.span().start().line))?;
    let mut walk = Walk {
        file_dir,
        found: Reaches::default(),
    };
    walk.items(&file.items, base, file_dir);
    Ok(walk.found)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Reaches {
    decls: Vec<Decl>,
    /// Declarations found in a macro's tokens, such as a `mod x;` inside `cfg_if!`, which `syn`
    /// does not parse.
    guessed: Vec<Decl>,
    /// Files spliced in with `include!`, carrying the including module's base, which is where their
    /// `mod`s resolve from.
    spliced: Vec<Site>,
}

impl Reaches {
    /// Every declaration, with whether to report it when unresolved: only those written in source,
    /// since a name a macro built may legitimately match no file.
    fn declarations(&self) -> Vec<(&Decl, bool)> {
        let source = self.decls.iter().map(|decl| (decl, true));
        let macros = self.guessed.iter().map(|decl| (decl, false));
        source.chain(macros).collect()
    }
}

/// A file to walk, and the directory its `mod` declarations look in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Site {
    path: String,
    base: String,
}

/// Walks one file's items. `file_dir` stays fixed because `include!` resolves from the file
/// holding it, while `dir` moves with each inline `mod`.
struct Walk<'a> {
    file_dir: &'a str,
    found: Reaches,
}

impl Walk<'_> {
    /// An inline `mod x { }` names no file but moves where its children, `#[path]` ones included,
    /// are looked for.
    fn items(&mut self, items: &[syn::Item], dir: &str, anchor: &str) {
        for item in items {
            match item {
                syn::Item::Mod(item) => self.module(item, dir, anchor),
                syn::Item::Macro(item) => {
                    self.splice(&item.mac, dir);
                    self.guess(&item.mac, dir);
                }
                _ => {}
            }
        }
    }

    fn module(&mut self, item: &syn::ItemMod, dir: &str, anchor: &str) {
        let name = item.ident.unraw().to_string();
        let redirects = redirects_from(&item.attrs, anchor);
        let Some((_, inner)) = &item.content else {
            self.found.decls.push(Decl {
                name,
                dir: dir.to_string(),
                redirects,
                line: u32::try_from(item.mod_token.span().start().line).unwrap_or(u32::MAX),
            });
            return;
        };
        let inner_dir = redirects.into_iter().next().unwrap_or(join(dir, &name));
        self.items(inner, &inner_dir, &inner_dir);
    }

    /// Records every `mod x;` in a macro's tokens; `cfg_if!` is a common way to declare one module
    /// per feature.
    fn guess(&mut self, mac: &syn::Macro, dir: &str) {
        if mac.path.is_ident("include") {
            return;
        }
        let line = u32::try_from(mac.path.span().start().line).unwrap_or(u32::MAX);
        for name in modules_in(&mac.tokens) {
            self.found.guessed.push(Decl {
                name,
                dir: dir.to_string(),
                redirects: Vec::new(),
                line,
            });
        }
    }

    /// Records the file an `include!("x.rs")` splices into this module, which rustc compiles though
    /// no `mod` names it.
    fn splice(&mut self, mac: &syn::Macro, dir: &str) {
        if !mac.path.is_ident("include") {
            return;
        }
        let Ok(named) = mac.parse_body::<syn::LitStr>() else {
            return;
        };
        self.found.spliced.push(Site {
            path: normalise(&join(self.file_dir, &named.value())),
            base: dir.to_string(),
        });
    }
}

/// Every path the `#[path]` attributes here could set, resolved from `anchor`, the directory of
/// the file holding them.
fn redirects_from(attrs: &[syn::Attribute], anchor: &str) -> Vec<String> {
    let written = attrs.iter().flat_map(redirects_of);
    written
        .map(|value| normalise(&join(anchor, &value)))
        .collect()
}

/// The paths one attribute sets: a plain `#[path]`, or every `path = "…"` in a `cfg_attr`, since
/// chock builds for no particular target.
fn redirects_of(attr: &syn::Attribute) -> Vec<String> {
    match &attr.meta {
        syn::Meta::NameValue(pair) if pair.path.is_ident("path") => {
            literal(&pair.value).into_iter().collect()
        }
        syn::Meta::List(list) if list.path.is_ident("cfg_attr") => redirects_in(&list.tokens),
        _ => Vec::new(),
    }
}

fn literal(value: &syn::Expr) -> Option<String> {
    match value {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(text),
            ..
        }) => Some(text.value()),
        _ => None,
    }
}

/// Every `path = "…"` in a `cfg_attr`'s arguments, at any depth. Read from tokens because
/// `syn::Meta` holds those arguments as one opaque list.
fn redirects_in(tokens: &proc_macro2::TokenStream) -> Vec<String> {
    scanned(tokens, &assigned_path, &redirects_in)
}

/// Every `mod x;` in a token stream, at any depth.
fn modules_in(tokens: &proc_macro2::TokenStream) -> Vec<String> {
    scanned(tokens, &declared_module, &modules_in)
}

/// Matches `shape` against every three-token window, then recurses into each group with `deeper`.
fn scanned(
    tokens: &proc_macro2::TokenStream,
    shape: &dyn Fn(&[proc_macro2::TokenTree]) -> Option<String>,
    deeper: &dyn Fn(&proc_macro2::TokenStream) -> Vec<String>,
) -> Vec<String> {
    let trees: Vec<proc_macro2::TokenTree> = tokens.clone().into_iter().collect();
    let mut found: Vec<String> = trees.windows(3).filter_map(shape).collect();
    for tree in &trees {
        if let proc_macro2::TokenTree::Group(group) = tree {
            found.extend(deeper(&group.stream()));
        }
    }
    found
}

fn assigned_path(window: &[proc_macro2::TokenTree]) -> Option<String> {
    let [
        proc_macro2::TokenTree::Ident(key),
        proc_macro2::TokenTree::Punct(sets),
        proc_macro2::TokenTree::Literal(text),
    ] = window
    else {
        return None;
    };
    (*key == "path" && sets.as_char() == '=')
        .then(|| syn::parse_str::<syn::LitStr>(&text.to_string()).ok())
        .flatten()
        .map(|text| text.value())
}

fn declared_module(window: &[proc_macro2::TokenTree]) -> Option<String> {
    let [
        proc_macro2::TokenTree::Ident(keyword),
        proc_macro2::TokenTree::Ident(name),
        proc_macro2::TokenTree::Punct(end),
    ] = window
    else {
        return None;
    };
    (*keyword == "mod" && end.as_char() == ';').then(|| name.unraw().to_string())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn names(src: &str) -> Vec<String> {
        decls(src).into_iter().map(|decl| decl.name).collect()
    }

    fn decls(src: &str) -> Vec<Decl> {
        reaches(src, "src", "src").unwrap().decls
    }

    fn spliced(src: &str) -> Vec<Site> {
        reaches(src, "src", "src/generated").unwrap().spliced
    }

    fn tree(files: &[(&str, &str)]) -> Tree {
        Tree(
            files
                .iter()
                .map(|(path, src)| ((*path).to_string(), Some((*src).to_string())))
                .collect(),
        )
    }

    fn rendered(files: &[(&str, &str)]) -> Vec<String> {
        faults(&tree(files))
            .unwrap()
            .iter()
            .map(Finding::render)
            .collect()
    }

    #[test]
    fn plain_and_visibility_qualified_declarations_are_found() {
        let src = "mod a;\npub mod b;\npub(crate) mod c;\n    pub(super) mod d;\n";
        assert_eq!(names(src), ["a", "b", "c", "d"]);
    }

    #[test]
    fn an_inline_module_is_not_a_file_declaration() {
        assert_eq!(
            names("mod inline {\n    fn f() {}\n}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_path_attribute_declares_the_file_it_points_at() {
        let src = "#[path = \"loom/cas_gc_race.rs\"]\nmod cas_gc_race;\nmod ordinary;\n";
        let found = decls(src);
        assert_eq!(found[0].redirects, ["src/loom/cas_gc_race.rs"]);
        assert_eq!(found[1].redirects, Vec::<String>::new());
        assert_eq!(names(src), ["cas_gc_race", "ordinary"]);
    }

    #[test]
    fn a_redirected_module_is_credited_to_its_file_not_its_name() {
        let found = decls("#[path = \"elsewhere/on_disk.rs\"]\nmod other_name;\n");
        assert_eq!(found[0].redirects, ["src/elsewhere/on_disk.rs"]);
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                (
                    "src/lib.rs",
                    "#[path = \"elsewhere/on_disk.rs\"]\nmod other_name;\n"
                ),
                ("src/elsewhere/on_disk.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_inline_module_does_not_stop_the_scan_at_the_declaration_behind_it() {
        assert_eq!(
            names("mod inline {\n    fn f() {}\n}\nmod later;\n"),
            ["later"]
        );
    }

    #[test]
    fn a_path_attribute_applies_only_to_the_next_declaration() {
        let found = decls("#[path = \"a/one.rs\"]\nmod first;\nmod second;\n");
        assert_eq!(found[0].redirects, ["src/a/one.rs"]);
        assert_eq!(found[1].redirects, Vec::<String>::new());
        assert_eq!(found[1].dir, "src");
    }

    /// `cargo fmt` ignores features when resolving modules, so a gated module's file must exist.
    #[test]
    fn a_cfg_gated_declaration_still_counts() {
        assert_eq!(
            names("#[cfg(feature = \"tabular\")]\nmod df_json;\n"),
            ["df_json"]
        );
    }

    #[test]
    fn an_attribute_between_a_path_and_its_declaration_does_not_break_the_redirect() {
        let found = decls("#[path = \"a/one.rs\"]\n#[cfg(test)]\n\nmod first;\n");
        assert_eq!(found[0].redirects, ["src/a/one.rs"]);
    }

    #[test]
    fn a_path_set_per_target_by_cfg_attr_reaches_every_file_it_could_name() {
        let src = "#[cfg_attr(target_arch = \"wasm32\", path = \"decode/wasm.rs\")]\n\
                   #[cfg_attr(not(target_arch = \"wasm32\"), path = \"decode/native.rs\")]\n\
                   mod wrapper;\n";
        let found = decls(src);
        assert_eq!(
            found[0].redirects,
            ["src/decode/wasm.rs", "src/decode/native.rs"]
        );
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", src),
                ("src/decode/wasm.rs", ""),
                ("src/decode/native.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_inline_module_given_a_path_looks_for_its_children_there() {
        let src = "#[path = \"elsewhere\"]\nmod named { mod child; }\n";
        let found = decls(src);
        assert_eq!(found[0].name, "child");
        assert_eq!(found[0].dir, "src/elsewhere");
    }

    #[test]
    fn a_module_declared_inside_a_macro_body_is_reached_like_any_other() {
        let src = "cfg_if! { if #[cfg(feature = \"s3\")] { mod s3; } else { mod fs; } }\n";
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", src),
                ("src/s3.rs", ""),
                ("src/fs.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    /// A macro may build module names from pieces, so a name found in one may match no file.
    #[test]
    fn a_module_a_macro_names_but_no_file_answers_to_is_not_reported() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", "build_me! { mod never_written; }\n"),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_word_starting_with_mod_is_not_a_declaration() {
        assert_eq!(
            names("struct Model;\nfn modify(x: u8) -> u8 { x }\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_mod_inside_a_string_or_a_comment_is_not_a_declaration() {
        let src = "// mod commented;\nconst S: &str = \"mod quoted;\";\nmod real;\n";
        assert_eq!(names(src), ["real"]);
    }

    #[test]
    fn a_declaration_with_a_trailing_comment_is_still_a_declaration() {
        assert_eq!(names("mod parser; // the one that matters\n"), ["parser"]);
    }

    #[test]
    fn a_keyword_module_is_looked_for_under_the_name_without_its_escape() {
        assert_eq!(names("mod r#async;\nmod r#match;\n"), ["async", "match"]);
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", "mod r#async;\n"),
                ("src/async.rs", "")
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_keyword_module_nested_inline_looks_under_the_escaped_name_too() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", "mod r#if {\n    mod inner;\n}\n"),
                ("src/if/inner.rs", "")
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_spliced_file_is_resolved_from_the_directory_of_the_file_that_splices_it() {
        assert_eq!(
            spliced("include!(\"prost.rs\");\n"),
            [Site {
                path: "src/generated/prost.rs".to_string(),
                base: "src".to_string(),
            }]
        );
    }

    #[test]
    fn a_splice_inside_an_inline_module_lands_in_that_module_not_the_file() {
        assert_eq!(
            spliced("mod proto {\n    include!(\"prost.rs\");\n}\n"),
            [Site {
                path: "src/generated/prost.rs".to_string(),
                base: "src/proto".to_string(),
            }]
        );
    }

    #[test]
    fn a_splice_naming_a_file_built_at_compile_time_reaches_nothing_in_the_tree() {
        assert_eq!(
            spliced("include!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n"),
            []
        );
        assert_eq!(spliced("include_str!(\"table.rs\");\n"), []);
    }

    #[test]
    fn a_file_reached_only_by_include_is_not_dead_code() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", "mod generated;\n"),
                ("src/generated.rs", "include!(\"generated/prost.rs\");\n"),
                ("src/generated/prost.rs", "")
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_module_declared_inside_a_spliced_file_looks_from_the_module_it_was_spliced_into() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", "mod generated;\n"),
                (
                    "src/generated/mod.rs",
                    "mod proto {\n    include!(\"prost.rs\");\n}\n"
                ),
                ("src/generated/prost.rs", "mod part;\n"),
                ("src/generated/proto/part.rs", "")
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_declaration_reports_the_line_it_sits_on() {
        assert_eq!(decls("\n\nmod late;\n")[0].line, 3);
    }

    #[test]
    fn a_crate_root_owns_its_src_directory() {
        assert_eq!(base_dir("crates/c/src/lib.rs", true), "crates/c/src");
        assert_eq!(base_dir("src/parser.rs", false), "src/parser");
    }

    #[test]
    fn a_mod_rs_file_shares_the_directory_it_sits_in() {
        assert_eq!(base_dir("src/gates/mod.rs", false), "src/gates");
    }

    #[test]
    fn a_mod_declared_with_no_file_behind_it_is_reported() {
        assert_eq!(
            rendered(&[("Cargo.toml", ""), ("src/lib.rs", "mod parser;\n")]),
            ["src/lib.rs:1: parser: names no file — looked for src/parser.rs or src/parser/mod.rs"]
        );
    }

    #[test]
    fn a_file_no_declaration_reaches_is_reported() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", ""),
                ("src/orphan.rs", "")
            ]),
            ["src/orphan.rs: no `mod` declaration reaches this file — rustc never compiles it"]
        );
    }

    #[test]
    fn a_module_file_named_mod_rs_satisfies_its_declaration() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", "mod gates;\n"),
                ("src/gates/mod.rs", "mod slop;\n"),
                ("src/gates/slop.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_nested_module_resolves_under_the_directory_its_parent_owns() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", "mod outer;\n"),
                ("src/outer.rs", "mod inner;\n"),
                ("src/outer/inner.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_inline_module_sends_its_children_into_a_directory_of_its_own() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", "mod wrap {\n    mod leaf;\n}\n"),
                ("src/wrap/leaf.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_crate_with_both_roots_resolves_its_modules_through_either() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", "mod cli;\n"),
                ("src/main.rs", "fn main() {}\n"),
                ("src/cli.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_integration_test_file_is_a_root_of_its_own() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", ""),
                ("tests/end_to_end.rs", "mod common;\n"),
                ("tests/common/mod.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_binary_under_src_bin_is_a_root_of_its_own() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", ""),
                ("src/bin/server.rs", "mod wiring;\n"),
                ("src/bin/wiring.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_directory_target_is_rooted_at_its_main_but_not_below_it() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", ""),
                ("examples/demo/main.rs", "mod steps;\n"),
                ("examples/demo/steps.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_stray_file_outside_src_is_left_alone_because_a_harness_may_read_it() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", ""),
                ("examples/demo/main.rs", ""),
                ("examples/demo/stray.rs", ""),
                ("tests/output/expected.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_stray_file_under_src_is_still_reported() {
        assert_eq!(
            rendered(&[("Cargo.toml", ""), ("src/lib.rs", ""), ("src/stray.rs", ""),]),
            ["src/stray.rs: no `mod` declaration reaches this file — rustc never compiles it"]
        );
    }

    #[test]
    fn a_build_script_is_a_root_whose_children_sit_beside_it() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", ""),
                ("build.rs", "mod codegen;\n"),
                ("codegen.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_path_attribute_reaching_out_of_its_own_directory_still_resolves() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", ""),
                (
                    "tests/one.rs",
                    "#[path = \"../shared/helper.rs\"]\nmod helper;\n"
                ),
                ("shared/helper.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_target_the_manifest_names_by_path_is_a_root() {
        assert_eq!(
            rendered(&[
                (
                    "Cargo.toml",
                    "[[bin]]\nname = \"tool\"\npath = \"src/tools/tool.rs\"\n"
                ),
                ("src/lib.rs", ""),
                ("src/tools/tool.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_rust_file_cargo_never_compiles_is_left_alone() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", ""),
                ("docs/sample.rs", "")
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_directory_holding_no_rust_at_all_is_not_a_module() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", ""),
                ("src/snapshots/one.snap", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_nested_crate_is_judged_against_its_own_layout() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", "[workspace]\n"),
                ("crates/a/Cargo.toml", ""),
                ("crates/a/src/lib.rs", "mod inner;\n"),
                ("crates/a/src/inner.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    /// A shape once reported as a false orphan: a workspace member whose root holds a public
    /// inline module with no file of its own, and one child that has children in turn.
    #[test]
    fn a_public_inline_module_in_a_workspace_member_reaches_each_file_below_it() {
        let root = "pub mod handlers {\n    pub mod action;\n    pub mod landing;\n}\n";
        assert_eq!(
            rendered(&[
                ("Cargo.toml", "[workspace]\n"),
                ("crates/a/Cargo.toml", ""),
                ("crates/a/src/lib.rs", root),
                ("crates/a/src/handlers/action.rs", ""),
                ("crates/a/src/handlers/landing.rs", "mod page;\n"),
                ("crates/a/src/handlers/landing/page.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_waived_file_is_taken_at_its_word_in_both_directions() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", "// chock:modcheck-exempt\nmod nowhere;\n"),
                ("src/generated.rs", ""),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_target_compiles_what_a_root_reaches_and_what_a_waived_file_owns() {
        let files = [
            ("Cargo.toml", ""),
            ("src/lib.rs", "mod held;\n"),
            ("src/held.rs", ""),
            ("src/loose.rs", ""),
            ("tests/it.rs", "mod made;\n"),
            ("tests/made/mod.rs", "// chock:modcheck-exempt\n"),
            ("tests/made/by_a_macro.rs", ""),
            ("tests/ui/pass.rs", ""),
        ];
        let tree = tree(&files);
        let reach = walked(&tree, &tree.crates()).unwrap();
        let compiled: Vec<&str> = (files.iter().map(|(path, _)| *path))
            .filter(|path| reach.compiles(path))
            .collect();
        let expected = [
            "src/lib.rs",
            "src/held.rs",
            "tests/it.rs",
            "tests/made/mod.rs",
            "tests/made/by_a_macro.rs",
        ];
        assert_eq!(compiled, expected);
    }

    #[test]
    fn both_directions_of_a_module_fault_are_reported_in_one_run() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", "mod parser;\n"),
                ("src/orphan.rs", ""),
            ]),
            [
                "src/lib.rs:1: parser: names no file — looked for src/parser.rs or src/parser/mod.rs",
                "src/orphan.rs: no `mod` declaration reaches this file — rustc never compiles it",
            ]
        );
    }

    #[test]
    fn a_declaration_a_file_nothing_reaches_makes_is_not_reported_over_the_orphan_itself() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", ""),
                ("src/orphan.rs", "mod also_missing;\n"),
            ]),
            ["src/orphan.rs: no `mod` declaration reaches this file — rustc never compiles it"]
        );
    }

    /// Both roots declare `cli`; the finding below it shows the walk went on.
    #[test]
    fn a_file_the_walk_reaches_twice_does_not_stop_it_reaching_the_rest() {
        assert_eq!(
            rendered(&[
                ("Cargo.toml", ""),
                ("src/lib.rs", "mod cli;\n"),
                ("src/main.rs", "mod cli;\n"),
                ("src/cli.rs", "mod deep;\n"),
                ("src/cli/deep.rs", "mod absent_one;\n"),
            ]),
            ["src/cli/deep.rs:1: absent_one: names no file — looked for \
                 src/cli/deep/absent_one.rs or src/cli/deep/absent_one/mod.rs"]
        );
    }

    /// A manifest target whose file was deleted queues a path with no source.
    #[test]
    fn a_queued_path_the_tree_holds_no_source_for_does_not_stop_the_walk() {
        assert_eq!(
            rendered(&[
                (
                    "Cargo.toml",
                    "[[bin]]\nname = \"tool\"\npath = \"src/absent_target.rs\"\n"
                ),
                ("src/lib.rs", "mod parser;\n"),
            ]),
            ["src/lib.rs:1: parser: names no file — looked for src/parser.rs or src/parser/mod.rs"]
        );
    }

    #[test]
    fn a_file_that_cannot_be_parsed_is_a_failure_to_run_not_a_pass() {
        let broken = tree(&[("Cargo.toml", ""), ("src/lib.rs", "fn f( {\n")]);
        assert!(
            faults(&broken)
                .unwrap_err()
                .starts_with("src/lib.rs: line 1")
        );
    }

    #[test]
    fn a_target_outside_src_that_cannot_be_parsed_does_not_take_the_gate_down() {
        let corpus = tree(&[
            ("Cargo.toml", ""),
            ("src/lib.rs", "mod parser;\n"),
            ("examples/weird.rs", "fn f( {\n"),
        ]);
        assert_eq!(
            faults(&corpus)
                .unwrap()
                .iter()
                .map(Finding::render)
                .collect::<Vec<_>>(),
            ["src/lib.rs:1: parser: names no file — looked for src/parser.rs or src/parser/mod.rs"]
        );
    }

    #[test]
    fn a_tree_with_no_manifest_is_refused_rather_than_passed() {
        assert_eq!(
            faults(&tree(&[("src/lib.rs", "")])),
            Err("no Cargo.toml under the project root".to_string())
        );
    }

    #[test]
    fn a_finding_names_the_declared_module_so_a_reader_can_search_for_it() {
        let tree = tree(&[("Cargo.toml", ""), ("src/lib.rs", "mod parser;\n")]);
        let found = faults(&tree).unwrap();
        assert_eq!(found[0].item.as_deref(), Some("parser"));
        assert_eq!(found[0].line, Some(1));
    }

    fn ctx_of(files: &[(&str, &str)]) -> crate::testdir::Held {
        crate::testdir::Held::tree("gate-modcheck", files)
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn modcheck_reads_a_real_tree_and_reports_paths_relative_to_its_root() {
        let ctx = ctx_of(&[("Cargo.toml", ""), ("src/lib.rs", "mod parser;\n")]);
        let inspection = inspect(&ctx).unwrap();
        assert_eq!(inspection.debt, Vec::new());
        assert_eq!(inspection.blockers[0].file, "src/lib.rs");
        assert_eq!(inspection.blockers[0].item.as_deref(), Some("parser"));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_real_tree_whose_modules_all_resolve_passes() {
        let ctx = ctx_of(&[
            ("Cargo.toml", ""),
            ("src/lib.rs", "mod parser;\n"),
            ("src/parser.rs", "fn f() {}\n"),
        ]);
        assert_eq!(inspect(&ctx).unwrap(), Inspection::default());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_fixture_directory_under_src_is_not_judged_as_the_crates_own_modules() {
        let ctx = ctx_of(&[
            ("Cargo.toml", ""),
            ("src/lib.rs", "fn f() {}\n"),
            ("src/evals/fixtures/before.rs", "mod nothing_here;\n"),
        ]);
        assert_eq!(inspect(&ctx).unwrap(), Inspection::default());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri did not end this test in 19 minutes")]
    fn chocks_own_tree_resolves_in_both_directions() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let found = faults(&Tree(read(root).unwrap())).unwrap();
        let rendered: Vec<String> = found.iter().map(Finding::render).collect();
        assert_eq!(rendered, Vec::<String>::new());
    }
}
