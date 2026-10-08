//! chock as a process: each case runs the built binary and checks its stdout, stderr and code.

// Every test here starts the `chock` binary, and Miri cannot start a process.
#![cfg(not(miri))]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap or panic in a test is the test failing, which is the point"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use chock::cli::VERSION;
use chock::run::report::{Run, SCHEMA, Verdict};

/// Three comment lines, escaped: as real comments they would trip `slop` on this file.
const OVER_LIMIT: &str = "// the first line\n// the second line\n// the third line\nfn main() {}\n";

const AT_LIMIT: &str = "// the first line\n// the second line\nfn main() {}\n";

const MANIFEST: &str = "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n";

/// A pin no machine can satisfy, so `doctor` has exactly one answer available to it.
const IMPOSSIBLE_PIN: &str = "CHOCK_NOT_A_REAL_TOOL_VERSION=9.9.9\n";

const IMPOSSIBLE_TOOL: &str = "chock-not-a-real-tool";

/// Every subcommand and exit code the usage must name; the prose around them is free.
const USAGE_LINES: [&str; 13] = [
    "chock run [GATE...]",
    "chock run --no-cache",
    "chock cache clear",
    "chock gates",
    "chock explain GATE",
    "chock baseline [GATE...]",
    "chock doctor",
    "chock message FILE",
    "chock edited PATH...",
    "chock slop [DIR]",
    "chock init --global",
    "--json                          machine-readable output",
    "Exit codes: 0 every gate passed, 1 a gate tripped, 2 a gate could not run.",
];

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A scratch directory that removes itself; leftovers once made a file watcher saturate the disk.
struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A failed test keeps its directory as evidence.
        if std::thread::panicking() {
            return;
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Mirrors `src/testdir.rs`, which is `#[cfg(test)]`-only and so unreachable from here: scratch
/// trees go under `target/` because a mutated `PathBuf` is empty and writes then land in the CWD.
fn scratch(name: &str) -> Scratch {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-scratch")
        .join(format!("cli-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

struct Ran {
    code: i32,
    out: String,
    err: String,
}

/// `CARGO_BIN_EXE_chock` is this build's binary, never an installed chock.
fn chock(cwd: &Path, args: &[&str]) -> Ran {
    ran(Command::new(env!("CARGO_BIN_EXE_chock"))
        .args(args)
        .current_dir(cwd))
}

/// The exit code and both streams of a command that ended on its own.
fn ran(command: &mut Command) -> Ran {
    let done = command.output().unwrap();
    Ran {
        code: done.status.code().unwrap(),
        out: String::from_utf8(done.stdout).unwrap(),
        err: String::from_utf8(done.stderr).unwrap(),
    }
}

/// The same, with the editor's tool JSON on stdin, which is what `--hook` reads.
fn chock_hook(cwd: &Path, tool_json: &str) -> Ran {
    use std::io::Write as _;
    let mut child = Command::new(env!("CARGO_BIN_EXE_chock"))
        .args(["edited", "--hook"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(tool_json.as_bytes())
        .unwrap();
    let done = child.wait_with_output().unwrap();
    Ran {
        code: done.status.code().unwrap(),
        out: String::from_utf8(done.stdout).unwrap(),
        err: String::from_utf8(done.stderr).unwrap(),
    }
}

fn put(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A fixture `Cargo.toml` shadows chock's own, so the scratch tree is the project under test.
fn project(name: &str, files: &[(&str, &str)]) -> Scratch {
    let dir = scratch(name);
    put(&dir, "Cargo.toml", MANIFEST);
    for (path, text) in files {
        put(&dir, path, text);
    }
    dir
}

/// Whether `chock doctor` printed a row for `tool`: the name is the second word of a row.
fn has_row(out: &str, tool: &str) -> bool {
    out.lines()
        .any(|row| row.split_whitespace().nth(1) == Some(tool))
}

/// Runs chock in `cwd` and asserts it exits with `code`, showing both streams when it does not.
fn exits(cwd: &Path, args: &[&str], code: i32) -> Ran {
    let ran = chock(cwd, args);
    assert_eq!(ran.code, code, "{}{}", ran.out, ran.err);
    ran
}

/// Writes a config that turns on only `gates`.
fn turn_on(dir: &Path, gates: &[&str]) {
    let config = chock::project::config::Config::of(gates.iter().copied());
    put(dir, chock::project::config::FILE, &config.render());
}

fn says(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected `{needle}` in:\n{haystack}"
    );
}

fn parsed(json: &str) -> Run {
    serde_json::from_str(json).unwrap_or_else(|e| panic!("not a chock run document: {e}\n{json}"))
}

fn gate_of<'a>(run: &'a Run, name: &str) -> &'a chock::run::report::GateReport {
    run.gates
        .iter()
        .find(|report| report.gate == name)
        .unwrap_or_else(|| panic!("no `{name}` in the run"))
}

#[test]
fn the_version_flag_prints_the_crate_version_and_exits_zero() {
    let dir = scratch("version");
    for flag in ["--version", "-V"] {
        let ran = chock(&dir, &[flag]);
        assert_eq!(ran.code, 0, "{flag}: {}", ran.err);
        assert_eq!(ran.out, format!("chock {VERSION}\n"));
        assert_eq!(ran.err, "");
    }
}

#[test]
fn the_help_flag_prints_the_usage_and_exits_zero() {
    let dir = scratch("help");
    for flag in ["--help", "-h"] {
        let ran = chock(&dir, &[flag]);
        assert_eq!(ran.code, 0, "{flag}: {}", ran.err);
        assert_eq!(ran.err, "");
        for line in USAGE_LINES {
            says(&ran.out, line);
        }
    }
}

#[test]
fn no_arguments_exits_two_and_names_the_problem() {
    let ran = exits(&scratch("no-arguments"), &[], 2);
    says(&ran.err, "chock: no command given");
    says(&ran.err, "chock run [GATE...]");
    assert_eq!(ran.out, "", "a usage error belongs on stderr");
}

#[test]
fn an_unknown_command_exits_two_and_names_it() {
    let ran = exits(&scratch("unknown-command"), &["doctr"], 2);
    says(&ran.err, "chock: unknown command `doctr`");
    says(&ran.err, "chock doctor");
    assert_eq!(ran.out, "");
}

/// Refused before chock looks for a project, so a bare scratch directory is enough.
#[test]
fn an_argument_a_subcommand_does_not_take_is_named_rather_than_ignored() {
    let dir = scratch("stray-argument");

    let ran = exits(&dir, &["run", "--deep"], 2);
    says(&ran.err, "chock: unknown option `--deep`");

    let ran = exits(&dir, &["gates", "all"], 2);
    says(&ran.err, "chock: gates takes no argument `all`");
}

#[test]
fn gates_lists_every_registered_gate_and_what_it_measures() {
    let ran = chock(&scratch("gates"), &["gates"]);
    assert_eq!(ran.code, 0, "{}", ran.err);
    assert_eq!(ran.err, "");
    for gate in chock::gates::registry() {
        says(&ran.out, gate.name);
        says(&ran.out, gate.about);
    }
}

#[test]
fn gates_json_carries_the_seven_fields_for_every_gate() {
    let ran = chock(&scratch("gates-json"), &["gates", "--json"]);
    assert_eq!(ran.code, 0, "{}", ran.err);
    let doc: serde_json::Value = serde_json::from_str(&ran.out).unwrap();
    assert_eq!(doc["chock"], serde_json::json!(VERSION));

    let rows = doc["gates"].as_array().unwrap();
    let listed: Vec<&str> = rows
        .iter()
        .map(|row| row["gate"].as_str().unwrap())
        .collect();
    let registered: Vec<&str> = chock::gates::registry().iter().map(|g| g.name).collect();
    assert_eq!(listed, registered);
    for row in rows {
        for field in [
            "gate", "about", "group", "ratchet", "enabled", "stage", "rerun",
        ] {
            assert!(!row[field].is_null(), "{field} missing from {row}");
        }
    }

    let slop = rows.iter().find(|row| row["gate"] == "slop").unwrap();
    assert_eq!(
        slop["about"],
        serde_json::json!(chock::gates::text::slop::GATE.about)
    );
    assert_eq!(slop["group"], serde_json::json!("quality"));
    // A scratch tree under `target/` finds chock's own manifest and config, where `slop` is on;
    // `gates_says_which_are_on_and_falls_back_to_the_set_a_run_would_use` covers the default set.
    assert_eq!(slop["enabled"], serde_json::json!(true));
    assert_eq!(
        slop["rerun"],
        serde_json::json!(chock::run::rerun(chock::gates::text::slop::GATE.name))
    );
    assert!(slop["ratchet"].as_bool().unwrap(), "slop is a ratchet");
}

#[test]
fn slop_names_the_file_and_the_line_of_an_over_long_block() {
    let dir = scratch("slop-tripped");
    put(&dir, "src/legacy.rs", OVER_LIMIT);
    let path = dir.display().to_string();

    let ran = chock(&dir, &["slop", &path]);
    assert_eq!(ran.code, 1, "{}", ran.err);
    says(&ran.out, "1 comment block over 2 lines.");
    says(
        &ran.out,
        "src/legacy.rs:1: comment block of 3 lines, over 2",
    );
    assert_eq!(ran.err, "");
}

/// No path argument, so this is also the case that proves chock falls back to its own directory.
#[test]
fn a_tree_within_the_limit_passes_slop_in_silence() {
    let dir = scratch("slop-clean");
    put(&dir, "src/tidy.rs", AT_LIMIT);

    let ran = chock(&dir, &["slop"]);
    assert_eq!(ran.code, 0, "{}", ran.err);
    assert_eq!(ran.out, "");
    assert_eq!(ran.err, "");
}

#[test]
fn slop_json_carries_the_finding_with_its_file_and_line() {
    let dir = scratch("slop-json");
    put(&dir, "src/legacy.rs", OVER_LIMIT);
    let path = dir.display().to_string();

    let ran = chock(&dir, &["slop", &path, "--json"]);
    assert_eq!(ran.code, 1, "{}", ran.err);
    let run = parsed(&ran.out);
    assert_eq!(run.version, SCHEMA);
    assert_eq!(run.chock, VERSION);

    let report = gate_of(&run, "slop");
    assert_eq!(report.verdict, Verdict::Tripped);
    assert_eq!(report.exit_code, 1);
    assert_eq!(report.measured, Some(1));
    assert_eq!(report.rerun, "chock slop");

    let finding = report
        .findings
        .iter()
        .find(|f| f.file == "src/legacy.rs")
        .unwrap();
    assert_eq!(finding.line, Some(1));
    assert_eq!(finding.message, "comment block of 3 lines, over 2");
}

#[test]
fn slop_given_something_that_is_not_a_directory_exits_two() {
    let dir = scratch("slop-not-a-directory");
    put(&dir, "notes.rs", AT_LIMIT);
    let path = dir.join("notes.rs").display().to_string();

    let ran = exits(&dir, &["slop", &path], 2);
    says(&ran.err, &format!("chock: slop: {path} is not a directory"));
    assert_eq!(ran.out, "");
}

#[test]
fn slop_takes_at_most_one_path() {
    let ran = exits(&scratch("slop-two-paths"), &["slop", "src", "tests"], 2);
    says(&ran.err, "chock: slop takes at most one path");
}

#[test]
fn doctor_without_a_pin_file_says_to_run_chock_init_local() {
    let dir = project("doctor-unpinned", &[]);

    let ran = exits(&dir, &["doctor"], 2);
    says(&ran.err, "chock: cannot read ");
    says(&ran.err, "tool-versions.env");
    says(&ran.err, "run `chock init --local` to write one");
    assert_eq!(ran.out, "");
}

#[test]
fn doctor_has_a_row_for_miri_only_where_the_check_is_on() {
    let dir = project("doctor-miri", &[("tool-versions.env", IMPOSSIBLE_PIN)]);

    let off = chock(&dir, &["doctor"]);
    assert!(!has_row(&off.out, "miri"), "{}", off.out);

    put(
        &dir,
        ".chock/config.json",
        r#"{"version": 1, "enabled": ["miri"]}"#,
    );
    let on = chock(&dir, &["doctor"]);
    assert!(has_row(&on.out, "miri"), "{}{}", on.out, on.err);
}

#[test]
fn doctor_reports_a_pin_no_machine_can_satisfy_as_missing() {
    let dir = project("doctor-missing", &[("tool-versions.env", IMPOSSIBLE_PIN)]);

    let ran = chock(&dir, &["doctor"]);
    assert_eq!(ran.code, 1, "{}", ran.err);
    says(&ran.out, &format!("MISSING  {IMPOSSIBLE_TOOL} want 9.9.9"));
    says(
        &ran.out,
        "Needs attention: 1 of 1 check. `chock init --global` installs the crates; a tool in tool-versions.env that is not a crate is yours to install.",
    );
}

/// An upgrade that changes what a gate counts leaves its old record behind. Doctor names that
/// record with the command that re-records it, before a commit refuses.
#[test]
fn doctor_names_a_record_in_a_unit_this_chock_no_longer_counts() {
    let dir = project("doctor-recounted", &[("tool-versions.env", "")]);
    let mut baseline = chock::run::baseline::Baseline::empty(VERSION);
    let series = chock::run::baseline::Series::new();
    baseline.record("slop", "comment line(s) of an older chock", series);
    put(&dir, chock::run::baseline::FILE, &baseline.render());

    let ran = exits(&dir, &["doctor"], 1);
    says(&ran.out, "RECOUNTED");
    says(&ran.out, "chock baseline slop");
}

#[test]
fn doctor_json_emits_the_run_schema() {
    let dir = project("doctor-json", &[("tool-versions.env", IMPOSSIBLE_PIN)]);

    let ran = chock(&dir, &["doctor", "--json"]);
    assert_eq!(ran.code, 1, "{}", ran.err);
    let run = parsed(&ran.out);
    assert_eq!(run.version, SCHEMA);
    assert_eq!(run.chock, VERSION);

    let report = gate_of(&run, "doctor");
    assert_eq!(report.verdict, Verdict::Tripped);
    assert_eq!(report.exit_code, 1);
    assert_eq!(report.rerun, "chock doctor");

    let finding = report
        .findings
        .iter()
        .find(|f| f.item.as_deref() == Some(IMPOSSIBLE_TOOL))
        .unwrap();
    assert_eq!(finding.file, "tool-versions.env");
    assert_eq!(
        finding.message,
        "not installed, pinned 9.9.9 — a crate is fetched by `chock init --global`"
    );
}

#[test]
fn run_refuses_a_gate_name_that_does_not_exist_and_lists_the_real_ones() {
    let dir = project("run-unknown-gate", &[]);

    let ran = exits(&dir, &["run", "nope"], 2);
    says(&ran.err, "chock: no gate named `nope`. There is: ");
    for gate in chock::gates::registry() {
        says(&ran.err, gate.name);
    }
    assert_eq!(ran.out, "");
}

#[test]
fn explain_before_any_run_says_to_run_chock_run_first() {
    let dir = project("explain-before-a-run", &[]);

    let ran = exits(&dir, &["explain", "slop"], 2);
    says(&ran.err, "no record of a run at ");
    says(&ran.err, ".chock/last-run.json");
    says(&ran.err, "run `chock run` first");
    assert_eq!(ran.out, "");
}

#[test]
fn explain_refuses_a_name_that_is_not_a_gate() {
    let dir = project("explain-unknown-gate", &[]);

    let ran = exits(&dir, &["explain", "nope"], 2);
    says(&ran.err, "chock: no gate named `nope`. There is: ");
}

#[test]
fn explain_refuses_a_last_run_that_lacks_the_gate_or_does_not_read() {
    let dir = project("explain-other-records", &[("src/legacy.rs", OVER_LIMIT)]);
    exits(&dir, &["run", "slop"], 0);
    let absent = chock(&dir, &["explain", "fmt"]);
    assert_eq!(absent.code, 2, "{}", absent.out);
    says(&absent.err, "chock: the last run did not include `fmt`");

    put(&dir, ".chock/last-run.json", "not a record");
    let unread = chock(&dir, &["explain", "slop"]);
    assert_eq!(unread.code, 2, "{}", unread.out);
    says(
        &unread.err,
        ".chock/last-run.json is not a chock run record",
    );
}

#[test]
fn the_first_run_of_a_gate_writes_its_record_and_ci_writes_none() {
    let dir = project("first-record-slop", &[("src/legacy.rs", OVER_LIMIT)]);

    let ci = chock(&dir, &["run", "slop", "--ci"]);
    assert_eq!(ci.code, 2, "{}", ci.err);
    says(
        &ci.out,
        "slop       CANNOT RUN  no record `slop` is committed — `chock run slop` outside CI takes \
         the first one and writes .chock/baseline.json",
    );
    assert!(
        !dir.join(".chock/baseline.json").exists(),
        "CI writes nothing"
    );

    let first = exits(&dir, &["run", "slop"], 0);
    says(&first.out, "slop       ok          1 against 1");
    says(
        &first.err,
        "chock: wrote the first record for slop; commit .chock/baseline.json with this change",
    );

    let written = std::fs::read_to_string(dir.join(".chock/baseline.json")).unwrap();
    let recorded = chock::project::document::parse::<chock::run::baseline::Baseline>(
        &written,
        ".chock/baseline.json",
    )
    .unwrap();
    assert_eq!(recorded.version, chock::run::baseline::SCHEMA);
    assert_eq!(recorded.chock, VERSION);
    assert_eq!(recorded.gate("slop").get("src/legacy.rs"), Some(1));
    assert_eq!(recorded.unit("slop"), Some("over-long comment block(s)"));

    let held = chock(&dir, &["run", "slop", "--ci"]);
    assert_eq!(held.code, 0, "{}", held.err);
    says(&held.out, "slop       ok          1 against 1");
    // The record is there, so a later run writes nothing and says nothing of it.
    let later = chock(&dir, &["run", "--no-cache", "slop"]);
    assert_eq!(later.code, 0, "{}", later.err);
    assert!(!later.err.contains("record for slop"), "{}", later.err);
    assert_eq!(
        std::fs::read_to_string(dir.join(".chock/baseline.json")).unwrap(),
        written
    );
}

#[test]
fn baseline_still_records_a_gate_by_name() {
    let dir = project("baseline-slop", &[("src/legacy.rs", OVER_LIMIT)]);

    let recording = chock(&dir, &["baseline", "slop"]);
    assert_eq!(recording.code, 0, "{}", recording.err);
    says(&recording.out, "recorded  slop         1 item");
    says(&recording.out, "wrote .chock/baseline.json");
    let after = chock(&dir, &["run", "slop"]);
    assert_eq!(after.code, 0, "{}", after.err);
    says(&after.out, "slop       ok          1 against 1");
    assert!(!after.err.contains("record for slop"), "{}", after.err);
}

#[test]
fn cache_clear_removes_every_kept_verdict_and_cache_alone_is_refused() {
    let dir = project("cache-clear", &[]);
    let none = chock(&dir, &["cache", "clear"]);
    assert_eq!(none.code, 0, "{}", none.err);
    assert_eq!(
        none.out,
        "chock: removed 0 kept verdict(s); the next run judges every gate again\n"
    );

    let passed = chock::run::report::GateReport::new(
        "fmt",
        chock::run::report::Verdict::Pass,
        "chock run fmt",
    );
    chock::run::verdicts::keep(&dir, "fmt", "one", &passed);
    chock::run::verdicts::keep(&dir, "lint", "one", &passed);
    let two = chock(&dir, &["cache", "clear"]);
    assert_eq!(two.code, 0, "{}", two.err);
    assert_eq!(
        two.out,
        "chock: removed 2 kept verdict(s); the next run judges every gate again\n"
    );
    assert!(!dir.join(chock::run::verdicts::FILE).exists());

    let bare = exits(&dir, &["cache"], 2);
    says(&bare.err, "chock: cache takes one word: clear");
}

#[test]
fn cache_clear_says_why_it_cannot_take_the_lock_or_remove_the_file() {
    // `.chock` is a file here, so no lock can be made in it.
    let blocked = project("cache-no-lock", &[(".chock", "")]);
    let refused = chock(&blocked, &["cache", "clear"]);
    assert_eq!(refused.code, 2, "{}", refused.out);
    says(&refused.err, "chock: cannot make ");

    // A directory where the kept verdicts go: nothing removes it as a file.
    let dir = project("cache-no-remove", &[]);
    std::fs::create_dir_all(dir.join(chock::run::verdicts::FILE)).unwrap();
    let kept = chock(&dir, &["cache", "clear"]);
    assert_eq!(kept.code, 2, "{}", kept.out);
    says(&kept.err, "chock: cannot remove .chock/verdicts.json: ");
}

#[test]
fn explain_with_no_gate_lists_the_debt_on_record_with_its_fix() {
    let dir = project("explain-debt", &[("src/legacy.rs", OVER_LIMIT)]);
    assert_eq!(chock(&dir, &["baseline", "slop"]).code, 0);
    let told = chock(&dir, &["explain"]);
    assert_eq!(told.code, 0, "{}", told.err);
    says(
        &told.out,
        "slop — 1 over-long comment block(s) in 1 place(s)\n  src/legacy.rs: 1\n  fix: ",
    );
    let json = chock(&dir, &["explain", "--json"]);
    says(
        &json.out,
        r#"{"debt":[{"gate":"slop","unit":"over-long comment block(s)","total":1"#,
    );
}

fn slop_record(dir: &Path) -> Option<u64> {
    let written = std::fs::read_to_string(dir.join(".chock/baseline.json")).unwrap();
    chock::project::document::parse::<chock::run::baseline::Baseline>(&written, "baseline")
        .unwrap()
        .gate("slop")
        .get("src/legacy.rs")
}

#[test]
fn a_fix_lowers_the_record_and_ci_refuses_a_change_that_did_not_commit_it() {
    let two = format!("{OVER_LIMIT}{OVER_LIMIT}");
    let dir = project("baseline-locks-in", &[("src/legacy.rs", &two)]);
    assert_eq!(chock(&dir, &["baseline", "slop"]).code, 0);
    put(&dir, "src/legacy.rs", OVER_LIMIT);
    let held = std::fs::read_to_string(dir.join(".chock/baseline.json")).unwrap();

    let ci = exits(&dir, &["run", "slop", "--ci"], 1);
    says(
        &ci.out,
        "src/legacy.rs: 1 over-long comment block(s) where the record holds 2",
    );
    assert_eq!(slop_record(&dir), Some(2), "CI writes nothing");

    let local = exits(&dir, &["run", "slop"], 0);
    says(
        &local.err,
        "chock: lowered the record for slop; commit .chock/baseline.json",
    );
    assert_eq!(slop_record(&dir), Some(1));
    assert_eq!(
        chock(&dir, &["run", "slop", "--ci"]).code,
        0,
        "the lower record is held"
    );

    put(&dir, ".chock/baseline.json", &held);
    let lowered = chock(&dir, &["baseline", "--lower", "slop"]);
    assert_eq!(lowered.code, 0, "{}", lowered.err);
    says(&lowered.out, "lowered   slop         1 item");
    assert_eq!(slop_record(&dir), Some(1));
}

#[test]
fn debt_on_record_fails_in_a_file_the_change_touched_where_the_project_asks() {
    let dir = project(
        "clean-when-touched",
        &[("src/legacy.rs", OVER_LIMIT), ("src/other.rs", OVER_LIMIT)],
    );
    put(
        &dir,
        ".chock/config.json",
        r#"{"version": 1, "enabled": ["slop"], "clean_when_touched": ["slop"]}"#,
    );
    assert_eq!(chock(&dir, &["baseline", "slop"]).code, 0);
    assert!(git(&dir, &["init", "-q", "."]).status.success());
    for (key, value) in [("user.email", "t@t"), ("user.name", "t")] {
        assert!(git(&dir, &["config", key, value]).status.success());
    }
    assert!(git(&dir, &["add", "-A"]).status.success());
    assert!(git(&dir, &["commit", "-q", "-m", "First"]).status.success());
    exits(&dir, &["run", "slop"], 0);

    put(
        &dir,
        "src/legacy.rs",
        &format!("{OVER_LIMIT}pub fn f() {{}}\n"),
    );
    let touched = exits(&dir, &["run", "slop"], 1);
    says(
        &touched.out,
        "src/legacy.rs: 1 over-long comment block(s) in a file this change touched",
    );
    assert!(!touched.out.contains("src/other.rs"), "{}", touched.out);
}

#[test]
fn run_writes_the_last_run_record_and_explain_reports_from_it_without_running_again() {
    let dir = project("explain-from-the-record", &[("src/legacy.rs", OVER_LIMIT)]);
    assert_eq!(chock(&dir, &["baseline", "slop"]).code, 0);

    let first = chock(&dir, &["run", "slop"]);
    assert_eq!(first.code, 0, "{}", first.err);
    let record = std::fs::read_to_string(dir.join(".chock/last-run.json")).unwrap();
    assert_eq!(gate_of(&parsed(&record), "slop").verdict, Verdict::Pass);

    // Debt the baseline never saw: a fresh run trips on it, so an `explain` that still answers
    // "ok" can only be reading the record rather than measuring the tree again.
    put(&dir, "src/fresh.rs", OVER_LIMIT);

    let explained = chock(&dir, &["explain", "slop"]);
    assert_eq!(explained.code, 0, "{}", explained.err);
    says(&explained.out, "slop       ok          1 against 1");
    says(
        &explained.out,
        "src/legacy.rs:1: comment block of 3 lines, over 2",
    );
    says(
        &explained.out,
        &format!(
            "re-run with: {}",
            chock::run::rerun(chock::gates::text::slop::GATE.name)
        ),
    );
    assert!(
        !explained.out.contains("src/fresh.rs"),
        "explain re-measured the tree:\n{}",
        explained.out
    );

    let again = chock(&dir, &["run", "slop"]);
    assert_eq!(again.code, 1, "{}", again.err);
    says(
        &again.out,
        "src/fresh.rs: 1 over-long comment block(s), not in the baseline",
    );
}

#[test]
fn running_one_gate_leaves_every_other_gates_last_result_where_explain_can_read_it() {
    let dir = project(
        "explain-after-another-gate",
        &[
            ("src/lib.rs", "mod legacy;\n"),
            ("src/legacy.rs", OVER_LIMIT),
        ],
    );
    assert_eq!(chock(&dir, &["baseline", "slop"]).code, 0);
    assert_eq!(chock(&dir, &["run", "slop"]).code, 0);

    // A second gate, on its own: the record it writes must not take `slop` with it.
    let other = chock(&dir, &["run", "modcheck"]);
    assert_eq!(other.code, 0, "{}", other.err);
    let record = parsed(&std::fs::read_to_string(dir.join(".chock/last-run.json")).unwrap());
    assert_eq!(gate_of(&record, "slop").verdict, Verdict::Pass);
    assert_eq!(gate_of(&record, "modcheck").verdict, Verdict::Pass);

    let explained = chock(&dir, &["explain", "slop"]);
    assert_eq!(explained.code, 0, "{}", explained.err);
    says(&explained.out, "slop       ok          1 against 1");
    // A kept result says when it was measured, so nobody reads it as a fresh one.
    says(&explained.out, "measured just now");
}

/// A real shell parses the one-line hook left for an older git, and it passes the message on.
#[cfg(unix)]
#[test]
fn the_hook_file_for_an_older_git_delegates_and_carries_its_argument() {
    let dir = scratch("hook-stub");
    let hooks = dir.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // A `chock` that reports what it was handed, so the stub's own argument passing is measured.
    std::fs::write(
        bin.join("chock"),
        "#!/usr/bin/env bash\necho \"called: $*\"\nexit 0\n",
    )
    .unwrap();
    make_runnable(&bin.join("chock"));

    for (name, expect) in [
        ("pre-commit", "called: hook pre-commit"),
        ("pre-push", "called: hook pre-push"),
        ("commit-msg", "called: hook commit-msg /tmp/msg"),
    ] {
        let path = hooks.join(name);
        put(
            &dir,
            &format!("hooks/{name}"),
            &chock::setup::hooks::stub(name),
        );
        make_runnable(&path);
        let ran = Command::new("bash")
            .arg(&path)
            .arg("/tmp/msg")
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .current_dir(&dir)
            .output()
            .unwrap();
        let out = String::from_utf8(ran.stdout).unwrap();
        assert_eq!(
            ran.status.code(),
            Some(0),
            "{name}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
        says(&out, expect);
    }
}

/// The tally goes to stdout and the pointer to stderr; exit 2 still means nothing was measured.
#[test]
fn the_hook_blocks_and_says_where_to_look() {
    let dir = project("hook-blocks", &[("src/lib.rs", OVER_LIMIT)]);
    let ran = chock(&dir, &["hook", "pre-commit"]);
    // Nothing is baselined here, so gates refuse rather than trip: the code is 2, not 1.
    assert_eq!(ran.code, 2, "{}", ran.err);
    says(&ran.err, "pre-commit: see the line above.");
    says(&ran.err, "chock explain <gate>");
    says(&ran.out, "could not run");
}

/// Git hands a declared hook no arguments at all — measured on 2.55 — so `commit-msg` cannot be
/// given `$1` and asks git for the message being composed. Only a real commit proves that.
#[test]
fn a_declared_commit_msg_hook_finds_the_message_git_is_composing() {
    let dir = project("hook-commit-msg", &[("src/lib.rs", "pub fn f() {}\n")]);
    assert!(git(&dir, &["init", "-q", "."]).status.success());
    for (key, value) in [("user.email", "t@t"), ("user.name", "t")] {
        assert!(git(&dir, &["config", key, value]).status.success());
    }
    // Declared against the binary, exactly as `init --local` does it.
    let name = "commit-msg";
    assert!(
        git(&dir, &["config", &format!("hook.chock-{name}.event"), name])
            .status
            .success()
    );
    assert!(
        git(
            &dir,
            &[
                "config",
                &format!("hook.chock-{name}.command"),
                &chock::setup::hooks::command(name)
            ]
        )
        .status
        .success()
    );
    assert!(git(&dir, &["add", "-A"]).status.success());

    let long = "x".repeat(90);
    let refused = commit(&dir, &long);
    assert!(
        !refused.status.success(),
        "a 90-character subject was allowed"
    );
    // git forwards a hook's output as it pleases, so the reason is looked for in either stream.
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    says(&said, "the subject is 90 characters");

    let allowed = commit(&dir, "Add a function nothing calls yet");
    assert!(
        allowed.status.success(),
        "{}{}",
        String::from_utf8_lossy(&allowed.stdout),
        String::from_utf8_lossy(&allowed.stderr)
    );
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

/// A commit with the built chock first on PATH, which is what the declared hook resolves.
fn commit(dir: &Path, message: &str) -> std::process::Output {
    let bin = Path::new(env!("CARGO_BIN_EXE_chock")).parent().unwrap();
    Command::new("git")
        .args(["commit", "-q", "-m", message])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .current_dir(dir)
        .output()
        .unwrap()
}

#[test]
fn a_hook_name_chock_does_not_run_is_refused() {
    let dir = project("hook-unknown", &[]);
    let ran = exits(&dir, &["hook", "post-merge"], 2);
    says(&ran.err, "chock hook does not run `post-merge`");
    let bare = exits(&dir, &["hook"], 2);
    says(&bare.err, "needs a hook name");
    // No message file and none being composed leaves nothing to check, which is not a pass.
    let unnamed = exits(&dir, &["hook", "commit-msg"], 2);
    says(&unnamed.err, "git names no message being composed");
}

#[cfg(unix)]
fn make_runnable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut mode = std::fs::metadata(path).unwrap().permissions();
    mode.set_mode(0o755);
    std::fs::set_permissions(path, mode).unwrap();
}

/// A run refuses a config that does not read, so `gates` and `message` refuse it too.
#[test]
fn a_config_that_does_not_read_is_refused_and_gates_shows_each_stage() {
    let config = ".chock/config.json";
    let retired = r#"{"version": 1, "enabled": ["binsize"], "at_ci": ["binsize"]}"#;
    let dir = project(
        "gates-config",
        &[(config, retired), ("MSG", "Add a test\n")],
    );
    for args in [&["gates"][..], &["message", "MSG"]] {
        let ran = chock(&dir, args);
        assert_eq!(ran.code, 2, "{args:?}");
        says(&ran.err, "unknown field `at_ci`");
    }

    put(
        &dir,
        config,
        r#"{"version": 1, "enabled": [], "stage": {"binsize": "ci"}}"#,
    );
    let ran = chock(&dir, &["gates", "--json"]);
    assert_eq!(ran.code, 0, "{}", ran.err);
    let doc: serde_json::Value = serde_json::from_str(&ran.out).unwrap();
    let rows = doc["gates"].as_array().unwrap();
    let stage = |name: &str| rows.iter().find(|row| row["gate"] == name).unwrap()["stage"].clone();
    assert_eq!(stage("binsize"), serde_json::json!("ci"));
    assert_eq!(stage("lint"), serde_json::json!("push"));
    assert_eq!(stage("slop"), serde_json::json!("commit"));
}

/// Off a project a run uses the default set, so `gates` reports those gates as on.
#[test]
fn gates_says_which_are_on_and_falls_back_to_the_set_a_run_would_use() {
    let outside = Path::new("/");
    let ran = chock(outside, &["gates", "--json"]);
    assert_eq!(ran.code, 0, "{}", ran.err);
    let doc: serde_json::Value = serde_json::from_str(&ran.out).unwrap();
    let enforced: Vec<&str> = chock::gates::enforced().iter().map(|g| g.name).collect();
    for row in doc["gates"].as_array().unwrap() {
        let name = row["gate"].as_str().unwrap();
        assert_eq!(
            row["enabled"],
            serde_json::json!(enforced.contains(&name)),
            "{name}"
        );
    }

    // The text gives the same answer as the JSON.
    let text = chock(outside, &["gates"]);
    assert_eq!(text.code, 0, "{}", text.err);
    for line in text.out.lines() {
        let mut words = line.split_whitespace();
        let (state, name) = (words.next().unwrap(), words.next().unwrap());
        assert_eq!(
            state,
            if enforced.contains(&name) {
                "on"
            } else {
                "off"
            },
            "{name}"
        );
    }
}

/// The filesystem root is the one directory with no `Cargo.toml` above it: a scratch tree under
/// `target/` would find chock's own manifest and be judged part of this project.
#[test]
fn a_command_run_outside_a_rust_project_names_the_manifest_it_could_not_find() {
    let outside = Path::new("/");
    assert!(
        !outside.join("Cargo.toml").is_file(),
        "the filesystem root holds a Cargo.toml, so this case proves nothing"
    );
    for args in [
        vec!["doctor"],
        vec!["run", "slop"],
        vec!["baseline", "slop"],
        vec!["explain", "slop"],
    ] {
        let ran = chock(outside, &args);
        assert_eq!(ran.code, 2, "{args:?}: {}", ran.err);
        says(
            &ran.err,
            "chock: no Cargo.toml here or in any parent — chock runs inside a Rust project",
        );
    }
}

/// `init --local` measures every gate before enabling any, so only its refusals are exercised
/// here: a passing case would run cargo over the fixture and take minutes.
#[test]
fn init_refuses_what_it_cannot_do_before_writing_anything() {
    let dir = project("init-refusals", &[]);
    let ran = exits(&dir, &["init", "--deep"], 2);
    says(&ran.err, "chock init: unknown option `--deep`");
    assert!(
        !dir.join("justfile").exists(),
        "init wrote a file despite refusing the option"
    );

    let ran = exits(Path::new("/"), &["init", "--local"], 2);
    says(
        &ran.err,
        "chock init: no Cargo.toml here or in any parent — chock installs into a Rust project",
    );
}

/// `--global` reads its pins before any install, so a pin file it cannot read or parse stops it
/// on every system with no tool started. A directory of that name is a file no system can read.
#[test]
fn init_global_stops_at_a_pin_file_it_cannot_read_or_parse() {
    let dir = project("init-pins", &[]);
    std::fs::create_dir_all(dir.join("tool-versions.env")).unwrap();
    let ran = exits(&dir, &["init", "--global"], 2);
    says(&ran.err, "chock init: cannot read ");
    says(&ran.err, "tool-versions.env: ");

    std::fs::remove_dir(dir.join("tool-versions.env")).unwrap();
    put(&dir, "tool-versions.env", "CARGO_MUTEST_VERSION\n");
    let ran = exits(&dir, &["init", "--global"], 2);
    says(
        &ran.err,
        "tool-versions.env: line 1: expected KEY=VALUE, found `CARGO_MUTEST_VERSION`",
    );
}

/// Read from the binary, so the gate reference cannot drift from the registry.
#[test]
fn the_gate_reference_names_every_gate_the_binary_has() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = chock(repo, &["gates"]);
    // The first column says whether the gate is on, so the name is the second word.
    let gates: Vec<&str> = listed
        .out
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect();
    assert!(gates.len() > 20, "only {} gates listed", gates.len());

    let reference = std::fs::read_to_string(repo.join(GATE_REFERENCE)).unwrap();
    let named: Vec<String> = gate_rows(&reference)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let missing: Vec<&&str> = gates
        .iter()
        .filter(|gate| !named.iter().any(|name| name == *gate))
        .collect();
    assert_eq!(
        missing,
        Vec::<&&str>::new(),
        "not named in {GATE_REFERENCE}"
    );

    // The README names each gate once more, in its table by topic.
    let readme = std::fs::read_to_string(repo.join("README.md")).unwrap();
    let absent: Vec<&&str> = gates
        .iter()
        .filter(|gate| !readme.contains(&format!("`{gate}`")))
        .collect();
    assert_eq!(absent, Vec::<&&str>::new(), "not named in README.md");

    // The other direction: no row for a gate the binary no longer has.
    let gone: Vec<&String> = named
        .iter()
        .filter(|name| !gates.contains(&name.as_str()))
        .collect();
    assert_eq!(
        gone,
        Vec::<&String>::new(),
        "named in {GATE_REFERENCE} but not a gate"
    );
}

#[test]
fn the_gate_reference_files_every_gate_under_the_kind_the_binary_gives_it() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = chock(repo, &["gates", "--json"]);
    let mut ratchets = ratchet_names(&listed.out);
    assert!(ratchets.len() > 10, "only {} ratchet(s)", ratchets.len());

    let reference = std::fs::read_to_string(repo.join(GATE_REFERENCE)).unwrap();
    let mut ticked: Vec<String> = gate_rows(&reference)
        .into_iter()
        .filter(|(_, ratchet)| *ratchet)
        .map(|(name, _)| name)
        .collect();
    ratchets.sort();
    ticked.sort();
    assert_eq!(
        ticked, ratchets,
        "the ratchet column of {GATE_REFERENCE} disagrees with the binary"
    );
}

const GATE_REFERENCE: &str = "docs/gates.md";

/// Each row of the reference's gate tables, with whether its ratchet column is ticked. Only tables
/// headed `| gate | ratchet | trips when` count: `coverage` also names a command in another.
fn gate_rows(readme: &str) -> Vec<(String, bool)> {
    let mut rows = Vec::new();
    let mut in_gate_table = false;
    for line in readme.lines() {
        let row = line.trim();
        if row.starts_with("| gate | ratchet | trips when") {
            in_gate_table = true;
            continue;
        }
        if !row.starts_with('|') {
            in_gate_table = false;
            continue;
        }
        let cells: Vec<&str> = row.split('|').map(str::trim).collect();
        if in_gate_table
            && let [_, name, ratchet, ..] = cells.as_slice()
            && let Some(name) = name.strip_prefix('`').and_then(|n| n.strip_suffix('`'))
        {
            rows.push((name.to_string(), *ratchet == "✓"));
        }
    }
    rows
}

/// The gates `chock gates --json` calls ratchets, read by field: column order is for display.
fn ratchet_names(json: &str) -> Vec<String> {
    json.lines()
        .map(str::trim)
        .fold((None, Vec::new()), |(named, mut found), line| {
            if let Some(rest) = line.strip_prefix("\"gate\": \"") {
                return (rest.split('"').next().map(str::to_string), found);
            }
            if line.starts_with("\"ratchet\": true")
                && let Some(name) = named.clone()
            {
                found.push(name);
            }
            (named, found)
        })
        .1
}

/// CI once named its gates by hand and fell ten behind.
#[test]
fn ci_runs_every_gate_rather_than_a_list_that_goes_stale() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow = std::fs::read_to_string(repo.join(".github/workflows/ci.yml")).unwrap();
    let named: Vec<&str> = workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix("run: chock run "))
        .filter(|rest| !rest.starts_with("--"))
        .collect();
    assert_eq!(
        named,
        Vec::<&str>::new(),
        "CI names gates instead of running them all"
    );
    // `--ci` leaves out only what the config's `local_only` names. The job with every gate may
    // skip a slow one only where a job of its own runs it.
    let skipped = workflow
        .lines()
        .find_map(|line| line.trim().strip_prefix("run: chock run --ci --skip="));
    assert!(skipped.is_some(), "no job runs the whole set its tier can");
    for gate in skipped.unwrap_or_default().split(',') {
        assert!(
            workflow.lines().map(str::trim).any(|line| {
                line.trim_start_matches("run: ")
                    .starts_with("chock run --ci")
                    && line.split_whitespace().any(|word| word == gate)
            }),
            "the job with every gate skips `{gate}`, and no job runs it"
        );
    }
}

/// A scratch project with `slop` on; without it, an edited file answers to chock's own tree.
fn set_up(dir: Scratch) -> Scratch {
    turn_on(&dir, &["slop"]);
    dir
}

/// The mistake this catches at the keystroke: an item inserted between a doc comment and the
/// thing it documents merges the two blocks, and the merged block is over the limit.
#[test]
fn edited_reports_a_file_at_its_line_and_says_nothing_about_a_clean_one() {
    let dir = set_up(project(
        "edited-one",
        &[("src/bad.rs", OVER_LIMIT), ("src/good.rs", AT_LIMIT)],
    ));
    let bad = exits(&dir, &["edited", "src/bad.rs"], 1);
    assert!(bad.out.contains("src/bad.rs:1"), "{}", bad.out);
    let good = exits(&dir, &["edited", "src/good.rs"], 0);
    assert_eq!(good.out, "");
}

#[test]
fn edited_with_no_path_says_what_it_takes() {
    let dir = project("edited-none", &[]);
    let ran = exits(&dir, &["edited"], 2);
    assert!(ran.err.contains("one or more paths"), "{}", ran.err);
}

#[test]
fn edited_refuses_a_path_it_cannot_open_rather_than_reporting_it_clean() {
    let dir = project("edited-absent", &[]);
    let ran = exits(&dir, &["edited", "src/gone.rs"], 2);
    assert!(ran.err.contains("cannot read"), "{}", ran.err);
}

/// The hook hands chock the editor's own JSON, so nothing between them needs `jq`. Its answer goes
/// to stderr with exit 2: measured, an exit 1 on stdout reached nobody, and the agent kept writing.
#[test]
fn the_hook_reads_the_file_the_editor_names_and_answers_where_the_agent_hears_it() {
    let dir = set_up(project("edited-hook", &[("src/bad.rs", OVER_LIMIT)]));
    let ran = chock_hook(
        &dir,
        r#"{"tool_name":"Edit","tool_input":{"file_path":"src/bad.rs"}}"#,
    );
    assert_eq!((ran.code, ran.out.as_str()), (2, ""));
    assert!(ran.err.contains("src/bad.rs:1"), "{}", ran.err);
}

#[test]
fn the_hook_is_silent_about_a_file_no_project_holds_and_a_direct_ask_still_judges_it() {
    let outside = std::env::temp_dir().join(format!("chock-outside-{}.rs", std::process::id()));
    std::fs::write(&outside, OVER_LIMIT).unwrap();
    let json = format!(
        r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}}}}"#,
        outside.display().to_string().replace('\\', "\\\\")
    );
    let dir = project("edited-hook-outside", &[]);
    let hooked = chock_hook(&dir, &json);
    let asked = chock(&dir, &["edited", outside.to_str().unwrap()]);
    std::fs::remove_file(&outside).unwrap();
    assert_eq!((hooked.code, hooked.err.as_str()), (0, ""));
    assert_eq!(asked.code, 1, "{}{}", asked.out, asked.err);
    says(&asked.out, "comment block of 3 lines");
}

/// A global hook fires in crates that never ran `init`, and must leave no `.chock` there.
#[test]
fn the_hook_is_silent_in_a_crate_not_set_up_for_chock() {
    // Outside chock's own tree, which is set up and would hold the file otherwise.
    let crate_dir = std::env::temp_dir().join(format!("chock-unadopted-{}", std::process::id()));
    let bad = crate_dir.join("src/bad.rs");
    std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
    std::fs::write(crate_dir.join("Cargo.toml"), MANIFEST).unwrap();
    std::fs::write(&bad, OVER_LIMIT).unwrap();
    let json = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        bad.display().to_string().replace('\\', "\\\\")
    );
    let dir = project("edited-hook-unadopted", &[]);
    let hooked = chock_hook(&dir, &json);
    let asked = chock(&dir, &["edited", bad.to_str().unwrap()]);
    let left = crate_dir.join(".chock").exists();
    std::fs::remove_dir_all(&crate_dir).unwrap();
    assert_eq!((hooked.code, hooked.err.as_str()), (0, ""));
    assert!(
        !left,
        "chock wrote a `.chock` into a crate that never ran init"
    );
    assert_eq!(asked.code, 1, "{}{}", asked.out, asked.err);
}

#[test]
fn the_hook_is_quiet_about_a_write_chock_has_no_rule_for() {
    let dir = project("edited-hook-quiet", &[]);
    let ran = chock_hook(
        &dir,
        r#"{"tool_name":"Write","tool_input":{"file_path":"logo.png"}}"#,
    );
    assert_eq!(ran.code, 0);
    assert_eq!(ran.out, "");
}

/// A gate name chock does not have is refused, not written into the config.
#[test]
fn switching_a_gate_on_and_off_is_recorded_in_the_project_config() {
    let dir = project("switch-gates", &[("src/lib.rs", "pub fn f() {}\n")]);
    // Switching a gate on in a project that never ran `init` would write a config nothing decided.
    let undecided = exits(&dir, &["enable", "slop"], 2);
    says(&undecided.err, "chock init --local");
    put(
        &dir,
        ".chock/config.json",
        "{\"version\": 1, \"enabled\": [\"lint\"]}\n",
    );
    exits(&dir, &["enable", "slop"], 0);
    let config = std::fs::read_to_string(dir.join(".chock/config.json")).unwrap();
    says(&config, "\"slop\"");
    exits(&dir, &["disable", "slop"], 0);
    let after = std::fs::read_to_string(dir.join(".chock/config.json")).unwrap();
    let after: chock::project::config::Config = serde_json::from_str(&after).unwrap();
    assert!(!after.is_on("slop"), "it is still on: {after:?}");
    assert_eq!(
        after.left_off.get("slop").map(String::as_str),
        Some("switched off by hand"),
        "switching it off is a decision the config keeps"
    );
    let wrong = exits(&dir, &["enable", "sloop"], 2);
    says(&wrong.err, "sloop");
}

/// A config a person wrote keeps its order and spelling, so the diff of `chock enable` is the switch.
#[test]
fn switching_a_gate_on_changes_only_the_list_of_gates() {
    let dir = project("switch-in-place", &[("src/lib.rs", "pub fn f() {}\n")]);
    let written = "{\n  \"version\": 1,\n  \"runner\": [\"cargo\", \"test\"],\n  \"enabled\": \
                   [\"lint\"],\n  \"left_off\": {\"typos\": \"kept\"}\n}\n";
    put(&dir, ".chock/config.json", written);
    exits(&dir, &["enable", "slop"], 0);
    assert_eq!(
        std::fs::read_to_string(dir.join(".chock/config.json")).unwrap(),
        written.replace("[\"lint\"]", "[\n    \"lint\",\n    \"slop\"\n  ]")
    );
}

/// `chock stage` records a stage other than the default, and its default removes the entry.
#[test]
fn staging_a_gate_records_only_a_stage_other_than_its_default() {
    let dir = project("stage-in-place", &[("src/lib.rs", "pub fn f() {}\n")]);
    put(
        &dir,
        ".chock/config.json",
        r#"{"version": 1, "enabled": ["binsize"]}"#,
    );
    let config = || std::fs::read_to_string(dir.join(".chock/config.json")).unwrap();
    let ci = exits(&dir, &["stage", "binsize", "ci"], 0);
    says(&ci.out, "binsize      runs at ci");
    says(&config(), "\"binsize\": \"ci\"");
    says(
        &chock(&dir, &["stage", "binsize", "ci"]).out,
        "already runs at ci",
    );
    let manual = chock(&dir, &["stage", "typos", "manual"]);
    says(&manual.out, "only `chock run` does");
    exits(&dir, &["stage", "binsize", "push"], 0);
    assert!(!config().contains("binsize\": \""), "{}", config());
}

/// A hook declared at the top of a repository runs from there, names the message file from there,
/// and judges the project below it by that project's own rules.
#[test]
fn a_hook_for_a_project_below_the_top_moves_into_it_and_still_finds_the_message() {
    let top = project(
        "hook-below",
        &[
            ("sub/Cargo.toml", MANIFEST),
            ("sub/src/lib.rs", "pub fn f() {}\n"),
        ],
    );
    std::fs::write(top.join("good.txt"), "A subject that says what changed\n").unwrap();
    exits(
        &top,
        &["hook", "commit-msg", "--project", "sub", "good.txt"],
        0,
    );
    std::fs::write(top.join("bad.txt"), format!("{}\n", "x".repeat(200))).unwrap();
    let bad = exits(
        &top,
        &["hook", "commit-msg", "--project", "sub", "bad.txt"],
        1,
    );
    says(&bad.err, "subject");
    // A project that is not there is a hook chock could not run, not one that passed.
    let gone = exits(
        &top,
        &["hook", "commit-msg", "--project", "gone", "good.txt"],
        2,
    );
    says(&gone.err, "no Cargo.toml there");
}

/// `chock message` is what the commit-msg hook runs, so the rule must also hold by hand.
#[test]
fn a_commit_message_is_checked_against_the_projects_own_limits() {
    let dir = project("message-by-hand", &[("src/lib.rs", "pub fn f() {}\n")]);
    std::fs::write(dir.join("good.txt"), "A subject that says what changed\n").unwrap();
    exits(&dir, &["message", "good.txt"], 0);
    let over = "x".repeat(200);
    std::fs::write(dir.join("bad.txt"), format!("{over}\n")).unwrap();
    let bad = exits(&dir, &["message", "bad.txt"], 1);
    says(&bad.err, "subject");
    // A file that is not there is a hook chock could not run, not a message that passed.
    let missing = exits(&dir, &["message", "nowhere.txt"], 2);
    says(&missing.err, "cannot read nowhere.txt");
}

const WORKSPACE: &str = "[workspace]\nmembers = [\"one\", \"two\"]\nresolver = \"2\"\n";

const MEMBER: &str = "[package]\nname = \"NAME\"\nversion = \"0.0.0\"\nedition = \"2021\"\n";

/// `wiring` reports a check that passes here but is off; after `init` there must be none.
#[test]
fn after_init_the_wiring_gate_passes_on_every_shape_of_project() {
    for shape in ["single", "workspace", "no-tests", "nested-sources"] {
        let dir = scratch(&format!("init-shape-{shape}"));
        match shape {
            "workspace" => {
                put(&dir, "Cargo.toml", WORKSPACE);
                for member in ["one", "two"] {
                    put(
                        &dir,
                        &format!("{member}/Cargo.toml"),
                        &MEMBER.replace("NAME", member),
                    );
                    put(
                        &dir,
                        &format!("{member}/src/lib.rs"),
                        "pub fn f() -> u32 {\n    1\n}\n",
                    );
                }
            }
            "nested-sources" => {
                put(&dir, "Cargo.toml", MANIFEST);
                put(&dir, "src/lib.rs", "pub mod deep;\n");
                put(&dir, "src/deep/mod.rs", "pub fn f() -> u32 {\n    1\n}\n");
            }
            _ => {
                put(&dir, "Cargo.toml", MANIFEST);
                put(&dir, "src/lib.rs", "pub fn f() -> u32 {\n    1\n}\n");
            }
        }
        assert!(git(&dir, &["init", "-q"]).status.success(), "{shape}");
        git(&dir, &["config", "user.email", "t@e"]);
        git(&dir, &["config", "user.name", "t"]);
        git(&dir, &["add", "-A"]);
        git(
            &dir,
            &["commit", "-q", "-m", "A first commit so a history exists"],
        );

        let init = chock(&dir, &["init", "--local", "--fast"]);
        assert_eq!(init.code, 0, "{shape}: {}{}", init.out, init.err);
        // The invariant: whatever init chose, nothing that would pass here is left switched off.
        let wired = chock(&dir, &["run", "wiring"]);
        assert_eq!(
            wired.code, 0,
            "{shape}: init left a gate off\n{}{}",
            wired.out, wired.err
        );
    }
}

/// A config an older chock wrote lacks the gates added since. `wiring` refuses a passing gate left
/// off, so init run again switches those on rather than the next commit refusing.
#[test]
fn init_run_again_switches_on_a_gate_newer_than_the_config_that_wiring_asks_for() {
    let dir = project("init-newer-gate", &[("src/lib.rs", "pub fn f() {}\n")]);
    assert!(git(&dir, &["init", "-q"]).status.success());
    git(&dir, &["config", "user.email", "t@e"]);
    git(&dir, &["config", "user.name", "t"]);
    git(&dir, &["add", "-A"]);
    git(
        &dir,
        &["commit", "-q", "-m", "A first commit so a history exists"],
    );
    exits(&dir, &["init", "--local", "--fast"], 0);
    let path = dir.join(chock::project::config::FILE);
    let mut config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    config["enabled"]
        .as_array_mut()
        .unwrap()
        .retain(|gate| gate != "lean" && gate != "splits");
    std::fs::write(&path, config.to_string()).unwrap();

    let again = exits(&dir, &["init", "--local", "--fast"], 0);
    says(&again.out, "switched on lean splits");
    exits(&dir, &["run", "wiring"], 0);
}

#[test]
fn init_keeps_a_broken_core_check_enabled_and_the_next_run_reports_it() {
    let dir = project("init-broken-module", &[("src/lib.rs", "mod absent;\n")]);
    let init = exits(&dir, &["init", "--local", "--fast"], 0);
    says(&init.out, "a required gate, and it trips today:");
    says(&init.out, "On does not mean passed");
    let config: chock::project::config::Config = serde_json::from_str(
        &std::fs::read_to_string(dir.join(chock::project::config::FILE)).unwrap(),
    )
    .unwrap();
    assert!(config.is_on("modcheck"));
    let run = exits(&dir, &["run", "modcheck", "--json"], 1);
    let report = parsed(&run.out);
    assert_eq!(report.gates[0].gate, "modcheck");
    assert_eq!(report.gates[0].verdict, Verdict::Tripped);
    assert_eq!(report.gates[0].findings[0].item.as_deref(), Some("absent"));
    assert!(
        report.gates[0].findings[0]
            .message
            .contains("names no file")
    );
}

#[test]
fn init_retains_unreadable_source_checks_without_turning_on_opt_in_tools() {
    let dir = project(
        "init-unreadable-source",
        &[("src/lib.rs", "fn broken( {\n")],
    );
    let init = exits(&dir, &["init", "--local", "--fast"], 0);
    says(&init.out, "on, error modcheck");
    says(&init.out, "on, error source");
    let config: chock::project::config::Config = serde_json::from_str(
        &std::fs::read_to_string(dir.join(chock::project::config::FILE)).unwrap(),
    )
    .unwrap();
    assert!(config.is_on("modcheck"));
    assert!(config.is_on("source"));
    assert!(!config.is_on("mutest"));
    assert!(!config.is_on("bsize"));
    assert!(
        !config.is_on("test"),
        "--fast remains an explicit scope choice"
    );
    let run = exits(&dir, &["run", "modcheck", "--json"], 2);
    let report = parsed(&run.out);
    assert_eq!(report.gates[0].verdict, Verdict::CannotRun);
    let why = report.gates[0].cannot_run_reason.as_deref().unwrap();
    assert!(why.contains("src/lib.rs"), "{why}");
}

/// A controlled child replaces Cargo for these tests. PATH is isolated in that child, so the installed
/// mutation tool is never invoked and test output cannot be mistaken for a live measurement.
#[cfg(unix)]
fn mutation_tool(dir: &Path, args: &[&str], exit: &str, failed: bool) -> Ran {
    let cargo = dir.join("bin/cargo");
    put(
        dir,
        "bin/cargo",
        r#"#!/bin/sh
case "$*" in
  --version) printf 'cargo 1.98.1\n'; exit 0 ;;
  'mutest --version') printf 'cargo-mutest fixture\n'; exit 0 ;;
  'mutest run --help') printf '      --require-progress\n'; exit 0 ;;
  'mutest run --call-graph-depth-limit 3 --isolate all --parallel-mutants --metadata-out-root-dir='*) ;;
  'metadata --no-deps --format-version 1') printf '%s\n' '{"workspace_default_members":["fixture"],"packages":[{"id":"fixture","name":"fixture","targets":[{"name":"fixture","kind":["lib"],"test":true},{"name":"commands","kind":["test"],"test":true}]}]}'; exit 0 ;;
  *) printf 'unexpected invocation\n' >&2; exit 1 ;;
esac
for arg; do
  case $arg in --metadata-out-root-dir=*) results=${arg#*=} ;; esac
done
if [ -n "$MUTEST_PROGRESS_DIR" ]; then
  umask 077
  instance=0123456789abcdef0123456789abcdef
  file="$MUTEST_PROGRESS_DIR/$MUTEST_PROGRESS_NONCE-$$-$instance.jsonl"
  IFS= read -r status < /proc/$$/stat
  exe=$(/usr/bin/readlink /proc/$$/exe)
  status=${status##*) }
  set -- $status
  shift 19
  ticks=$1
  prefix="\"schema\":\"mutest-progress\",\"version\":1,\"nonce\":\"$MUTEST_PROGRESS_NONCE\",\"instance_id\":\"$instance\""
  printf '{%s,"event":"header","seq":0,"elapsed_ms":0,"pid":%s,"process_start":{"kind":"linux-proc-starttime","ticks":%s},"exe":"%s","record_limit_bytes":16384,"file_limit_bytes":67108864}\n' "$prefix" "$$" "$ticks" "$exe" > "$file"
  printf '{%s,"event":"phase","seq":1,"elapsed_ms":0,"phase":"evaluation"}\n' "$prefix" >> "$file"
fi
printf '%s\n' 'warning: integration test `commands` links no mutant crate, so no mutation can reach its tests' >&2
/bin/mkdir -p "$results/fixture/lib"
printf '%s\n' '{"mutations":[{"mutation_id":1,"mutation_op":"eq_op_invert","display_name":"flips a comparison","origin_span":{"path":"src/lib.rs","begin":[1,1],"end":[1,5]}}]}' > "$results/fixture/lib/mutations.json"
printf '%s\n' '{"mutation_runs":[{"mutation_detection_matrix":{"overall_detections":"-"}}]}' > "$results/fixture/lib/evaluation.json"
printf '%s\n' 'mutations: 90%. 9 detected (0 timed out; 0 crashed); 1 undetected; 10 total'
if [ "$FIXTURE_FAILED" = yes ]; then
  printf '%s\n' 'error: the test harness of `launch` in package `fixture` did not build, so none of its mutations were evaluated' >&2
fi
# mutest writes the terminal event last; chock stops reading at one that reports an abnormal exit.
if [ -n "$MUTEST_PROGRESS_DIR" ]; then
  printf '{%s,"event":"terminal","seq":2,"elapsed_ms":0,"status":"completed","exit_code":%s}\n' "$prefix" "$FIXTURE_EXIT" >> "$file"
fi
exit "$FIXTURE_EXIT"
"#,
    );
    make_runnable(&cargo);
    with_tools(
        dir,
        args,
        &[
            ("FIXTURE_EXIT", exit),
            ("FIXTURE_FAILED", if failed { "yes" } else { "no" }),
        ],
    )
}

#[cfg(unix)]
fn with_tools(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Ran {
    ran(Command::new(env!("CARGO_BIN_EXE_chock"))
        .args(args)
        .current_dir(dir)
        .env("PATH", dir.join("bin"))
        .envs(env.iter().copied()))
}

#[cfg(unix)]
fn coverage_project(name: &str) -> Scratch {
    let dir = project(name, &[("src/lib.rs", "pub fn f() {}\n")]);
    put(
        &dir,
        "bin/coverage",
        r#"#!/bin/sh
printf 'coverage\n' >> calls
if [ "$FIXTURE_FAILED" = yes ]; then
  printf 'FAIL [0.001s] fixture fails_to_measure\n'
  printf 'error: test run failed\n' >&2
  exit 1
fi
printf 'SF:src/lib.rs\nDA:1,%s\nend_of_record\n' "$FIXTURE_HITS" > "$1"
"#,
    );
    put(
        &dir,
        "bin/cargo",
        r#"#!/bin/sh
nothing='{"entries":[]}'
case "$*" in
  --version | 'crap --version') printf 'cargo fixture\n';;
  'crap --lcov lcov.info --workspace --exclude build.rs --format json --sort file' | 'crap --lcov lcov.info --workspace --exclude build.rs --format json --baseline .chock/crap-baseline'*'.json --fail-regression')
    printf 'crap\n' >> calls
    printf '%s\n' "${FIXTURE_CRAP:-$nothing}";;
  *) printf 'unexpected invocation\n' >&2; exit 1;;
esac
"#,
    );
    make_runnable(&dir.join("bin/coverage"));
    make_runnable(&dir.join("bin/cargo"));
    let config = chock::project::config::Config::of(["coverage", "crap"])
        .with_coverage(["bin/coverage", "{lcov}"]);
    put(&dir, chock::project::config::FILE, &config.render());
    put(
        &dir,
        &chock::gates::coverage::crap::baseline(),
        r#"{"entries":[]}"#,
    );
    let mut baseline = chock::run::baseline::Baseline::empty(VERSION);
    let mut series = chock::run::baseline::Series::new();
    series.set("src/lib.rs", 1);
    baseline.record("coverage", "uncovered line(s)", series);
    put(&dir, chock::run::baseline::FILE, &baseline.render());
    dir
}

#[cfg(unix)]
#[test]
fn coverage_and_crap_share_only_the_report_from_their_own_invocation() {
    let dir = coverage_project("coverage-shared");
    let checks = [
        ["run", "coverage", "crap", "--json"],
        ["run", "crap", "coverage", "--json"],
    ];
    let mut expected = String::new();
    for args in checks {
        let result = with_tools(&dir, &args, &[("FIXTURE_HITS", "0")]);
        assert_eq!(result.code, 0, "{}{}", result.out, result.err);
        let report = parsed(&result.out);
        assert_eq!(gate_of(&report, "coverage").measured, Some(1));
        assert_eq!(gate_of(&report, "crap").verdict, Verdict::Pass);
        expected.push_str("coverage\ncrap\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("calls")).unwrap(),
            expected
        );
    }
    let recorded = with_tools(
        &dir,
        &["baseline", "coverage", "crap"],
        &[("FIXTURE_HITS", "1")],
    );
    assert_eq!(recorded.code, 0, "{}{}", recorded.out, recorded.err);
    expected.push_str("coverage\ncrap\n");
    assert_eq!(
        std::fs::read_to_string(dir.join("calls")).unwrap(),
        expected
    );
}

#[cfg(unix)]
#[test]
fn baseline_crap_keeps_a_higher_score_on_record_until_lower_asks_for_the_measured_one() {
    let dir = coverage_project("crap-lower");
    let record = dir.join(chock::gates::coverage::crap::baseline());
    let scored = |crap: f64| {
        serde_json::json!({"entries": [{"file": "src/lib.rs", "function": "f", "line": 1, "crap": crap}]})
            .to_string()
    };
    std::fs::write(&record, scored(20.0)).unwrap();
    let measured = scored(6.0);
    let env = [("FIXTURE_HITS", "1"), ("FIXTURE_CRAP", measured.as_str())];
    let on_record = || {
        let held = std::fs::read_to_string(&record).unwrap();
        let held: serde_json::Value = serde_json::from_str(&held).unwrap();
        held["entries"][0]["crap"].as_f64()
    };
    let kept = with_tools(&dir, &["baseline", "crap"], &env);
    assert_eq!(kept.code, 0, "{}{}", kept.out, kept.err);
    assert!(kept.out.contains("  recorded  crap "), "{}", kept.out);
    assert_eq!(on_record(), Some(20.0));
    let lowered = with_tools(&dir, &["baseline", "--lower", "crap"], &env);
    assert_eq!(lowered.code, 0, "{}{}", lowered.out, lowered.err);
    assert!(lowered.out.contains("  lowered   crap "), "{}", lowered.out);
    assert_eq!(on_record(), Some(6.0));
}

#[cfg(unix)]
#[test]
fn a_local_run_with_no_crap_record_writes_the_first_one_from_the_scores_of_today() {
    let dir = coverage_project("crap-first-record");
    let record = dir.join(chock::gates::coverage::crap::baseline());
    std::fs::remove_file(&record).unwrap();
    let measured = r#"{"entries":[{"file":"src/lib.rs","function":"f","line":1,"crap":6.0}]}"#;
    let env = [("FIXTURE_HITS", "0"), ("FIXTURE_CRAP", measured)];
    let first = with_tools(&dir, &["run", "coverage", "crap"], &env);
    assert_eq!(first.code, 0, "{}{}", first.out, first.err);
    says(
        &first.err,
        "chock: wrote the first record for crap; commit ",
    );
    let held = std::fs::read_to_string(&record).unwrap();
    let held: serde_json::Value = serde_json::from_str(&held).unwrap();
    assert_eq!(held["entries"][0]["crap"].as_f64(), Some(6.0));
}

#[test]
fn a_local_run_adopts_orphan_debt_without_weakening_missing_module_checks() {
    let dir = project(
        "adopt-orphans",
        &[
            ("src/lib.rs", "mod missing;\n"),
            ("src/orphan.rs", "pub fn old() {}\n"),
        ],
    );
    let config = chock::project::config::Config::of(["modcheck"]);
    put(&dir, chock::project::config::FILE, &config.render());
    // A module that names no file is no debt to record, so it fails with no record written.
    let unresolved = exits(&dir, &["run", "modcheck", "--json"], 1);
    says(&unresolved.out, "names no file");
    assert!(!dir.join(chock::run::baseline::FILE).exists());
    put(&dir, "src/lib.rs", "pub fn f() {}\n");
    // CI writes no record, so there the orphan fails.
    exits(&dir, &["run", "modcheck", "--ci", "--json"], 1);
    assert!(!dir.join(chock::run::baseline::FILE).exists());
    let first = exits(&dir, &["run", "modcheck", "--json"], 0);
    says(&first.err, "chock: wrote the first record for modcheck");
    let baseline = std::fs::read_to_string(dir.join(chock::run::baseline::FILE)).unwrap();
    let adopted = exits(&dir, &["run", "modcheck", "--json"], 0);
    assert_eq!(gate_of(&parsed(&adopted.out), "modcheck").measured, Some(1));
    put(&dir, "src/new.rs", "pub fn new() {}\n");
    let new = exits(&dir, &["run", "modcheck", "--json"], 1);
    says(&new.out, "src/new.rs");
    put(&dir, "src/lib.rs", "mod missing;\n");
    let broken = exits(&dir, &["run", "modcheck", "--json"], 1);
    says(&broken.out, "names no file");
    let refused = exits(&dir, &["baseline", "modcheck"], 2);
    says(
        &refused.err,
        "cannot baseline unresolved correctness failures",
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(chock::run::baseline::FILE)).unwrap(),
        baseline
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(chock::project::config::FILE)).unwrap(),
        config.render()
    );
}

#[test]
fn feature_debt_can_be_adopted_but_a_new_feature_issue_cannot_hide_in_it() {
    let dir = project("adopt-features", &[("src/lib.rs", "pub fn f() {}\n")]);
    put(
        &dir,
        "Cargo.toml",
        &format!("{MANIFEST}\n[workspace]\n[features]\nstale = []\n"),
    );
    turn_on(&dir, &["features"]);
    exits(&dir, &["run", "features", "--ci", "--json"], 1);
    exits(&dir, &["baseline", "features"], 0);
    exits(&dir, &["run", "features", "--json"], 0);
    put(
        &dir,
        "src/lib.rs",
        "#[cfg(feature = \"missing\")] pub fn f() {}\n",
    );
    let grew = exits(&dir, &["run", "features", "--json"], 1);
    says(&grew.out, "missing");
}

#[test]
fn an_explicit_gate_filtered_out_never_becomes_an_empty_or_partial_success() {
    let dir = project("filtered-explicit", &[("src/lib.rs", "pub fn f() {}\n")]);
    for args in [
        vec!["run", "--fast", "lint", "codeslop", "--json"],
        vec!["run", "--fast", "lint", "manifest", "--json"],
    ] {
        let result = exits(&dir, &args, 2);
        says(&result.err, "leaves out what you named");
        says(&result.err, "lint");
        assert_eq!(result.out, "");
        assert!(!dir.join(".chock/last-run.json").exists());
    }
}

#[test]
fn an_empty_user_run_is_not_the_same_as_a_hook_with_no_assigned_checks() {
    let dir = project("empty-selection", &[("src/lib.rs", "pub fn f() {}\n")]);
    turn_on(&dir, &["test"]);
    let run = exits(&dir, &["run", "--fast", "--json"], 2);
    says(&run.err, "no gate is left after --fast/--ci");
    assert_eq!(run.out, "");
    let hook = exits(&dir, &["hook", "pre-commit"], 0);
    says(&hook.err, "nothing was measured");
    assert!(!dir.join(".chock/last-run.json").exists());
    let valid = exits(&dir, &["run", "--fast", "modcheck", "--json"], 0);
    assert_eq!(
        gate_of(&parsed(&valid.out), "modcheck").verdict,
        Verdict::Pass
    );
}

#[test]
fn ci_cannot_silently_exclude_an_explicit_local_only_gate() {
    let dir = project("ci-explicit", &[("src/lib.rs", "pub fn f() {}\n")]);
    let mut config = chock::project::config::Config::of(["manifest"]);
    config.local_only = Some(vec!["manifest".to_string()]);
    put(&dir, chock::project::config::FILE, &config.render());
    let result = exits(&dir, &["run", "--ci", "manifest", "--json"], 2);
    says(&result.err, "manifest");
    says(&result.err, "leaves out what you named");
}

#[cfg(unix)]
#[test]
fn proof_requires_every_listed_harness_even_after_an_exit_zero() {
    let dir = project("proof-census", &[("src/lib.rs", "pub fn f() {}\n")]);
    put(
        &dir,
        "bin/cargo",
        r#"#!/bin/sh
case "$*" in
  --version) printf 'cargo fixture\n';;
  'kani --workspace --features testkit list --format json')
    printf '%s\n' '{"standard-harnesses":{"src/lib.rs":["proofs::first","proofs::second"]},"contract-harnesses":{}}' > kani-list.json;;
  'kani --workspace --features testkit')
    printf 'Checking harness proofs::first...\n ** 0 of 2 failed\n';;
  *) printf 'unexpected invocation: %s\n' "$*" >&2; exit 1;;
esac
"#,
    );
    put(&dir, "bin/kani", "#!/bin/sh\nprintf 'kani fixture\\n'\n");
    make_runnable(&dir.join("bin/cargo"));
    make_runnable(&dir.join("bin/kani"));
    let config = chock::project::config::Config {
        features: Some(vec!["testkit".to_string()]),
        ..chock::project::config::Config::of(["proof"])
    };
    put(&dir, chock::project::config::FILE, &config.render());
    let result = with_tools(&dir, &["run", "proof", "--json"], &[]);
    assert_eq!(result.code, 2, "{}{}", result.out, result.err);
    let report = parsed(&result.out);
    assert_eq!(gate_of(&report, "proof").verdict, Verdict::CannotRun);
    says(
        gate_of(&report, "proof")
            .cannot_run_reason
            .as_deref()
            .unwrap(),
        "proofs::second",
    );
}

#[cfg(unix)]
#[test]
fn external_tool_wrappers_preserve_scope_and_execution_failures() {
    let dir = project("tool-wrappers", &[]);
    put(
        &dir,
        "bin/cargo",
        r#"#!/bin/sh
case "$*" in
  --version | 'acl --version') printf 'cargo fixture\n';;
  'metadata --no-deps --format-version 1')
    printf '{"workspace_root":"%s","workspace_members":["private"],"packages":[{"id":"private","name":"private","manifest_path":"%s/Cargo.toml","publish":[],"targets":[{"name":"app","kind":["bin"]}]}]}\n' "$PWD" "$PWD";;
  'bsize --bin app' | 'acl --no-ui --quiet --fail-on-warnings --features testkit' | 'doc --no-deps --workspace --features testkit')
    printf 'measured\n'
    if [ "$FIXTURE_FAILED" = yes ]; then printf 'error: fixture could not inspect\n' >&2; exit 1; fi;;
  *) printf 'unexpected invocation: %s\n' "$*" >&2; exit 1;;
esac
"#,
    );
    make_runnable(&dir.join("bin/cargo"));
    turn_on(&dir, &["bsize"]);
    for (failed, verdict, code) in [("no", Verdict::Pass, 0), ("yes", Verdict::CannotRun, 2)] {
        let result = with_tools(
            &dir,
            &["run", "bsize", "--json"],
            &[("FIXTURE_FAILED", failed)],
        );
        assert_eq!(result.code, code, "{}{}", result.out, result.err);
        assert_eq!(gate_of(&parsed(&result.out), "bsize").verdict, verdict);
    }
    let mut config = chock::project::config::Config::of(["acl"]);
    config.features = Some(vec!["testkit".to_string()]);
    put(&dir, chock::project::config::FILE, &config.render());
    let acl = with_tools(&dir, &["run", "acl", "--json"], &[]);
    if cfg!(target_os = "linux") {
        assert_eq!(acl.code, 0, "{}{}", acl.out, acl.err);
    } else {
        assert_eq!(acl.code, 2, "{}", acl.err);
        says(&acl.err, "this system leaves out what you named: acl;");
    }
    let result = with_tools(&dir, &["run", "doc", "--json"], &[]);
    assert_eq!(result.code, 0, "{}{}", result.out, result.err);
    let failed = with_tools(
        &dir,
        &["run", "doc", "--json"],
        &[("FIXTURE_FAILED", "yes")],
    );
    assert_eq!(failed.code, 2, "{}{}", failed.out, failed.err);
    config.all_features = Some(true);
    put(&dir, chock::project::config::FILE, &config.render());
    let result = with_tools(&dir, &["run", "acl", "--json"], &[]);
    assert_eq!(result.code, 2, "{}{}", result.out, result.err);
    if cfg!(target_os = "linux") {
        says(&result.out, "cannot honor");
    }
    let result = with_tools(&dir, &["run", "bsize", "--json"], &[]);
    assert_eq!(result.code, 2, "{}{}", result.out, result.err);
    says(&result.out, "no feature flags");
}

#[cfg(unix)]
#[test]
fn doctor_checks_a_missing_pinned_toolchain_without_installing_it() {
    let dir = project("missing-nightly", &[]);
    put(
        &dir,
        "tool-versions.env",
        "MUTEST_NIGHTLY=nightly-2026-09-26\n",
    );
    put(
        &dir,
        "bin/rustup",
        r#"#!/bin/sh
if [ "$*" != 'toolchain list' ]; then exit 7; fi
printf 'stable-x86_64-unknown-linux-gnu (default)\n'
"#,
    );
    make_runnable(&dir.join("bin/rustup"));
    let result = with_tools(&dir, &["doctor", "--json"], &[]);
    assert_eq!(result.code, 1, "{}{}", result.out, result.err);
    says(&result.out, "nightly-2026-09-26");
    says(&result.out, "not installed");
    put(
        &dir,
        "bin/rustup",
        "#!/bin/sh\nprintf 'unrecognized output\\n'\n",
    );
    let unreadable = with_tools(&dir, &["doctor", "--json"], &[]);
    assert_eq!(unreadable.code, 2, "{}{}", unreadable.out, unreadable.err);
    says(&unreadable.out, "cannot_run_reason");
}

/// A cargo that logs what it was asked: `binstall -V` and a binstall fetch exit as the test says.
#[cfg(unix)]
const LOGGING_CARGO: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> cargo.log
case "$*" in
  "binstall -V") exit "$BINSTALL_HERE" ;;
  binstall*) exit "$BINSTALL_FETCHES" ;;
esac
"#;

#[cfg(unix)]
#[test]
fn init_global_takes_a_prebuilt_release_through_binstall_and_compiles_when_it_cannot() {
    let fetched = "binstall -y --locked --disable-strategies quick-install cargo-deny@0.20.2";
    let compiled = "install cargo-deny --version 0.20.2 --locked";
    for (here, fetching, asked) in [
        ("0", "0", vec![fetched]),
        ("0", "1", vec![fetched, compiled]),
        ("1", "0", vec![compiled]),
    ] {
        let dir = project("init-binstall", &[]);
        put(&dir, "tool-versions.env", "CARGO_DENY_VERSION=0.20.2\n");
        put(&dir, "bin/cargo", LOGGING_CARGO);
        make_runnable(&dir.join("bin/cargo"));
        let env = [("BINSTALL_HERE", here), ("BINSTALL_FETCHES", fetching)];
        let ran = with_tools(&dir, &["init", "--global"], &env);
        assert_eq!(ran.code, 0, "{}{}", ran.out, ran.err);
        says(&ran.out, "  installing cargo-deny 0.20.2 — ");
        says(&ran.out, "  installed cargo-deny 0.20.2");
        // No rustup on this `PATH`: Miri is skipped, and that is no failed install.
        says(
            &ran.out,
            "  skipped   miri — only rustup installs it, and chock could not start rustup: ",
        );
        let log = std::fs::read_to_string(dir.join("cargo.log")).unwrap();
        let fetches: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("cargo-deny"))
            .collect();
        assert_eq!(fetches, asked, "binstall here {here}, fetching {fetching}");
    }
}

/// A rustup whose `nightly` holds what `RUSTUP_HAS` names, and that installs where `RUSTUP_ADDS` is 0.
#[cfg(unix)]
const LOGGING_RUSTUP: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> rustup.log
case "$1 $2 $RUSTUP_HAS" in
  "component list none") echo "error: toolchain 'nightly' is not installed" >&2; exit 1 ;;
  "component list both") printf 'miri-x86_64-unknown-linux-gnu\nrust-src\n'; exit 0 ;;
  "component list part") printf 'rust-src\n'; exit 0 ;;
esac
[ "$RUSTUP_ADDS" = 0 ] || echo 'error: no download' >&2
exit "$RUSTUP_ADDS"
"#;

#[cfg(unix)]
#[test]
fn init_global_adds_miri_to_the_nightly_toolchain_and_fails_where_rustup_refuses() {
    let listed = "component list --toolchain nightly --installed";
    let toolchain = "toolchain install nightly --profile minimal --no-self-update";
    let parts = "component add --toolchain nightly miri rust-src";
    let installed = "  installed miri — on the `nightly` toolchain";
    let refused = format!("  FAILED    miri — `rustup {toolchain}` failed: error: no download");
    for (has, adds, code, line, asked) in [
        (
            "both",
            "1",
            0,
            "  current   miri — on the `nightly` toolchain",
            vec![listed],
        ),
        ("part", "0", 0, installed, vec![listed, parts]),
        ("none", "0", 0, installed, vec![listed, toolchain, parts]),
        ("none", "1", 1, refused.as_str(), vec![listed, toolchain]),
    ] {
        let dir = project("init-miri", &[("tool-versions.env", "")]);
        put(&dir, "bin/rustup", LOGGING_RUSTUP);
        make_runnable(&dir.join("bin/rustup"));
        let env = [("RUSTUP_HAS", has), ("RUSTUP_ADDS", adds)];
        let ran = with_tools(&dir, &["init", "--global"], &env);
        assert_eq!(ran.code, code, "{has} {adds}: {}{}", ran.out, ran.err);
        says(&ran.out, line);
        let log = std::fs::read_to_string(dir.join("rustup.log")).unwrap();
        assert_eq!(log.lines().collect::<Vec<_>>(), asked, "{has} {adds}");
        let failed = "chock init: could not install: miri\n";
        assert_eq!(ran.err, if code == 0 { "" } else { failed }, "{has} {adds}");
    }
}

#[cfg(unix)]
#[test]
fn init_global_builds_the_fork_of_mutest_from_its_repository_and_fails_where_it_cannot() {
    let dir = project("init-fork", &[]);
    put(
        &dir,
        "tool-versions.env",
        "CARGO_MUTEST_VERSION=0.0.0\nOUTPOST_VERSION=0.0.0\n",
    );
    put(&dir, "bin/cargo", LOGGING_CARGO);
    make_runnable(&dir.join("bin/cargo"));
    // No git on this `PATH`, so the newest commit of `main` cannot be asked for.
    let ran = with_tools(&dir, &["init", "--global"], &[]);
    assert_eq!(ran.code, 1, "{}{}", ran.out, ran.err);
    says(
        &ran.out,
        "  checking  cargo-mutest — against `main` at https://github.com/outpostHQ/mutest-rs; a build of a new commit takes minutes\n  FAILED    cargo-mutest — could not start git: ",
    );
    assert!(!ran.out.contains("  local     cargo-mutest"), "{}", ran.out);
    // Any other tool that no registry holds is still the project's own to build.
    says(
        &ran.out,
        "  local     outpost — not on crates.io; build it from its checkout",
    );
    assert_eq!(ran.err, "chock init: could not install: cargo-mutest\n");
    // cargo was not asked to fetch or to build it: the install stopped at the repository.
    let log = std::fs::read_to_string(dir.join("cargo.log")).unwrap();
    assert!(!log.contains("mutest"), "{log}");
}

/// A cargo that has no `mutest` command and no installed crate.
#[cfg(unix)]
const BARE_CARGO: &str = r#"#!/bin/sh
case "$1" in
  mutest) echo 'error: no such command: `mutest`' >&2; exit 101 ;;
esac
"#;

#[cfg(unix)]
#[test]
fn doctor_names_a_missing_fork_of_mutest_and_a_missing_miri_with_the_install_for_each() {
    let dir = project(
        "doctor-fork",
        &[("tool-versions.env", "CARGO_MUTEST_VERSION=0.0.0\n")],
    );
    put(
        &dir,
        ".chock/config.json",
        r#"{"version": 1, "enabled": ["miri"]}"#,
    );
    for (tool, script) in [("bin/cargo", BARE_CARGO), ("bin/rustup", LOGGING_RUSTUP)] {
        put(&dir, tool, script);
        make_runnable(&dir.join(tool));
    }
    let ran = with_tools(&dir, &["doctor"], &[("RUSTUP_HAS", "part")]);
    assert_eq!(ran.code, 1, "{}{}", ran.out, ran.err);
    let row = |name: &str, said: &str| format!("  BROKEN   {name:<16} {said}\n");
    says(
        &ran.out,
        &row("cargo-mutest", "not installed: run `chock init --global`"),
    );
    says(
        &ran.out,
        &row(
            "miri",
            "the `nightly` toolchain lacks `miri` or `rust-src`: run `chock init --global`",
        ),
    );
    assert!(
        !ran.out.contains("build it from its checkout"),
        "{}",
        ran.out
    );
    // The same project with the check off has no row for Miri, and starts no rustup.
    put(
        &dir,
        ".chock/config.json",
        r#"{"version": 1, "enabled": ["slop"]}"#,
    );
    std::fs::remove_file(dir.join("rustup.log")).unwrap();
    let off = with_tools(&dir, &["doctor"], &[("RUSTUP_HAS", "part")]);
    assert!(!off.out.contains(" miri "), "{}", off.out);
    assert!(!dir.join("rustup.log").exists());
}

#[cfg(unix)]
#[test]
fn doctor_names_an_unstartable_declared_hook_without_executing_it() {
    let dir = project("missing-declared-hook", &[]);
    put(&dir, ".git/HEAD", "ref: refs/heads/main\n");
    put(&dir, "tool-versions.env", "");
    put(
        &dir,
        "bin/git",
        r#"#!/bin/sh
case "$*" in
  --version) printf 'git version 2.55.0\n';;
  'config --null --get-regexp '* )
    printf 'hook.chock-pre-commit.event\npre-commit\000hook.chock-pre-commit.command\n.chock/hooks/pre-commit\000';;
  *) exit 1;;
esac
"#,
    );
    make_runnable(&dir.join("bin/git"));
    let result = with_tools(&dir, &["doctor", "--json"], &[]);
    assert_eq!(result.code, 1, "{}{}", result.out, result.err);
    says(&result.out, "chock-pre-commit");
    says(&result.out, "missing or not executable");
}

#[cfg(unix)]
#[test]
fn configured_features_reach_default_test_and_coverage_processes() {
    let dir = coverage_project("default-features");
    let config = chock::project::config::Config {
        features: Some(vec!["testkit".to_string()]),
        no_default_features: Some(true),
        ..chock::project::config::Config::of(["test", "coverage"])
    };
    put(&dir, chock::project::config::FILE, &config.render());
    put(
        &dir,
        "bin/cargo",
        r#"#!/bin/sh
case "$*" in
  --version | 'nextest --version') printf 'cargo fixture\n';;
  'nextest run --workspace --no-tests=fail --no-fail-fast --no-default-features --features testkit')
    printf 'test-features\n' >> calls;;
  'llvm-cov nextest --workspace --no-tests=pass --lcov --output-path lcov.info --no-default-features --features testkit')
    printf 'coverage-features\n' >> calls
    printf 'SF:src/lib.rs\nDA:1,0\nend_of_record\n' > lcov.info;;
  *) printf 'unexpected invocation: %s\n' "$*" >&2; exit 1;;
esac
"#,
    );
    let result = with_tools(&dir, &["run", "test", "coverage", "--json"], &[]);
    assert_eq!(result.code, 0, "{}{}", result.out, result.err);
    let report = parsed(&result.out);
    assert_eq!(gate_of(&report, "test").verdict, Verdict::Pass);
    assert_eq!(gate_of(&report, "coverage").measured, Some(1));
    assert_eq!(
        std::fs::read_to_string(dir.join("calls")).unwrap(),
        "test-features\ncoverage-features\n"
    );
}

#[cfg(unix)]
#[test]
fn changed_external_coverage_inputs_cannot_reuse_a_previous_report() {
    let dir = coverage_project("coverage-external");
    for (hits, missed) in [("1", 0), ("0", 1)] {
        let result = with_tools(
            &dir,
            &["run", "coverage", "--json"],
            &[("FIXTURE_HITS", hits)],
        );
        assert_eq!(result.code, 0, "{}{}", result.out, result.err);
        assert_eq!(
            gate_of(&parsed(&result.out), "coverage").measured,
            Some(missed)
        );
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("calls")).unwrap(),
        "coverage\ncoverage\n"
    );
}

#[cfg(unix)]
#[test]
fn timeout_diagnostics_survive_the_run_record_and_explain() {
    let dir = coverage_project("coverage-timeout");
    put(
        &dir,
        "bin/coverage",
        r#"#!/bin/sh
printf 'fixture is waiting for its input\n'
printf 'error: fixture cannot continue\n' >&2
while :; do :; done
"#,
    );
    let failed = with_tools(
        &dir,
        &["run", "coverage", "--json"],
        &[("CHOCK_TIMEOUT", "1")],
    );
    assert_eq!(failed.code, 2, "{}{}", failed.out, failed.err);
    let report = parsed(&failed.out);
    let why = gate_of(&report, "coverage")
        .cannot_run_reason
        .as_deref()
        .unwrap();
    says(why, "still running after 1s");
    says(why, "fixture is waiting for its input");
    says(why, "fixture cannot continue");
    let explained = with_tools(&dir, &["explain", "coverage"], &[]);
    says(&explained.out, "fixture cannot continue");
    // The reason stands in the gate's own line; nothing prints it a second time.
    let once = why.trim_end();
    assert_eq!(explained.out.matches(once).collect::<Vec<_>>(), [once]);
}

#[cfg(unix)]
#[test]
fn failed_coverage_is_not_retried_by_crap_or_recorded_as_a_new_baseline() {
    let dir = coverage_project("coverage-failed");
    let prior = std::fs::read_to_string(dir.join(chock::run::baseline::FILE)).unwrap();
    let env = [("FIXTURE_FAILED", "yes")];
    let failed = with_tools(&dir, &["run", "coverage", "crap", "--json"], &env);
    assert_eq!(failed.code, 2, "{}{}", failed.out, failed.err);
    let report = parsed(&failed.out);
    for name in ["coverage", "crap"] {
        assert_eq!(gate_of(&report, name).verdict, Verdict::CannotRun);
        assert_eq!(gate_of(&report, name).measured, None);
        says(
            gate_of(&report, name).cannot_run_reason.as_deref().unwrap(),
            "fails_to_measure",
        );
    }
    let explained = with_tools(&dir, &["explain", "coverage"], &env);
    says(&explained.out, "fails_to_measure");
    assert_eq!(
        std::fs::read_to_string(dir.join("calls")).unwrap(),
        "coverage\n"
    );
    let recorded = with_tools(&dir, &["baseline", "coverage", "crap"], &env);
    assert_eq!(recorded.code, 2, "{}{}", recorded.out, recorded.err);
    assert_eq!(
        std::fs::read_to_string(dir.join("calls")).unwrap(),
        "coverage\ncoverage\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(chock::run::baseline::FILE)).unwrap(),
        prior
    );
}

#[cfg(unix)]
#[test]
fn mutation_scope_notes_reach_baseline_output_and_run_json() {
    let dir = project("mutation-scope", &[("src/lib.rs", "pub fn f() {}\n")]);
    turn_on(&dir, &["mutest"]);
    let recorded = mutation_tool(&dir, &["baseline", "mutest"], "2", false);
    assert_eq!(recorded.code, 0, "{}{}", recorded.out, recorded.err);
    says(&recorded.err, "integration test commands");
    says(&recorded.err, "not mutation-covered");
    let baseline = std::fs::read_to_string(dir.join(chock::run::baseline::FILE)).unwrap();
    let result = mutation_tool(&dir, &["run", "mutest", "--json"], "2", false);
    assert_eq!(result.code, 0, "{}{}", result.out, result.err);
    let report = parsed(&result.out);
    assert_eq!(report.gates[0].verdict, Verdict::Pass);
    assert_eq!(report.gates[0].measured, Some(1));
    assert_eq!(
        report.gates[0].findings[0].item.as_deref(),
        Some("integration test commands")
    );
    says(&report.gates[0].findings[0].message, "not mutation-covered");
    assert_eq!(
        std::fs::read_to_string(dir.join(chock::run::baseline::FILE)).unwrap(),
        baseline
    );
}

/// Upstream mutest-rs as `cargo mutest`: it prints the fork's version, and its `run` refuses the
/// flag chock passes.
#[cfg(unix)]
const UPSTREAM_MUTEST: &str = r#"#!/bin/sh
case "$*" in
  --version) printf 'cargo 1.98.1\n'; exit 0 ;;
  'mutest run --help') printf 'Usage: cargo mutest run [OPTIONS]\n      --isolate <MODE>\n'; exit 0 ;;
  *) printf "error: unexpected argument '--require-progress' found\n" >&2; exit 2 ;;
esac
"#;

#[cfg(unix)]
#[test]
fn a_cargo_mutest_that_is_not_the_fork_stops_the_check_and_names_the_install() {
    let dir = project("mutation-foreign", &[("src/lib.rs", "pub fn f() {}\n")]);
    turn_on(&dir, &["mutest"]);
    put(&dir, "bin/cargo", UPSTREAM_MUTEST);
    make_runnable(&dir.join("bin/cargo"));
    let install = "run `chock init --global`: it builds `cargo-mutest` from the newest commit";
    let recorded = with_tools(&dir, &["baseline", "mutest"], &[]);
    assert_eq!(recorded.code, 2, "{}{}", recorded.out, recorded.err);
    let said = format!("{}{}", recorded.out, recorded.err);
    says(
        &said,
        "  SKIPPED   mutest       this machine's `cargo-mutest` is not a current build of Outpost's fork",
    );
    says(&said, &format!("\n  fix: {install}"));
    assert!(!said.contains("unexpected argument"), "{said}");
    let result = with_tools(&dir, &["run", "mutest", "--json"], &[]);
    assert_eq!(result.code, 2, "{}{}", result.out, result.err);
    let report = parsed(&result.out);
    assert_eq!(report.gates[0].verdict, Verdict::CannotRun);
    says(report.gates[0].fix.as_deref().unwrap(), install);
}

#[cfg(unix)]
#[test]
fn mutation_target_failure_never_replaces_a_baseline_with_partial_results() {
    let dir = project(
        "mutation-failed-target",
        &[("src/lib.rs", "pub fn f() {}\n")],
    );
    turn_on(&dir, &["mutest"]);
    let mut baseline = chock::run::baseline::Baseline::empty(VERSION);
    let mut series = chock::run::baseline::Series::new();
    series.set("src/lib.rs#eq_op_invert", 7);
    baseline.record("mutest", "mutation(s) no test killed", series.clone());
    put(&dir, chock::run::baseline::FILE, &baseline.render());
    for code in ["0", "4", "101"] {
        let result = mutation_tool(&dir, &["run", "mutest", "--json"], code, true);
        assert_eq!(result.code, 2, "{}{}", result.out, result.err);
        let report = parsed(&result.out);
        assert_eq!(report.gates[0].verdict, Verdict::CannotRun);
        assert_eq!(report.gates[0].measured, None);
        says(
            report.gates[0].cannot_run_reason.as_deref().unwrap(),
            "launch",
        );
        let recorded = mutation_tool(&dir, &["baseline", "mutest"], code, true);
        assert_eq!(recorded.code, 2, "{}{}", recorded.out, recorded.err);
        let after: chock::run::baseline::Baseline = serde_json::from_str(
            &std::fs::read_to_string(dir.join(chock::run::baseline::FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(after.gate("mutest"), series);
    }
}
