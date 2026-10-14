//! Paths and crates a project lets only some files or packages use, listed under `seams`. A seam
//! keeps a dependency behind one module, so replacing it changes one place.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use syn::visit::Visit;

use crate::gates::metrics::prodlines::{for_each_source, parse_rust};
use crate::gates::rules::{Aliases, leaves, line_of, production, segments};
use crate::gates::text::phrases::globbed;
use crate::project::config::Seam;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Inspection, Kind};

pub const GATE: Gate = Gate {
    name: "seams",
    about: "a path or crate listed under `seams` is used outside the files or packages it names",
    group: Group::OptIn,
    builds: false,
    reads: None,
    kind: Kind::Debt {
        inspect,
        unit: "use(s) past a seam",
    },
};

fn inspect(ctx: &Ctx) -> Result<Inspection, String> {
    let (paths, crates) = split(&ctx.seams)?;
    let mut debt = Vec::new();
    if !paths.is_empty() {
        for (shown, uses) in for_each_source(ctx, &|src| uses(src, &paths))? {
            debt.extend(outside(&shown, &uses, &paths));
        }
    }
    if !crates.is_empty() {
        debt.extend(depending(&crate::project::resolved(&ctx.root)?, &crates)?);
    }
    Ok(Inspection::debt(debt))
}

/// The seams split into paths and crates. An entry with neither or both, or with no reason, is
/// refused rather than read as allowing everything.
fn split(seams: &[Seam]) -> Result<(Vec<&Seam>, Vec<&Seam>), String> {
    if seams.is_empty() {
        return Err(format!(
            "no seam is listed under `seams` in {}",
            crate::project::config::FILE
        ));
    }
    let mut paths = Vec::new();
    let mut crates = Vec::new();
    for seam in seams {
        let path = seam
            .path
            .as_deref()
            .filter(|path| !segments(path).is_empty());
        let krate = seam
            .krate
            .as_deref()
            .filter(|krate| !krate.trim().is_empty());
        let named = match (path, krate) {
            (Some(path), None) => {
                paths.push(seam);
                path
            }
            (None, Some(krate)) => {
                crates.push(seam);
                krate
            }
            (Some(path), Some(_)) => {
                return Err(format!(
                    "the seam `{path}` names a `crate` too; give each its own entry"
                ));
            }
            (None, None) => return Err("a `seams` entry names no `path` and no `crate`".into()),
        };
        if seam.why.trim().is_empty() {
            return Err(format!("the seam `{named}` gives no `why`"));
        }
    }
    Ok((paths, crates))
}

/// Each path seam a file uses, by its place in `seams`, with each line it is used on. Read through
/// the file's `use` aliases; a macro body and test code are not read.
fn uses(src: &str, seams: &[&Seam]) -> Result<BTreeSet<(usize, u32)>, String> {
    let file = parse_rust(src)?;
    let mut found = Paths {
        aliases: Aliases::of(&file),
        seen: Vec::new(),
    };
    for item in production(&file.items) {
        found.visit_item(item);
    }
    let wanted: Vec<Vec<String>> = seams
        .iter()
        .map(|seam| segments(seam.path.as_deref().unwrap_or_default()))
        .collect();
    let mut hits = BTreeSet::new();
    for (path, line) in &found.seen {
        for (index, seam) in wanted.iter().enumerate() {
            if path.starts_with(seam) {
                hits.insert((index, *line));
            }
        }
    }
    Ok(hits)
}

/// Every path a file writes, as the full path it stands for, with the line it starts on. Each
/// path a `use` names counts: `use a::{b, c::*}` names `a::b` and `a::c`.
struct Paths {
    aliases: Aliases,
    seen: Vec<(Vec<String>, u32)>,
}

impl<'ast> Visit<'ast> for Paths {
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        let mut found = Vec::new();
        leaves(&[], &node.tree, &mut found);
        let named = found
            .into_iter()
            .map(|leaf| (leaf.path, line_of(leaf.span)));
        self.seen.extend(named);
    }

    fn visit_path(&mut self, node: &'ast syn::Path) {
        if let Some(first) = node.segments.first() {
            let line = line_of(first.ident.span());
            self.seen.push((self.aliases.full(node), line));
        }
        syn::visit::visit_path(self, node);
    }
}

/// A finding for each use in a file its seam's `only` does not name.
fn outside(shown: &str, uses: &BTreeSet<(usize, u32)>, seams: &[&Seam]) -> Vec<Finding> {
    uses.iter()
        .filter(|(index, _)| !seams[*index].only.iter().any(|place| globbed(place, shown)))
        .map(|(index, line)| {
            let seam = seams[*index];
            let path = seam.path.as_deref().unwrap_or_default();
            let told = format!(
                "`{path}` is used outside {}: {}",
                places(&seam.only),
                seam.why
            );
            Finding::at(shown, &told).item(path).line(*line)
        })
        .collect()
}

fn places(only: &[String]) -> String {
    if only.is_empty() {
        return "every place, since `only` names none".to_string();
    }
    let named: Vec<String> = only.iter().map(|place| format!("`{place}`")).collect();
    named.join(", ")
}

/// One package in the resolved graph.
struct Package {
    name: String,
    manifest: String,
    /// The packages it depends on as a normal dependency, not only to build or to test.
    normal: Vec<String>,
}

/// Each workspace package outside a crate seam's `only` that holds the crate anywhere in its normal
/// dependency tree, found at its manifest. A package `only` names that is no member is refused.
fn depending(metadata: &str, seams: &[&Seam]) -> Result<Vec<Finding>, String> {
    let (members, graph) = graph(metadata)?;
    let names: BTreeSet<&str> = members.iter().map(|id| graph[id].name.as_str()).collect();
    let mut found = Vec::new();
    for seam in seams {
        let krate = seam.krate.as_deref().unwrap_or_default();
        if let Some(unknown) = seam.only.iter().find(|name| !names.contains(name.as_str())) {
            return Err(format!(
                "the seam `{krate}` allows `{unknown}`, which is no package in this workspace"
            ));
        }
        for member in &members {
            let package = &graph[member];
            if package.name == krate || seam.only.contains(&package.name) {
                continue;
            }
            if let Some(chain) = route(member, krate, &graph) {
                let told = format!(
                    "`{}` depends on `{krate}` through {chain}, outside {}: {}",
                    package.name,
                    places(&seam.only),
                    seam.why
                );
                found.push(Finding::at(&package.manifest, &told).item(krate));
            }
        }
    }
    Ok(found)
}

/// The workspace members and every package by id, from `cargo metadata` with its resolved graph.
fn graph(metadata: &str) -> Result<(Vec<String>, BTreeMap<String, Package>), String> {
    let read: serde_json::Value = serde_json::from_str(metadata)
        .map_err(|e| format!("cargo metadata printed a graph chock cannot read: {e}"))?;
    let unread = || "cargo metadata printed no resolved dependency graph".to_string();
    let root = read["workspace_root"].as_str().ok_or_else(unread)?;
    let nodes = read["resolve"]["nodes"].as_array().ok_or_else(unread)?;
    // A dependency is normal when one of its kinds is neither `dev` nor `build`.
    let normal_deps = |node: &serde_json::Value| -> Vec<String> {
        let deps = node["deps"].as_array().into_iter().flatten();
        deps.filter(|dep| {
            let mut kinds = dep["dep_kinds"].as_array().into_iter().flatten();
            kinds.any(|kind| kind["kind"].is_null())
        })
        .filter_map(|dep| dep["pkg"].as_str().map(str::to_string))
        .collect()
    };
    let normal: BTreeMap<&str, Vec<String>> = nodes
        .iter()
        .filter_map(|node| Some((node["id"].as_str()?, normal_deps(node))))
        .collect();
    let mut graph = BTreeMap::new();
    for package in read["packages"].as_array().ok_or_else(unread)? {
        let (Some(id), Some(name)) = (package["id"].as_str(), package["name"].as_str()) else {
            return Err(unread());
        };
        let manifest = package["manifest_path"].as_str().unwrap_or_default();
        let shown = crate::project::relative(Path::new(root), Path::new(manifest));
        graph.insert(
            id.to_string(),
            Package {
                name: name.to_string(),
                manifest: shown,
                normal: normal.get(id).cloned().unwrap_or_default(),
            },
        );
    }
    let members: Vec<String> = read["workspace_members"]
        .as_array()
        .ok_or_else(unread)?
        .iter()
        .filter_map(|id| id.as_str().map(str::to_string))
        .filter(|id| graph.contains_key(id))
        .collect();
    Ok((members, graph))
}

/// The shortest chain of normal dependencies from a package to a crate, as `a → b → crate`.
fn route(from: &str, krate: &str, graph: &BTreeMap<String, Package>) -> Option<String> {
    let mut came_from: BTreeMap<&str, &str> = BTreeMap::new();
    let mut queue = VecDeque::from([from]);
    while let Some(id) = queue.pop_front() {
        let package = graph.get(id)?;
        if package.name == krate && id != from {
            let mut chain = vec![package.name.as_str()];
            let mut at = id;
            while let Some(previous) = came_from.get(at) {
                chain.push(graph[*previous].name.as_str());
                at = previous;
            }
            chain.reverse();
            return Some(chain.join(" → "));
        }
        for next in &package.normal {
            if next != from && graph.contains_key(next) && !came_from.contains_key(next.as_str()) {
                came_from.insert(next.as_str(), id);
                queue.push_back(next.as_str());
            }
        }
    }
    None
}

/// The path seams that apply to one file, for the editor hook: those whose `only` does not name it.
#[must_use]
pub fn applying(seams: &[Seam], shown: &str) -> Vec<Seam> {
    seams
        .iter()
        .filter(|seam| seam.krate.is_none() && seam.path.is_some())
        .filter(|seam| !seam.only.iter().any(|place| globbed(place, shown)))
        .cloned()
        .collect()
}

/// The uses of `seams` in one file, as the gate finds them. A file that does not parse has none.
#[must_use]
pub fn found(path: &str, src: &str, seams: &[Seam]) -> Vec<Finding> {
    let listed: Vec<&Seam> = seams.iter().collect();
    uses(src, &listed).map_or_else(|_| Vec::new(), |hits| outside(path, &hits, &listed))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn seam(path: &str, only: &[&str]) -> Seam {
        Seam {
            path: Some(path.to_string()),
            only: only.iter().map(|place| (*place).to_string()).collect(),
            why: "one module talks to it".to_string(),
            ..Seam::default()
        }
    }

    fn hits(src: &str, seams: &[Seam]) -> Vec<String> {
        found("src/lib.rs", src, seams)
            .iter()
            .map(Finding::render)
            .collect()
    }

    #[test]
    fn a_use_of_a_seam_outside_its_place_is_found_on_each_line_through_aliases() {
        let src = "use iroh::Endpoint;\nuse std::process::{self as proc};\n\
                   fn f(e: Endpoint) { let _ = iroh::Builder::new(); proc::exit(1) }\n\
                   fn g() -> Vec<Endpoint> { todo!(\"iroh::x\") }\nuse iroh::*;\n\
                   #[cfg(test)]\nmod tests { use iroh::Endpoint; }\n\
                   #[test]\nfn t() { iroh::go() }\n\
                   #[cfg(test)]\nimpl X { fn h() { iroh::go() } }\nfn irohx() { irohx::f() }\n";
        let told = "`iroh` is used outside `src/net/**`: one module talks to it";
        assert_eq!(
            hits(src, &[seam("iroh", &["src/net/**"])]),
            [1, 3, 4, 5].map(|line| format!("src/lib.rs:{line}: iroh: {told}"))
        );
        assert_eq!(
            hits(src, &[seam("std::process::exit", &[])]),
            [
                "src/lib.rs:3: std::process::exit: `std::process::exit` is used outside every \
              place, since `only` names none: one module talks to it"
            ]
        );
        assert_eq!(
            hits(src, &[seam("iroh", &["src/*.rs"])]),
            Vec::<String>::new()
        );
        assert_eq!(
            hits("fn broken( {", &[seam("iroh", &[])]),
            Vec::<String>::new()
        );
        let applies = applying(&[seam("iroh", &["src/net/**"])], "src/net/api.rs");
        assert!(applies.is_empty());
        let elsewhere = applying(&[seam("iroh", &["src/net/**"])], "src/lib.rs");
        assert_eq!(elsewhere, [seam("iroh", &["src/net/**"])]);
    }

    #[test]
    fn a_seam_names_one_path_or_one_crate_and_gives_a_reason() {
        let both = Seam {
            krate: Some("iroh".to_string()),
            ..seam("iroh", &[])
        };
        let crate_only = Seam {
            krate: Some("aws-sdk-s3".to_string()),
            path: None,
            ..seam("", &[])
        };
        let unexplained = Seam {
            why: " ".to_string(),
            ..seam("iroh", &[])
        };
        assert_eq!(
            split(&[]).unwrap_err(),
            "no seam is listed under `seams` in .chock/config.json"
        );
        assert_eq!(
            split(&[both]).unwrap_err(),
            "the seam `iroh` names a `crate` too; give each its own entry"
        );
        assert_eq!(
            split(&[seam("::", &[])]).unwrap_err(),
            "a `seams` entry names no `path` and no `crate`"
        );
        assert_eq!(
            split(&[unexplained]).unwrap_err(),
            "the seam `iroh` gives no `why`"
        );
        let listed = [seam("iroh", &[]), crate_only.clone()];
        let (paths, crates) = split(&listed).unwrap();
        assert_eq!(
            (paths, crates),
            (vec![&seam("iroh", &[])], vec![&crate_only])
        );
    }

    fn node(id: &str, deps: &[(&str, &str)]) -> String {
        let deps: Vec<String> = deps
            .iter()
            .map(|(pkg, kind)| {
                format!(r#"{{"pkg": "{pkg}", "dep_kinds": [{{"kind": {kind}, "target": null}}]}}"#)
            })
            .collect();
        format!(r#"{{"id": "{id}", "deps": [{}]}}"#, deps.join(","))
    }

    /// `app` reaches the cloud client through `store`, `cli` only to test, `tool` only to build.
    fn metadata() -> String {
        let package = |id: &str, dir: &str| {
            format!(r#"{{"id": "{id}", "name": "{id}", "manifest_path": "/w/{dir}Cargo.toml"}}"#)
        };
        let packages = [
            package("app", "app/"),
            package("store", "store/"),
            package("cli", ""),
            package("tool", "tool/"),
            package("s3", "reg/"),
        ];
        let nodes = [
            node("app", &[("store", "null")]),
            node("store", &[("s3", "null")]),
            node("cli", &[("s3", "\"dev\""), ("app", "null")]),
            node("tool", &[("s3", "\"build\"")]),
            node("s3", &[]),
        ];
        format!(
            r#"{{"workspace_root": "/w", "workspace_members": ["app", "store", "cli", "tool"],
                "packages": [{}], "resolve": {{"nodes": [{}]}}}}"#,
            packages.join(","),
            nodes.join(",")
        )
    }

    fn cloud(only: &[&str]) -> Seam {
        Seam {
            krate: Some("s3".to_string()),
            path: None,
            ..seam("", only)
        }
    }

    #[test]
    fn a_package_outside_a_crate_seam_that_holds_the_crate_is_found_at_its_manifest() {
        let found = depending(&metadata(), &[&cloud(&["store"])]).unwrap();
        assert_eq!(
            found.iter().map(Finding::render).collect::<Vec<_>>(),
            [
                "app/Cargo.toml: s3: `app` depends on `s3` through app → store → s3, outside \
                 `store`: one module talks to it",
                "Cargo.toml: s3: `cli` depends on `s3` through cli → app → store → s3, outside \
                 `store`: one module talks to it",
            ]
        );
        let every = depending(&metadata(), &[&cloud(&[])]).unwrap();
        let manifests: Vec<&str> = every.iter().map(|found| found.file.as_str()).collect();
        // `store` too, and never `cli` for its dev dependency or `tool` for its build dependency.
        assert_eq!(
            manifests,
            ["app/Cargo.toml", "store/Cargo.toml", "Cargo.toml"]
        );
        assert_eq!(
            depending(&metadata(), &[&cloud(&["stor"])]).unwrap_err(),
            "the seam `s3` allows `stor`, which is no package in this workspace"
        );
        assert_eq!(
            depending("{}", &[&cloud(&[])]).unwrap_err(),
            "cargo metadata printed no resolved dependency graph"
        );
        assert!(
            depending("not json", &[])
                .unwrap_err()
                .starts_with("cargo metadata printed a graph")
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_gate_reads_each_production_file_against_the_seams() {
        let mut held = crate::testdir::Held::tree(
            "seams-tree",
            &[
                ("Cargo.toml", "[package]\nname = \"x\"\n"),
                ("src/lib.rs", "mod net;\nuse iroh::Endpoint;\n"),
                ("src/net.rs", "use iroh::Endpoint;\n"),
                ("tests/end.rs", "use iroh::Endpoint;\n"),
            ],
        );
        held.seams = vec![seam("iroh", &["src/net.rs"])];
        let debt = inspect(&held).unwrap().debt;
        assert_eq!(
            debt.iter().map(Finding::render).collect::<Vec<_>>(),
            ["src/lib.rs:2: iroh: `iroh` is used outside `src/net.rs`: one module talks to it"]
        );
        held.seams = Vec::new();
        assert!(inspect(&held).is_err());
    }
}
