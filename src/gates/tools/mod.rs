//! Gates that run an external tool and turn what it says into a pass, a trip, or the reason it
//! could not run.

pub mod binsize;
pub mod codeslop;
mod deps;
mod doc;
pub mod dupdeps;
pub mod machete;
pub mod miri;
pub mod proof;

use std::collections::BTreeSet;
use std::path::Path;

use crate::exec;
use crate::run::baseline::{Keys, Series};
use crate::run::report::Finding;
use crate::run::verdicts::Reads;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

pub const TEST: Gate = Gate {
    name: "test",
    about: "the suite passes, and the crate has one",
    group: Group::Gates,
    builds: true,
    // A project's own runner must name its tools in `runner_tools`, or no verdict is recalled.
    reads: Some(Reads::tree_runner_and(&["cargo"]).versioned("features-v1")),
    kind: Kind::Binary(test),
};

pub const LINT: Gate = Gate {
    name: "lint",
    about: "formatting matches, and clippy is silent",
    group: Group::Gates,
    builds: true,
    // rustfmt and clippy-driver are versioned apart from cargo, so each is named.
    reads: Some(Reads::tree_and(&["cargo", "rustfmt", "clippy-driver"])),
    kind: Kind::Binary(lint),
};

/// Formatting alone, which needs no compiler, so a commit can wait for it; `lint` checks it again.
pub const FMT: Gate = Gate {
    name: "fmt",
    about: "formatting matches rustfmt; it needs no compiler, so a commit can wait for it",
    group: Group::OptIn,
    builds: false,
    reads: Some(Reads::tree_and(&["cargo", "rustfmt"])),
    kind: Kind::Binary(fmt),
};

pub const DOC: Gate = Gate {
    name: "doc",
    about: "the documentation builds with no warnings",
    group: Group::Gates,
    builds: true,
    reads: Some(Reads::tree_and(&["cargo", "rustdoc"]).versioned("targets-v1")),
    kind: Kind::Binary(doc::check),
};

pub const DEPS: Gate = Gate {
    name: "deps",
    about: "no advisory, no disallowed licence, no unknown source",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Binary(deps::check),
};

pub const UNUSED: Gate = Gate {
    name: "unused",
    about: "dependencies nothing references, by source scan, against the count recorded",
    group: Group::Quality,
    builds: false,
    reads: None,
    // A ratchet, since `cargo machete` objects to most existing projects.
    kind: Kind::Ratchet {
        measure: machete::unused,
        keys: Keys::Items,
        unit: "unused dependency(ies)",
    },
};

pub const ACL: Gate = Gate {
    name: "acl",
    about: "no dependency reaches an API its entry in cackle.toml does not grant",
    group: Group::OptIn,
    builds: true,
    // Slow, and its inputs are the tree (policy, manifests, lockfile) and cargo-acl, so recall it.
    reads: Some(Reads::tree_and(&["cargo", "cargo-acl"]).versioned("warnings-v1")),
    kind: Kind::Binary(acl),
};

pub const SORT: Gate = Gate {
    name: "sort",
    about: "manifests whose dependency tables are out of order, against the count recorded",
    group: Group::Quality,
    builds: false,
    reads: None,
    // A ratchet, since `cargo sort --check` objects to most existing projects.
    kind: Kind::Ratchet {
        measure: sort,
        keys: Keys::Items,
        unit: "unsorted manifest(s)",
    },
};

pub const TYPOS: Gate = Gate {
    name: "typos",
    about: "misspellings in source, comments and docs, per file and word",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Ratchet {
        measure: typos,
        keys: Keys::Items,
        unit: "misspelling(s)",
    },
};

pub const MSRV: Gate = Gate {
    name: "msrv",
    about: "the crate still builds on the oldest Rust it claims to support",
    group: Group::Quality,
    builds: true,
    // The declared rust-version is in a manifest the tree holds, and cargo carries the toolchain.
    reads: Some(Reads::tree_and(&["cargo"])),
    kind: Kind::Binary(msrv),
};

pub const MUTEST: Gate = Gate {
    name: "mutest",
    about: "mutations that changed a decision and no test noticed, per file and operator",
    group: Group::OptIn,
    builds: true,
    // Both halves are in the tree: the code it mutates and the tests that fail to notice. A local
    // run mutates only the files the change touched.
    reads: Some(
        Reads::tree_and(&["cargo", "cargo-mutest"])
            .versioned("watchdog-v1:isolated-v1:results-v1:scoped-v1")
            .and_change_set(),
    ),
    kind: Kind::AnnotatedRatchet {
        measure: super::mutation::mutest::measure,
        keys: Keys::Items,
        unit: "mutation(s) no test killed",
    },
};

pub const UNUSED_DEEP: Gate = Gate {
    name: "unused-deep",
    about: "unused dependencies, compiled rather than scanned; needs nightly",
    group: Group::OptIn,
    builds: true,
    // A nightly all-features compile whose inputs are the tree and cargo-udeps.
    reads: Some(Reads::tree_and(&["cargo", "cargo-udeps"]).versioned("features-v1")),
    kind: Kind::Binary(unused_deep),
};

pub const BSIZE: Gate = Gate {
    name: "bsize",
    about: "where the binary's bytes are — an instrument, not a gate",
    group: Group::Instrument,
    builds: true,
    reads: Some(Reads::tree_and(&["cargo", "cargo-bsize"])),
    kind: Kind::Binary(binsize::bsize),
};

/// Every place a diagnostic points at, after rustc's `-->` or codespan's `┌─`.
#[must_use]
pub fn spans(text: &str, root: &Path) -> Vec<Finding> {
    pointed_at(text, root)
        .into_iter()
        .map(|(_, finding)| finding)
        .collect()
}

/// Only the places a tool called an error, since those explain a failed run and warnings do not.
fn errors(text: &str, root: &Path) -> Vec<Finding> {
    pointed_at(text, root)
        .into_iter()
        .filter_map(|(error, finding)| error.then_some(finding))
        .collect()
}

/// Each place a tool pointed at, and whether it said so as an error.
fn pointed_at(text: &str, root: &Path) -> Vec<(bool, Finding)> {
    let mut found = Vec::new();
    let mut last = Note::default();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = located(trimmed) {
            if let Some(finding) = span(rest, &last, root) {
                found.push((last.error, finding));
            }
        } else if let Some(note) = note(trimmed) {
            last = note;
        }
    }
    found
}

/// What a tool said before it pointed at a place; `code` is the rule or advisory ID, if printed.
#[derive(Debug, Default, PartialEq, Eq)]
struct Note {
    code: Option<String>,
    message: String,
    error: bool,
}

/// The location after rustc's `-->` or codespan-reporting's `┌─`, which cargo-deny prints.
fn located(line: &str) -> Option<&str> {
    line.strip_prefix("--> ")
        .or_else(|| line.strip_prefix("┌─ "))
}

/// `error: what` from rustc, or `warning[a-rule]: what` from codespan-reporting.
fn note(line: &str) -> Option<Note> {
    let (head, message) = line.split_once(": ")?;
    let (level, code) = match head.split_once('[') {
        Some((level, rest)) => (level, rest.strip_suffix(']').map(str::to_string)),
        None => (head, None),
    };
    if !matches!(level, "error" | "warning") || message.is_empty() {
        return None;
    }
    Some(Note {
        code,
        message: message.to_string(),
        error: level == "error",
    })
}

/// `path:line:col`, right to left: a Windows-style path holds colons of its own.
fn span(text: &str, note: &Note, root: &Path) -> Option<Finding> {
    // A location with no message gives a reader no reason to go there.
    if note.message.is_empty() {
        return None;
    }
    let (rest, _col) = text.rsplit_once(':')?;
    let (path, line) = rest.rsplit_once(':')?;
    let line: u32 = line.parse().ok()?;
    let path = path.trim_start_matches("./");
    let shown = crate::project::relative(root, Path::new(path));
    let found = Finding::at(&shown, &note.message).line(line);
    Some(match &note.code {
        Some(code) => found.item(code),
        None => found,
    })
}

/// Reads diagnostics whatever the exit code, since `cargo deny` can warn of an advisory and exit 0.
pub(super) fn verdict(out: &exec::Output, root: &Path) -> Outcome {
    match out.success() {
        true => Outcome::noted(from_both(spans, out, root)),
        // The raw output goes along, for when no finding could be read from it.
        false => Outcome::failed(whole(from_both(errors, out, root), out)).saying(out),
    }
}

/// Whether this is a failure that no finding was read from, which reports as unable to run.
pub(super) fn unread(outcome: &Outcome) -> bool {
    !outcome.passed && outcome.findings.is_empty()
}

/// What ends a list of findings where the tool printed more than chock keeps.
const CUT: &str = "the tool printed more than chock keeps, so this list is not complete";

/// The findings, and one more that says the list is partial where the output was cut. An empty
/// list gains nothing, so that failure still reports as unable to run.
fn whole(mut found: Vec<Finding>, out: &exec::Output) -> Vec<Finding> {
    if out.truncated && !found.is_empty() {
        found.push(Finding::at("", CUT).item("output"));
    }
    found
}

fn from_both(
    read: fn(&str, &Path) -> Vec<Finding>,
    out: &exec::Output,
    root: &Path,
) -> Vec<Finding> {
    let mut found = read(&out.stderr, root);
    found.extend(read(&out.stdout, root));
    found
}

fn test(ctx: &Ctx) -> Result<Outcome, String> {
    let out = first_run(ctx)?;
    Ok(read_suite(&out, &ctx.root))
}

/// The suite's outcome: the failing tests or worsened measures it names, else compiler spans.
fn read_suite(out: &exec::Output, root: &Path) -> Outcome {
    if out.success() {
        return Outcome::passed();
    }
    let mut found: Vec<Finding> = failing_tests(out)
        .iter()
        .map(|test| Finding::at("", "this test failed").item(test))
        .collect();
    found.extend(worsened(out));
    match found.is_empty() {
        // Nothing named a test or a measure, so the build failed; rustc's spans say where.
        true => verdict(out, root),
        false => Outcome::failed(whole(found, out)),
    }
}

/// Every measure `outpost check` says got worse, for a runner that calls outpost as a stage.
fn worsened(out: &exec::Output) -> Vec<Finding> {
    let mut lines: BTreeSet<String> = BTreeSet::new();
    for text in [&out.stdout, &out.stderr] {
        lines.extend(
            text.lines()
                .map(|line| exec::strip_colour(line).trim().to_string())
                .filter(|line| line.starts_with("worse ")),
        );
    }
    lines.iter().filter_map(|line| worse(line)).collect()
}

/// `worse <measure> <from> -> <to>`; a line without the `->` is prose and names nothing.
fn worse(line: &str) -> Option<Finding> {
    let rest = line.strip_prefix("worse ")?;
    let (measure, moved) = rest.split_once(' ')?;
    let (from, to) = moved.split_once(" -> ")?;
    let message = format!("got worse: {} to {}", from.trim(), to.trim());
    Some(Finding::at("", &message).item(measure))
}

/// Every test nextest gave a failing verdict, from both streams.
pub(super) fn failing_tests(out: &exec::Output) -> BTreeSet<String> {
    let mut named = failed_tests(&out.stdout);
    named.extend(failed_tests(&out.stderr));
    named
}

/// Every test nextest gave a failing verdict. A set, since the same line can reach both streams.
fn failed_tests(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| failed_test(&exec::strip_colour(line)))
        .collect()
}

/// `FAIL [   0.005s] <binary-id> <test path>`. `FLAKY` is not a failure: the test passed on retry.
fn failed_test(line: &str) -> Option<String> {
    marked(line, &["FAIL", "TIMEOUT", "ABORT", "SIGSEGV", "LEAK-FAIL"])
}

/// Every test nextest stopped at its time limit, from both streams.
pub(super) fn timed_out_tests(out: &exec::Output) -> BTreeSet<String> {
    [&out.stdout, &out.stderr]
        .into_iter()
        .flat_map(|text| text.lines())
        .filter_map(|line| marked(&exec::strip_colour(line), &["TIMEOUT"]))
        .collect()
}

/// The test a nextest verdict line names, when the line starts with one of `verdicts`.
fn marked(line: &str, verdicts: &[&str]) -> Option<String> {
    let trimmed = line.trim();
    let rest = verdicts
        .iter()
        .find_map(|mark| trimmed.strip_prefix(mark))?;
    let (_duration, named) = rest.trim_start().strip_prefix('[')?.split_once(']')?;
    let named = uncounted(named.trim());
    (!named.is_empty()).then(|| named.to_string())
}

/// The name after the progress count newer nextest prints first: `(  12/1783) chock tests::x`.
fn uncounted(named: &str) -> &str {
    let counted = named
        .strip_prefix('(')
        .and_then(|rest| rest.split_once(')'))
        .filter(|(count, _)| {
            count
                .chars()
                .all(|c| c.is_ascii_digit() || c == '/' || c == ' ')
        });
    counted.map_or(named, |(_count, after)| after.trim_start())
}

pub const IDEMPOTENT: Gate = Gate {
    name: "idempotent",
    about: "the suite passes a second time in the same tree, without cleaning between",
    group: Group::OptIn,
    builds: true,
    // Same inputs as `test`.
    reads: Some(Reads::tree_runner_and(&["cargo"]).versioned("features-v1")),
    kind: Kind::Binary(idempotent),
};

/// Runs the suite again in the same tree. Failing only the second time means runs share state, such
/// as a file left behind.
fn idempotent(ctx: &Ctx) -> Result<Outcome, String> {
    // Reuses the run `test` already took when both gates are on.
    let first = first_run(ctx)?;
    if !first.success() {
        return Err("the suite fails on its first run, so a second says nothing".to_string());
    }
    let second = suite(ctx)?;
    if second.success() {
        return Ok(Outcome::passed());
    }
    Ok(Outcome::failed(failed_twice(&second, &ctx.root)))
}

/// What the second run says, most specific first: a compiler span, else the tests that failed, else
/// that it failed at all.
fn failed_twice(second: &exec::Output, root: &Path) -> Vec<Finding> {
    let mut spanned = spans(&second.stderr, root);
    spanned.extend(spans(&second.stdout, root));
    if !spanned.is_empty() {
        return spanned;
    }
    let named = failing_tests(second);
    if !named.is_empty() {
        return named
            .iter()
            .map(|test| {
                Finding::at("", "passed on the first run and failed on the second").item(test)
            })
            .collect();
    }
    vec![
        Finding::at(
            "",
            "the suite passed once and failed the second time in the same tree",
        )
        .item("idempotent"),
    ]
}

/// The suite's first run in this invocation, shared by every gate that needs one.
fn first_run(ctx: &Ctx) -> Result<exec::Output, String> {
    ctx.suite.get_or_init(|| suite(ctx)).clone()
}

/// Runs whatever the project's `runner` setting names, since some suites need their own setup.
fn suite(ctx: &Ctx) -> Result<exec::Output, String> {
    let (program, args) = ctx
        .runner
        .argv
        .split_first()
        .ok_or("the configured `runner` names no command")?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    exec::tool(&ctx.root, program, &args)
}

const UNFORMATTED: &str = "formatting differs — run `cargo fmt --all`";

/// Formatting, then clippy with warnings denied. Both run, so a file to reformat does not hide
/// what clippy found.
fn lint(ctx: &Ctx) -> Result<Outcome, String> {
    let formatted = fmt(ctx)?;
    let mut args = vec!["clippy", "--workspace", "--all-targets", "--no-deps"];
    args.extend(ctx.cargo_args());
    args.extend(["--", "-D", "warnings"]);
    let clippy = exec::tool(&ctx.root, "cargo", &args)?;
    Ok(linted(formatted, verdict(&clippy, &ctx.root)))
}

const CLIPPY_UNREAD: &str = "failed, and chock read no finding from its output — `cargo clippy \
                             --workspace --all-targets` shows it";

/// Formatting's findings, then clippy's. Where formatting passed, clippy's outcome is the answer.
fn linted(formatted: Outcome, clippy: Outcome) -> Outcome {
    if formatted.passed {
        return clippy;
    }
    let mut findings = formatted.findings;
    // Clippy's raw output does not travel with a trip, so a failure nothing was read from is named.
    if unread(&clippy) {
        findings.push(Finding::at("", CLIPPY_UNREAD).item("clippy"));
    }
    findings.extend(clippy.findings);
    Outcome::failed(findings)
}

fn fmt(ctx: &Ctx) -> Result<Outcome, String> {
    let out = exec::tool(&ctx.root, "cargo", &["fmt", "--all", "--", "--check"])?;
    formatted(&out, &ctx.root)
}

/// A failure naming no file is rustfmt refusing to run, such as a missing component, not a finding.
fn formatted(out: &exec::Output, root: &Path) -> Result<Outcome, String> {
    if out.success() {
        return Ok(Outcome::passed());
    }
    let findings = unformatted(&out.stdout, root);
    if findings.is_empty() {
        return Err(format!(
            "cargo fmt named no file: {}",
            out.failure_details()
        ));
    }
    Ok(Outcome::failed(whole(findings, out)))
}

/// One finding per file, not per hunk: the fix is a single command.
fn unformatted(stdout: &str, root: &Path) -> Vec<Finding> {
    let files: BTreeSet<String> = stdout
        .lines()
        .filter_map(diffed_file)
        .map(|path| crate::project::relative(root, Path::new(path)))
        .collect();
    files
        .into_iter()
        .map(|path| Finding::at(&path, UNFORMATTED))
        .collect()
}

/// The path in a hunk header: `Diff in <path>:<line>:`, or `Diff in <path> at line <n>:` from
/// older rustfmt.
fn diffed_file(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("Diff in ")?;
    if let Some((path, _)) = rest.split_once(" at line") {
        return Some(path);
    }
    let (path, line_number) = rest.strip_suffix(':')?.rsplit_once(':')?;
    line_number.parse::<u32>().ok()?;
    Some(path)
}

/// A build script or proc-macro that reaches the network or spawns a process fails unless its entry
/// in cackle.toml allows it.
fn acl(ctx: &Ctx) -> Result<Outcome, String> {
    ctx.default_build("cargo acl")?;
    acl_args(&ctx.features)
        .and_then(|args| exec::tool(&ctx.root, "cargo", &args))
        .and_then(|out| read_acl(&out))
}

fn acl_args(features: &[String]) -> Result<Vec<&str>, String> {
    if features
        .iter()
        .any(|flag| matches!(flag.as_str(), "--all-features" | "--no-default-features"))
    {
        return Err("cargo acl cannot honor --all-features or --no-default-features; its CLI only accepts named --features".to_string());
    }
    // Without a UI, cackle settles a warning without writing the fix, then checks again forever.
    let mut args = vec!["acl", "--no-ui", "--quiet", "--fail-on-warnings"];
    args.extend(features.iter().map(String::as_str));
    Ok(args)
}

/// Reads cargo-acl's answer. A missing cackle.toml or sandbox is a reason it could not run.
fn read_acl(out: &exec::Output) -> Result<Outcome, String> {
    if out.success() {
        return Ok(Outcome::passed());
    }
    let text = format!("{}{}", out.stdout, out.stderr);
    if text.contains(SANDBOX) {
        return Err(
            "the sandbox could not start, so nothing was analysed: install bubblewrap, \
             and on Ubuntu 23.10 or newer allow unprivileged user namespaces \
             (`sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`)"
                .to_string(),
        );
    }
    if no_policy(&text) {
        return Err(format!(
            "no {ACL_CONFIG}, so no dependency is held to anything — write one with \
             `cargo acl --no-ui --auto-accept-fixes`"
        ));
    }
    let found: Vec<Finding> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("ERROR: "))
        .map(|message| Finding::at(ACL_CONFIG, message))
        .collect();
    if found.is_empty() {
        return Err(format!(
            "cargo acl refused the tree without naming a package: {}",
            text.lines().last().unwrap_or("no reason given")
        ));
    }
    Ok(Outcome::failed(whole(found, out)))
}

const ACL_CONFIG: &str = "cackle.toml";

/// Whether cackle says, in any of its wordings, that cackle.toml is missing.
fn no_policy(text: &str) -> bool {
    text.contains(ACL_CONFIG)
        && (text.contains("not found")
            || text.contains("Failed to open")
            || text.contains("No such file"))
}

/// What bubblewrap prints when the kernel refuses it the namespaces it needs.
const SANDBOX: &str = "bwrap:";

/// Checks order only; `--check-format` would also fail on whitespace.
fn sort(ctx: &Ctx) -> Result<Series, String> {
    let out = exec::tool(&ctx.root, "cargo", &["sort", "--check", "--workspace"])?;
    read_sort(&out)
}

/// Keyed by the package cargo-sort names. It names no file and colours its output even in a pipe,
/// so the colour is stripped before reading.
fn read_sort(out: &exec::Output) -> Result<Series, String> {
    // A count read from a cut output could be under the record, so that output is refused.
    if !exec::objected(out, "cargo sort")? {
        return Ok(Series::new());
    }
    Some(named_unsorted(&exec::strip_colour(&out.stderr)))
        .filter(|series| !series.0.is_empty())
        .ok_or_else(|| "cargo sort rejected a manifest without saying which".to_string())
}

/// One row per package the output names.
fn named_unsorted(said: &str) -> Series {
    let mut series = Series::new();
    for package in said.lines().filter_map(unsorted) {
        series.set(&package, 1);
    }
    series
}

/// The package in `error: Dependencies for <package> are not sorted`; any other line names none.
fn unsorted(line: &str) -> Option<String> {
    let said = line.trim().strip_prefix("error: ")?;
    let named = said.strip_prefix("Dependencies for ")?;
    Some(named.strip_suffix(" are not sorted")?.to_string())
}

fn unused_deep(ctx: &Ctx) -> Result<Outcome, String> {
    let mut args = vec![
        "+nightly",
        "udeps",
        "--all-targets",
        // Without it, a dependency used only under a feature reads as unused.
        "--all-features",
        "--output",
        "json",
    ];
    args.extend(ctx.build.iter().map(String::as_str));
    let out = exec::tool(&ctx.root, "cargo", &args)?;
    read_deep(&out, &ctx.root)
}

fn read_deep(out: &exec::Output, root: &Path) -> Result<Outcome, String> {
    if !exec::objected(out, "cargo udeps")? {
        return Ok(Outcome::passed());
    }
    // With no answer on stdout, the reason is on stderr, so the output goes with the outcome.
    if out.stdout.trim().is_empty() {
        return Ok(Outcome::failed(Vec::new()).saying(out));
    }
    read_udeps(&out.stdout, root)
}

/// What `cargo udeps --output json` prints.
#[derive(serde::Deserialize)]
struct Udeps {
    #[serde(default)]
    unused_deps: std::collections::BTreeMap<String, UnusedIn>,
}

#[derive(serde::Deserialize)]
struct UnusedIn {
    manifest_path: String,
    #[serde(default)]
    normal: Vec<String>,
    #[serde(default)]
    development: Vec<String>,
    #[serde(default)]
    build: Vec<String>,
}

/// Each unused dependency, with the manifest table that declares it.
fn read_udeps(stdout: &str, root: &Path) -> Result<Outcome, String> {
    let read: Udeps = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("cargo udeps printed output chock cannot read: {e}"))?;
    let mut found = Vec::new();
    for unused in read.unused_deps.values() {
        let shown = crate::project::relative(root, Path::new(&unused.manifest_path));
        for (table, names) in [
            ("dependencies", &unused.normal),
            ("dev-dependencies", &unused.development),
            ("build-dependencies", &unused.build),
        ] {
            for name in names {
                let said = format!("nothing compiled from this crate uses this {table} entry");
                found.push(Finding::at(&shown, &said).item(name));
            }
        }
    }
    if found.is_empty() {
        return Err("cargo udeps objected but named no dependency, so nothing was measured".into());
    }
    Ok(Outcome::failed(found))
}

/// Runs typos for JSON, since its human format draws spans the shared reader cannot follow.
fn typos(ctx: &Ctx) -> Result<Series, String> {
    let asked = typos_invocation(&crate::project::SKIPPED);
    let argv: Vec<&str> = asked.iter().map(String::as_str).collect();
    let out = exec::tool(&ctx.root, "typos", &argv)?;
    read_typos(&out)
}

/// Excludes every skipped directory explicitly, since typos honours `.gitignore` only inside git.
fn typos_invocation(skipped: &[&str]) -> Vec<String> {
    let directories = skipped.iter().map(|name| format!("**/{name}/**"));
    let excluded = directories.chain(WRITTEN_BY_CHOCK.iter().map(ToString::to_string));
    ["--format", "json"]
        .map(String::from)
        .into_iter()
        .chain(excluded.flat_map(|glob| ["--exclude".to_string(), glob]))
        .collect()
}

/// Files chock's own gates write into the tree, which typos must not read.
const WRITTEN_BY_CHOCK: [&str; 3] = ["**/lcov.info", "**/kani-list.json", "**/.chock/**"];

/// A word typos does not know, and the file holding it; never the line, which moves with edits.
#[derive(serde::Deserialize)]
struct Typo {
    path: String,
    typo: String,
}

/// `typos` exits 2 when it finds something, 0 when it finds nothing; any other exit is a refusal.
fn read_typos(out: &exec::Output) -> Result<Series, String> {
    const FOUND: i32 = 2;
    if out.truncated {
        return Err("typos printed more than chock keeps; the count would be short".to_string());
    }
    if !matches!(out.code, Some(0 | FOUND)) {
        return Err(format!("typos could not run: {}", complaint(&out.stderr)));
    }
    let mut series = Series::new();
    for typo in misspellings(&out.stdout) {
        let key = format!("{}#{}", typo.path.trim_start_matches("./"), typo.typo);
        series.set(&key, series.get(&key).unwrap_or(0) + 1);
    }
    Ok(series)
}

/// One object per line. Objects of another shape are the tool's own errors and are skipped.
fn misspellings(stdout: &str) -> Vec<Typo> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Typo>(line).ok())
        .collect()
}

/// What a tool said about why it would not run, short enough to sit in a one-line reason.
fn complaint(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("it printed no reason")
        .to_string()
}

/// Why `msrv` cannot run in a crate that names no oldest Rust.
pub(in crate::gates) const NO_MSRV: &str =
    "Cargo.toml declares no rust-version, so there is no promise to check";

/// Checks the workspace on the oldest Rust its `rust-version` promises, not on the toolchain pin.
fn msrv(ctx: &Ctx) -> Result<Outcome, String> {
    let manifest = std::fs::read_to_string(ctx.root.join("Cargo.toml"))
        .map_err(|e| format!("cannot read Cargo.toml: {e}"))?;
    let Some(version) = rust_version(&manifest) else {
        return Err(NO_MSRV.into());
    };
    let listed =
        exec::run("rustup", &["toolchain", "list"], &ctx.root).map_err(|e| e.to_string())?;
    let toolchain = format!("+{}", tests_the_promise(&listed, &version)?);
    // Stop rustup installing a missing toolchain, a large download inside a commit hook.
    let out = exec::run_env(
        "cargo",
        &{
            let mut args = vec![
                toolchain.as_str(),
                "check",
                "--locked",
                "--workspace",
                "--all-targets",
            ];
            args.extend(ctx.cargo_args());
            args
        },
        &ctx.root,
        &[("RUSTUP_AUTO_INSTALL", "0")],
    )
    .map_err(|e| e.to_string())?;
    read_msrv(&out, &version, &ctx.root)
}

/// The installed toolchain to test the promise on. A failed `rustup` listing is an error, not none.
fn tests_the_promise(listed: &exec::Output, promised: &str) -> Result<String, String> {
    if !listed.success() {
        return Err(format!(
            "rustup could not say which toolchains are installed: {}",
            listed.why_it_failed()
        ));
    }
    floor(promised, &listed.stdout).ok_or_else(|| {
        format!(
            "this crate promises Rust {promised}, which this machine does not have: run \
             `rustup toolchain install {promised}`"
        )
    })
}

/// The lowest installed toolchain that keeps the promise, by its full name: `+1.94` would need one
/// installed under exactly that name.
fn floor(promised: &str, listed: &str) -> Option<String> {
    listed
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|name| Some((satisfies(channel(name), promised)?, name.to_string())))
        .min()
        .map(|(_, name)| name)
}

/// The release a toolchain name begins with: `1.94.1-x86_64-unknown-linux-gnu` is `1.94.1`, and
/// `nightly-2026-01-01-…` is `nightly`, which satisfies no numbered promise.
fn channel(name: &str) -> &str {
    name.split('-').next().unwrap_or(name)
}

/// The patch number of a release that keeps the promise, for picking the lowest. `1.94` is kept by
/// every `1.94.x`; `1.94.1` only by itself.
fn satisfies(channel: &str, promised: &str) -> Option<u64> {
    let want: Vec<&str> = promised.split('.').collect();
    let have: Vec<&str> = channel.split('.').collect();
    if have.len() < want.len() || have[..want.len()] != want[..] {
        return None;
    }
    Some(
        have.get(2)
            .and_then(|patch| patch.parse().ok())
            .unwrap_or(0),
    )
}

fn read_msrv(out: &exec::Output, version: &str, root: &Path) -> Result<Outcome, String> {
    if let Some(fix) = missing_toolchain(&out.stderr) {
        return Err(format!(
            "this crate promises Rust {version}, which this machine does not have: {fix}"
        ));
    }
    if let Some((package, wants)) = out_of_reach(&out.stderr) {
        let said =
            format!("needs Rust {wants}, past the {version} this crate promises in its manifest");
        return Ok(Outcome::failed(vec![
            Finding::at(crate::project::MANIFEST, &said).item(&package),
        ]));
    }
    Ok(verdict(out, root))
}

/// The dependency that will not build on the promised Rust, and the version it wants. cargo says
/// "rustc 1.94.1 is not supported by the following package: sysinfo@0.39.6 requires rustc 1.95".
#[must_use]
fn out_of_reach(stderr: &str) -> Option<(String, String)> {
    let said = stderr
        .lines()
        .map(str::trim)
        .find(|line| line.contains("is not supported by the following package"))?;
    let named = said.split("package:").nth(1)?.trim();
    let (package, wants) = named.split_once(" requires rustc ")?;
    Some((
        package.trim_end_matches(':').trim().to_string(),
        wants.trim_end_matches('.').trim().to_string(),
    ))
}

/// The fix for a toolchain rustup does not hold, from its `help:` line where it prints one.
fn missing_toolchain(stderr: &str) -> Option<String> {
    stderr
        .contains("is not installed")
        .then(|| match rustup_help(stderr) {
            Some(command) => format!("run `{command}`"),
            None => "install it with `rustup toolchain install`".to_string(),
        })
}

/// `help: run `rustup toolchain install 1.85-x86_64-unknown-linux-gnu` to install it`.
fn rustup_help(stderr: &str) -> Option<String> {
    stderr.lines().map(str::trim).find_map(|line| {
        let rest = line.strip_prefix("help: run `")?;
        let (command, _) = rest.split_once('`')?;
        Some(command.to_string())
    })
}

/// `rust-version` from `[package]`, or from `[workspace.package]` where the member inherits it.
/// Hand-read rather than parsed: chock takes no TOML dependency.
#[must_use]
pub fn rust_version(manifest: &str) -> Option<String> {
    let mut section = "";
    let (mut package, mut workspace) = (None, None);
    for raw in manifest.lines() {
        let line = without_comment(raw.trim());
        if line.starts_with('[') {
            section = line;
            continue;
        }
        let held = match section {
            "[package]" => &mut package,
            // `[workspace] package.rust-version = …` is the dotted spelling of the table below it.
            "[workspace]" => match line.strip_prefix("package.") {
                Some(_) => &mut workspace,
                None => continue,
            },
            "[workspace.package]" => &mut workspace,
            _ => continue,
        };
        let key = line.strip_prefix("package.").unwrap_or(line);
        if held.is_none() {
            *held = declared_version(key);
        }
    }
    // A member's own literal wins; either spelling of inheritance declares none of its own.
    package.or(workspace)
}

/// Everything before an unquoted `#`, so `rust-version = "1.85"  # MSRV` reads as `1.85`.
fn without_comment(line: &str) -> &str {
    let mut quoted = false;
    for (at, byte) in line.bytes().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b'#' if !quoted => return line[..at].trim_end(),
            _ => {}
        }
    }
    line
}

/// The literal on a `rust-version` line, if it carries one. Both `rust-version.workspace = true`
/// and `rust-version = { workspace = true }` name no version of their own.
fn declared_version(line: &str) -> Option<String> {
    let value = line.strip_prefix("rust-version")?.trim_start();
    let value = value.strip_prefix('=')?.trim();
    if value.starts_with('{') {
        return None;
    }
    let value = value.trim_matches('"').trim_matches('\'');
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new("/w/proj")
    }

    /// Every gate this file defines, so a test can hold them all to the same rules.
    fn all() -> [&'static Gate; 14] {
        [
            &ACL,
            &TEST,
            &LINT,
            &FMT,
            &DOC,
            &DEPS,
            &SORT,
            &UNUSED,
            &TYPOS,
            &MSRV,
            &IDEMPOTENT,
            &MUTEST,
            &UNUSED_DEEP,
            &BSIZE,
        ]
    }

    #[test]
    fn acl_receives_named_features_and_refuses_scopes_its_tool_cannot_express() {
        let base = ["acl", "--no-ui", "--quiet", "--fail-on-warnings"];
        assert_eq!(acl_args(&[]).unwrap(), base);
        let features = ["--features".to_string(), "testkit".to_string()];
        assert_eq!(
            acl_args(&features).unwrap(),
            [&base[..], &["--features", "testkit"]].concat()
        );
        for flag in ["--all-features", "--no-default-features"] {
            assert!(
                acl_args(&[flag.to_string()])
                    .unwrap_err()
                    .contains("cannot honor")
            );
        }
    }

    #[test]
    fn a_rustc_shaped_span_becomes_a_navigable_finding() {
        let text = "warning: length comparison to zero\n  --> /w/proj/src/a.rs:7:8\n";
        let found = spans(text, root());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].render(), "src/a.rs:7: length comparison to zero");
    }

    /// Real cargo-deny 0.20.2 output.
    #[test]
    fn a_span_in_codespan_shape_is_read_and_keeps_the_rule_that_found_it() {
        let text = "warning[license-not-encountered]: license was not encountered\n   \u{250c}\u{2500} /w/proj/deny.toml:13:6\n   \u{2502}\n";
        let found = spans(text, root());
        assert_eq!(
            found.iter().map(Finding::render).collect::<Vec<_>>(),
            vec!["deny.toml:13: license-not-encountered: license was not encountered"]
        );
    }

    #[test]
    fn an_advisory_keeps_the_identifier_someone_would_search_for() {
        let text = "error[vulnerability]: a crate has a known vulnerability\n   \u{250c}\u{2500} /w/proj/Cargo.lock:42:1\n";
        let found = spans(text, root());
        assert_eq!(found[0].item.as_deref(), Some("vulnerability"));
    }

    #[test]
    fn a_note_or_a_help_line_names_no_finding() {
        let text = "note[something]: for more information\n   \u{250c}\u{2500} /w/proj/a.rs:1:1\n";
        assert_eq!(spans(text, root()), vec![]);
    }

    #[test]
    fn every_span_in_the_output_is_reported_not_only_the_first() {
        let text = "error: one\n --> /w/proj/a.rs:1:1\nerror: two\n --> /w/proj/b.rs:2:1\n";
        let found = spans(text, root());
        assert_eq!(
            found.iter().map(Finding::render).collect::<Vec<_>>(),
            vec!["a.rs:1: one", "b.rs:2: two"]
        );
    }

    #[test]
    fn a_path_written_relative_with_a_dot_slash_loses_it() {
        let found = spans("error: typo\n  --> ./src/a.rs:3:9\n", root());
        assert_eq!(found[0].file, "src/a.rs");
    }

    #[test]
    fn a_span_whose_line_is_not_a_number_is_skipped_rather_than_guessed() {
        assert_eq!(spans("error: x\n --> src/a.rs:nine:1\n", root()), vec![]);
    }

    #[test]
    fn output_with_no_spans_at_all_yields_no_findings() {
        assert_eq!(spans("everything is fine\n", root()), vec![]);
    }

    #[test]
    fn a_tool_that_exited_clean_with_nothing_to_say_passes_silently() {
        let out = exec::Output {
            code: Some(0),
            stdout: String::new(),
            stderr: "advisories ok\n".to_string(),
            truncated: false,
        };
        assert_eq!(verdict(&out, root()), Outcome::passed());
    }

    #[test]
    fn a_tool_that_exited_clean_still_reports_the_advisory_it_printed() {
        let out = exec::Output {
            code: Some(0),
            stdout: String::new(),
            stderr: "warning[unmaintained]: crate is unmaintained\n  ┌─ /w/proj/Cargo.lock:12:1\n"
                .to_string(),
            truncated: false,
        };
        let outcome = verdict(&out, root());
        assert!(outcome.passed);
        assert_eq!(
            outcome
                .findings
                .iter()
                .map(Finding::render)
                .collect::<Vec<_>>(),
            vec!["Cargo.lock:12: unmaintained: crate is unmaintained"]
        );
    }

    #[test]
    fn a_failing_tool_carries_the_spans_from_both_streams() {
        let out = exec::Output {
            code: Some(101),
            stdout: "error: b\n --> /w/proj/b.rs:2:1\n".to_string(),
            stderr: "error: a\n --> /w/proj/a.rs:1:1\n".to_string(),
            truncated: false,
        };
        let outcome = verdict(&out, root());
        assert!(!outcome.passed);
        assert_eq!(
            outcome
                .findings
                .iter()
                .map(Finding::render)
                .collect::<Vec<_>>(),
            vec!["a.rs:1: a", "b.rs:2: b"]
        );
    }

    #[test]
    fn an_error_is_told_from_a_warning_where_the_tool_said_which() {
        assert!(note("error[E0425]: cannot find value").unwrap().error);
        assert!(!note("warning: unused import").unwrap().error);
        let both = "warning: w\n --> /w/proj/a.rs:1:1\nerror: e\n --> /w/proj/b.rs:2:1\n";
        let rendered: Vec<String> = errors(both, root()).iter().map(Finding::render).collect();
        assert_eq!(rendered, ["b.rs:2: e"]);
    }

    #[test]
    fn a_failure_is_explained_by_its_errors_and_never_by_a_warning_beside_them() {
        let warned =
            "warning: unused workspace dependency `thiserror`\n --> /w/proj/Cargo.toml:9:1\n";
        let died = exec::Output {
            code: Some(101),
            stdout: String::new(),
            stderr: format!("{warned}error: failed to run custom build command for `glib-sys`\n"),
            truncated: false,
        };
        assert_eq!(verdict(&died, root()).findings, Vec::new());
        let erred = exec::Output {
            stderr: format!("{warned}error[E0425]: cannot find value\n --> /w/proj/a.rs:3:5\n"),
            ..died.clone()
        };
        let pointed: Vec<String> = verdict(&erred, root())
            .findings
            .iter()
            .map(Finding::render)
            .collect();
        assert_eq!(pointed, ["a.rs:3: E0425: cannot find value"]);
        let passed = exec::Output {
            code: Some(0),
            ..died
        };
        let noted: Vec<String> = verdict(&passed, root())
            .findings
            .iter()
            .map(Finding::render)
            .collect();
        assert_eq!(
            noted,
            ["Cargo.toml:9: unused workspace dependency `thiserror`"]
        );
    }

    #[test]
    fn the_declared_msrv_is_read_from_the_package_table() {
        let manifest = "[package]\nname = \"x\"\nrust-version = \"1.85\"\n";
        assert_eq!(rust_version(manifest), Some("1.85".to_string()));
    }

    #[test]
    fn a_table_before_the_package_one_does_not_end_the_read() {
        let manifest =
            "[workspace]\nmembers = [\"a\"]\n\n[package]\nname = \"x\"\nrust-version = \"1.85\"\n";
        assert_eq!(rust_version(manifest), Some("1.85".to_string()));
    }

    #[test]
    fn an_msrv_in_another_table_is_not_read_as_the_packages() {
        let manifest = "[package]\nname = \"x\"\n\n[dependencies.foo]\nrust-version = \"1.60\"\n";
        assert_eq!(rust_version(manifest), None);
    }

    /// The common workspace layout: the member inherits and the root declares.
    #[test]
    fn a_version_inherited_from_the_workspace_is_the_promise() {
        let manifest = "[package]\nname = \"x\"\nrust-version.workspace = true\n\n\
                        [workspace.package]\nrust-version = \"1.85\"\n";
        assert_eq!(rust_version(manifest), Some("1.85".to_string()));
    }

    #[test]
    fn an_inheriting_line_does_not_end_the_read() {
        let manifest = "[workspace.package]\nrust-version = \"1.85\"\n\n\
                        [package]\nrust-version.workspace = true\nname = \"x\"\n";
        assert_eq!(rust_version(manifest), Some("1.85".to_string()));
    }

    /// nextest's real status lines.
    #[test]
    fn every_test_nextest_failed_is_named() {
        let out = "        FAIL [   0.005s] chock lock::tests::a_lock_is_taken\n\
                   \u{1b}[31m     TIMEOUT [  60.000s]\u{1b}[0m chock slow::hangs\n\
                           PASS [   0.001s] chock other::fine\n\
                      FLAKY 2/2 [   2.000s] chock retried::eventually_passed\n";
        assert_eq!(
            failed_tests(out)
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["chock lock::tests::a_lock_is_taken", "chock slow::hangs"]
        );
    }

    /// A summary line carries a duration in the same brackets and names no test.
    #[test]
    fn a_line_that_names_no_test_is_not_a_failure() {
        assert_eq!(failed_test("     Summary [  15.750s] 8/10 tests run"), None);
        assert_eq!(failed_test("        FAIL [   0.005s] "), None);
        assert_eq!(failed_test("nothing here"), None);
    }

    /// As nextest 0.9.146 prints it: the count changes from run to run, and the name does not.
    #[test]
    fn the_progress_count_before_a_test_is_not_part_of_its_name() {
        let line = "        FAIL [   2.319s] (  12/1783) chock cli::args::tests::one";
        assert_eq!(
            failed_test(line).as_deref(),
            Some("chock cli::args::tests::one")
        );
        let out = exec::Output {
            code: Some(100),
            stdout: String::new(),
            stderr: "     TIMEOUT [ 300.004s] (1783/1783) chock slow::hangs\n".to_string(),
            truncated: false,
        };
        assert_eq!(
            timed_out_tests(&out).into_iter().collect::<Vec<_>>(),
            ["chock slow::hangs"]
        );
        assert_eq!(uncounted("(not a count) x"), "(not a count) x");
        assert_eq!(uncounted("(12/1783 x"), "(12/1783 x");
    }

    /// The shape `--output json` prints, from cargo-udeps 0.1.61's own `Outcome`.
    #[test]
    fn a_deep_unused_dependency_names_the_table_that_declares_it() {
        let json = r#"{"success":false,"unused_deps":{"p 0.1.0 (path+file:///w)":
            {"manifest_path":"/w/Cargo.toml","normal":["serde"],"development":["tempfile"],
             "build":[]}},"note":null}"#;
        let Ok(Outcome {
            passed, findings, ..
        }) = read_udeps(json, Path::new("/w"))
        else {
            panic!("a shape taken from the tool's own source should read")
        };
        assert!(!passed);
        assert_eq!(
            findings.iter().map(Finding::render).collect::<Vec<_>>(),
            [
                "Cargo.toml: serde: nothing compiled from this crate uses this dependencies entry",
                "Cargo.toml: tempfile: nothing compiled from this crate uses this dev-dependencies entry"
            ]
        );
    }

    #[test]
    fn a_deep_run_that_named_nothing_could_not_run() {
        let empty = r#"{"success":true,"unused_deps":{},"note":null}"#;
        assert!(read_udeps(empty, Path::new("/w")).is_err());
        assert!(read_udeps("not json", Path::new("/w")).is_err());
    }

    #[test]
    fn a_reason_written_beside_the_version_is_not_part_of_it() {
        let manifest = "[package]\nrust-version = \"1.85\"  # MSRV\n";
        assert_eq!(rust_version(manifest), Some("1.85".to_string()));
    }

    #[test]
    fn the_inline_table_spelling_of_inheritance_names_no_version_of_its_own() {
        let alone = "[package]\nrust-version = { workspace = true }\n";
        assert_eq!(rust_version(alone), None);
        let inherited = "[workspace.package]\nrust-version = \"1.85\"\n\
                         [package]\nrust-version = { workspace = true }\n";
        assert_eq!(rust_version(inherited), Some("1.85".to_string()));
    }

    #[test]
    fn a_table_chock_reads_nothing_from_does_not_end_the_read() {
        let manifest = "[dependencies]\nserde = \"1\"\n\n[package]\nrust-version = \"1.85\"\n";
        assert_eq!(rust_version(manifest), Some("1.85".to_string()));
    }

    #[test]
    fn a_dotted_workspace_key_declares_the_same_promise_as_the_table() {
        let manifest = "[workspace]\nmembers = []\npackage.rust-version = \"1.83\"\n";
        assert_eq!(rust_version(manifest), Some("1.83".to_string()));
    }

    #[test]
    fn a_hash_inside_quotes_is_not_a_comment() {
        assert_eq!(without_comment("a = \"x#y\"  # why"), "a = \"x#y\"");
    }

    #[test]
    fn a_packages_own_version_wins_over_the_workspaces() {
        let manifest = "[workspace.package]\nrust-version = \"1.80\"\n\n\
                        [package]\nrust-version = \"1.85\"\n";
        assert_eq!(rust_version(manifest), Some("1.85".to_string()));
    }

    #[test]
    fn a_manifest_that_promises_nothing_reports_no_version() {
        assert_eq!(rust_version("[package]\nname = \"x\"\n"), None);
    }

    #[test]
    fn every_directory_chock_skips_is_named_to_typos_rather_than_left_to_gitignore() {
        let asked = typos_invocation(&["target", "vendor"]);
        assert_eq!(
            asked,
            [
                "--format",
                "json",
                "--exclude",
                "**/target/**",
                "--exclude",
                "**/vendor/**",
                "--exclude",
                "**/lcov.info",
                "--exclude",
                "**/kani-list.json",
                "--exclude",
                "**/.chock/**"
            ]
        );
    }

    #[test]
    fn a_misspelling_is_counted_against_its_file_and_the_word_itself() {
        let out = r#"{"type":"typo","path":"./src/a.rs","line_num":12,"typo":"teh","corrections":["the"]}"#;
        let series = read_typos(&ran(out, Some(2), false)).unwrap();
        assert_eq!(series.get("src/a.rs#teh"), Some(1));
    }

    /// Per word, so fixing one of two misspellings in a file lowers the count.
    #[test]
    fn two_words_misspelled_in_one_file_are_counted_apart() {
        let out = concat!(
            r#"{"type":"typo","path":"a.rs","line_num":1,"typo":"teh","corrections":["the"]}"#,
            "\n",
            r#"{"type":"typo","path":"a.rs","line_num":9,"typo":"adn","corrections":["and"]}"#
        );
        let series = read_typos(&ran(out, Some(2), false)).unwrap();
        assert_eq!(series.get("a.rs#teh"), Some(1));
        assert_eq!(series.get("a.rs#adn"), Some(1));
    }

    #[test]
    fn one_word_misspelled_twice_in_a_file_counts_twice() {
        let out = concat!(
            r#"{"type":"typo","path":"a.rs","line_num":1,"typo":"teh","corrections":["the"]}"#,
            "\n",
            r#"{"type":"typo","path":"a.rs","line_num":40,"typo":"teh","corrections":["the"]}"#
        );
        let series = read_typos(&ran(out, Some(2), false)).unwrap();
        assert_eq!(series.get("a.rs#teh"), Some(2));
    }

    #[test]
    fn a_line_of_another_shape_is_skipped_rather_than_failing_the_gate() {
        let out = concat!(
            r#"{"type":"error","message":"could not read a file"}"#,
            "\n",
            r#"{"type":"typo","path":"a.rs","line_num":1,"typo":"teh","corrections":["the"]}"#
        );
        let series = read_typos(&ran(out, Some(2), false)).unwrap();
        assert_eq!(series.get("a.rs#teh"), Some(1));
        assert_eq!(series.len(), 1);
    }

    #[test]
    fn a_tree_with_nothing_misspelled_measures_nothing() {
        assert_eq!(read_typos(&ran("", Some(0), false)).unwrap(), Series::new());
    }

    /// typos exits 78, printing nothing, over a config it cannot parse.
    #[test]
    fn a_config_typos_cannot_parse_stops_the_gate_rather_than_measuring_zero() {
        let err = read_typos(&ran("", Some(78), false)).unwrap_err();
        assert_eq!(err, "typos could not run: it broke");
    }

    #[test]
    fn output_typos_had_cut_short_would_under_count_and_stops_the_gate() {
        let out =
            r#"{"type":"typo","path":"a.rs","line_num":1,"typo":"teh","corrections":["the"]}"#;
        let err = read_typos(&ran(out, Some(2), true)).unwrap_err();
        assert!(err.contains("more than chock keeps"), "{err}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_suite_is_started_by_the_command_the_project_named() {
        let mut ctx = ctx_here();
        ctx.runner.argv = vec!["true".to_string()];
        assert!(suite(&ctx).unwrap().success());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_failing_runner_is_a_failing_suite_and_not_a_gate_that_could_not_run() {
        let mut ctx = ctx_here();
        ctx.runner.argv = vec!["false".to_string()];
        assert!(!suite(&ctx).unwrap().success());
    }

    /// Falling back to a default would run something the project did not ask for.
    #[test]
    fn a_runner_naming_no_command_stops_the_gate() {
        let mut ctx = ctx_here();
        ctx.runner.argv = Vec::new();
        assert_eq!(
            suite(&ctx).unwrap_err(),
            "the configured `runner` names no command"
        );
    }

    fn ctx_here() -> Ctx {
        Ctx::for_root(
            std::path::PathBuf::from("."),
            crate::run::baseline::Baseline::empty("0.1.0"),
        )
    }

    #[test]
    fn a_toolchain_this_machine_lacks_is_reported_with_the_command_that_installs_it() {
        let stderr = "error: toolchain '1.61-x86_64-unknown-linux-gnu' is not installed\n\
                      help: run `rustup toolchain install 1.61-x86_64-unknown-linux-gnu` to install it\n";
        assert_eq!(
            missing_toolchain(stderr),
            Some("run `rustup toolchain install 1.61-x86_64-unknown-linux-gnu`".to_string())
        );
    }

    #[test]
    fn a_refusal_with_no_help_line_still_names_how_to_install() {
        let stderr = "error: toolchain '1.61' is not installed\n";
        assert_eq!(
            missing_toolchain(stderr),
            Some("install it with `rustup toolchain install`".to_string())
        );
    }

    #[test]
    fn a_real_compile_error_is_not_mistaken_for_a_missing_toolchain() {
        let stderr = "error[E0432]: unresolved import\n --> src/a.rs:1:5\n";
        assert_eq!(missing_toolchain(stderr), None);
    }

    #[test]
    fn a_suite_that_passed_is_a_pass_with_nothing_to_point_at() {
        let out = ran("", Some(0), false);
        assert_eq!(read_suite(&out, root()), Outcome::passed());
    }

    #[test]
    fn a_test_nextest_named_on_both_streams_is_one_finding() {
        let line = "        FAIL [   0.004s] walkdir tests::recursive::empty_follow\n";
        let out = exec::Output {
            code: Some(100),
            stdout: line.to_string(),
            stderr: line.to_string(),
            truncated: false,
        };
        let rendered: Vec<String> = read_suite(&out, root())
            .findings
            .iter()
            .map(Finding::render)
            .collect();
        assert_eq!(
            rendered,
            vec!["walkdir tests::recursive::empty_follow: this test failed"]
        );
    }

    #[test]
    fn a_suite_that_failed_names_the_tests_that_failed() {
        let out = exec::Output {
            code: Some(100),
            stdout: "        FAIL [   0.004s] chock a_rule_that_holds\n".to_string(),
            stderr: String::new(),
            truncated: false,
        };
        let outcome = read_suite(&out, root());
        assert!(!outcome.passed);
        assert_eq!(
            outcome
                .findings
                .iter()
                .map(Finding::render)
                .collect::<Vec<_>>(),
            vec!["chock a_rule_that_holds: this test failed"]
        );
    }

    #[test]
    fn a_runner_stage_that_named_a_measure_getting_worse_reports_that_measure() {
        let out = exec::Output {
            code: Some(1),
            stdout: "worse sparse 122 -> 130\nworse dead_items 0 -> 2\n".to_string(),
            stderr: "Error: 2 measure(s) got worse\n".to_string(),
            truncated: false,
        };
        let outcome = read_suite(&out, root());
        assert!(!outcome.passed);
        assert_eq!(
            outcome
                .findings
                .iter()
                .map(Finding::render)
                .collect::<Vec<_>>(),
            vec![
                "dead_items: got worse: 0 to 2",
                "sparse: got worse: 122 to 130"
            ]
        );
    }

    #[test]
    fn a_measure_named_on_both_streams_is_reported_once() {
        let out = exec::Output {
            code: Some(1),
            stdout: "worse sparse 1 -> 2\n".to_string(),
            stderr: "worse sparse 1 -> 2\nError: 1 measure(s) got worse\n".to_string(),
            truncated: false,
        };
        let rendered: Vec<String> = read_suite(&out, root())
            .findings
            .iter()
            .map(Finding::render)
            .collect();
        assert_eq!(rendered, vec!["sparse: got worse: 1 to 2"]);
    }

    #[test]
    fn a_line_naming_no_movement_is_not_read_as_a_measure() {
        assert_eq!(worse("worse things have happened"), None);
        assert_eq!(worse("worse sparse"), None);
        assert!(worse("worse sparse 1 -> 2").is_some());
    }

    #[test]
    fn a_crate_that_would_not_build_is_reported_at_the_span_rustc_gave() {
        let out = exec::Output {
            code: Some(101),
            stdout: String::new(),
            stderr: "error[E0432]: unresolved import\n --> /w/proj/src/a.rs:3:5\n".to_string(),
            truncated: false,
        };
        let outcome = read_suite(&out, root());
        assert!(!outcome.passed);
        assert_eq!(
            outcome
                .findings
                .iter()
                .map(Finding::render)
                .collect::<Vec<_>>(),
            vec!["src/a.rs:3: E0432: unresolved import"]
        );
    }

    #[test]
    fn a_tree_the_compiler_finds_no_unused_dependency_in_passes_the_deep_check() {
        let out = ran("", Some(0), false);
        assert_eq!(read_deep(&out, root()).unwrap(), Outcome::passed());
    }

    #[test]
    fn output_udeps_had_cut_short_stops_the_deep_check() {
        let out = ran("{}", Some(1), true);
        let err = read_deep(&out, root()).unwrap_err();
        assert!(err.contains("more than chock keeps"), "{err}");
    }

    #[test]
    fn a_dependency_past_the_promised_rust_is_named_rather_than_left_in_the_tail() {
        let said = "error: rustc 1.94.1 is not supported by the following package: \
                    sysinfo@0.39.6 requires rustc 1.95";
        assert_eq!(
            out_of_reach(said),
            Some(("sysinfo@0.39.6".to_string(), "1.95".to_string()))
        );
        let mut out = ran("", Some(101), false);
        out.stderr = said.to_string();
        let outcome = read_msrv(&out, "1.94", root()).unwrap();
        assert!(!outcome.passed);
        let shown: Vec<String> = outcome.findings.iter().map(Finding::render).collect();
        assert_eq!(
            shown,
            vec![
                "Cargo.toml: sysinfo@0.39.6: needs Rust 1.95, past the 1.94 this crate promises in \
                 its manifest"
                    .to_string()
            ]
        );
        // A build that failed for another reason is not this one.
        assert_eq!(out_of_reach("error[E0433]: failed to resolve"), None);
    }

    #[test]
    fn a_udeps_that_could_not_build_carries_its_reason_rather_than_a_parse_error() {
        let mut out = ran("", Some(101), false);
        out.stderr = "error[E0433]: failed to resolve: use of undeclared crate `nope`".to_string();
        let outcome = read_deep(&out, root()).unwrap();
        assert!(!outcome.passed);
        assert!(outcome.findings.is_empty(), "{:?}", outcome.findings);
        // Carried so `run` can quote it in the reason it could not run.
        let said = outcome.said.unwrap_or_default();
        assert!(said.contains("undeclared crate `nope`"), "{said}");
    }

    #[test]
    fn a_rustup_that_could_not_list_its_toolchains_is_named_rather_than_read_as_empty() {
        let broken = exec::Output {
            code: Some(1),
            stdout: String::new(),
            stderr: "error: could not open the rustup home directory\n".to_string(),
            truncated: false,
        };
        assert_eq!(
            tests_the_promise(&broken, "1.94").unwrap_err(),
            "rustup could not say which toolchains are installed: error: could not open the \
             rustup home directory"
        );
        let holds_none = exec::Output {
            code: Some(0),
            stdout: "stable-x86_64-unknown-linux-gnu (default)\n".to_string(),
            stderr: String::new(),
            truncated: false,
        };
        assert_eq!(
            tests_the_promise(&holds_none, "1.94").unwrap_err(),
            "this crate promises Rust 1.94, which this machine does not have: run `rustup \
             toolchain install 1.94`"
        );
        let holds_it = exec::Output {
            code: Some(0),
            stdout: "1.94.1-x86_64-unknown-linux-gnu\n".to_string(),
            stderr: String::new(),
            truncated: false,
        };
        assert_eq!(
            tests_the_promise(&holds_it, "1.94").unwrap(),
            "1.94.1-x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn a_promise_naming_no_patch_is_kept_by_the_lowest_patch_release_installed() {
        let listed = "stable-x86_64-unknown-linux-gnu (default)\n\
                      1.94.1-x86_64-unknown-linux-gnu\n\
                      1.94.3-x86_64-unknown-linux-gnu\n\
                      nightly-x86_64-unknown-linux-gnu\n";
        assert_eq!(
            floor("1.94", listed),
            Some("1.94.1-x86_64-unknown-linux-gnu".to_string())
        );
        // The release itself, where rustup named it without a patch.
        assert_eq!(
            floor("1.85", "1.85-x86_64-unknown-linux-gnu\n"),
            Some("1.85-x86_64-unknown-linux-gnu".to_string())
        );
    }

    /// Checking with a later patch would prove a promise the crate did not make.
    #[test]
    fn a_promise_naming_a_patch_is_kept_by_that_release_and_no_later_one() {
        let listed = "1.94.1-x86_64-unknown-linux-gnu\n1.94.2-x86_64-unknown-linux-gnu\n";
        assert_eq!(
            floor("1.94.1", listed),
            Some("1.94.1-x86_64-unknown-linux-gnu".to_string())
        );
        assert_eq!(floor("1.94.4", listed), None);
        // `1.94` alone is `1.94.0`, which is older than the promise.
        assert_eq!(floor("1.94.1", "1.94-x86_64-unknown-linux-gnu\n"), None);
    }

    #[test]
    fn a_named_channel_keeps_no_numbered_promise() {
        let listed = "stable-x86_64-unknown-linux-gnu (default)\n\
                      nightly-2026-01-01-x86_64-unknown-linux-gnu\n\
                      1.9.0-x86_64-unknown-linux-gnu\n";
        assert_eq!(floor("1.94", listed), None);
    }

    #[test]
    fn a_missing_toolchain_stops_the_msrv_gate_and_names_the_version_promised() {
        let out = exec::Output {
            code: Some(1),
            stdout: String::new(),
            stderr: "error: toolchain '1.85-x86_64-unknown-linux-gnu' is not installed\n\
                     help: run `rustup toolchain install 1.85` to install it\n"
                .to_string(),
            truncated: false,
        };
        let err = read_msrv(&out, "1.85", root()).unwrap_err();
        assert_eq!(
            err,
            "this crate promises Rust 1.85, which this machine does not have: \
             run `rustup toolchain install 1.85`"
        );
    }

    #[test]
    fn a_crate_that_still_builds_on_its_oldest_rust_passes() {
        let out = ran("", Some(0), false);
        assert_eq!(read_msrv(&out, "1.85", root()).unwrap(), Outcome::passed());
    }

    #[test]
    fn a_crate_that_no_longer_builds_on_its_oldest_rust_is_reported_at_the_span() {
        let out = exec::Output {
            code: Some(101),
            stdout: String::new(),
            stderr: "error[E0658]: let...else is unstable\n --> /w/proj/src/a.rs:9:5\n".to_string(),
            truncated: false,
        };
        let outcome = read_msrv(&out, "1.64", root()).unwrap();
        assert!(!outcome.passed);
        assert_eq!(
            outcome
                .findings
                .iter()
                .map(Finding::render)
                .collect::<Vec<_>>(),
            vec!["src/a.rs:9: E0658: let...else is unstable"]
        );
    }

    fn said(outcome: &Outcome) -> Vec<String> {
        outcome.findings.iter().map(Finding::render).collect()
    }

    /// The false green this answers: a file to reformat ended `lint` before clippy ran.
    #[test]
    fn clippy_findings_follow_the_formatting_findings_and_never_wait_behind_them() {
        let unformatted = || Outcome::failed(vec![Finding::at("src/a.rs", UNFORMATTED)]);
        let reformat = format!("src/a.rs: {UNFORMATTED}");
        let clippy = Outcome::failed(vec![Finding::at("src/b.rs", "a needless clone")]);
        let both = linted(unformatted(), clippy.clone());
        assert_eq!(
            (both.passed, said(&both)),
            (
                false,
                vec![reformat.clone(), "src/b.rs: a needless clone".to_string()]
            )
        );
        let quiet = linted(unformatted(), Outcome::passed());
        assert_eq!(
            (quiet.passed, said(&quiet)),
            (false, vec![reformat.clone()])
        );
        let unread = linted(unformatted(), Outcome::failed(Vec::new()));
        assert_eq!(
            said(&unread),
            vec![reformat, format!("clippy: {CLIPPY_UNREAD}")]
        );
        assert_eq!(linted(Outcome::passed(), clippy.clone()), clippy);
    }

    #[test]
    fn every_tool_gate_names_how_to_rerun_itself() {
        for gate in all() {
            assert!(
                crate::run::rerun(gate.name).contains(gate.name),
                "{} rerun is wrong",
                gate.name
            );
        }
    }

    #[test]
    fn only_the_gates_counting_what_already_exists_hold_a_number() {
        for gate in all() {
            let ratchets = gate.counts_in().is_some();
            let counts = matches!(gate.name, "mutest" | "typos" | "sort" | "unused");
            assert_eq!(ratchets, counts, "{}", gate.name);
        }
    }

    #[test]
    fn the_slow_and_the_reporting_gates_are_kept_out_of_the_default_set() {
        assert_eq!(MUTEST.group, Group::OptIn);
        assert_eq!(UNUSED_DEEP.group, Group::OptIn);
        assert_eq!(BSIZE.group, Group::Instrument);
    }

    #[test]
    fn a_rustfmt_hunk_header_names_its_file_whichever_release_wrote_it() {
        assert_eq!(diffed_file("Diff in /w/src/a.rs:624:"), Some("/w/src/a.rs"));
        assert_eq!(
            diffed_file("Diff in /w/src/a.rs at line 624:"),
            Some("/w/src/a.rs")
        );
        assert_eq!(diffed_file("-  let x = 1;"), None);
        assert_eq!(diffed_file("Diff in /w/src/a.rs"), None);
    }

    #[test]
    fn a_file_with_many_hunks_is_named_once() {
        let out = "Diff in /w/src/a.rs:3:\nDiff in /w/src/a.rs:99:\nDiff in /w/src/b.rs:7:\n";
        let rendered: Vec<String> = unformatted(out, Path::new("/w"))
            .iter()
            .map(Finding::render)
            .collect();
        assert_eq!(
            rendered,
            vec![
                format!("src/a.rs: {UNFORMATTED}"),
                format!("src/b.rs: {UNFORMATTED}"),
            ]
        );
    }

    #[test]
    fn a_missing_rustfmt_could_not_run_rather_than_calling_the_code_unformatted() {
        let out = exec::Output {
            code: Some(1),
            stdout: String::new(),
            stderr: "error: 'cargo-fmt' is not installed for the toolchain '1.98.1'\n".to_string(),
            truncated: false,
        };
        let refused = formatted(&out, Path::new("/w")).unwrap_err();
        assert!(
            refused.contains("'cargo-fmt' is not installed"),
            "{refused}"
        );
    }

    #[test]
    fn only_a_hunk_trips_formatting_and_a_clean_check_passes() {
        let hunk = exec::Output {
            code: Some(1),
            stdout: "Diff in /w/src/a.rs:3:\n".to_string(),
            stderr: String::new(),
            truncated: false,
        };
        let tripped = formatted(&hunk, Path::new("/w")).unwrap();
        let rendered: Vec<String> = tripped.findings.iter().map(Finding::render).collect();
        assert_eq!(rendered, vec![format!("src/a.rs: {UNFORMATTED}")]);
        assert!(!tripped.passed);

        let clean = exec::Output {
            code: Some(0),
            ..hunk
        };
        assert!(formatted(&clean, Path::new("/w")).unwrap().passed);
    }

    fn ran(stdout: &str, code: Option<i32>, truncated: bool) -> exec::Output {
        exec::Output {
            code,
            stdout: stdout.to_string(),
            stderr: "it broke".to_string(),
            truncated,
        }
    }

    fn coloured(message: &str) -> exec::Output {
        exec::Output {
            code: Some(1),
            stdout: String::new(),
            stderr: format!("\u{1b}[0m\u{1b}[31merror: \u{1b}[0m{message}\n"),
            truncated: false,
        }
    }

    #[test]
    fn the_tests_that_failed_only_the_second_time_are_named() {
        let second = exec::Output {
            code: Some(100),
            stdout: "        FAIL [   0.005s] ws db::tests::a_second_open_reuses_the_file\n"
                .to_string(),
            stderr: "        FAIL [   1.200s] ws fs::tests::a_scratch_dir_is_removed\n".to_string(),
            truncated: false,
        };
        let findings = failed_twice(&second, Path::new("/w"));
        let named: Vec<&str> = findings
            .iter()
            .filter_map(|finding| finding.item.as_deref())
            .collect();
        assert_eq!(
            named,
            vec![
                "ws db::tests::a_second_open_reuses_the_file",
                "ws fs::tests::a_scratch_dir_is_removed"
            ]
        );
    }

    /// `run` reads a failure with no finding as could-not-run, so the last resort reports one.
    #[test]
    fn a_second_run_that_named_no_test_still_reports_that_it_failed() {
        let second = exec::Output {
            code: Some(101),
            stdout: String::new(),
            stderr: "killed".to_string(),
            truncated: false,
        };
        let findings = failed_twice(&second, Path::new("/w"));
        let named: Vec<&str> = findings
            .iter()
            .filter_map(|finding| finding.item.as_deref())
            .collect();
        assert_eq!(named, vec!["idempotent"]);
    }

    #[test]
    fn a_tree_every_dependency_stays_inside_passes() {
        assert!(read_acl(&ran("", Some(0), false)).unwrap().passed);
    }

    #[test]
    fn a_tree_with_no_policy_file_could_not_run_rather_than_tripping() {
        for said in [
            "Config file `cackle.toml` not found",
            "Failed to open cackle.toml",
            "cackle.toml: No such file or directory",
        ] {
            let err = read_acl(&ran(said, Some(1), false)).unwrap_err();
            assert_eq!(
                err,
                "no cackle.toml, so no dependency is held to anything — write one with \
                 `cargo acl --no-ui --auto-accept-fixes`",
                "{said}"
            );
        }
    }

    #[test]
    fn a_dependency_reaching_past_its_permissions_is_reported_with_the_api_it_reached() {
        let out = ran(
            "ERROR: 'proc-macro2' uses disallowed API `process`\n\
             ERROR: 'proc-macro2' uses disallowed API `fs`\n",
            Some(255),
            false,
        );
        let found = read_acl(&out).unwrap();
        assert!(!found.passed);
        assert_eq!(
            found
                .findings
                .iter()
                .map(Finding::render)
                .collect::<Vec<_>>(),
            [
                "cackle.toml: 'proc-macro2' uses disallowed API `process`",
                "cackle.toml: 'proc-macro2' uses disallowed API `fs`"
            ]
        );
    }

    #[test]
    fn a_tree_with_no_config_could_not_run_rather_than_passing() {
        let mut out = ran("", Some(255), false);
        out.stderr = "Error: Failed to open /x/cackle.toml: No such file".to_string();
        let err = read_acl(&out).unwrap_err();
        assert!(err.contains("no cackle.toml"), "{err}");
        assert!(err.contains("--auto-accept-fixes"), "{err}");
    }

    #[test]
    fn only_a_failure_to_open_the_config_itself_reads_as_the_config_being_absent() {
        let mut other = ran("", Some(255), false);
        other.stderr = "Error: Failed to open /x/lcov.info: No such file".to_string();
        assert!(!read_acl(&other).unwrap_err().contains("no cackle.toml"));

        let mut named = ran("", Some(255), false);
        named.stderr = "Error: cackle.toml is version 3, which is newer".to_string();
        assert!(!read_acl(&named).unwrap_err().contains("no cackle.toml"));
    }

    #[test]
    fn a_sandbox_that_could_not_start_is_refused_rather_than_passed() {
        let out = ran(
            "bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted\n",
            Some(255),
            false,
        );
        let err = read_acl(&out).unwrap_err();
        assert!(err.contains("bubblewrap"), "{err}");
        assert!(
            err.contains("apparmor_restrict_unprivileged_userns"),
            "{err}"
        );
    }

    #[test]
    fn a_refusal_naming_no_package_is_refused_rather_than_reported_as_a_finding() {
        let out = ran("something went wrong\n", Some(255), false);
        assert!(read_acl(&out).is_err());
    }

    #[test]
    fn a_manifest_cargo_sort_accepts_measures_nothing() {
        assert_eq!(read_sort(&ran("", Some(0), false)).unwrap(), Series::new());
    }

    #[test]
    fn an_unsorted_manifest_is_keyed_by_the_package_cargo_sort_names() {
        let out = coloured("Dependencies for chock are not sorted");
        let series = read_sort(&out).unwrap();
        assert_eq!(series.get("chock"), Some(1));
    }

    /// cargo-sort ends with a summary line that names no package.
    #[test]
    fn the_trailing_summary_line_is_not_a_second_key() {
        let mut out = coloured("Dependencies for chock are not sorted");
        out.stderr
            .push_str("error: Some Cargo.toml files are not sorted or formatted\n");
        let series = read_sort(&out).unwrap();
        assert_eq!(series.0.keys().cloned().collect::<Vec<_>>(), ["chock"]);
    }

    #[test]
    fn an_objection_naming_no_package_could_not_be_measured() {
        assert!(read_sort(&coloured("something broke")).is_err());
    }

    #[test]
    fn a_refusal_with_no_message_is_a_gate_that_could_not_measure() {
        let why = read_sort(&ran("", Some(1), false)).unwrap_err();
        assert!(why.contains("without saying which"), "{why}");
    }

    #[test]
    fn a_cut_objection_from_cargo_sort_is_refused_since_its_count_would_be_partial() {
        let why = read_sort(&ran("", Some(1), true)).unwrap_err();
        assert!(why.contains("printed more than chock keeps"), "{why}");
    }

    const FAILED_TEST: &str = "        FAIL [   0.004s] chock a_rule_that_holds\n";

    #[test]
    fn a_list_read_from_a_cut_output_ends_with_a_finding_that_says_it_is_partial() {
        let partial = format!("output: {CUT}");
        let erred = "error[E0425]: cannot find value\n --> /w/proj/a.rs:3:5\n";
        assert_eq!(
            said(&verdict(&ran(erred, Some(101), true), root())),
            ["a.rs:3: E0425: cannot find value", partial.as_str()]
        );
        assert_eq!(
            said(&read_suite(&ran(FAILED_TEST, Some(100), true), root())),
            [
                "chock a_rule_that_holds: this test failed",
                partial.as_str()
            ]
        );
        let differs = ran("Diff in /w/proj/src/a.rs:624:\n", Some(1), true);
        assert_eq!(
            said(&formatted(&differs, root()).unwrap()),
            [format!("src/a.rs: {UNFORMATTED}"), partial.clone()]
        );
        let reached = ran("ERROR: 'syn' uses disallowed API `fs`\n", Some(255), true);
        assert_eq!(
            said(&read_acl(&reached).unwrap()),
            [
                "cackle.toml: 'syn' uses disallowed API `fs`",
                partial.as_str()
            ]
        );
    }

    #[test]
    fn a_whole_output_and_a_cut_one_nothing_was_read_from_gain_no_such_finding() {
        assert_eq!(
            said(&read_suite(&ran(FAILED_TEST, Some(100), false), root())),
            ["chock a_rule_that_holds: this test failed"]
        );
        let silent = verdict(&ran("", Some(101), true), root());
        assert_eq!(silent.findings, Vec::new());
    }
}
