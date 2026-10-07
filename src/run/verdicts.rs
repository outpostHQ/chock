//! Kept verdicts a later run may recall instead of running the gate, keyed on everything it reads.

use std::collections::BTreeMap;
// Named, not `as _`: mutest's flattened harness drops anonymous trait imports.
use std::hash::{Hash, Hasher};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::run::report::{GateReport, Verdict};

/// The kept verdicts. Per clone and never committed: the keys hold this machine's tool versions.
pub const FILE: &str = ".chock/verdicts.json";

/// What a gate's verdict depends on beyond chock's own code; a gate without one is never recalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reads {
    /// Every file the run's walk can see, a superset of any one gate's inputs.
    pub tree: bool,
    /// The commands whose version changes the answer. `cargo` covers the toolchain beneath it.
    pub tools: &'static [&'static str],
    /// Whether the answer comes from the project's own runner, whose tools the project must name.
    pub runner: bool,
    /// Whether the answer comes from the coverage command. Only chock's own names its tools.
    pub coverage: bool,
    /// The version of the gate's reader. Raise it when the reading changes, since chock's own
    /// version may not.
    pub reader: &'static str,
    /// Whether the answer also depends on which files the change touched.
    pub change_set: bool,
}

impl Reads {
    /// The tree and these tools.
    #[must_use]
    pub const fn tree_and(tools: &'static [&'static str]) -> Self {
        Self {
            tree: true,
            tools,
            runner: false,
            coverage: false,
            reader: "",
            change_set: false,
        }
    }

    /// The tree, these tools, and whatever the project says its own runner calls.
    #[must_use]
    pub const fn tree_runner_and(tools: &'static [&'static str]) -> Self {
        Self {
            runner: true,
            ..Self::tree_and(tools)
        }
    }

    /// The same inputs, and whatever the coverage command calls.
    #[must_use]
    pub const fn and_coverage(self) -> Self {
        Self {
            coverage: true,
            ..self
        }
    }

    /// The same inputs, read by this version of the gate's reader.
    #[must_use]
    pub const fn versioned(self, reader: &'static str) -> Self {
        Self { reader, ..self }
    }

    /// The same inputs, and the files the change touched.
    #[must_use]
    pub const fn and_change_set(self) -> Self {
        Self {
            change_set: true,
            ..self
        }
    }
}

/// What a gate reads when its answer comes from one `outpost` call over the tree.
pub const OUTPOST: Reads = Reads::tree_and(&["outpost"]).versioned("sites-v1");

/// One kept verdict, with the key it was kept under.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Kept {
    key: String,
    report: GateReport,
}

/// Each gate's kept verdicts, newest first.
type Record = BTreeMap<String, Vec<Kept>>;

/// How many verdicts a gate keeps, so a tree seen before, such as another branch, still answers.
const KEPT_PER_GATE: usize = 4;

/// What one run holds that a key reads beyond the gate's own declaration.
pub struct Held<'a> {
    pub root: &'a Path,
    pub listed: &'a std::sync::OnceLock<Result<crate::project::Listing, String>>,
    pub version_of: &'a dyn Fn(&str) -> Option<String>,
    /// What the project's runner calls, where it named them.
    pub runner_tools: &'a [String],
    /// What the coverage command calls: known for chock's own, empty for a project's.
    pub coverage_tools: &'a [String],
    /// A digest of the gate's own record. No gate reads another's.
    pub record: u64,
}

/// A digest of everything a gate's verdict depends on. `None` if any input cannot be read, since a
/// partial key could match a tree it should not.
#[must_use]
pub fn key(reads: Reads, gate: &str, held: &Held) -> Option<String> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // A new chock may judge the same tree differently.
    env!("CARGO_PKG_VERSION").hash(&mut hasher);
    gate.hash(&mut hasher);
    held.record.hash(&mut hasher);
    for tool in reads.tools {
        tool.hash(&mut hasher);
        (held.version_of)(tool)?.hash(&mut hasher);
    }
    // A command the project chose does not show its tools in its bytes, so none named means no key.
    let chosen = [
        reads.runner.then_some(held.runner_tools),
        reads.coverage.then_some(held.coverage_tools),
    ];
    for tools in chosen {
        for tool in named(tools)? {
            tool.hash(&mut hasher);
            (held.version_of)(tool)?.hash(&mut hasher);
        }
    }
    settings(held.root).hash(&mut hasher);
    // The walk prunes `.outpost/`, so its config is named here.
    std::fs::read(held.root.join(".outpost/config.toml"))
        .ok()
        .hash(&mut hasher);
    if reads.tree {
        tree(held.root, held.listed)?.hash(&mut hasher);
    }
    Some(format!("{:016x}", hasher.finish()))
}

/// The tools of a command the gate runs: none where it runs no such command, and `None` where it
/// does and the tools are not named.
fn named(tools: Option<&[String]>) -> Option<&[String]> {
    match tools {
        None => Some(&[]),
        Some([]) => None,
        Some(tools) => Some(tools),
    }
}

/// Config keys that choose which gates run and when. No gate reads them, so `chock enable` and
/// `chock stage` keep every verdict.
const CHOOSES: [&str; 5] = ["$schema", "enabled", "left_off", "local_only", "stage"];

/// The config as a gate reads it: every key but those in `CHOOSES`. A file that is not a JSON
/// object is taken whole, since over-hashing costs a recall and under-hashing a wrong verdict.
fn settings(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(crate::project::config::FILE)).ok()?;
    Some(judged_by(&text).unwrap_or(text))
}

fn judged_by(config: &str) -> Option<String> {
    let mut held: serde_json::Map<String, serde_json::Value> = serde_json::from_str(config).ok()?;
    held.retain(|name, _| !CHOOSES.contains(&name.as_str()));
    Some(serde_json::Value::Object(held).to_string())
}

/// A digest of every file the run's walk sees, by path and content, or `None` if one is unreadable.
fn tree(
    root: &Path,
    listed: &std::sync::OnceLock<Result<crate::project::Listing, String>>,
) -> Option<u64> {
    crate::project::listed_once(listed, root)?.digest()
}

/// The verdict kept for this gate under this exact key, if there is one.
#[must_use]
pub fn recall(root: &Path, gate: &str, key: &str) -> Option<GateReport> {
    let kept = read(root)?.remove(gate)?;
    kept.into_iter()
        .find(|kept| kept.key == key)
        .map(|kept| kept.report)
}

/// Forgets every kept verdict, and says how many there were. Nothing kept is not an error.
pub fn clear(root: &Path) -> std::io::Result<usize> {
    let kept = read(root).map_or(0, |held| held.values().map(Vec::len).sum());
    match std::fs::remove_file(root.join(FILE)) {
        Ok(()) => Ok(kept),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

/// Whether something was measured and no record waits to be written.
fn keepable(report: &GateReport) -> bool {
    report.verdict != Verdict::CannotRun && report.tightened.is_none()
}

static KEEPING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Keeps a verdict for later runs. A `cannot_run` is never kept, since nothing was measured, nor
/// a verdict that leaves a record to write: recalled, it would write nothing.
pub fn keep(root: &Path, gate: &str, key: &str, report: &GateReport) {
    if !keepable(report) {
        return;
    }
    // Gates finish in parallel, and concurrent read-modify-writes would drop each other's verdicts.
    let _one_at_a_time = KEEPING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut held = read(root).unwrap_or_default();
    let kept = held.entry(gate.to_string()).or_default();
    newest_first(
        kept,
        Kept {
            key: key.to_string(),
            report: report.clone(),
        },
    );
    // Best effort: an unwritten record only means the next run takes the verdict again.
    let _ = write(root, &held);
}

/// Puts a verdict in front of the gate's others, in place of one under the same key, and drops the
/// oldest past `KEPT_PER_GATE`.
fn newest_first(kept: &mut Vec<Kept>, new: Kept) {
    kept.retain(|old| old.key != new.key);
    kept.insert(0, new);
    kept.truncate(KEPT_PER_GATE);
}

fn read(root: &Path) -> Option<Record> {
    let text = std::fs::read_to_string(root.join(FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes a staged file and renames it over the record, so a crash never leaves it truncated.
fn write(root: &Path, held: &Record) -> Option<()> {
    let path = root.join(FILE);
    let dir = path.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    let text = serde_json::to_string_pretty(held).ok()?;
    let staged = dir.join("verdicts.json.new");
    std::fs::write(&staged, text).ok()?;
    std::fs::rename(&staged, &path).ok()
}

/// A tool's identity for the key: its version output, binary stamp and cargo build environment.
/// The output is used whole, not parsed, so a new format changes the key instead of breaking it.
#[must_use]
pub fn installed(root: &Path, tool: &str) -> Option<String> {
    let (program, args) = asked(tool);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let said = version_in(&crate::exec::run(&program, &argv, root).ok()?)?;
    let home = std::env::var_os("CARGO_HOME").map(std::path::PathBuf::from);
    let flags: Vec<Option<std::ffi::OsString>> = BUILD_ENV.iter().map(std::env::var_os).collect();
    Some(format!(
        "{} {:016x}",
        built(&said, stamped(tool)),
        build_environment(&flags, &cargo_configs(root, home))
    ))
}

/// Environment variables that change how cargo builds without any file in the tree changing.
const BUILD_ENV: [&str; 6] = [
    "RUSTFLAGS",
    "RUSTDOCFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_BUILD_TARGET",
    "RUSTUP_TOOLCHAIN",
];

fn build_environment(flags: &[Option<std::ffi::OsString>], configs: &[std::path::PathBuf]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    flags.hash(&mut hasher);
    for config in configs {
        std::fs::read(config).ok().hash(&mut hasher);
    }
    hasher.finish()
}

/// Every config file cargo reads for a build here: `.cargo/` in the project and each directory
/// above it, then cargo's home.
fn cargo_configs(root: &Path, home: Option<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
    root.ancestors()
        .map(|dir| dir.join(".cargo"))
        .chain(home)
        .flat_map(|dir| [dir.join("config.toml"), dir.join("config")])
        .collect()
}

/// Size and mtime of the tool's binary as the process's `PATH` resolves it.
fn stamped(tool: &str) -> Option<(u64, std::time::SystemTime)> {
    binary_of(&std::env::var_os("PATH").unwrap_or_default(), tool)
}

/// The version plus the binary's size and mtime, since one version string can name many builds.
/// Not a hash: hashing every tool's binary on every run costs more than a rerun.
#[must_use]
fn built(said: &str, stamp: Option<(u64, std::time::SystemTime)>) -> String {
    match stamp {
        Some((bytes, at)) => format!("{said}\n{bytes} {at:?}"),
        None => said.to_string(),
    }
}

/// Size and mtime of `program` as `listed` resolves it, or `None` where there is no file to stat.
fn binary_of(listed: &std::ffi::OsStr, program: &str) -> Option<(u64, std::time::SystemTime)> {
    let held = std::fs::metadata(which(listed, program)?).ok()?;
    Some((held.len(), held.modified().ok()?))
}

/// The first file named `program` on `listed`, or `program` itself where it is already a path.
fn which(listed: &std::ffi::OsStr, program: &str) -> Option<std::path::PathBuf> {
    if program.contains('/') {
        return Some(std::path::PathBuf::from(program));
    }
    on_path(program, listed)
        .into_iter()
        .find(|path| path.is_file())
}

/// Every place `PATH` could hold `program`, in the order a shell looks.
pub(crate) fn on_path(program: &str, path: &std::ffi::OsStr) -> Vec<std::path::PathBuf> {
    std::env::split_paths(path)
        .flat_map(|dir| {
            EXECUTABLE_SUFFIXES
                .iter()
                .map(move |suffix| dir.join(format!("{program}{suffix}")))
        })
        .collect()
}

/// Windows starts `chock.exe` when asked for `chock`; elsewhere the name is the whole file name.
#[cfg(windows)]
const EXECUTABLE_SUFFIXES: &[&str] = &["", ".exe"];
#[cfg(not(windows))]
const EXECUTABLE_SUFFIXES: &[&str] = &[""];

/// The command that asks a tool its version. A cargo subcommand is asked through cargo, since
/// `cargo-udeps --version` prints nothing; `cargo +nightly miri` names the toolchain that holds it.
#[must_use]
fn asked(tool: &str) -> (String, Vec<String>) {
    let through_cargo = tool
        .strip_prefix("cargo-")
        .or_else(|| tool.strip_prefix("cargo "));
    match through_cargo {
        Some(words) => (
            "cargo".to_string(),
            words
                .split(' ')
                .chain(["--version"])
                .map(str::to_string)
                .collect(),
        ),
        None => (tool.to_string(), vec!["--version".to_string()]),
    }
}

/// The trimmed stdout of a successful version query; `None` for a failure or an empty answer.
#[must_use]
fn version_in(out: &crate::exec::Output) -> Option<String> {
    out.success()
        .then(|| out.stdout.trim().to_string())
        .filter(|said| !said.is_empty())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn tree_of(files: &[(&str, &str)]) -> crate::testdir::Scratch {
        let dir = crate::testdir::make("verdicts");
        for (path, text) in files {
            let at = dir.join(path);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, text).unwrap();
        }
        dir
    }

    #[test]
    fn a_verdict_is_kept_only_where_it_measured_and_left_no_record_to_write() {
        let mut passed = GateReport::new("crap", Verdict::Pass, "chock run crap");
        assert!(keepable(&passed));
        passed.tightened = Some(crate::run::baseline::Series::new());
        assert!(!keepable(&passed));
        let blocked = GateReport::new("crap", Verdict::CannotRun, "chock run crap");
        assert!(!keepable(&blocked));
    }

    type Listed = std::sync::OnceLock<Result<crate::project::Listing, String>>;

    fn one_version(tool: &str) -> Option<String> {
        Some(format!("{tool} 1.0.0"))
    }

    /// What a run holds over `root`: every tool at one version, no tool the project named, no
    /// record.
    fn held<'a>(root: &'a Path, listed: &'a Listed) -> Held<'a> {
        Held {
            root,
            listed,
            version_of: &one_version,
            runner_tools: &[],
            coverage_tools: &[],
            record: 0,
        }
    }

    fn keyed(root: &Path, tools: &'static [&'static str]) -> Option<String> {
        key(Reads::tree_and(tools), "probe", &held(root, &Listed::new()))
    }

    fn passed() -> GateReport {
        GateReport::new("probe", Verdict::Pass, "chock run probe")
    }

    /// A verdict that measured nothing, or that leaves a record to write, is no answer to keep.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn only_a_measured_verdict_with_nothing_left_to_write_is_kept() {
        let dir = tree_of(&[]);
        let stopped = GateReport::new("probe", Verdict::CannotRun, "chock run probe");
        let writes = GateReport {
            tightened: Some(crate::run::baseline::Series::new()),
            ..passed()
        };
        for report in [stopped, writes] {
            keep(&dir, "probe", "k", &report);
            assert_eq!(recall(&dir, "probe", "k"), None);
        }
        keep(&dir, "probe", "k", &passed());
        assert_eq!(recall(&dir, "probe", "k"), Some(passed()));
    }

    #[test]
    fn a_versioned_read_names_its_reader_and_keeps_its_inputs() {
        for plain in [
            Reads::tree_and(&["cargo"]),
            Reads::tree_runner_and(&["cargo"]),
        ] {
            assert_eq!(plain.reader, "");
            let versioned = plain.versioned("features-v1");
            assert_eq!(versioned.reader, "features-v1");
            assert_eq!(
                Reads {
                    reader: "",
                    ..versioned
                },
                plain
            );
            let narrowed = plain.and_change_set();
            assert!(narrowed.change_set && !plain.change_set);
            assert_eq!(
                Reads {
                    change_set: false,
                    ..narrowed
                },
                plain
            );
        }
    }

    #[test]
    fn two_builds_a_tool_gives_one_version_string_for_do_not_key_alike() {
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        let later = at + std::time::Duration::from_secs(60);
        let said = "outpost 0.1.0";
        assert_ne!(built(said, Some((90, at))), built(said, Some((90, later))));
        assert_ne!(built(said, Some((90, at))), built(said, Some((91, at))));
        assert_eq!(built(said, Some((90, at))), built(said, Some((90, at))));
    }

    /// Dropping the key instead would stop every recall for such a tool.
    #[test]
    fn a_tool_with_no_binary_to_stat_is_still_keyed_by_what_it_says() {
        assert_eq!(built("typos 1.16.0", None), "typos 1.16.0");
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_that_reads_a_tool_keys_on_what_that_tool_answered() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let bare = keyed(&dir, &[]).unwrap();
        let with_tool = keyed(&dir, &["clippy-driver"]).unwrap();
        assert_ne!(with_tool, bare);
        assert_ne!(keyed(&dir, &["kani"]).unwrap(), with_tool);
    }

    /// The way a shell resolves it, so the binary stamped is the one that runs.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tool_is_found_at_the_first_path_entry_that_holds_it() {
        let dir = tree_of(&[("early/outpost", ""), ("late/outpost", "")]);
        let listed = std::env::join_paths([dir.join("late"), dir.join("early")]).unwrap();
        assert_eq!(
            which(&listed, "outpost"),
            Some(dir.join("late").join("outpost"))
        );
        assert_eq!(which(&listed, "nothing-installs-this"), None);
    }

    #[test]
    fn a_tool_named_by_its_path_is_not_looked_for_on_the_path() {
        assert_eq!(
            which(std::ffi::OsStr::new(""), "/opt/outpost/bin/outpost"),
            Some(std::path::PathBuf::from("/opt/outpost/bin/outpost"))
        );
    }

    /// A split on `:` would cut a Windows drive letter off its path.
    #[test]
    fn path_entries_are_split_by_the_platforms_separator_and_kept_in_order() {
        let listed = std::env::join_paths(["first", "second"]).unwrap();
        let found = on_path("tool", &listed);
        assert_eq!(found.first(), Some(&Path::new("first").join("tool")));
        assert!(
            found.contains(&Path::new("second").join("tool")),
            "{found:?}"
        );
        assert!(
            !found
                .iter()
                .any(|candidate| candidate.starts_with("first:second")),
            "{found:?}"
        );
    }

    /// Uses the real `PATH`, which holds `cargo` whenever this suite runs.
    #[test]
    #[cfg_attr(
        all(miri, windows),
        ignore = "Miri finds no `PATH`: Windows spells it `Path`"
    )]
    fn the_tool_a_run_would_start_is_the_one_stamped() {
        assert!(stamped("cargo").is_some());
        assert_eq!(stamped("nothing-installs-this-either"), None);
    }

    #[test]
    fn cargo_config_is_read_from_the_project_up_and_then_from_cargo_home() {
        let found = cargo_configs(Path::new("/w/app"), Some(std::path::PathBuf::from("/h")));
        let shown: Vec<String> = found
            .iter()
            .map(|p| p.display().to_string().replace('\\', "/"))
            .collect();
        assert_eq!(
            shown,
            [
                "/w/app/.cargo/config.toml",
                "/w/app/.cargo/config",
                "/w/.cargo/config.toml",
                "/w/.cargo/config",
                "/.cargo/config.toml",
                "/.cargo/config",
                "/h/config.toml",
                "/h/config",
            ]
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_changed_cargo_config_or_flag_changes_the_build_environment() {
        let dir = crate::testdir::make("verdicts-build-environment");
        let config = dir.join(".cargo/config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let configs = cargo_configs(&dir, None);
        std::fs::write(&config, "[build]\nrustflags = []\n").unwrap();
        let before = build_environment(&[None], &configs);
        assert_eq!(
            build_environment(&[None], &configs),
            before,
            "nothing moved"
        );
        std::fs::write(&config, "[build]\nrustflags = [\"-Dwarnings\"]\n").unwrap();
        let edited = build_environment(&[None], &configs);
        assert_ne!(edited, before, "the config changed");
        assert_ne!(
            build_environment(&[Some("-Dwarnings".into())], &configs),
            edited
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_stamp_is_the_binarys_own_size_and_a_missing_tool_has_none() {
        let dir = tree_of(&[("bin/outpost", "0123456789")]);
        let listed = dir.join("bin").into_os_string();
        assert_eq!(
            binary_of(&listed, "outpost").map(|(bytes, _)| bytes),
            Some(10)
        );
        assert_eq!(binary_of(&listed, "nothing-installs-this"), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_that_has_not_moved_keys_the_same_and_one_that_has_does_not() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let first = keyed(&dir, &[]).unwrap();
        assert_eq!(keyed(&dir, &[]).unwrap(), first);
        std::fs::write(dir.join("src/a.rs"), "fn a() { }\n").unwrap();
        assert_ne!(keyed(&dir, &[]).unwrap(), first);
    }

    /// The digest is over the walk, not the index, so an untracked file counts.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_nothing_tracks_still_changes_the_key() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let first = keyed(&dir, &[]).unwrap();
        std::fs::write(dir.join("src/b.rs"), "fn b() {}\n").unwrap();
        assert_ne!(keyed(&dir, &[]).unwrap(), first);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn moving_a_file_without_changing_a_byte_changes_the_key() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let first = keyed(&dir, &[]).unwrap();
        std::fs::rename(dir.join("src/a.rs"), dir.join("src/b.rs")).unwrap();
        assert_ne!(keyed(&dir, &[]).unwrap(), first);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tool_that_reports_a_new_version_changes_the_key() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let reads = Reads::tree_and(&["cargo"]);
        let listed = Listed::new();
        let at = |version: &'static str| {
            let said = move |_: &str| Some(version.to_string());
            let run = Held {
                version_of: &said,
                ..held(&dir, &listed)
            };
            key(reads, "probe", &run)
        };
        let (one, two) = (at("cargo 1.90.0"), at("cargo 1.91.0"));
        assert!(one.is_some() && two.is_some());
        assert_ne!(one, two);
    }

    #[test]
    fn a_cargo_subcommand_is_asked_the_way_cargo_asks_it() {
        assert_eq!(
            asked("cargo-udeps"),
            (
                "cargo".to_string(),
                vec!["udeps".to_string(), "--version".to_string()]
            )
        );
        assert_eq!(
            asked("cargo +nightly miri"),
            (
                "cargo".to_string(),
                vec![
                    "+nightly".to_string(),
                    "miri".to_string(),
                    "--version".to_string()
                ]
            )
        );
        assert_eq!(
            asked("kani"),
            ("kani".to_string(), vec!["--version".to_string()])
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_reading_the_runner_has_no_key_until_its_tools_are_named() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let reads = Reads::tree_runner_and(&["cargo"]);
        let listed = Listed::new();
        let keyed = |tools: &[String]| {
            let run = Held {
                runner_tools: tools,
                ..held(&dir, &listed)
            };
            key(reads, "probe", &run)
        };
        assert_eq!(keyed(&[]), None, "nothing named, so nothing to key on");
        let named = keyed(&["kani".to_string()]).unwrap();
        assert_ne!(keyed(&["clippy-driver".to_string()]).unwrap(), named);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_reading_the_coverage_command_has_no_key_until_its_tools_are_known() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let reads = Reads::tree_and(&["cargo"]).and_coverage();
        let listed = Listed::new();
        let keyed = |tools: &[String]| {
            let run = Held {
                coverage_tools: tools,
                ..held(&dir, &listed)
            };
            key(reads, "probe", &run)
        };
        assert_eq!(keyed(&[]), None, "a project's own command names no tool");
        let named = keyed(&["cargo-llvm-cov".to_string()]).unwrap();
        assert_ne!(keyed(&["cargo-tarpaulin".to_string()]).unwrap(), named);
        // A gate that runs no coverage command keys without them, and not as one that does.
        let plain = key(Reads::tree_and(&["cargo"]), "probe", &held(&dir, &listed));
        assert!(plain.is_some_and(|plain| plain != named));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_named_tool_that_says_nothing_leaves_no_key() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let listed = Listed::new();
        let only_cargo = |tool: &str| (tool == "cargo").then(|| "cargo 1.90.0".to_string());
        let tools = ["kani".to_string()];
        let run = Held {
            version_of: &only_cargo,
            runner_tools: &tools,
            ..held(&dir, &listed)
        };
        assert_eq!(key(Reads::tree_runner_and(&["cargo"]), "probe", &run), None);
        assert!(key(Reads::tree_and(&["cargo"]), "probe", &run).is_some());
    }

    fn said(code: i32, stdout: &str) -> crate::exec::Output {
        crate::exec::Output {
            code: Some(code),
            stdout: stdout.to_string(),
            stderr: String::new(),
            truncated: false,
        }
    }

    /// An empty answer would key every version of the tool alike.
    #[test]
    fn a_tool_that_answers_with_silence_is_read_as_no_version_at_all() {
        assert_eq!(version_in(&said(0, "")), None);
        assert_eq!(version_in(&said(0, "  \n ")), None);
        assert_eq!(
            version_in(&said(0, "kani 0.68.0\n")),
            Some("kani 0.68.0".to_string())
        );
        assert_eq!(version_in(&said(1, "kani 0.68.0")), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tool_that_says_nothing_leaves_no_key_at_all() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let listed = Listed::new();
        let silent = Held {
            version_of: &|_| None,
            ..held(&dir, &listed)
        };
        assert_eq!(key(Reads::tree_and(&["cargo"]), "probe", &silent), None);
    }

    /// A ratchet's verdict is a comparison against its record.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_new_record_and_another_gate_each_change_the_key() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let listed = Listed::new();
        let reads = Reads::tree_and(&[]);
        let first = key(reads, "probe", &held(&dir, &listed)).unwrap();
        let recorded = Held {
            record: 7,
            ..held(&dir, &listed)
        };
        assert_ne!(key(reads, "probe", &recorded).unwrap(), first);
        assert_ne!(key(reads, "other", &held(&dir, &listed)).unwrap(), first);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_config_key_a_gate_reads_changes_the_key_and_one_that_chooses_gates_does_not() {
        let dir = tree_of(&[]);
        // Without the tree, whose walk may see the config file too.
        let apart = Reads {
            tree: false,
            ..Reads::tree_and(&[])
        };
        let with = |config: &str| {
            let at = dir.join(crate::project::config::FILE);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, config).unwrap();
            key(apart, "probe", &held(&dir, &Listed::new())).unwrap()
        };
        let first = with(r#"{"enabled":["slop"],"strict":["slop"]}"#);
        let chosen = r#"{"enabled":["lint"],"stage":{"lint":"push"},"strict":["slop"]}"#;
        assert_eq!(with(chosen), first);
        assert_ne!(with(r#"{"enabled":["slop"],"strict":[]}"#), first);
        // Not an object, so it is taken whole and any byte moves the key.
        assert_ne!(with("[1]"), with("[1] "));
    }

    #[test]
    fn only_the_keys_a_gate_reads_are_left_of_a_config() {
        let config = r#"{"$schema":"s","enabled":["a"],"left_off":{},"strict":["b"]}"#;
        assert_eq!(judged_by(config).as_deref(), Some(r#"{"strict":["b"]}"#));
        assert_eq!(judged_by("not json"), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_verdict_is_recalled_only_under_the_key_it_was_kept_with() {
        let dir = tree_of(&[]);
        keep(&dir, "probe", "abc", &passed());
        assert_eq!(
            recall(&dir, "probe", "abc").map(|r| r.verdict),
            Some(Verdict::Pass)
        );
        assert_eq!(recall(&dir, "probe", "def"), None);
        assert_eq!(recall(&dir, "other", "abc"), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_that_could_not_run_is_never_kept() {
        let dir = tree_of(&[]);
        let refused = GateReport::cannot_run("probe", "chock run probe", "no tool");
        keep(&dir, "probe", "abc", &refused);
        assert_eq!(recall(&dir, "probe", "abc"), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_verdict_that_leaves_a_record_to_write_is_never_kept() {
        let dir = tree_of(&[]);
        let mut wrote = passed();
        wrote.tightened = Some(crate::run::baseline::Series::new());
        keep(&dir, "probe", "abc", &wrote);
        assert_eq!(recall(&dir, "probe", "abc"), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_keeps_its_newest_verdicts_and_drops_the_oldest_past_the_limit() {
        let dir = tree_of(&[]);
        for key in ["k1", "k2", "k3", "k4", "k5"] {
            keep(&dir, "probe", key, &passed());
        }
        assert_eq!(recall(&dir, "probe", "k1"), None, "five were kept");
        assert!(recall(&dir, "probe", "k2").is_some() && recall(&dir, "probe", "k5").is_some());
        // Kept again under a key the gate holds, a verdict takes that key's place.
        keep(&dir, "probe", "k2", &passed());
        assert!(recall(&dir, "probe", "k3").is_some(), "a rewrite evicted");
        keep(&dir, "probe", "k6", &passed());
        assert_eq!(recall(&dir, "probe", "k3"), None, "the oldest stayed");
        assert!(recall(&dir, "probe", "k2").is_some());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn clearing_removes_every_kept_verdict_and_says_how_many() {
        let dir = tree_of(&[]);
        assert_eq!(clear(&dir).unwrap(), 0, "nothing kept is not an error");
        keep(&dir, "probe", "k1", &passed());
        keep(&dir, "probe", "k2", &passed());
        keep(&dir, "other", "k1", &passed());
        assert_eq!(clear(&dir).unwrap(), 3);
        assert_eq!(recall(&dir, "probe", "k1"), None);
        assert!(!dir.join(FILE).exists());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a directory")]
    fn a_record_that_cannot_be_removed_is_an_error() {
        let dir = tree_of(&[]);
        std::fs::create_dir_all(dir.join(FILE)).unwrap();
        assert!(clear(&dir).is_err());
    }

    /// Its findings hold until the tree moves, so re-running only fails the same way.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tripped_verdict_is_kept_like_a_passing_one() {
        let dir = tree_of(&[]);
        let mut tripped = GateReport::new("probe", Verdict::Tripped, "chock run probe");
        tripped.findings = vec![crate::run::report::Finding::at("src/a.rs", "too long")];
        keep(&dir, "probe", "abc", &tripped);
        let back = recall(&dir, "probe", "abc").unwrap();
        assert_eq!(back.verdict, Verdict::Tripped);
        assert_eq!(
            back.findings
                .iter()
                .map(crate::run::report::Finding::render)
                .collect::<Vec<_>>(),
            vec!["src/a.rs: too long"]
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_record_that_cannot_be_read_recalls_nothing_and_does_not_stop_the_run() {
        let dir = tree_of(&[]);
        let at = dir.join(FILE);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(&at, "{ not json").unwrap();
        assert_eq!(recall(&dir, "probe", "abc"), None);
        keep(&dir, "probe", "abc", &passed());
        assert_eq!(
            recall(&dir, "probe", "abc").map(|r| r.verdict),
            Some(Verdict::Pass)
        );
    }
}
