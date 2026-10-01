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
            reader: "",
            change_set: false,
        }
    }

    /// The tree, these tools, and whatever the project says its own runner calls.
    #[must_use]
    pub const fn tree_runner_and(tools: &'static [&'static str]) -> Self {
        Self {
            tree: true,
            tools,
            runner: true,
            reader: "",
            change_set: false,
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

/// A gate's last kept verdict, with the key it was kept under.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Kept {
    key: String,
    report: GateReport,
}

type Record = BTreeMap<String, Kept>;

/// A digest of everything a gate's verdict depends on. `None` if any input cannot be read, since a
/// partial key could match a tree it should not.
#[must_use]
pub fn key(
    reads: Reads,
    gate: &str,
    root: &Path,
    listed: &std::sync::OnceLock<Result<crate::project::Listing, String>>,
    version_of: &dyn Fn(&str) -> Option<String>,
    runner_tools: &[String],
) -> Option<String> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // A new chock may judge the same tree differently.
    env!("CARGO_PKG_VERSION").hash(&mut hasher);
    gate.hash(&mut hasher);
    for tool in reads.tools {
        tool.hash(&mut hasher);
        version_of(tool)?.hash(&mut hasher);
    }
    if reads.runner {
        // A runner's tools do not show in its own bytes, so unnamed tools mean no key.
        if runner_tools.is_empty() {
            return None;
        }
        for tool in runner_tools {
            tool.hash(&mut hasher);
            version_of(tool)?.hash(&mut hasher);
        }
    }
    // Whole files: over-hashing costs a recall, under-hashing a wrong verdict. The walk prunes
    // `.outpost/`, so its config is named here.
    for named in [
        crate::project::config::FILE,
        crate::run::baseline::FILE,
        ".outpost/config.toml",
    ] {
        std::fs::read(root.join(named)).ok().hash(&mut hasher);
    }
    if reads.tree {
        tree(root, listed)?.hash(&mut hasher);
    }
    Some(format!("{:016x}", hasher.finish()))
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
    (kept.key == key).then_some(kept.report)
}

static KEEPING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Keeps a verdict for later runs. A `cannot_run` is never kept, since nothing was measured.
pub fn keep(root: &Path, gate: &str, key: &str, report: &GateReport) {
    if report.verdict == Verdict::CannotRun {
        return;
    }
    // Gates finish in parallel, and concurrent read-modify-writes would drop each other's verdicts.
    let _one_at_a_time = KEEPING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut held = read(root).unwrap_or_default();
    held.insert(
        gate.to_string(),
        Kept {
            key: key.to_string(),
            report: report.clone(),
        },
    );
    // Best effort: an unwritten record only means the next run takes the verdict again.
    let _ = write(root, &held);
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
/// `cargo-udeps --version` prints nothing.
#[must_use]
fn asked(tool: &str) -> (String, Vec<String>) {
    match tool.strip_prefix("cargo-") {
        Some(sub) => (
            "cargo".to_string(),
            vec![sub.to_string(), "--version".to_string()],
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

    fn keyed(root: &Path, tools: &'static [&'static str]) -> Option<String> {
        key(
            Reads::tree_and(tools),
            "probe",
            root,
            &std::sync::OnceLock::new(),
            &|tool| Some(format!("{tool} 1.0.0")),
            &[],
        )
    }

    fn passed() -> GateReport {
        GateReport::new("probe", Verdict::Pass, "chock run probe")
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
    fn a_gate_that_reads_a_tool_keys_on_what_that_tool_answered() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let bare = keyed(&dir, &[]).unwrap();
        let with_tool = keyed(&dir, &["clippy-driver"]).unwrap();
        assert_ne!(with_tool, bare);
        assert_ne!(keyed(&dir, &["kani"]).unwrap(), with_tool);
    }

    /// The way a shell resolves it, so the binary stamped is the one that runs.
    #[test]
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
    fn a_tree_that_has_not_moved_keys_the_same_and_one_that_has_does_not() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let first = keyed(&dir, &[]).unwrap();
        assert_eq!(keyed(&dir, &[]).unwrap(), first);
        std::fs::write(dir.join("src/a.rs"), "fn a() { }\n").unwrap();
        assert_ne!(keyed(&dir, &[]).unwrap(), first);
    }

    /// The digest is over the walk, not the index, so an untracked file counts.
    #[test]
    fn a_file_nothing_tracks_still_changes_the_key() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let first = keyed(&dir, &[]).unwrap();
        std::fs::write(dir.join("src/b.rs"), "fn b() {}\n").unwrap();
        assert_ne!(keyed(&dir, &[]).unwrap(), first);
    }

    #[test]
    fn moving_a_file_without_changing_a_byte_changes_the_key() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let first = keyed(&dir, &[]).unwrap();
        std::fs::rename(dir.join("src/a.rs"), dir.join("src/b.rs")).unwrap();
        assert_ne!(keyed(&dir, &[]).unwrap(), first);
    }

    #[test]
    fn a_tool_that_reports_a_new_version_changes_the_key() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let held = Reads::tree_and(&["cargo"]);
        let said = |version: &str| {
            let version = version.to_string();
            move |_: &str| Some(version.clone())
        };
        let one = key(
            held,
            "probe",
            &dir,
            &std::sync::OnceLock::new(),
            &said("cargo 1.90.0"),
            &[],
        );
        let two = key(
            held,
            "probe",
            &dir,
            &std::sync::OnceLock::new(),
            &said("cargo 1.91.0"),
            &[],
        );
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
            asked("kani"),
            ("kani".to_string(), vec!["--version".to_string()])
        );
    }

    #[test]
    fn a_gate_reading_the_runner_has_no_key_until_its_tools_are_named() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let reads = Reads::tree_runner_and(&["cargo"]);
        let keyed = |tools: &[String]| {
            key(
                reads,
                "probe",
                &dir,
                &std::sync::OnceLock::new(),
                &|tool| Some(format!("{tool} 1.0.0")),
                tools,
            )
        };
        assert_eq!(keyed(&[]), None, "nothing named, so nothing to key on");
        let named = keyed(&["kani".to_string()]).unwrap();
        assert_ne!(keyed(&["clippy-driver".to_string()]).unwrap(), named);
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
    fn a_tool_that_says_nothing_leaves_no_key_at_all() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let none = key(
            Reads::tree_and(&["cargo"]),
            "probe",
            &dir,
            &std::sync::OnceLock::new(),
            &|_| None,
            &[],
        );
        assert_eq!(none, None);
    }

    /// A ratchet's verdict is a comparison against the baseline.
    #[test]
    fn recording_a_new_baseline_changes_the_key() {
        let dir = tree_of(&[("src/a.rs", "fn a() {}\n")]);
        let first = keyed(&dir, &[]).unwrap();
        let at = dir.join(crate::run::baseline::FILE);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, "{\"version\":1,\"gates\":{}}").unwrap();
        assert_ne!(keyed(&dir, &[]).unwrap(), first);
    }

    #[test]
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
    fn a_gate_that_could_not_run_is_never_kept() {
        let dir = tree_of(&[]);
        let refused = GateReport::cannot_run("probe", "chock run probe", "no tool");
        keep(&dir, "probe", "abc", &refused);
        assert_eq!(recall(&dir, "probe", "abc"), None);
    }

    /// Its findings hold until the tree moves, so re-running only fails the same way.
    #[test]
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
