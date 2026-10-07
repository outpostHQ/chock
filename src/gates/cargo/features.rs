//! The `features` gate: declared features against the `cfg`s that read them, both ways round.
//! cargo-machete and cargo-udeps judge dependencies and never read a `cfg`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::str::FromStr;

use crate::project::workspace::{Metadata, Package};
use proc_macro2::{Delimiter, Span, TokenStream, TokenTree};
use syn::visit::Visit;

use crate::gates::source::targets::{TARGET_DIRS, join};
use crate::project;
use crate::project::{MANIFEST, crate_dir};
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Inspection, Kind};

pub const GATE: Gate = Gate {
    name: "features",
    about: "every declared feature is reached by something, and every `cfg(feature)` names a \
            feature the package declares",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Debt {
        inspect,
        unit: "feature declaration issue(s)",
    },
};

const BUILD_SCRIPT: &str = "build.rs";

/// Prefix of the variable cargo sets for a build script, one per active feature.
const ENV_PREFIX: &str = "CARGO_FEATURE_";

impl Package {
    /// The manifest's name for each optional dependency: the rename when there is one.
    fn optional(&self) -> BTreeSet<&str> {
        self.dependencies
            .iter()
            .filter(|dependency| dependency.optional)
            .map(|dependency| dependency.rename.as_deref().unwrap_or(&dependency.name))
            .collect()
    }

    /// The names a `cfg` in this package may test: its features and optional dependencies.
    fn declares(&self) -> BTreeSet<&str> {
        self.features
            .keys()
            .map(String::as_str)
            .chain(self.optional())
            .collect()
    }

    /// The package directory relative to the root, or `None` when the package lies outside it.
    fn dir(&self, root: &Path) -> Option<String> {
        let manifest = project::relative(root, Path::new(&self.manifest_path));
        if manifest.starts_with('/') {
            return None;
        }
        crate_dir(&manifest)
    }
}

fn inspect(ctx: &Ctx) -> Result<Inspection, String> {
    let metadata = project::metadata(&ctx.root)?;
    faults(&ctx.root, &metadata, &read(&ctx.root)?).map(Inspection::debt)
}

/// Every `.rs` file and manifest under the root, keyed by the path a finding prints.
type Tree = BTreeMap<String, String>;

/// The gate's only filesystem read. A file that is not UTF-8 holds no `cfg` and is dropped; any
/// other read failure stops the gate.
fn read(root: &Path) -> Result<Tree, String> {
    Ok(super::modcheck::read(root)?
        .into_iter()
        .filter_map(|(path, text)| Some((path, text?)))
        .collect())
}

/// One site where a `cfg` names a feature.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Site {
    feature: String,
    line: u32,
}

/// What one package's own sources say about features.
#[derive(Debug, Default)]
struct Read {
    /// Each feature a `cfg` names, to the first line naming it in each file.
    cfgs: BTreeMap<String, BTreeMap<String, u32>>,
    /// The `CARGO_FEATURE_…` variables this package's build script writes as literals.
    env: BTreeSet<String>,
}

/// Unreferenced and undeclared features of each workspace member. Read from `cargo metadata`, not
/// the TOML, so `dep:` and the features cargo adds for optional dependencies arrive resolved.
fn faults(root: &Path, metadata_json: &str, tree: &Tree) -> Result<Vec<Finding>, String> {
    let metadata: Metadata = serde_json::from_str(metadata_json)
        .map_err(|e| format!("cargo metadata produced something unreadable: {e}"))?;
    let members = owners(root, &metadata);
    let reads = sources(tree, &members)?;
    let silent = Read::default();
    let mut findings = Vec::new();
    for (dir, package) in &members {
        let read = reads.get(dir.as_str()).unwrap_or(&silent);
        findings.extend(unreferenced(package, read, &join(dir, MANIFEST)));
        findings.extend(undeclared(package, read));
    }
    findings.sort_by(|a, b| (&a.file, a.line, &a.item).cmp(&(&b.file, b.line, &b.item)));
    Ok(findings)
}

/// Workspace members and their directories, deepest first so a nested member claims its files.
fn owners<'a>(root: &Path, metadata: &'a Metadata) -> Vec<(String, &'a Package)> {
    let mut members: Vec<(String, &Package)> = metadata
        .packages
        .iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
        .filter_map(|package| Some((package.dir(root)?, package)))
        .collect();
    members.sort_by_key(|(dir, _)| std::cmp::Reverse(dir.len()));
    members
}

/// Whether another file in the tree `include!`s this one, matched by file name.
fn pulled_in(tree: &Tree, path: &str) -> bool {
    project::included_elsewhere(
        tree.iter().map(|(at, src)| (at.as_str(), src.as_str())),
        path,
    )
}

/// What each member's compiled sources say, in one pass over the tree. A crate file that does not
/// parse stops the gate rather than leaving a feature looking unread.
fn sources<'a>(
    tree: &Tree,
    members: &'a [(String, &'a Package)],
) -> Result<BTreeMap<&'a str, Read>, String> {
    let dirs = homes(tree);
    let mine: BTreeSet<&str> = members.iter().map(|(dir, _)| dir.as_str()).collect();
    let mut reads: BTreeMap<&str, Read> = BTreeMap::new();
    for (path, src) in tree.iter().filter(|(path, _)| path.ends_with(".rs")) {
        let Some((home, script)) = compiled_by(path, &dirs, &mine) else {
            continue;
        };
        let read = reads.entry(home).or_default();
        let found = match cfg_sites(src) {
            Ok(found) => found,
            // Outside `src/` a file may be test input that never parses, as may an `include!`d
            // fragment; under `src/` the build would fail too.
            Err(_) if !project::is_crate_code(path, &dirs) || pulled_in(tree, path) => {
                continue;
            }
            Err(why) => return Err(format!("{path}: {why}")),
        };
        for site in found {
            let seen = read.cfgs.entry(site.feature).or_default();
            let first = seen.get(path).copied().unwrap_or(u32::MAX);
            seen.insert(path.clone(), first.min(site.line));
        }
        if script {
            read.env.extend(env_names(src));
        }
    }
    Ok(reads)
}

/// Every directory holding a manifest, deepest first. One that is no workspace member still owns
/// its files, which keeps a vendored or excluded crate out of the count.
fn homes(tree: &Tree) -> Vec<String> {
    let mut dirs: Vec<String> = tree.keys().filter_map(|path| crate_dir(path)).collect();
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.len()));
    dirs
}

/// The workspace member cargo compiles this file into, and whether it is that package's build
/// script; `None` when another package owns the file or cargo compiles it nowhere.
fn compiled_by<'a>(
    path: &str,
    dirs: &[String],
    members: &BTreeSet<&'a str>,
) -> Option<(&'a str, bool)> {
    let (home, rest) = dirs
        .iter()
        .find_map(|home| Some((home, project::under(path, home)?)))?;
    let owner = members.get(home.as_str()).copied()?;
    if rest == BUILD_SCRIPT {
        return Some((owner, true));
    }
    let compiled = TARGET_DIRS
        .iter()
        .any(|dir| project::under(rest, dir).is_some());
    compiled.then_some((owner, false))
}

/// Declared features that activate nothing and that nothing reaches. `default` and an optional
/// dependency's implicit feature are exempt.
fn unreferenced(package: &Package, read: &Read, manifest: &str) -> Vec<Finding> {
    let optional = package.optional();
    let reached = reached(package, read);
    package
        .features
        .iter()
        .filter(|(feature, activates)| {
            feature.as_str() != "default"
                && activates.is_empty()
                && !optional.contains(feature.as_str())
                && !reached.contains(feature.as_str())
        })
        .map(|(feature, _)| {
            let message = "declared but nothing reaches it — no cfg names it, \
                           it activates nothing, and no target requires it";
            Finding::at(manifest, message).item(&format!("{}: {feature}", package.name))
        })
        .collect()
}

/// The features something reaches: a `cfg`, another feature activating it, a target's
/// `required-features`, or a build script reading its `CARGO_FEATURE_` variable.
fn reached<'a>(package: &'a Package, read: &'a Read) -> BTreeSet<&'a str> {
    let listed = package
        .features
        .values()
        .flatten()
        .filter_map(|entry| plain(entry.as_str()));
    let required = package
        .targets
        .iter()
        .flat_map(|target| target.required_features.iter().map(String::as_str));
    let scripted = package
        .features
        .keys()
        .filter(|feature| read.env.contains(&env_name(feature)))
        .map(String::as_str);
    read.cfgs
        .keys()
        .map(String::as_str)
        .chain(listed)
        .chain(required)
        .chain(scripted)
        .collect()
}

/// The entry as a feature of this manifest, or `None` for `dep:serde` or `serde/derive`, which
/// name a dependency.
fn plain(entry: &str) -> Option<&str> {
    (!entry.contains('/') && !entry.starts_with("dep:")).then_some(entry)
}

/// The build-script variable for a feature: `slow-tests` becomes `CARGO_FEATURE_SLOW_TESTS`.
fn env_name(feature: &str) -> String {
    format!(
        "{ENV_PREFIX}{}",
        feature.to_ascii_uppercase().replace('-', "_")
    )
}

/// `cfg`s naming a feature their package does not declare, so the code behind them never compiles.
/// Reported once per file, at the first site.
fn undeclared(package: &Package, read: &Read) -> Vec<Finding> {
    let declared = package.declares();
    read.cfgs
        .iter()
        .filter(|(feature, _)| !declared.contains(feature.as_str()))
        .flat_map(|(feature, sites)| {
            sites.iter().map(move |(file, line)| {
                let message = format!(
                    "no [features] entry and no optional dependency of \"{}\" declares it, \
                     so this cfg is never true",
                    package.name
                );
                Finding::at(file, &message).line(*line).item(feature)
            })
        })
        .collect()
}

/// Every feature a `cfg` in one file names. Parsed rather than matched, so a `cfg` in a string or
/// comment names nothing.
fn cfg_sites(src: &str) -> Result<Vec<Site>, String> {
    let file = syn::parse_file(src).map_err(|e| format!("line {}: {e}", e.span().start().line))?;
    let mut walk = Cfgs::default();
    walk.visit_file(&file);
    Ok(walk.sites)
}

#[derive(Debug, Default)]
struct Cfgs {
    sites: Vec<Site>,
}

impl<'ast> Visit<'ast> for Cfgs {
    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        if let syn::Meta::List(list) = &attr.meta
            && (list.path.is_ident("cfg") || list.path.is_ident("cfg_attr"))
        {
            named(list.tokens.clone(), &mut self.sites);
        }
        syn::visit::visit_attribute(self, attr);
    }

    /// Reads `cfg!(…)` as a condition and scans any other macro body, which `syn` leaves as
    /// tokens, for a `cfg` (`cfg_if!`, `macro_rules!`).
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if mac.path.is_ident("cfg") || mac.path.is_ident("cfg_attr") {
            named(mac.tokens.clone(), &mut self.sites);
        } else {
            gated(mac.tokens.clone(), &mut self.sites);
        }
        syn::visit::visit_macro(self, mac);
    }
}

/// Every `feature = "…"` in one condition at any depth; one under `not` still counts as named.
fn named(tokens: TokenStream, out: &mut Vec<Site>) {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    for window in trees.windows(3) {
        let [
            TokenTree::Ident(key),
            TokenTree::Punct(eq),
            TokenTree::Literal(value),
        ] = window
        else {
            continue;
        };
        if key != "feature" || eq.as_char() != '=' {
            continue;
        }
        if let syn::Lit::Str(text) = syn::Lit::new(value.clone()) {
            out.push(Site {
                feature: text.value(),
                line: line_of(value.span()),
            });
        }
    }
    for tree in trees {
        if let TokenTree::Group(group) = tree {
            named(group.stream(), out);
        }
    }
}

/// Finds each `cfg` or `cfg_attr` in loose macro tokens and reads its condition.
fn gated(tokens: TokenStream, out: &mut Vec<Site>) {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    for (at, tree) in trees.iter().enumerate() {
        match tree {
            TokenTree::Ident(name) if name == "cfg" || name == "cfg_attr" => {
                if let Some(condition) = condition(&trees, at) {
                    named(condition, out);
                }
            }
            TokenTree::Group(group) => gated(group.stream(), out),
            _ => {}
        }
    }
}

/// The parenthesised condition after a `cfg` ident, stepping over the `!` of the macro form.
fn condition(trees: &[TokenTree], at: usize) -> Option<TokenStream> {
    let next = trees.get(at + 1)?;
    let holder = match next {
        TokenTree::Punct(bang) if bang.as_char() == '!' => trees.get(at + 2)?,
        other => other,
    };
    match holder {
        TokenTree::Group(group) if group.delimiter() == Delimiter::Parenthesis => {
            Some(group.stream())
        }
        _ => None,
    }
}

/// Every `CARGO_FEATURE_…` string literal in a build script. Tokens, not the syntax tree, because
/// the name is often passed to a macro rather than `env::var`.
fn env_names(src: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Ok(tokens) = TokenStream::from_str(src) {
        literals(tokens, &mut names);
    }
    names
}

fn literals(tokens: TokenStream, out: &mut BTreeSet<String>) {
    for tree in tokens {
        match tree {
            TokenTree::Literal(value) => {
                if let syn::Lit::Str(text) = syn::Lit::new(value) {
                    let text = text.value();
                    if text.starts_with(ENV_PREFIX) {
                        out.insert(text);
                    }
                }
            }
            TokenTree::Group(group) => literals(group.stream(), out),
            _ => {}
        }
    }
}

fn line_of(span: Span) -> u32 {
    u32::try_from(span.start().line).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::run::baseline::Baseline;

    const ROOT: &str = "/w";

    const DECLARED: &str = "declared but nothing reaches it — no cfg names it, \
                            it activates nothing, and no target requires it";

    const UNDECLARED: &str = "no [features] entry and no optional dependency of \"demo\" \
                              declares it, so this cfg is never true";

    fn root() -> &'static Path {
        Path::new(ROOT)
    }

    /// One workspace member at the root, with whatever `[features]` and extra tables a case needs.
    fn metadata(features: &str, extra: &str) -> String {
        format!(
            r#"{{"packages":[{{"id":"me","name":"demo","manifest_path":"{ROOT}/Cargo.toml",
               "features":{{{features}}}{extra}}}],"workspace_members":["me"]}}"#
        )
    }

    fn tree(files: &[(&str, &str)]) -> Tree {
        files
            .iter()
            .map(|(path, src)| ((*path).to_string(), (*src).to_string()))
            .collect()
    }

    fn rendered(features: &str, extra: &str, files: &[(&str, &str)]) -> Vec<String> {
        let mut all = vec![(MANIFEST, "")];
        all.extend_from_slice(files);
        faults(root(), &metadata(features, extra), &tree(&all))
            .unwrap()
            .iter()
            .map(Finding::render)
            .collect()
    }

    fn features_of(src: &str) -> Vec<String> {
        cfg_sites(src)
            .unwrap()
            .into_iter()
            .map(|site| site.feature)
            .collect()
    }

    #[test]
    fn a_feature_no_cfg_names_and_nothing_activates_is_reported() {
        assert_eq!(
            rendered(r#""stale":[]"#, "", &[("src/lib.rs", "fn f() {}\n")]),
            [format!("Cargo.toml: demo: stale: {DECLARED}")]
        );
    }

    #[test]
    fn a_feature_a_cfg_reads_is_not_reported() {
        assert_eq!(
            rendered(
                r#""live":[]"#,
                "",
                &[("src/lib.rs", "#[cfg(feature = \"live\")]\nfn f() {}\n")]
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_default_feature_is_read_by_cargo_itself_and_never_reported() {
        assert_eq!(
            rendered(r#""default":[]"#, "", &[("src/lib.rs", "")]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_optional_dependency_is_not_reported_as_an_unreferenced_feature() {
        let extra = r#","dependencies":[{"name":"serde","optional":true}]"#;
        assert_eq!(
            rendered(r#""serde":[]"#, extra, &[("src/lib.rs", "")]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_renamed_optional_dependency_is_known_by_the_name_the_manifest_uses() {
        let extra = r#","dependencies":[{"name":"real","optional":true,"rename":"alias"}]"#;
        assert_eq!(
            rendered(r#""alias":[]"#, extra, &[("src/lib.rs", "")]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_dependency_that_is_not_optional_declares_no_feature_of_its_name() {
        let extra = r#","dependencies":[{"name":"serde","optional":false}]"#;
        assert_eq!(
            rendered(
                "",
                extra,
                &[("src/lib.rs", "#[cfg(feature = \"serde\")]\nfn f() {}\n")]
            ),
            [format!("src/lib.rs:1: serde: {UNDECLARED}")]
        );
    }

    #[test]
    fn a_feature_that_exists_only_to_turn_a_dependency_feature_on_is_not_reported() {
        assert_eq!(
            rendered(
                r#""tabular":["polars/lazy"]"#,
                "",
                &[("src/lib.rs", "fn f() {}\n")]
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_feature_another_feature_activates_is_referenced_by_the_manifest() {
        assert_eq!(
            rendered(
                r#""leaf":[],"production":["leaf"]"#,
                "",
                &[("src/lib.rs", "")]
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_dep_entry_in_another_features_list_does_not_vouch_for_a_feature_of_that_name() {
        assert_eq!(
            rendered(
                r#""serde":[],"full":["dep:serde"]"#,
                "",
                &[("src/lib.rs", "")]
            ),
            [format!("Cargo.toml: demo: serde: {DECLARED}")]
        );
    }

    #[test]
    fn a_feature_a_target_cannot_be_built_without_is_referenced() {
        let extra = r#","targets":[{"required-features":["testkit"]}]"#;
        assert_eq!(
            rendered(r#""testkit":[]"#, extra, &[("src/lib.rs", "")]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_feature_a_build_script_reads_through_its_cargo_variable_is_referenced() {
        let src = "fn main() {\n    let on = std::env::var(\"CARGO_FEATURE_SLOW_TESTS\").is_ok();\n    println!(\"{on}\");\n}\n";
        assert_eq!(
            rendered(r#""slow-tests":[]"#, "", &[(BUILD_SCRIPT, src)]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn that_variable_in_an_ordinary_source_file_is_not_a_build_script_reading_it() {
        let src = "fn f() -> &'static str {\n    \"CARGO_FEATURE_STALE\"\n}\n";
        assert_eq!(
            rendered(r#""stale":[]"#, "", &[("src/lib.rs", src)]),
            [format!("Cargo.toml: demo: stale: {DECLARED}")]
        );
    }

    #[test]
    fn a_cfg_naming_a_feature_the_manifest_never_declares_is_reported() {
        assert_eq!(
            rendered(
                "",
                "",
                &[("src/lib.rs", "#[cfg(feature = \"nightly\")]\nfn f() {}\n")]
            ),
            [format!("src/lib.rs:1: nightly: {UNDECLARED}")]
        );
    }

    #[test]
    fn an_optional_dependency_declares_the_feature_a_cfg_tests() {
        let extra = r#","dependencies":[{"name":"serde","optional":true}]"#;
        assert_eq!(
            rendered(
                "",
                extra,
                &[("src/lib.rs", "#[cfg(feature = \"serde\")]\nfn f() {}\n")]
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn both_directions_are_reported_in_one_run() {
        assert_eq!(
            rendered(
                r#""stale":[]"#,
                "",
                &[("src/lib.rs", "#[cfg(feature = \"ghost\")]\nfn f() {}\n")]
            ),
            [
                format!("Cargo.toml: demo: stale: {DECLARED}"),
                format!("src/lib.rs:1: ghost: {UNDECLARED}"),
            ]
        );
    }

    #[test]
    fn one_undeclared_feature_read_twice_in_a_file_is_one_finding_at_the_first_site() {
        let src =
            "#[cfg(feature = \"ghost\")]\nfn a() {}\n#[cfg(feature = \"ghost\")]\nfn b() {}\n";
        assert_eq!(
            rendered("", "", &[("src/lib.rs", src)]),
            [format!("src/lib.rs:1: ghost: {UNDECLARED}")]
        );
    }

    #[test]
    fn the_same_undeclared_feature_in_two_files_is_reported_in_each_of_them() {
        let src = "#[cfg(feature = \"ghost\")]\nfn f() {}\n";
        assert_eq!(
            rendered("", "", &[("src/a.rs", src), ("src/b.rs", src)]),
            [
                format!("src/a.rs:1: ghost: {UNDECLARED}"),
                format!("src/b.rs:1: ghost: {UNDECLARED}"),
            ]
        );
    }

    #[test]
    fn a_cfg_on_a_module_declaration_counts_as_reading_the_feature() {
        assert_eq!(
            rendered(
                r#""tabular":[]"#,
                "",
                &[("src/lib.rs", "#[cfg(feature = \"tabular\")]\nmod df;\n")]
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_cfg_in_a_build_script_counts_as_reading_the_feature() {
        let src = "#[cfg(feature = \"probe\")]\nfn probe() {}\nfn main() {}\n";
        assert_eq!(
            rendered(r#""probe":[]"#, "", &[(BUILD_SCRIPT, src)]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_cfg_in_a_test_an_example_or_a_benchmark_counts_as_reading_the_feature() {
        for dir in TARGET_DIRS {
            let path = format!("{dir}/case.rs");
            assert_eq!(
                rendered(
                    r#""live":[]"#,
                    "",
                    &[(path.as_str(), "#[cfg(feature = \"live\")]\nfn f() {}\n")]
                ),
                Vec::<String>::new(),
                "{dir}"
            );
        }
    }

    #[test]
    fn a_cfg_inside_a_comment_or_a_string_literal_names_nothing() {
        let src = "// #[cfg(feature = \"commented\")]\nconst S: &str = \"cfg(feature = \\\"quoted\\\")\";\n";
        assert_eq!(features_of(src), Vec::<String>::new());
    }

    #[test]
    fn a_feature_named_only_inside_a_string_is_still_reported_as_unreferenced() {
        let src = "const S: &str = \"#[cfg(feature = \\\"stale\\\")]\";\n";
        assert_eq!(
            rendered(r#""stale":[]"#, "", &[("src/lib.rs", src)]),
            [format!("Cargo.toml: demo: stale: {DECLARED}")]
        );
    }

    #[test]
    fn a_feature_nested_inside_all_any_or_not_is_read_through() {
        let src = "#[cfg(all(unix, any(feature = \"a\", not(feature = \"b\"))))]\nfn f() {}\n";
        assert_eq!(features_of(src), ["a", "b"]);
    }

    #[test]
    fn a_cfg_attr_names_the_feature_it_gates_on() {
        let src = "#[cfg_attr(feature = \"serde\", derive(Serialize))]\nstruct S;\n";
        assert_eq!(features_of(src), ["serde"]);
    }

    #[test]
    fn the_cfg_macro_in_an_expression_names_its_feature() {
        assert_eq!(
            features_of("fn f() -> bool {\n    cfg!(feature = \"live\")\n}\n"),
            ["live"]
        );
    }

    #[test]
    fn a_crate_level_inner_attribute_names_its_feature() {
        assert_eq!(
            features_of("#![cfg(feature = \"whole\")]\nfn f() {}\n"),
            ["whole"]
        );
    }

    #[test]
    fn a_cfg_inside_a_macro_body_the_grammar_never_enters_is_still_found() {
        let src = "macro_rules! wrap {\n    () => {\n        #[cfg(feature = \"inner\")]\n        fn f() {}\n    };\n}\n";
        assert_eq!(features_of(src), ["inner"]);
    }

    #[test]
    fn a_cfg_if_block_names_the_feature_it_switches_on() {
        let src = "fn f() {\n    cfg_if! {\n        if #[cfg(feature = \"fast\")] { g() } else { h() }\n    }\n}\n";
        assert_eq!(features_of(src), ["fast"]);
    }

    #[test]
    fn a_local_binding_called_feature_is_not_a_cfg() {
        assert_eq!(
            features_of("fn f() {\n    let feature = \"nightly\";\n    drop(feature);\n}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_cfg_reports_the_line_the_feature_is_written_on() {
        let sites = cfg_sites("fn f() {}\n\n#[cfg(feature = \"late\")]\nfn g() {}\n").unwrap();
        assert_eq!(
            sites,
            [Site {
                feature: "late".to_string(),
                line: 3,
            }]
        );
    }

    #[test]
    fn a_source_file_that_will_not_parse_stops_the_gate_rather_than_passing() {
        let broken = tree(&[(MANIFEST, ""), ("src/lib.rs", "fn f( {\n")]);
        let err = faults(root(), &metadata("", ""), &broken).unwrap_err();
        assert!(err.starts_with("src/lib.rs: line 1"), "{err}");
    }

    #[test]
    fn metadata_that_is_not_json_is_refused_rather_than_read_as_clean() {
        assert!(faults(root(), "not json", &tree(&[])).is_err());
    }

    #[test]
    fn a_rust_file_cargo_compiles_nowhere_is_neither_evidence_nor_a_finding() {
        assert_eq!(
            rendered(
                r#""stale":[]"#,
                "",
                &[("docs/sample.rs", "#[cfg(feature = \"stale\")]\nfn f() {}\n")]
            ),
            [format!("Cargo.toml: demo: stale: {DECLARED}")]
        );
    }

    #[test]
    fn a_file_cargo_compiles_nowhere_does_not_stop_the_read_at_the_one_behind_it() {
        assert_eq!(
            rendered(
                "",
                "",
                &[
                    ("docs/sample.rs", "fn f() {}\n"),
                    ("src/lib.rs", "#[cfg(feature = \"ghost\")]\nfn g() {}\n"),
                ]
            ),
            [format!("src/lib.rs:1: ghost: {UNDECLARED}")]
        );
    }

    #[test]
    fn a_file_belonging_to_a_nested_crate_is_not_read_for_its_neighbour() {
        assert_eq!(
            rendered(
                "",
                "",
                &[
                    ("tests/fixtures/demo/Cargo.toml", ""),
                    (
                        "tests/fixtures/demo/src/lib.rs",
                        "#[cfg(feature = \"magic\")]\nfn f() {}\n"
                    ),
                ]
            ),
            Vec::<String>::new()
        );
    }

    /// Metadata for two workspace members, `core` and `cli`, each declaring one feature.
    fn workspace() -> String {
        format!(
            r#"{{"packages":[
               {{"id":"a","name":"core","manifest_path":"{ROOT}/crates/core/Cargo.toml",
                 "features":{{"tabular":[]}}}},
               {{"id":"b","name":"cli","manifest_path":"{ROOT}/crates/cli/Cargo.toml",
                 "features":{{"live":[]}}}}],
               "workspace_members":["a","b"]}}"#
        )
    }

    fn workspace_faults(files: &[(&str, &str)]) -> Vec<String> {
        faults(root(), &workspace(), &tree(files))
            .unwrap()
            .iter()
            .map(Finding::render)
            .collect()
    }

    #[test]
    fn each_member_of_a_workspace_answers_for_its_own_features() {
        let found = workspace_faults(&[
            ("crates/core/Cargo.toml", ""),
            (
                "crates/core/src/lib.rs",
                "#[cfg(feature = \"tabular\")]\nfn f() {}\n",
            ),
            ("crates/cli/Cargo.toml", ""),
            (
                "crates/cli/src/main.rs",
                "#[cfg(feature = \"live\")]\nfn f() {}\n",
            ),
        ]);
        assert_eq!(found, Vec::<String>::new());
    }

    #[test]
    fn a_cfg_in_one_member_neither_declares_nor_reads_a_feature_of_another() {
        let found = workspace_faults(&[
            ("crates/core/Cargo.toml", ""),
            ("crates/core/src/lib.rs", "fn f() {}\n"),
            ("crates/cli/Cargo.toml", ""),
            (
                "crates/cli/src/main.rs",
                "#[cfg(feature = \"tabular\")]\nfn f() {}\n",
            ),
        ]);
        assert_eq!(
            found,
            [
                "crates/cli/Cargo.toml: cli: live: declared but nothing reaches it — \
                 no cfg names it, it activates nothing, and no target requires it",
                "crates/cli/src/main.rs:1: tabular: no [features] entry and no optional \
                 dependency of \"cli\" declares it, so this cfg is never true",
                "crates/core/Cargo.toml: core: tabular: declared but nothing reaches it — \
                 no cfg names it, it activates nothing, and no target requires it",
            ]
        );
    }

    #[test]
    fn a_package_outside_the_root_chock_was_given_is_left_alone() {
        let json = r#"{"packages":[{"id":"a","name":"far","manifest_path":"/elsewhere/Cargo.toml",
                      "features":{"stale":[]}}],"workspace_members":["a"]}"#;
        assert_eq!(
            faults(root(), json, &tree(&[(MANIFEST, "")])).unwrap(),
            vec![]
        );
    }

    #[test]
    fn a_package_the_workspace_does_not_hold_is_not_this_projects_problem() {
        let json = format!(
            r#"{{"packages":[{{"id":"other","name":"vendored",
               "manifest_path":"{ROOT}/Cargo.toml","features":{{"stale":[]}}}}],
               "workspace_members":["me"]}}"#
        );
        assert_eq!(
            faults(root(), &json, &tree(&[(MANIFEST, "")])).unwrap(),
            vec![]
        );
    }

    fn ctx_of(files: &[(&str, &str)]) -> crate::testdir::Held {
        crate::testdir::Held::tree("gate-features", files)
    }

    const CRATE: &str = "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_gate_runs_cargo_against_a_real_tree_and_reports_what_it_finds() {
        let ctx = ctx_of(&[
            (
                MANIFEST,
                &format!("{CRATE}\n[features]\nstale = []\n\n[workspace]\n"),
            ),
            ("src/lib.rs", "pub fn f() {}\n"),
        ]);
        let inspection = inspect(&ctx).unwrap();
        assert_eq!(inspection.blockers, Vec::new());
        assert_eq!(inspection.debt[0].file, MANIFEST);
        assert_eq!(inspection.debt[0].item.as_deref(), Some("probe: stale"));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_real_tree_whose_features_are_all_read_passes() {
        let ctx = ctx_of(&[
            (
                MANIFEST,
                &format!("{CRATE}\n[features]\nlive = []\n\n[workspace]\n"),
            ),
            ("src/lib.rs", "#[cfg(feature = \"live\")]\npub fn f() {}\n"),
        ]);
        assert_eq!(inspect(&ctx).unwrap(), Inspection::default());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_cfg_inside_a_fixture_directory_is_not_the_surrounding_crates_own() {
        let ctx = ctx_of(&[
            (MANIFEST, &format!("{CRATE}\n[workspace]\n")),
            ("src/lib.rs", "pub fn f() {}\n"),
            (
                "src/evals/fixtures/before.rs",
                "#[cfg(feature = \"tree-sitter-tags\")]\nfn g() {}\n",
            ),
        ]);
        assert_eq!(inspect(&ctx).unwrap(), Inspection::default());
    }

    /// chock declares no feature, so its own tree must come out clean.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn chocks_own_tree_invents_nothing() {
        let ctx = Ctx::for_root(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            Baseline::empty("0.1.0"),
        );
        assert_eq!(inspect(&ctx).unwrap(), Inspection::default());
    }
}
