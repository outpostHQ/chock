//! Lines no test executed, per file, read from the LCOV report the coverage run writes. A ratchet
//! rather than a percentage floor, so an existing project can adopt it.

pub mod crap;

use std::collections::BTreeMap;
use std::path::Path;

use crate::gates::metrics::prodlines;
use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::report::Finding;
use crate::run::verdicts::Reads;
use crate::run::{Ctx, Gate, Group, Kind, Measurement};

pub const FILE: &str = "lcov.info";

pub const GATE: Gate = Gate {
    name: "coverage",
    about: "lines no test executed, per file, against the count recorded",
    group: Group::Quality,
    builds: true,
    // The command that writes the report names its own tools; a project's own says `coverage_tools`.
    reads: Some(Reads::tree_and(&["cargo"]).and_coverage()),
    kind: Kind::AnnotatedRatchet {
        measure,
        keys: Keys::Items,
        unit: "uncovered line(s)",
    },
};

fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let path = ensure(ctx)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {FILE}: {e}"))?;
    let read = uncovered(&text, &ctx.root)?;
    let said = listed(&ctx.root, &read.lines)
        .map(|listed| format!(" (every file's lines: {listed})"))
        .unwrap_or_default();
    let mut measured = read.held_to(&ctx.named_against(GATE.name));
    for finding in &mut measured.findings {
        finding.message.push_str(&said);
    }
    measured.unmeasured.extend(unmerged(ctx));
    Ok(measured)
}

/// Where a run leaves the unrun lines of every file, so that two machines' lists can be compared.
const LISTED: &str = "target/test-scratch/chock/coverage.txt";

/// Writes `file: line(s) ...` for each file with an unrun line to `LISTED`, and gives its path;
/// `None` when the file cannot be written.
fn listed(root: &Path, lines: &BTreeMap<String, Vec<u32>>) -> Option<&'static str> {
    let each = lines
        .iter()
        .filter(|(_, lines)| !lines.is_empty())
        .map(|(file, lines)| format!("{file}: {}\n", runs(lines)));
    let text: String = each.collect();
    let path = root.join(LISTED);
    let dir = path.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    crate::project::document::write(&path, &text).ok()?;
    Some(LISTED)
}

/// The LCOV report for this run, produced once and shared by every gate that reads it, failure
/// included. A report from an earlier invocation is never reused.
pub fn ensure(ctx: &Ctx) -> Result<std::path::PathBuf, String> {
    let report = ctx.coverage.report.get_or_init(|| write_report(ctx));
    match report {
        Ok(report) => report.current(ctx),
        Err(why) => Err(why.clone()),
    }
}

#[derive(Debug)]
pub(crate) struct Report {
    root: std::path::PathBuf,
    argv: Vec<String>,
    digest: u64,
    /// Each profile the run left out of its merge, as `path: reason`.
    unmerged: Vec<String>,
}

impl Report {
    fn current(&self, ctx: &Ctx) -> Result<std::path::PathBuf, String> {
        if self.root != ctx.root || self.argv != ctx.coverage.argv {
            return Err("the coverage context changed after its measurement".to_string());
        }
        let path = ctx.root.join(FILE);
        if report_digest(&path)? != self.digest {
            return Err(format!(
                "{FILE} changed after the coverage command completed"
            ));
        }
        Ok(path)
    }
}

fn report_digest(path: &Path) -> Result<u64, String> {
    use std::hash::{Hash, Hasher};
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {FILE}: {e}"))?;
    let mut digest = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut digest);
    Ok(digest.finish())
}

fn write_report(ctx: &Ctx) -> Result<Report, String> {
    let path = ctx.root.join(FILE);
    let argv = substituted(&ctx.coverage.argv, FILE);
    let (program, args) = argv
        .split_first()
        .ok_or("the configured `coverage` names no command")?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // Removed before the run rather than compared by time: file times are coarser than the clock.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot remove the old {FILE}: {e}")),
    }
    let flags = profdata_flags(&std::env::var("LLVM_PROFDATA_FLAGS").unwrap_or_default());
    let run = crate::exec::run_env(
        program,
        &args,
        &ctx.root,
        &[("LLVM_PROFDATA_FLAGS", &flags)],
    )
    .map_err(|e| format!("{e} — this gate needs a command that writes {FILE}"))?;
    if !run.success() {
        return Err(format!(
            "the coverage run failed, so there is nothing to measure: {}",
            run.failure_details()
        ));
    }
    wrote(&path)?;
    Ok(Report {
        root: ctx.root.clone(),
        argv: ctx.coverage.argv.clone(),
        digest: report_digest(&path)?,
        unmerged: left_out_profiles(&run.stderr),
    })
}

/// The flags for llvm-profdata, which then leaves out a profile it cannot read (one cut short) and
/// merges the rest. A failure mode that the project set stays.
fn profdata_flags(set: &str) -> String {
    match set.contains("failure-mode") {
        true => set.to_string(),
        false => format!("{set} -failure-mode=all").trim_start().to_string(),
    }
}

/// Each profile the merge left out, from llvm-profdata's `warning: <path>: <reason>` lines.
fn left_out_profiles(stderr: &str) -> Vec<String> {
    let said = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("warning: "));
    said.filter(|said| said.contains(".profraw: "))
        .map(|said| format!("{said} (left out of the merge)"))
        .collect()
}

/// Each profile the coverage run left out of its merge, as `path: reason`.
pub fn unmerged(ctx: &Ctx) -> Vec<String> {
    let report = ctx
        .coverage
        .report
        .get()
        .and_then(|report| report.as_ref().ok());
    report
        .map(|report| report.unmerged.clone())
        .unwrap_or_default()
}

/// Refuses a record from a run that left a profile out: what that profile ran would read as debt.
pub fn merged_whole(ctx: &Ctx) -> Result<(), String> {
    let left = unmerged(ctx);
    match left.is_empty() {
        true => Ok(()),
        false => Err(crate::run::unread_of(&left)),
    }
}

/// Whether the run wrote its report; a command can exit clean without writing one.
fn wrote(path: &Path) -> Result<(), String> {
    if path.is_file() {
        return Ok(());
    }
    Err(format!(
        "the coverage run exited clean without writing {FILE}, so nothing was measured"
    ))
}

/// The project's coverage command with `{lcov}` replaced by the report path.
#[must_use]
fn substituted(argv: &[String], path: &str) -> Vec<String> {
    argv.iter()
        .map(|arg| arg.replace(crate::run::LCOV, path))
        .collect()
}

/// The count of uncovered lines per file, and the lines themselves where the report has them.
#[derive(Debug, Default, PartialEq, Eq)]
struct Uncovered {
    series: Series,
    lines: BTreeMap<String, Vec<u32>>,
}

impl Uncovered {
    /// The counts, and for each file whose count rose past its record the lines a test has to run.
    fn held_to(self, was: &Series) -> Measurement {
        let count = |of: &Series, file: &str| of.get(file).unwrap_or(0);
        let findings = self
            .lines
            .iter()
            .filter(|(file, _)| count(&self.series, file) > count(was, file))
            .filter_map(|(file, lines)| Some((file, lines, *lines.first()?)))
            .map(|(file, lines, first)| {
                Finding::at(file, &format!("no test ran {}", runs(lines))).line(first)
            })
            .collect();
        Measurement::of(self.series, findings)
    }

    /// Keeps one file's record, without the code only tests compile, as the walk leaves it out: a
    /// `tests.rs` goes, and the lines of a `#[cfg(test)]` module count as run.
    fn keep(&mut self, root: &Path, name: String, ran: &mut BTreeMap<u32, bool>, read: (u64, u64)) {
        let base = Path::new(&name).file_name().and_then(|base| base.to_str());
        if base.is_some_and(prodlines::is_test_file) {
            return;
        }
        let src = std::fs::read_to_string(root.join(&name)).unwrap_or_default();
        let spans = prodlines::test_spans(&src).unwrap_or_default();
        for (at, covered) in ran.iter_mut() {
            let at = usize::try_from(*at).unwrap_or(usize::MAX);
            *covered |= spans.iter().any(|(from, to)| (*from..=*to).contains(&at));
        }
        self.series.set(&name, missed(ran, read.0, read.1));
        self.lines.insert(name, never(ran));
    }
}

/// Line numbers as runs: 3, 4, 5 and 9 read `line(s) 3-5, 9`.
fn runs(lines: &[u32]) -> String {
    let mut spans: Vec<(u32, u32)> = Vec::new();
    for &at in lines {
        match spans.last_mut() {
            Some((_, end)) if end.saturating_add(1) == at => *end = at,
            _ => spans.push((at, at)),
        }
    }
    let each: Vec<String> = spans.into_iter().map(span).collect();
    format!("line(s) {}", each.join(", "))
}

fn span((from, to): (u32, u32)) -> String {
    match from == to {
        true => from.to_string(),
        false => format!("{from}-{to}"),
    }
}

/// Uncovered lines per file, as a count rather than a percentage.
fn uncovered(lcov: &str, root: &Path) -> Result<Uncovered, String> {
    let mut read = Uncovered::default();
    let mut file: Option<String> = None;
    let mut found = 0_u64;
    let mut hit = 0_u64;
    let mut ran: BTreeMap<u32, bool> = BTreeMap::new();
    let mut records = 0_usize;
    for line in lcov.lines() {
        if let Some(path) = line.strip_prefix("SF:") {
            file = Some(project::relative(root, Path::new(path)));
        } else if let Some(count) = line.strip_prefix("LF:") {
            found = count.trim().parse().map_err(|_| unreadable(line))?;
        } else if let Some(count) = line.strip_prefix("LH:") {
            hit = count.trim().parse().map_err(|_| unreadable(line))?;
        } else if let Some(record) = line.strip_prefix("DA:") {
            let (at, times) = executed(record).ok_or_else(|| unreadable(line))?;
            *ran.entry(at).or_insert(false) |= times > 0;
        } else if line.trim() == "end_of_record" {
            let Some(name) = file.take() else {
                return Err("a record ended before any SF: named its file".to_string());
            };
            read.keep(root, name, &mut ran, (found, hit));
            records = records.saturating_add(1);
            found = 0;
            hit = 0;
            ran.clear();
        }
    }
    if records == 0 {
        return Err(format!(
            "{FILE} holds no coverage record, so nothing was measured"
        ));
    }
    Ok(read)
}

/// The lines no record ever ran, in order.
fn never(ran: &BTreeMap<u32, bool>) -> Vec<u32> {
    let missed = ran.iter().filter(|(_, covered)| !**covered);
    missed.map(|(at, _)| *at).collect()
}

/// The line and execution count of a `DA:<line>,<times>` record, ignoring any checksum after it.
fn executed(record: &str) -> Option<(u32, u64)> {
    let mut fields = record.trim().split(',');
    let at = fields.next()?.parse().ok()?;
    let times = fields.next()?.parse().ok()?;
    Some((at, times))
}

/// Lines no record ever ran, across test binaries and generic instantiations. `LF`/`LH` count
/// regions, so they are used only when no `DA` record came.
fn missed(ran: &BTreeMap<u32, bool>, found: u64, hit: u64) -> u64 {
    if ran.is_empty() {
        return found.saturating_sub(hit);
    }
    let never = ran.values().filter(|covered| !**covered).count();
    u64::try_from(never).unwrap_or(u64::MAX)
}

fn unreadable(line: &str) -> String {
    format!("{FILE} holds a count chock cannot read: {line}")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn the_report_path_is_substituted_into_the_command_that_writes_it() {
        let argv = ["bin/coverage", "--all-targets", "--lcov", "{lcov}"].map(String::from);
        assert_eq!(
            substituted(&argv, "lcov.info"),
            vec![
                "bin/coverage".to_string(),
                "--all-targets".to_string(),
                "--lcov".to_string(),
                "lcov.info".to_string()
            ]
        );
    }

    #[test]
    fn a_command_naming_no_report_path_is_left_exactly_as_it_came() {
        let argv = ["bin/coverage".to_string()];
        assert_eq!(
            substituted(&argv, "lcov.info"),
            vec!["bin/coverage".to_string()]
        );
    }

    #[test]
    fn the_default_command_names_the_report_path_so_one_code_path_serves_both() {
        assert!(
            crate::run::default_coverage()
                .argv
                .contains(&crate::run::LCOV.to_string())
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_run_that_wrote_no_report_is_refused_rather_than_read() {
        let dir = crate::testdir::make("coverage-absent");
        let err = wrote(&dir.join(FILE)).unwrap_err();
        assert_eq!(
            err,
            "the coverage run exited clean without writing lcov.info, so nothing was measured"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_report_the_run_wrote_is_accepted() {
        let dir = crate::testdir::make("coverage-written");
        std::fs::write(dir.join(FILE), "").unwrap();
        assert_eq!(wrote(&dir.join(FILE)), Ok(()));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_directory_where_the_report_should_be_is_not_a_report() {
        let dir = crate::testdir::make("coverage-directory");
        std::fs::create_dir_all(dir.join(FILE)).unwrap();
        assert!(wrote(&dir.join(FILE)).is_err());
    }

    const TWO: &str = "SF:/w/src/a.rs\nLF:100\nLH:80\nend_of_record\n\
                       SF:/w/src/b.rs\nLF:10\nLH:10\nend_of_record\n";

    #[test]
    fn a_summary_with_no_per_line_detail_is_still_read() {
        let series = uncovered(TWO, Path::new("/w")).unwrap().series;
        assert_eq!(series.get("src/a.rs"), Some(20));
        assert_eq!(series.get("src/b.rs"), Some(0));
    }

    #[test]
    fn a_line_any_record_executed_is_covered_however_many_records_missed_it() {
        let both = "SF:/w/src/a.rs\nDA:1,0\nDA:1,7\nDA:2,0\nLF:2\nLH:1\nend_of_record\n";
        assert_eq!(
            uncovered(both, Path::new("/w"))
                .unwrap()
                .series
                .get("src/a.rs"),
            Some(1),
            "line 1 ran in one record, so only line 2 is missed"
        );
    }

    #[test]
    fn the_per_line_detail_wins_over_a_summary_that_counts_regions() {
        let disagreeing = "SF:/w/src/a.rs\nDA:1,3\nDA:2,0\nLF:9\nLH:4\nend_of_record\n";
        assert_eq!(
            uncovered(disagreeing, Path::new("/w"))
                .unwrap()
                .series
                .get("src/a.rs"),
            Some(1),
            "one line never ran; LF - LH would have said five"
        );
    }

    const DETAILED: &str = "SF:/w/src/a.rs\nDA:1,1\nDA:2,0\nDA:3,0\nDA:7,0\nend_of_record\n\
                            SF:/w/src/b.rs\nDA:4,0\nend_of_record\n";

    #[test]
    fn a_file_past_its_record_names_its_lines_and_a_file_at_its_record_names_none() {
        let mut was = Series::new();
        was.set("src/a.rs", 2);
        was.set("src/b.rs", 1);
        let read = uncovered(DETAILED, Path::new("/w")).unwrap().held_to(&was);
        assert_eq!(read.series.get("src/a.rs"), Some(3));
        let said: Vec<String> = Finding::rendered(&read.findings);
        assert_eq!(said, ["src/a.rs:2: no test ran line(s) 2-3, 7"]);
    }

    /// Every file's unrun lines go to one file, so the list of a CI runner can be compared with
    /// the list of another machine; a file with no unrun line is left out.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_run_lists_the_unrun_lines_of_every_file() {
        let root = crate::testdir::make("coverage-listed");
        let mut lines = uncovered(DETAILED, Path::new("/w")).unwrap().lines;
        lines.insert("src/c.rs".to_string(), Vec::new());
        assert_eq!(listed(&root, &lines), Some(LISTED));
        let text = std::fs::read_to_string(root.join(LISTED)).unwrap();
        assert_eq!(text, "src/a.rs: line(s) 2-3, 7\nsrc/b.rs: line(s) 4\n");
        let blocked = crate::testdir::make("coverage-listed-blocked");
        std::fs::write(blocked.join("target"), "a file, not a directory").unwrap();
        assert_eq!(listed(&blocked, &lines), None);
    }

    #[test]
    fn a_file_the_record_does_not_hold_names_every_line_no_test_ran() {
        let read = uncovered(DETAILED, Path::new("/w")).unwrap();
        let said: Vec<String> = Finding::rendered(&read.held_to(&Series::new()).findings);
        assert_eq!(
            said,
            [
                "src/a.rs:2: no test ran line(s) 2-3, 7",
                "src/b.rs:4: no test ran line(s) 4"
            ]
        );
    }

    #[test]
    fn a_summary_with_no_per_line_detail_has_no_lines_to_name() {
        let read = uncovered(TWO, Path::new("/w")).unwrap();
        let read = read.held_to(&Series::new());
        assert_eq!(read.series.get("src/a.rs"), Some(20));
        assert!(read.findings.is_empty());
    }

    #[test]
    fn neighbouring_lines_read_as_one_run_and_a_lone_line_as_itself() {
        assert_eq!(runs(&[3, 4, 5, 9]), "line(s) 3-5, 9");
        assert_eq!(runs(&[7]), "line(s) 7");
        assert_eq!(runs(&[1, 3]), "line(s) 1, 3");
    }

    #[test]
    fn a_record_carrying_a_checksum_is_read_past_rather_than_refused() {
        assert_eq!(executed("7,2,f0b3a1"), Some((7, 2)));
        assert_eq!(executed("7,0"), Some((7, 0)));
        assert_eq!(executed("nonsense"), None);
    }

    /// A test module counts the same in its parent file and in its own `tests.rs`: not at all.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_code_only_tests_compile_is_left_out_as_the_walk_leaves_it_out() {
        let parent = "fn a() {}\nfn b() {}\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";
        let files = [
            ("src/a.rs", parent),
            ("src/tests.rs", "fn t() {}\n"),
            ("src/only.rs", "#![cfg(test)]\nfn t() {}\n"),
        ];
        let root = crate::testdir::tree("coverage-test-code", &files);
        let record = |file: &str, lines: &str| {
            format!("SF:{}\n{lines}end_of_record\n", root.join(file).display())
        };
        let lcov = [
            record("src/a.rs", "DA:1,0\nDA:2,1\nDA:4,1\nDA:5,0\nDA:6,0\n"),
            record("src/tests.rs", "DA:1,0\n"),
            record("src/only.rs", "DA:2,0\n"),
        ]
        .concat();
        let read = uncovered(&lcov, &root).unwrap();
        let counts: Vec<(&str, u64)> = read
            .series
            .0
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect();
        assert_eq!(counts, [("src/a.rs", 1), ("src/only.rs", 0)]);
        assert_eq!(read.lines.get("src/a.rs"), Some(&vec![1]));
    }

    #[test]
    fn a_path_is_recorded_relative_to_the_project_so_it_means_the_same_elsewhere() {
        let series = uncovered(TWO, Path::new("/w")).unwrap().series;
        assert_eq!(
            series.0.keys().cloned().collect::<Vec<_>>(),
            ["src/a.rs", "src/b.rs"]
        );
    }

    #[test]
    fn a_report_with_no_record_in_it_is_a_gate_that_could_not_run() {
        assert_eq!(
            uncovered("TN:\n", Path::new("/w")),
            Err(format!(
                "{FILE} holds no coverage record, so nothing was measured"
            ))
        );
    }

    #[test]
    fn a_count_that_is_not_a_number_stops_the_gate_rather_than_reading_as_zero() {
        let err = uncovered(
            "SF:/w/a.rs\nLF:many\nLH:1\nend_of_record\n",
            Path::new("/w"),
        )
        .unwrap_err();
        assert!(
            err.starts_with(&format!("{FILE} holds a count chock cannot read")),
            "{err}"
        );
    }

    #[test]
    fn a_record_that_ends_before_its_file_is_named_is_refused() {
        assert_eq!(
            uncovered("LF:1\nLH:1\nend_of_record\n", Path::new("/w")),
            Err("a record ended before any SF: named its file".to_string())
        );
    }

    fn held(name: &str) -> crate::testdir::Held {
        let dir = crate::testdir::make(name);
        let ctx = Ctx::at(&dir);
        crate::testdir::Held::new(dir, ctx)
    }

    fn writer(dir: &mut Ctx, body: &str) {
        std::fs::write(dir.root.join("coverage.sh"), body).unwrap();
        dir.coverage = vec!["sh".to_string(), "coverage.sh".to_string()].into();
    }

    const WRITE: &str = "printf 'measured\\n' >> calls; printf 'SF:src/lib.rs\\nDA:1,0\\nend_of_record\\n' > lcov.info";

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_lines_are_named_against_the_record_the_project_keeps() {
        let mut dir = held("coverage-named");
        writer(&mut dir, WRITE);
        let named = measure(&dir).unwrap().findings;
        let said: Vec<String> = named.iter().map(Finding::render).collect();
        let listed = "every file's lines: target/test-scratch/chock/coverage.txt";
        assert_eq!(
            said,
            [format!("src/lib.rs:1: no test ran line(s) 1 ({listed})")]
        );
        let mut was = Series::new();
        was.set("src/lib.rs", 1);
        dir.baseline.record(GATE.name, "uncovered line(s)", was);
        assert!(measure(&dir).unwrap().findings.is_empty());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_profile_the_merge_left_out_is_named_and_the_rest_is_shown_but_not_ruled_on() {
        let mut dir = held("coverage-unmerged");
        let warned = "echo 'warning: /t/a.profraw: invalid instrumentation profile data' >&2";
        let flags = "printf '%s' \"$LLVM_PROFDATA_FLAGS\" > flags";
        writer(&mut dir, &format!("{warned}; {flags}; {WRITE}"));
        let read = measure(&dir).unwrap();
        let left = "/t/a.profraw: invalid instrumentation profile data (left out of the merge)";
        let left = [left.to_string()];
        assert_eq!(read.unmeasured, left);
        assert_eq!(read.series.0.get("src/lib.rs"), Some(&1));
        let unread = crate::run::unread_of(&left);
        assert_eq!(read.whole().err(), Some(unread.clone()));
        assert_eq!(merged_whole(&dir).err(), Some(unread.clone()));
        assert_eq!(crap::whole_scores(&dir).err(), Some(unread));
        let set = std::env::var("LLVM_PROFDATA_FLAGS").unwrap_or_default();
        let passed = std::fs::read_to_string(dir.root.join("flags")).unwrap();
        assert_eq!(passed, profdata_flags(&set));
        let mut clean = held("coverage-merged");
        writer(&mut clean, WRITE);
        measure(&clean).unwrap();
        assert_eq!(merged_whole(&clean), Ok(()));
    }

    #[test]
    fn llvm_profdata_leaves_out_what_it_cannot_read_unless_the_project_said_otherwise() {
        assert_eq!(profdata_flags(""), "-failure-mode=all");
        assert_eq!(
            profdata_flags("-sparse=true"),
            "-sparse=true -failure-mode=all"
        );
        assert_eq!(profdata_flags("-failure-mode=any"), "-failure-mode=any");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn an_existing_report_is_replaced_before_the_first_measurement() {
        let mut dir = held("coverage-current");
        std::fs::write(dir.root.join(FILE), TWO).unwrap();
        writer(&mut dir, WRITE);
        assert_eq!(measure(&dir).unwrap().series.get("src/lib.rs"), Some(1));
        assert_eq!(
            std::fs::read_to_string(dir.root.join("calls")).unwrap(),
            "measured\n"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn consumers_and_cloned_contexts_share_one_completed_measurement() {
        let mut dir = held("coverage-shared");
        writer(&mut dir, WRITE);
        assert_eq!(ensure(&dir).unwrap(), dir.root.join(FILE));
        let cloned = (*dir).clone();
        assert_eq!(measure(&cloned).unwrap().series.get("src/lib.rs"), Some(1));
        assert_eq!(ensure(&dir).unwrap(), dir.root.join(FILE));
        assert_eq!(
            std::fs::read_to_string(dir.root.join("calls")).unwrap(),
            "measured\n"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_new_command_context_measures_again_without_any_rust_change() {
        let mut dir = held("coverage-fresh");
        writer(&mut dir, WRITE);
        assert_eq!(measure(&dir).unwrap().series.get("src/lib.rs"), Some(1));
        writer(&mut dir, &WRITE.replace("DA:1,0", "DA:1,1"));
        assert_eq!(measure(&dir).unwrap().series.get("src/lib.rs"), Some(0));
        assert_eq!(
            std::fs::read_to_string(dir.root.join("calls")).unwrap(),
            "measured\nmeasured\n"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_failed_measurement_is_shared_instead_of_retried() {
        let mut dir = held("coverage-shared-failure");
        std::fs::write(dir.root.join(FILE), TWO).unwrap();
        writer(
            &mut dir,
            "printf 'attempted\\n' >> calls; printf 'fixture failed\\n' >&2; exit 1",
        );
        let why = ensure(&dir).unwrap_err();
        assert!(why.contains("fixture failed"), "{why}");
        assert_eq!(ensure(&dir), Err(why));
        assert_eq!(
            std::fs::read_to_string(dir.root.join("calls")).unwrap(),
            "attempted\n"
        );
        assert!(!dir.root.join(FILE).exists());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_successful_command_cannot_reuse_the_report_it_did_not_write() {
        let mut dir = held("coverage-no-output");
        std::fs::write(dir.root.join(FILE), TWO).unwrap();
        writer(&mut dir, "exit 0");
        assert_eq!(
            ensure(&dir),
            Err(
                "the coverage run exited clean without writing lcov.info, so nothing was measured"
                    .to_string()
            )
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_report_replaced_or_deleted_between_consumers_is_refused() {
        let mut dir = held("coverage-replaced");
        writer(&mut dir, WRITE);
        ensure(&dir).unwrap();
        std::fs::write(dir.root.join(FILE), TWO).unwrap();
        assert_eq!(
            ensure(&dir),
            Err("lcov.info changed after the coverage command completed".to_string())
        );
        std::fs::remove_file(dir.root.join(FILE)).unwrap();
        assert!(ensure(&dir).unwrap_err().contains("cannot read lcov.info"));
        assert_eq!(
            std::fs::read_to_string(dir.root.join("calls")).unwrap(),
            "measured\n"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_context_with_changed_root_or_command_cannot_share_another_measurement() {
        let mut dir = held("coverage-context-changed");
        writer(&mut dir, WRITE);
        ensure(&dir).unwrap();
        let mut changed = (*dir).clone();
        changed.coverage.argv.push("other".to_string());
        assert_eq!(
            ensure(&changed),
            Err("the coverage context changed after its measurement".to_string())
        );
        changed.coverage.argv.pop();
        changed.root = dir.root.join("other");
        assert_eq!(
            ensure(&changed),
            Err("the coverage context changed after its measurement".to_string())
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_report_that_cannot_be_removed_stops_before_the_command_runs() {
        let mut dir = held("coverage-cannot-remove");
        std::fs::create_dir(dir.root.join(FILE)).unwrap();
        writer(&mut dir, WRITE);
        assert!(
            ensure(&dir)
                .unwrap_err()
                .contains("cannot remove the old lcov.info")
        );
        assert!(!dir.root.join("calls").exists());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_coverage_command_must_name_an_executable() {
        let mut dir = held("coverage-empty-command");
        dir.coverage = Vec::new().into();
        assert_eq!(
            ensure(&dir),
            Err("the configured `coverage` names no command".to_string())
        );
    }
}
