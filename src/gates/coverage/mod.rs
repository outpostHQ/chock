//! Lines no test executed, per file, read from the LCOV report the coverage run writes. A ratchet
//! rather than a percentage floor, so an existing project can adopt it.

pub mod crap;

use std::collections::BTreeMap;
use std::path::Path;

use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

pub const FILE: &str = "lcov.info";

pub const GATE: Gate = Gate {
    name: "coverage",
    about: "lines no test executed, per file, against the count recorded",
    group: Group::Quality,
    builds: true,
    reads: None,
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "uncovered line(s)",
    },
};

fn measure(ctx: &Ctx) -> Result<Series, String> {
    let path = ensure(ctx)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {FILE}: {e}"))?;
    uncovered(&text, &ctx.root)
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
    let run = crate::exec::run(program, &args, &ctx.root)
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
    })
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

/// Uncovered lines per file, as a count rather than a percentage.
fn uncovered(lcov: &str, root: &Path) -> Result<Series, String> {
    let mut series = Series::new();
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
            series.set(&name, missed(&ran, found, hit));
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
    Ok(series)
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
    fn a_run_that_wrote_no_report_is_refused_rather_than_read() {
        let dir = crate::testdir::make("coverage-absent");
        let err = wrote(&dir.join(FILE)).unwrap_err();
        assert_eq!(
            err,
            "the coverage run exited clean without writing lcov.info, so nothing was measured"
        );
    }

    #[test]
    fn a_report_the_run_wrote_is_accepted() {
        let dir = crate::testdir::make("coverage-written");
        std::fs::write(dir.join(FILE), "").unwrap();
        assert_eq!(wrote(&dir.join(FILE)), Ok(()));
    }

    #[test]
    fn a_directory_where_the_report_should_be_is_not_a_report() {
        let dir = crate::testdir::make("coverage-directory");
        std::fs::create_dir_all(dir.join(FILE)).unwrap();
        assert!(wrote(&dir.join(FILE)).is_err());
    }

    const TWO: &str = "SF:/w/src/a.rs\nLF:100\nLH:80\nend_of_record\n\
                       SF:/w/src/b.rs\nLF:10\nLH:10\nend_of_record\n";

    #[test]
    fn a_summary_with_no_per_line_detail_is_still_read() {
        let series = uncovered(TWO, Path::new("/w")).unwrap();
        assert_eq!(series.get("src/a.rs"), Some(20));
        assert_eq!(series.get("src/b.rs"), Some(0));
    }

    #[test]
    fn a_line_any_record_executed_is_covered_however_many_records_missed_it() {
        let both = "SF:/w/src/a.rs\nDA:1,0\nDA:1,7\nDA:2,0\nLF:2\nLH:1\nend_of_record\n";
        assert_eq!(
            uncovered(both, Path::new("/w")).unwrap().get("src/a.rs"),
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
                .get("src/a.rs"),
            Some(1),
            "one line never ran; LF - LH would have said five"
        );
    }

    #[test]
    fn a_record_carrying_a_checksum_is_read_past_rather_than_refused() {
        assert_eq!(executed("7,2,f0b3a1"), Some((7, 2)));
        assert_eq!(executed("7,0"), Some((7, 0)));
        assert_eq!(executed("nonsense"), None);
    }

    #[test]
    fn a_path_is_recorded_relative_to_the_project_so_it_means_the_same_elsewhere() {
        let series = uncovered(TWO, Path::new("/w")).unwrap();
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
        let ctx = Ctx::for_root(
            dir.to_path_buf(),
            crate::run::baseline::Baseline::empty("0.1.0"),
        );
        crate::testdir::Held::new(dir, ctx)
    }

    fn writer(dir: &mut Ctx, body: &str) {
        std::fs::write(dir.root.join("coverage.sh"), body).unwrap();
        dir.coverage = vec!["sh".to_string(), "coverage.sh".to_string()].into();
    }

    const WRITE: &str = "printf 'measured\\n' >> calls; printf 'SF:src/lib.rs\\nDA:1,0\\nend_of_record\\n' > lcov.info";

    #[test]
    fn an_existing_report_is_replaced_before_the_first_measurement() {
        let mut dir = held("coverage-current");
        std::fs::write(dir.root.join(FILE), TWO).unwrap();
        writer(&mut dir, WRITE);
        assert_eq!(measure(&dir).unwrap().get("src/lib.rs"), Some(1));
        assert_eq!(
            std::fs::read_to_string(dir.root.join("calls")).unwrap(),
            "measured\n"
        );
    }

    #[test]
    fn consumers_and_cloned_contexts_share_one_completed_measurement() {
        let mut dir = held("coverage-shared");
        writer(&mut dir, WRITE);
        assert_eq!(ensure(&dir).unwrap(), dir.root.join(FILE));
        let cloned = (*dir).clone();
        assert_eq!(measure(&cloned).unwrap().get("src/lib.rs"), Some(1));
        assert_eq!(ensure(&dir).unwrap(), dir.root.join(FILE));
        assert_eq!(
            std::fs::read_to_string(dir.root.join("calls")).unwrap(),
            "measured\n"
        );
    }

    #[test]
    fn a_new_command_context_measures_again_without_any_rust_change() {
        let mut dir = held("coverage-fresh");
        writer(&mut dir, WRITE);
        assert_eq!(measure(&dir).unwrap().get("src/lib.rs"), Some(1));
        writer(&mut dir, &WRITE.replace("DA:1,0", "DA:1,1"));
        assert_eq!(measure(&dir).unwrap().get("src/lib.rs"), Some(0));
        assert_eq!(
            std::fs::read_to_string(dir.root.join("calls")).unwrap(),
            "measured\nmeasured\n"
        );
    }

    #[test]
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
    fn a_coverage_command_must_name_an_executable() {
        let mut dir = held("coverage-empty-command");
        dir.coverage = Vec::new().into();
        assert_eq!(
            ensure(&dir),
            Err("the configured `coverage` names no command".to_string())
        );
    }
}
