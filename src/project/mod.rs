//! The project tree: its root, found by `Cargo.toml` rather than by layout, and its files.

pub mod config;
pub mod document;
pub mod vcs;
pub mod workspace;

// Named, not `as _`: mutest's flattened harness drops anonymous trait imports.
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

pub const PIN_FILE: &str = "tool-versions.env";
pub const MANIFEST: &str = "Cargo.toml";

/// The nearest ancestor of `start` holding a `Cargo.toml`, `start` included.
#[must_use]
pub fn find(start: &Path) -> Option<PathBuf> {
    find_with(start, &|p| p.is_file())
}

/// The project around the current directory: the nearest `Cargo.toml` at or above it.
pub fn here() -> Result<PathBuf, String> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    find(&cwd).ok_or_else(|| {
        "no Cargo.toml here or in any parent — chock runs inside a Rust project".into()
    })
}

/// `find`, with the existence test injected so the walk can be tested without a filesystem.
#[must_use]
pub fn find_with(start: &Path, exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| exists(&dir.join(MANIFEST)))
        .map(Path::to_path_buf)
}

/// A path as the baseline spells it: relative to the root with forward slashes, so a key matches
/// on every machine.
#[must_use]
pub fn relative(root: &Path, path: &Path) -> String {
    let Ok(shown) = path.strip_prefix(root) else {
        // Not under the root: shown as itself, and refused by `unportable` where committed.
        return path.to_string_lossy().replace('\\', "/");
    };
    lexically(shown)
}

/// Resolves each `..` against the part before it, as a `#[path = "../x.rs"]` module needs.
fn lexically(shown: &Path) -> String {
    let mut parts: Vec<std::borrow::Cow<'_, str>> = Vec::new();
    for component in shown.components() {
        match component {
            std::path::Component::ParentDir if parts.last().is_some_and(|last| last != "..") => {
                parts.pop();
            }
            other => parts.push(other.as_os_str().to_string_lossy()),
        }
    }
    parts.join("/")
}

/// The file a root-relative path names, if it lies inside the tree; a `#[path]` can point anywhere.
#[must_use]
pub fn in_tree(root: &Path, shown: &str) -> Option<PathBuf> {
    let path = root.join(shown).canonicalize().ok()?;
    path.starts_with(root.canonicalize().ok()?).then_some(path)
}

/// The first key naming one machine's filesystem, which could never match in another checkout.
#[must_use]
pub fn unportable<'a>(mut keys: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    keys.find(|key| {
        let windows_drive = || {
            let mut chars = key.chars();
            chars.next().is_some_and(char::is_alphabetic)
                && chars.next() == Some(':')
                && matches!(chars.next(), Some('/' | '\\'))
        };
        key.starts_with('/') || key.starts_with('\\') || key.contains("../") || windows_drive()
    })
}

/// `cargo metadata --no-deps` for the workspace. It omits `[profile]` and `[lints]`.
pub fn metadata(root: &Path) -> Result<String, String> {
    let out = crate::exec::run(
        "cargo",
        &["metadata", "--no-deps", "--format-version", "1"],
        root,
    )
    .map_err(|e| e.to_string())?;
    if out.success() {
        return Ok(out.stdout);
    }
    Err(format!(
        "cargo metadata failed, so nothing was read: {}",
        cargo_failure(&out)
    ))
}

/// cargo's failure headline plus each `Caused by:`; the headline rarely names what is missing.
fn cargo_failure(out: &crate::exec::Output) -> String {
    let causes: Vec<&str> = out
        .stderr
        .split("Caused by:")
        .skip(1)
        .filter_map(|chunk| chunk.lines().map(str::trim).find(|line| !line.is_empty()))
        .collect();
    if causes.is_empty() {
        return out.why_it_failed().to_string();
    }
    format!("{}: {}", out.why_it_failed(), causes.join(": "))
}

/// The whole resolved dependency graph, with `--all-features` so feature-gated dependencies count.
pub fn resolved(root: &Path) -> Result<String, String> {
    let out = crate::exec::run(
        "cargo",
        &["metadata", "--format-version", "1", "--all-features"],
        root,
    )
    .map_err(|e| e.to_string())?;
    if out.success() {
        return Ok(out.stdout);
    }
    Err(format!(
        "cargo metadata could not resolve the dependency graph: {}",
        cargo_failure(&out)
    ))
}

/// A compile-fail fixture: a `.rs` file with the `.stderr` its harness compares against beside it.
#[must_use]
pub fn is_compile_fail_fixture(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "rs") && path.with_extension("stderr").is_file()
}

/// Directories no gate looks inside. Matched by name at any depth, so only names never used for
/// a project's own source.
pub const SKIPPED: [&str; 12] = [
    // What a version control system, a build tool or an editor keeps for itself.
    ".git",
    ".jj",
    ".hg",
    ".svn",
    ".outpost",
    "target",
    "node_modules",
    ".chock",
    ".claude",
    ".cursor",
    // Somebody else's code, kept here to build against.
    "vendor",
    "third_party",
];

/// Directories holding somebody else's source, kept to test a tool against.
pub const FIXTURE_DIRS: [&str; 2] = ["fixtures", "testdata"];

/// Files chock's own gates write. Walked, since `coverage` reads one, but left out of the digest.
pub const PRODUCED: [&str; 2] = ["lcov.info", "kani-list.json"];

/// The marker file of a build cache; cargo writes one in each target directory it creates.
const CACHE_MARKER: &str = "CACHEDIR.TAG";

/// Whether this directory says it holds generated files rather than anybody's source.
#[must_use]
pub fn is_build_cache(dir: &Path) -> bool {
    dir.join(CACHE_MARKER).is_file()
}

/// The directories a project's own `.gitignore` names, which `SKIPPED` cannot know.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Ignored {
    /// `/name`, which git anchors to the directory holding the `.gitignore`.
    anchored: std::collections::BTreeSet<String>,
    /// `name`, which git matches at any depth because the pattern holds no slash.
    named: std::collections::BTreeSet<String>,
}

impl Ignored {
    /// Whether the project said it ignores this directory. `shown` is the path relative to the root.
    #[must_use]
    pub fn holds(&self, shown: &str, name: &str) -> bool {
        self.named.contains(name) || self.anchored.contains(shown)
    }
}

/// The simple directory patterns in a `.gitignore`: one component, no glob, no negation. Any other
/// pattern is passed over, so its directory is still walked.
#[must_use]
pub fn ignored_dirs(text: &str) -> Ignored {
    let mut held = Ignored::default();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        if line.contains(['*', '?', '[', ']']) {
            continue;
        }
        let anchored = line.starts_with('/');
        let bare = line.trim_start_matches('/').trim_end_matches('/');
        if bare.is_empty() || bare.contains('/') {
            continue;
        }
        if anchored {
            held.anchored.insert(bare.to_string());
        } else {
            held.named.insert(bare.to_string());
        }
    }
    held
}

/// Whether a walk may descend into `path`: a directory, never a symlink, which could loop.
#[must_use]
pub fn descends_into(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|about| about.is_dir())
}

/// Every file under one root, walked once per run. Pruned only by what every walk prunes, so
/// filtering it answers every caller.
#[derive(Debug, Clone)]
pub struct Listing {
    root: PathBuf,
    files: Vec<PathBuf>,
    /// The digest of every file, taken at most once per run.
    digest: std::sync::OnceLock<Option<u64>>,
}

impl Listing {
    /// A digest of every file by path and content, so a rename counts. `None` if a file cannot be
    /// read, rather than a digest of part of the tree.
    #[must_use]
    pub fn digest(&self) -> Option<u64> {
        *self.digest.get_or_init(|| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for path in &self.files {
                let shown = relative(&self.root, path);
                if PRODUCED.contains(&shown.as_str()) {
                    continue;
                }
                shown.hash(&mut hasher);
                std::fs::read(path).ok()?.hash(&mut hasher);
            }
            Some(hasher.finish())
        })
    }
}

/// The run's listing, walked on first ask.
fn taken<'a>(
    held: &'a std::sync::OnceLock<Result<Listing, String>>,
    root: &Path,
) -> &'a Result<Listing, String> {
    held.get_or_init(|| {
        walk(root, &|name| !SKIPPED.contains(&name), &|_, _| true).map(|files| Listing {
            root: root.to_path_buf(),
            files,
            digest: std::sync::OnceLock::new(),
        })
    })
}

/// The run's whole listing, or `None` if the walk failed or covered another root.
pub fn listed_once<'a>(
    held: &'a std::sync::OnceLock<Result<Listing, String>>,
    root: &Path,
) -> Option<&'a Listing> {
    taken(held, root)
        .as_ref()
        .ok()
        .filter(|listing| listing.root == root)
}

/// `walk`, answered from the run's shared listing instead of walking again.
pub fn walked(
    held: &std::sync::OnceLock<Result<Listing, String>>,
    root: &Path,
    enter: &dyn Fn(&str) -> bool,
    keep: &dyn Fn(&str, &Path) -> bool,
) -> Result<Vec<PathBuf>, String> {
    let listing = taken(held, root).as_ref().map_err(String::clone)?;
    // `chock slop <path>` can ask about a root other than the listing's.
    if listing.root != root {
        return walk(root, enter, keep);
    }
    Ok(listing
        .files
        .iter()
        .filter(|path| {
            let shown = relative(root, path);
            let mut parts: Vec<&str> = shown.split('/').collect();
            let Some(name) = parts.pop() else {
                return false;
            };
            parts.iter().all(|dir| enter(dir)) && keep(name, path)
        })
        .cloned()
        .collect())
}

/// Every file under `root` that `keep` accepts, sorted; `enter` picks the directories descended.
/// A directory that cannot be listed is an error, not a skip.
pub fn walk(
    root: &Path,
    enter: &dyn Fn(&str) -> bool,
    keep: &dyn Fn(&str, &Path) -> bool,
) -> Result<Vec<PathBuf>, String> {
    let ignored = std::fs::read_to_string(root.join(".gitignore"))
        .map(|text| ignored_dirs(&text))
        .unwrap_or_default();
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let unreadable = |e: &std::io::Error| format!("cannot read {}: {e}", dir.display());
        for entry in std::fs::read_dir(&dir).map_err(|e| unreadable(&e))? {
            let path = entry.map_err(|e| unreadable(&e))?.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if descends_into(&path) {
                let shown = relative(root, &path);
                if enter(&name) && !is_build_cache(&path) && !ignored.holds(&shown, &name) {
                    stack.push(path);
                }
            } else if keep(&name, &path) {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// `walk`, with every file read and keyed relative to the root.
pub fn read_tree(
    root: &Path,
    enter: &dyn Fn(&str) -> bool,
    keep: &dyn Fn(&str, &Path) -> bool,
) -> Result<std::collections::BTreeMap<String, Option<String>>, String> {
    let mut tree = std::collections::BTreeMap::new();
    for path in walk(root, enter, keep)? {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => None,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        tree.insert(relative(root, &path), text);
    }
    Ok(tree)
}

/// Where a repository keeps sample projects; a crate under one is never built from here.
const FIXTURE_HOLDERS: [&str; 3] = ["tests", "examples", "benches"];

/// The part of `path` below `dir`, or `None` if it is not below it. `""` is the project root.
#[must_use]
pub fn under<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    if dir.is_empty() {
        return Some(path);
    }
    path.strip_prefix(dir)?.strip_prefix('/')
}

/// The crates among `dirs` that sit under another crate's `tests`, `examples` or `benches`.
#[must_use]
pub fn fixture_crates(dirs: &[String]) -> std::collections::BTreeSet<String> {
    dirs.iter()
        .filter(|dir| {
            dirs.iter()
                .filter(|outer| outer.len() < dir.len())
                .filter_map(|outer| under(dir, outer))
                .any(|rest| {
                    FIXTURE_HOLDERS
                        .iter()
                        .any(|holder| under(rest, holder).is_some())
                })
        })
        .cloned()
        .collect()
}

/// The directory a manifest sits in, keyed the way a baseline spells a path. `""` is the project
/// root itself, and anything that is not a manifest is `None`.
#[must_use]
pub fn crate_dir(path: &str) -> Option<String> {
    let dir = path.strip_suffix(MANIFEST)?;
    (dir.is_empty() || dir.ends_with('/')).then(|| dir.trim_end_matches('/').to_string())
}

/// Whether this path ships: no `not_shipped` pattern names it or a directory above it. A trailing
/// `/**` or `/*` is ignored; there is no other globbing.
#[must_use]
pub fn ships(path: &str, not_shipped: &[String]) -> bool {
    !not_shipped.iter().any(|named| {
        let named = named
            .trim_end_matches("/**")
            .trim_end_matches("/*")
            .trim_end_matches('/');
        !named.is_empty() && (path == named || path.starts_with(&format!("{named}/")))
    })
}

/// Whether cargo compiles this file as a crate's own code: under `src/`, or the build script.
#[must_use]
pub fn is_crate_code(path: &str, crates: &[String]) -> bool {
    crates
        .iter()
        .filter_map(|dir| under(path, dir))
        .any(|rest| rest.starts_with("src/") || rest == "build.rs")
}

/// Whether another source in the tree pulls this file in with `include!`. Asked only after a parse
/// fails, since it reads every Rust file.
#[must_use]
pub fn only_included(root: &Path, shown: &str) -> bool {
    let read: Vec<String> = walk(root, &|name| !SKIPPED.contains(&name), &|name, _| {
        name.ends_with(".rs")
    })
    .unwrap_or_default()
    .iter()
    .filter_map(|path| std::fs::read_to_string(path).ok())
    .collect();
    included_among(read.iter().map(String::as_str), shown)
}

/// Whether a file other than `path`, among these `(file, source)` pairs, includes it by name.
#[must_use]
pub fn included_elsewhere<'a>(
    sources: impl Iterator<Item = (&'a str, &'a str)>,
    path: &str,
) -> bool {
    included_among(
        sources.filter(|(at, _)| *at != path).map(|(_, src)| src),
        path,
    )
}

/// Whether any of these sources includes the named file.
#[must_use]
pub fn included_among<'a>(mut sources: impl Iterator<Item = &'a str>, path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    sources.any(|src| includes_by_name(src, name))
}

/// Whether an `include!` names this file, whatever path precedes it; a bare mention does not count.
#[must_use]
pub fn includes_by_name(src: &str, name: &str) -> bool {
    src.split("include!")
        .skip(1)
        .filter_map(|rest| rest.split(')').next())
        .any(|args| args.contains(name))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_included_elsewhere_only_when_another_file_includes_it() {
        let read = [
            ("src/a.rs", "include!(\"gen.rs\");"),
            ("src/gen.rs", "include!(\"gen.rs\");"),
        ];
        assert!(included_elsewhere(read.into_iter(), "src/gen.rs"));
        assert!(
            !included_elsewhere(read[1..].iter().copied(), "src/gen.rs"),
            "a file naming itself is no other file"
        );
    }

    fn only(manifest: &'static str) -> impl Fn(&Path) -> bool {
        move |p: &Path| p == Path::new(manifest)
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_a_gate_produced_leaves_the_digest_where_it_was() {
        let dir = crate::testdir::make("project-digest");
        std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        let first = listed_once(&std::sync::OnceLock::new(), &dir)
            .and_then(Listing::digest)
            .unwrap();
        for produced in PRODUCED {
            std::fs::write(dir.join(produced), "whatever a gate wrote").unwrap();
        }
        let after = listed_once(&std::sync::OnceLock::new(), &dir)
            .and_then(Listing::digest)
            .unwrap();
        assert_eq!(after, first, "a gate's own output moved the digest");

        // A source file still moves it.
        std::fs::write(dir.join("a.rs"), "fn a() { }\n").unwrap();
        let moved = listed_once(&std::sync::OnceLock::new(), &dir)
            .and_then(Listing::digest)
            .unwrap();
        assert_ne!(moved, first);
    }

    #[test]
    fn a_fragment_another_file_includes_is_not_a_module_that_failed_to_parse() {
        let holder = "include!(\"palette_literals.rs\");\n";
        assert!(includes_by_name(holder, "palette_literals.rs"));
        assert!(includes_by_name(
            "include!(concat!(\"gen/\", \"palette_literals.rs\"));",
            "palette_literals.rs"
        ));
        // A file nobody includes is a module, so its parse failure is the project's own.
        assert!(!includes_by_name(holder, "other.rs"));
        assert!(!includes_by_name("fn main() {}\n", "palette_literals.rs"));
    }

    #[test]
    fn a_mention_outside_an_include_excuses_nothing() {
        assert!(!includes_by_name(
            "// see palette_literals.rs\n",
            "palette_literals.rs"
        ));
        assert!(!includes_by_name(
            "let s = \"palette_literals.rs\";",
            "palette_literals.rs"
        ));
    }

    #[test]
    fn only_a_crates_own_source_and_build_script_are_code_cargo_compiles() {
        let crates = ["".to_string(), "vendored/grammar".to_string()];
        assert!(is_crate_code("src/lib.rs", &crates));
        assert!(is_crate_code("build.rs", &crates));
        assert!(is_crate_code("vendored/grammar/src/lib.rs", &crates));
        assert!(is_crate_code("vendored/grammar/build.rs", &crates));
        assert!(!is_crate_code(
            "vendored/grammar/examples/weird.rs",
            &crates
        ));
        assert!(!is_crate_code("templates/c_macros.rs", &crates));
        assert!(!is_crate_code("tests/cli.rs", &crates));
        // Nothing is a crate's code when no crate was found.
        assert!(!is_crate_code("src/lib.rs", &[]));
    }

    /// The `.rs` files the walk reaches under `dir`, shown from it.
    fn reached(dir: &Path) -> Vec<String> {
        let found = walk(dir, &|_| true, &|name, _| name.ends_with(".rs")).unwrap();
        found.iter().map(|p| relative(dir, p)).collect()
    }

    /// A name list cannot catch a renamed target directory; cargo's marker can.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_directory_that_marks_itself_a_build_cache_is_not_descended_however_it_is_named() {
        let marker = "Signature: 8a477f597d28d172";
        let tag = ("target-coverage/CACHEDIR.TAG", marker);
        let files = [("target-coverage/f.rs", ""), ("kept/f.rs", ""), tag];
        let dir = crate::testdir::tree("project-build-cache", &files);
        assert_eq!(reached(&dir), ["kept/f.rs"]);
    }

    /// A target directory made before cargo runs has no marker, but the `.gitignore` names it.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_directory_the_project_gitignores_is_not_walked() {
        // An anchored pattern names the root's own directory and nothing deeper with that name.
        let files = [
            (".gitignore", "/target-coverage/\nnode_cache\n"),
            ("target-coverage/f.rs", ""),
            ("node_cache/f.rs", ""),
            ("kept/f.rs", ""),
            ("kept/target-coverage/f.rs", ""),
        ];
        let dir = crate::testdir::tree("project-gitignored", &files);
        assert_eq!(reached(&dir), ["kept/f.rs", "kept/target-coverage/f.rs"]);
    }

    #[test]
    fn only_the_gitignore_patterns_git_and_chock_agree_on_are_read() {
        let held = ignored_dirs(
            "# a comment\n\
             \n\
             /anchored/\n\
             /also-anchored\n\
             anywhere/\n\
             !negated/\n\
             glob*/\n\
             deep/nested/\n\
             /\n",
        );
        assert!(held.holds("anchored", "anchored"));
        assert!(held.holds("also-anchored", "also-anchored"));
        // Anchored means the root's own, so the same name deeper is still walked.
        assert!(!held.holds("crates/a/anchored", "anchored"));
        // No slash in the pattern: matched at any depth, as git does.
        assert!(held.holds("crates/a/anywhere", "anywhere"));
        // A negation, a glob and a nested path are all left walked.
        assert!(!held.holds("negated", "negated"));
        assert!(!held.holds("globbed", "globbed"));
        assert!(!held.holds("deep/nested", "nested"));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_that_ignores_nothing_is_walked_whole() {
        assert_eq!(ignored_dirs(""), Ignored::default());
        let dir = crate::testdir::tree("project-no-gitignore", &[("build/f.rs", "")]);
        assert_eq!(reached(&dir), ["build/f.rs"]);
    }

    #[test]
    fn vendored_code_is_not_this_projects_source() {
        assert!(SKIPPED.contains(&"vendor"));
        assert!(SKIPPED.contains(&"third_party"));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_the_walk_passes_over_does_not_end_the_listing_it_sits_in() {
        let dir = crate::testdir::make("project-walk-skip");
        std::fs::create_dir_all(dir.join("a_skipped")).unwrap();
        std::fs::write(dir.join("a_skipped/hidden.rs"), "").unwrap();
        std::fs::write(dir.join("b_passed_over.md"), "").unwrap();
        std::fs::write(dir.join("c_kept.rs"), "").unwrap();
        let found = walk(&dir, &|name| name != "a_skipped", &|name, _| {
            name.ends_with(".rs")
        })
        .unwrap();
        let names: Vec<String> = found.iter().map(|p| relative(&dir, p)).collect();
        assert_eq!(names, vec!["c_kept.rs".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_link_pointing_at_its_own_parent_is_walked_once_and_not_forever() {
        let dir = crate::testdir::make("project-walk-symlink-loop");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/one.rs"), "").unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("src/back")).unwrap();
        let found = walk(&dir, &|_| true, &|name, _| name.ends_with(".rs")).unwrap();
        let names: Vec<String> = found.iter().map(|p| relative(&dir, p)).collect();
        assert_eq!(names, vec!["src/one.rs".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_file_is_still_a_file_the_walk_keeps() {
        let dir = crate::testdir::make("project-walk-symlink-file");
        std::fs::write(dir.join("real.rs"), "").unwrap();
        std::os::unix::fs::symlink(dir.join("real.rs"), dir.join("linked.rs")).unwrap();
        let found = walk(&dir, &|_| true, &|name, _| name.ends_with(".rs")).unwrap();
        let names: Vec<String> = found.iter().map(|p| relative(&dir, p)).collect();
        assert_eq!(names, vec!["linked.rs".to_string(), "real.rs".to_string()]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_directory_that_cannot_be_listed_stops_the_walk_rather_than_being_skipped() {
        let dir = crate::testdir::make("project-walk-unreadable");
        std::fs::write(dir.join("kept.rs"), "").unwrap();
        let err = walk(&dir.join("absent"), &|_| true, &|_, _| true).unwrap_err();
        assert!(err.starts_with("cannot read "), "{err}");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_rust_file_with_a_stderr_beside_it_is_a_compile_fail_fixture() {
        let dir = crate::testdir::make("project-fixture");
        std::fs::write(dir.join("bad.rs"), "fn broken( {").unwrap();
        std::fs::write(dir.join("bad.stderr"), "error: expected").unwrap();
        std::fs::write(dir.join("ordinary.rs"), "fn f() {}").unwrap();
        assert!(is_compile_fail_fixture(&dir.join("bad.rs")));
        assert!(!is_compile_fail_fixture(&dir.join("ordinary.rs")));
        assert!(!is_compile_fail_fixture(&dir.join("bad.stderr")));
    }

    #[test]
    fn a_manifest_in_the_starting_directory_is_the_root() {
        assert_eq!(
            find_with(Path::new("/w/proj"), &only("/w/proj/Cargo.toml")),
            Some(PathBuf::from("/w/proj"))
        );
    }

    #[test]
    fn the_nearest_manifest_wins_over_a_further_one() {
        let exists =
            |p: &Path| p == Path::new("/w/proj/Cargo.toml") || p == Path::new("/w/Cargo.toml");
        assert_eq!(
            find_with(Path::new("/w/proj/src/deep"), &exists),
            Some(PathBuf::from("/w/proj"))
        );
    }

    #[test]
    fn a_workspace_root_with_no_crates_directory_is_still_found() {
        assert_eq!(
            find_with(Path::new("/w/single/src"), &only("/w/single/Cargo.toml")),
            Some(PathBuf::from("/w/single"))
        );
    }

    #[test]
    fn a_tree_with_no_manifest_anywhere_yields_nothing() {
        assert_eq!(find_with(Path::new("/w/proj/src"), &|_| false), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn find_reads_a_real_tree_and_not_only_the_injected_one() {
        let dir = crate::testdir::make("project-find");
        let deep = dir.join("a/b");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(find(&deep), Some(dir.to_path_buf()));
        // The scratch directory is inside this crate, so the walk then reaches chock's manifest.
        std::fs::remove_file(dir.join("Cargo.toml")).unwrap();
        assert_eq!(find(&deep), Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))));
    }

    #[test]
    fn a_sibling_manifest_is_not_mistaken_for_an_ancestor() {
        assert_eq!(
            find_with(Path::new("/w/a/src"), &only("/w/b/Cargo.toml")),
            None
        );
    }

    fn dirs(of: &[&str]) -> Vec<String> {
        of.iter().map(|d| (*d).to_string()).collect()
    }

    #[test]
    fn a_crate_under_another_crates_tests_directory_is_that_crates_material() {
        let found = fixture_crates(&dirs(&["", "tests/fixtures/broken", "crates/core"]));
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            ["tests/fixtures/broken"]
        );
    }

    #[test]
    fn a_workspace_member_beside_the_others_is_not_material() {
        assert_eq!(
            fixture_crates(&dirs(&["", "crates/core", "crates/cli"])),
            std::collections::BTreeSet::new()
        );
    }

    #[test]
    fn a_sample_project_under_examples_or_benches_counts_the_same_as_one_under_tests() {
        let found = fixture_crates(&dirs(&["", "examples/demo", "benches/harness"]));
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            ["benches/harness", "examples/demo"]
        );
    }

    #[test]
    fn a_crate_named_tests_with_nothing_above_it_is_not_material() {
        assert_eq!(
            fixture_crates(&dirs(&["tests"])),
            std::collections::BTreeSet::new()
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_that_does_not_decode_is_held_as_a_path_with_nothing_to_say() {
        let dir = crate::testdir::make("project-undecodable");
        std::fs::write(dir.join("good.rs"), "fn f() {}\n").unwrap();
        std::fs::write(dir.join("bad.rs"), [0xff, 0xfe, 0x00]).unwrap();
        let tree = read_tree(&dir, &|_| true, &|name, _| name.ends_with(".rs")).unwrap();
        assert_eq!(tree.get("good.rs"), Some(&Some("fn f() {}\n".to_string())));
        assert_eq!(tree.get("bad.rs"), Some(&None));
    }

    #[test]
    fn the_bookkeeping_a_repository_keeps_is_never_walked() {
        for name in [".git", ".jj", ".hg", ".svn", ".outpost", ".chock"] {
            assert!(SKIPPED.contains(&name), "{name} is walked into");
        }
    }

    #[test]
    fn a_directory_a_project_might_legitimately_own_is_still_walked() {
        for name in ["src", "tests", "build", "dist", "examples", "crates"] {
            assert!(!SKIPPED.contains(&name), "{name} is skipped");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_workspace_metadata_names_this_package() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let json = metadata(&root).unwrap();
        assert!(json.contains("\"name\":\"chock\""));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_resolved_graph_carries_the_dependencies_the_workspace_one_leaves_out() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        assert!(
            !metadata(&root)
                .unwrap()
                .contains("\"name\":\"serde_derive\"")
        );
        assert!(
            resolved(&root)
                .unwrap()
                .contains("\"name\":\"serde_derive\"")
        );
    }

    /// cargo names the missing file below its first line, so the reason quotes past it.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_workspace_cargo_cannot_read_is_refused_with_the_file_it_could_not_find() {
        let dir = crate::testdir::make("project-missing-member");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"integrations/gone\"]\n",
        )
        .unwrap();
        for refused in [metadata(&dir).unwrap_err(), resolved(&dir).unwrap_err()] {
            assert!(
                refused
                    .replace('\\', "/")
                    .contains("integrations/gone/Cargo.toml"),
                "{refused}"
            );
        }
        let plain = crate::exec::Output {
            code: Some(101),
            stdout: String::new(),
            stderr: "error: the lock file needs to be updated\n".to_string(),
            truncated: false,
        };
        assert_eq!(
            cargo_failure(&plain),
            "error: the lock file needs to be updated"
        );
    }

    #[test]
    fn a_path_outside_the_root_is_rendered_as_itself_rather_than_doubled() {
        let root = Path::new("/home/me/project");
        assert_eq!(
            relative(root, Path::new("/home/me/project/src/lib.rs")),
            "src/lib.rs"
        );
        assert_eq!(
            relative(root, Path::new("/home/me/other/src/lib.rs")),
            "/home/me/other/src/lib.rs"
        );
        // A crate can reach `codegen.rs` above its own `src/` through `#[path = "../codegen.rs"]`.
        assert_eq!(
            relative(
                root,
                Path::new("/home/me/project/crates/a/src/../codegen.rs")
            ),
            "crates/a/codegen.rs"
        );
        assert_eq!(
            relative(root, Path::new("/home/me/project/src/../../x.rs")),
            "../x.rs"
        );
        assert_eq!(
            relative(root, Path::new("/home/me/project/./src/lib.rs")),
            "src/lib.rs"
        );
    }

    #[test]
    fn a_key_no_other_machine_could_match_is_named_rather_than_recorded() {
        fn bad(key: &str) -> Option<&str> {
            unportable([key].into_iter())
        }
        assert_eq!(
            bad("/home/me/project/src/lib.rs"),
            Some("/home/me/project/src/lib.rs")
        );
        assert_eq!(
            bad("C:/Users/me/src/lib.rs"),
            Some("C:/Users/me/src/lib.rs")
        );
        assert_eq!(bad("../outside/src/lib.rs"), Some("../outside/src/lib.rs"));
        // A relative path, and a key that is not a path at all, are both fine.
        assert_eq!(bad("src/lib.rs"), None);
        assert_eq!(bad("src/lib.rs#deep"), None);
        assert_eq!(bad("excess_lines_production"), None);
        // The first offender is the one named, out of many.
        assert_eq!(
            unportable(["src/a.rs", "/abs/b.rs", "/abs/c.rs"].into_iter()),
            Some("/abs/b.rs")
        );
    }
}
