//! The `mutest` gate. An eligible target that failed to build stops the run before any survivor is
//! compared; an ineligible one is only an advisory.

use std::collections::BTreeSet;

use crate::exec;
use crate::run::baseline::Series;
use crate::run::report::Finding;
use crate::run::{Ctx, Measurement};

use super::results::Results;
use super::scope;
use super::survivors::{self, Survivor};

const CALL_GRAPH_DEPTH: &str = "3";
const TIMEOUT_LIMIT: u64 = 2;
const SKIP: &str = " links no mutant crate, so no mutation can reach its tests";
const NOT_LINKED: &str = "not applicable to linked-crate mutation: no mutant crate is linked; \
                         this target was skipped, not mutation-covered. Ordinary integration tests remain required";

/// The whole crate, or on a local run only the Rust files the change touched.
pub(in crate::gates) fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    ctx.default_build("cargo mutest")?;
    let was = ctx.record(crate::gates::tools::MUTEST.name);
    let named = ctx.named_against(crate::gates::tools::MUTEST.name);
    match scope::of(ctx) {
        None => mutated(ctx, None, &named, read),
        Some(files) if files.is_empty() => Ok(scope::unchanged(was)),
        Some(files) => scoped(ctx, &files, &was, &named),
    }
}

/// Only `files`. Where their mutations alone pass the timeout limit, the whole crate instead: CI
/// holds the whole crate to that limit, so the local verdict is the one CI gives.
fn scoped(
    ctx: &Ctx,
    files: &[String],
    was: &Series,
    named: &Series,
) -> Result<Measurement, String> {
    mutated(ctx, Some(&scope::filter(files)), named, read_within)
        .and_then(|only| scope::widened(only, was, files, || mutated(ctx, None, named, read)))
}

/// How a finished run's output and results are read.
type Judge<T> = fn(&exec::Output, Survivors<'_>, &Series) -> Result<T, String>;

#[cfg(target_os = "linux")]
fn mutated<T>(ctx: &Ctx, filter: Option<&str>, was: &Series, judge: Judge<T>) -> Result<T, String> {
    let results = Results::create(&ctx.root)?;
    let mut watch = super::watch::Watch::create(&ctx.root)?;
    let values = watch
        .environment()?
        .map(|(name, value)| (name.to_string(), value.to_string()));
    let env: Vec<_> = values
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let output = exec::run_watched(
        "cargo",
        &invocation(&ctx.features, &results.flag(), filter),
        &ctx.root,
        &env,
        &mut watch,
    )
    .map_err(|error| format!("{error}{}{}", watch.retained(), results.retained()))?;
    let measured = judge(&output, &|expected| results.survivors(expected), was)
        .and_then(|measured| accounted(ctx, &output, &results).map(|()| measured))
        .map_err(|error| format!("{error}{}{}", watch.retained(), results.retained()))?;
    watch.accept()?;
    results.accept()?;
    Ok(measured)
}

/// Checks that every target cargo tests either wrote results or was named ineligible.
fn accounted(ctx: &Ctx, out: &exec::Output, results: &Results) -> Result<(), String> {
    let metadata = crate::project::metadata(&ctx.root)?;
    super::targets::all_written(&metadata, &results.targets()?, &skipped_names(out))
}

#[cfg(not(target_os = "linux"))]
fn mutated<T>(ctx: &Ctx, filter: Option<&str>, was: &Series, judge: Judge<T>) -> Result<T, String> {
    let results = Results::create(&ctx.root)?;
    let output = exec::tool(
        &ctx.root,
        "cargo",
        &invocation(&ctx.features, &results.flag(), filter),
    )?;
    let measured = judge(&output, &|expected| results.survivors(expected), was)
        .and_then(|measured| accounted(ctx, &output, &results).map(|()| measured))
        .map_err(|error| format!("{error}{}", results.retained()))?;
    results.accept()?;
    Ok(measured)
}

// `--isolate all`: even a safe mutation can abort, e.g. a `Default::default` that calls itself.
// Mutants run in parallel, so survivors come from `results`, not the interleaved text.
fn invocation<'a>(
    features: &'a [String],
    results: &'a str,
    filter: Option<&'a str>,
) -> Vec<&'a str> {
    let mut argv = vec![
        "mutest",
        "run",
        "--call-graph-depth-limit",
        CALL_GRAPH_DEPTH,
        "--isolate",
        "all",
        "--parallel-mutants",
        results,
    ];
    #[cfg(target_os = "linux")]
    argv.push("--require-progress");
    argv.extend(filter);
    argv.extend(features.iter().map(String::as_str));
    argv
}

/// The survivors a run's results name, given how many its totals said there are.
type Survivors<'a> = &'a dyn Fn(u64) -> Result<Vec<Survivor>, String>;

/// What one run measured: totals and exit from its output, survivors from what `results` read, and
/// where each telling survivor past the record `was` is.
pub(super) fn read(
    out: &exec::Output,
    results: Survivors,
    was: &Series,
) -> Result<Measurement, String> {
    let found = read_survivors(out, results)?;
    let mut findings = ineligible(out);
    findings.extend(timeouts(out));
    findings.extend(survivors::sites_over(&found, was));
    Ok(Measurement {
        series: Series(survivors::telling(&found)),
        findings,
    })
}

fn read_survivors(out: &exec::Output, results: Survivors) -> Result<Vec<Survivor>, String> {
    if out.truncated {
        return Err("mutest printed more than chock keeps; the survivors would be partial".into());
    }
    if let Some(reason) = failed_target(out) {
        return Err(format!(
            "mutest failed to analyse an eligible target: {reason}"
        ));
    }
    let totals = survivors::totals(&out.stdout)?.ok_or_else(|| unmeasured(out))?;
    completed(out)?;
    survivor_counts(out, totals, results)
}

/// Every survivor the results name. A finished run is still refused if its results are unreadable or
/// more than `TIMEOUT_LIMIT` percent of its mutations timed out.
fn survivor_counts(
    out: &exec::Output,
    totals: survivors::Totals,
    results: Survivors,
) -> Result<Vec<Survivor>, String> {
    let found = results(totals.undetected)?;
    if out.code == Some(2) && found.is_empty() {
        return Err(
            "mutest reported missed mutations without naming a survivor; the result is incomplete"
                .to_string(),
        );
    }
    let gave_up = totals.timed_out;
    let attempted = totals.total;
    if over_limit(&totals) {
        return Err(format!(
            "mutest gave up on {gave_up} of {attempted} mutations, so the score is not \
             trustworthy; re-run on an idle machine"
        ));
    }
    Ok(found)
}

/// Whether more than `TIMEOUT_LIMIT` percent of the mutations a run attempted timed out.
fn over_limit(totals: &survivors::Totals) -> bool {
    u128::from(totals.timed_out) * 100 > u128::from(totals.total) * u128::from(TIMEOUT_LIMIT)
}

/// `read` for a run of only some files, or `None` where their mutations alone pass the timeout
/// limit: that verdict is the whole crate's to give.
fn read_within(
    out: &exec::Output,
    results: Survivors,
    was: &Series,
) -> Result<Option<Measurement>, String> {
    let totals = survivors::totals(&out.stdout).ok().flatten();
    match totals.is_some_and(|totals| over_limit(&totals)) {
        true => Ok(None),
        false => read(out, results, was).map(Some),
    }
}

/// An advisory where mutations timed out within the limit: mutest counts each one as detected,
/// so no survivor count shows them.
fn timeouts(out: &exec::Output) -> Option<Finding> {
    let totals = survivors::totals(&out.stdout).ok().flatten()?;
    let (gave_up, attempted) = (totals.timed_out, totals.total);
    let note = format!(
        "{gave_up} of {attempted} mutation(s) timed out. mutest counts each one as detected, so \
         the survivor count leaves them out; run again on an idle machine to judge them"
    );
    (gave_up > 0).then(|| Finding::at("", &note).item("timeouts"))
}

fn unmeasured(out: &exec::Output) -> String {
    let skipped: Vec<String> = ineligible(out).iter().map(Finding::render).collect();
    let reason = if skipped.is_empty() {
        out.why_it_failed().to_string()
    } else {
        skipped.join("; ")
    };
    format!("mutest reported no mutation totals, so nothing was measured: {reason}")
}

/// Codes 2 and 3 describe evaluated mutations. Code 4 and abnormal exits describe an incomplete run,
/// even when another target has already produced a complete survivor list.
fn completed(out: &exec::Output) -> Result<(), String> {
    match out.code {
        Some(0 | 2 | 3) => Ok(()),
        code => Err(format!(
            "mutest did not finish (exit {code:?}); partial totals are not a complete measurement: {}",
            out.failure_details()
        )),
    }
}

/// mutest's diagnostic for a test harness that did not build. It may arrive with exit zero, and it
/// overrides any other target's totals.
fn failed_target(out: &exec::Output) -> Option<String> {
    [&out.stdout, &out.stderr]
        .into_iter()
        .flat_map(|text| text.lines())
        .map(exec::strip_colour)
        .find(|line| {
            line.contains("the test harness of `")
                && line.contains("did not build, so none of its mutations were evaluated")
        })
        .map(|line| line.trim().to_string())
}

/// An advisory per integration test mutest skipped. It changes no survivor baseline; the test gate
/// still runs those tests.
fn ineligible(out: &exec::Output) -> Vec<Finding> {
    skipped_names(out)
        .into_iter()
        .map(|name| Finding::at("", NOT_LINKED).item(&format!("integration test {name}")))
        .collect()
}

fn skipped_names(out: &exec::Output) -> BTreeSet<String> {
    [&out.stdout, &out.stderr]
        .into_iter()
        .flat_map(|text| text.lines())
        .filter_map(|line| skipped(&exec::strip_colour(line)))
        .collect()
}

fn skipped(line: &str) -> Option<String> {
    let name = line.trim().strip_prefix("warning: integration test `")?;
    let (name, reason) = name.split_once('`')?;
    (reason == SKIP && !name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    const TOTALS: &str =
        "mutations: 90%. 9 detected (0 timed out; 0 crashed); 1 undetected; 10 total\n";
    const SKIPPED: &str = "warning: integration test `commands` links no mutant crate, so no mutation can reach its tests\n  = note: its tests are skipped\n";
    const FAILED: &str = "error: the test harness of `launch` in package `app-adapters` did not build, so none of its mutations were evaluated\n";

    fn ran(stdout: &str, code: Option<i32>, truncated: bool) -> exec::Output {
        exec::Output {
            code,
            stdout: stdout.to_string(),
            stderr: "it broke".to_string(),
            truncated,
        }
    }

    fn survivor(operator: &str) -> Survivor {
        Survivor {
            operator: operator.to_string(),
            file: "src/a.rs".to_string(),
            line: 7,
            what: "does a thing".to_string(),
        }
    }

    /// What the results of a run matching `TOTALS` name: its one survivor.
    fn one(_expected: u64) -> Result<Vec<Survivor>, String> {
        Ok(vec![survivor("eq_op_invert")])
    }

    fn none(_expected: u64) -> Result<Vec<Survivor>, String> {
        Ok(Vec::new())
    }

    fn counted(out: &exec::Output, results: Survivors) -> Result<Series, String> {
        read_survivors(out, results).map(|found| Series(survivors::telling(&found)))
    }

    fn unrecorded(out: &exec::Output, results: Survivors) -> Result<Measurement, String> {
        read(out, results, &Series::default())
    }

    /// A file as the root, so making the results directory fails before cargo starts.
    fn touched(files: &[&str]) -> Ctx {
        let mut baseline = crate::run::baseline::Baseline::empty("0.1.0");
        let mut was = Series::new();
        was.set("src/a.rs#eq_op_invert", 1);
        baseline.set(crate::gates::tools::MUTEST.name, was);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let files = files.iter().map(|file| (*file).to_string()).collect();
        Ctx {
            changed: std::sync::Arc::new(std::sync::OnceLock::from(Ok(files))),
            ..Ctx::for_root(root, baseline)
        }
    }

    #[test]
    fn a_local_run_keeps_the_record_when_no_rust_file_moved_and_mutates_when_one_did() {
        let kept = measure(&touched(&["README.md"])).map(|measured| measured.series);
        assert_eq!(
            kept,
            Ok(touched(&[]).record(crate::gates::tools::MUTEST.name))
        );
        let started = measure(&touched(&["src/a.rs"]));
        assert!(
            started
                .as_ref()
                .is_err_and(|error| error.contains("mutest's results")),
            "{started:?}"
        );
    }

    #[test]
    fn a_survivor_past_its_record_is_named_where_it_is_and_one_at_it_is_not() {
        let out = ran(TOTALS, Some(2), false);
        let over = unrecorded(&out, &one).unwrap();
        let sites: Vec<String> = over.findings.iter().map(Finding::render).collect();
        assert_eq!(
            sites,
            ["src/a.rs:7: no test caught: does a thing (eq_op_invert)"]
        );
        let was = Series([("src/a.rs#eq_op_invert".to_string(), 1)].into());
        assert!(read(&out, &one, &was).unwrap().findings.is_empty());
    }

    #[test]
    fn a_run_that_named_survivors_is_keyed_by_file_and_operator() {
        let found = counted(&ran(TOTALS, Some(0), false), &one).unwrap();
        assert_eq!(found.get("src/a.rs#eq_op_invert"), Some(1));
    }

    #[test]
    fn a_run_that_detected_everything_holds_nothing() {
        let clean = "mutations: 100.00%. 10 detected; 0 undetected; 10 total\n";
        assert_eq!(
            counted(&ran(clean, Some(0), false), &none),
            Ok(Series::new())
        );
    }

    #[test]
    fn a_run_that_left_too_much_unresolved_is_refused_rather_than_scored() {
        let totals =
            "mutations: 90%. 5 detected (4 timed out; 0 crashed); 1 undetected; 10 total\n";
        let err = counted(&ran(totals, Some(3), false), &one).unwrap_err();
        assert_eq!(
            err,
            "mutest gave up on 4 of 10 mutations, so the score is not trustworthy; re-run on an idle machine"
        );
    }

    /// Past the limit the files' own results go unread; at the limit they are read as any run's.
    #[test]
    fn files_whose_mutations_alone_pass_the_limit_leave_the_verdict_to_the_whole_crate() {
        let unread = |_: u64| Err("the results were read".to_string());
        let past = "mutations: 90%. 5 detected (4 timed out; 0 crashed); 1 undetected; 10 total\n";
        let left = read_within(&ran(past, Some(3), false), &unread, &Series::default());
        assert_eq!(left.map(|read| read.map(|read| read.series)), Ok(None));
        let at = "mutations: 98%. 48 detected (1 timed out; 0 crashed); 1 undetected; 50 total\n";
        let read = read_within(&ran(at, Some(3), false), &one, &Series::default());
        let series = read.unwrap().map(|read| read.series).unwrap();
        assert_eq!(series.get("src/a.rs#eq_op_invert"), Some(1));
        let nothing = read_within(&ran("", Some(1), false), &none, &Series::default());
        assert_eq!(
            nothing.map(|read| read.map(|read| read.series)),
            Err(
                "mutest reported no mutation totals, so nothing was measured: it broke".to_string()
            )
        );
    }

    #[test]
    fn a_run_at_the_limit_itself_is_still_scored() {
        let totals =
            "mutations: 98%. 48 detected (1 timed out; 0 crashed); 1 undetected; 50 total\n";
        let series = counted(&ran(totals, Some(3), false), &one).unwrap();
        assert_eq!(series.get("src/a.rs#eq_op_invert"), Some(1));
    }

    #[test]
    fn a_scored_run_says_how_many_mutations_timed_out_and_nothing_when_none_did() {
        let totals =
            "mutations: 98%. 48 detected (1 timed out; 0 crashed); 1 undetected; 50 total\n";
        let was = Series([("src/a.rs#eq_op_invert".to_string(), 1)].into());
        let some = read(&ran(totals, Some(3), false), &one, &was).unwrap();
        let said: Vec<String> = some.findings.iter().map(Finding::render).collect();
        assert_eq!(
            said,
            [
                "timeouts: 1 of 50 mutation(s) timed out. mutest counts each one as detected, so \
                 the survivor count leaves them out; run again on an idle machine to judge them"
            ]
        );
        let none = read(&ran(TOTALS, Some(2), false), &one, &was).unwrap();
        assert!(none.findings.is_empty(), "{:?}", none.findings);
    }

    #[test]
    fn a_run_that_resolved_nearly_everything_is_scored() {
        let totals =
            "mutations: 99%. 98 detected (1 timed out; 0 crashed); 1 undetected; 100 total\n";
        let series = counted(&ran(totals, Some(0), false), &one).unwrap();
        assert_eq!(series.get("src/a.rs#eq_op_invert"), Some(1));
    }

    #[test]
    fn a_failed_run_naming_no_survivor_measured_nothing() {
        assert_eq!(
            counted(&ran("", Some(1), false), &none).unwrap_err(),
            "mutest reported no mutation totals, so nothing was measured: it broke"
        );
    }

    #[test]
    fn a_run_whose_harness_would_not_compile_measured_nothing_though_it_exited_zero() {
        let mut out = ran("", Some(0), false);
        out.stderr = "error: extern location for mutest_runtime does not exist: /tool/target/release/libmutest_runtime.rlib\n".to_string();
        let err = counted(&out, &none).unwrap_err();
        assert!(err.contains("reported no mutation totals"), "{err}");
        assert!(err.contains("libmutest_runtime.rlib"), "{err}");
    }

    #[test]
    fn a_crate_with_nothing_to_mutate_measures_none_rather_than_refusing() {
        let nothing =
            "mutations: 100%. 0 detected (0 timed out; 0 crashed); 0 undetected; 0 total\n";
        assert_eq!(
            counted(&ran(nothing, Some(0), false), &none).unwrap(),
            Series::new()
        );
    }

    #[test]
    fn output_chock_cut_short_would_under_count_every_file_after_the_cut() {
        assert_eq!(
            counted(&ran(TOTALS, Some(0), true), &one),
            Err("mutest printed more than chock keeps; the survivors would be partial".to_string())
        );
    }

    #[test]
    fn a_failed_target_overrides_totals_and_survivors_from_a_successful_target() {
        for code in [Some(0), Some(2), Some(4), Some(101)] {
            let mut out = ran(TOTALS, code, false);
            out.stderr = FAILED.to_string();
            let why = unrecorded(&out, &one).unwrap_err();
            assert!(why.contains("launch"), "{why}");
            assert!(why.contains("app-adapters"), "{why}");
            assert!(why.contains("failed to analyse"), "{why}");
        }
    }

    #[test]
    fn incomplete_exit_codes_never_become_a_measurement_when_survivors_exist() {
        for code in [None, Some(1), Some(4), Some(101), Some(137), Some(99)] {
            let why = unrecorded(&ran(TOTALS, code, false), &one).unwrap_err();
            assert!(why.contains("partial totals"), "{why}");
        }
    }

    #[test]
    fn missed_and_timeout_exit_codes_preserve_the_survivor_measurement() {
        for code in [Some(0), Some(2), Some(3)] {
            let measured = unrecorded(&ran(TOTALS, code, false), &one).unwrap();
            assert_eq!(measured.series.get("src/a.rs#eq_op_invert"), Some(1));
        }
    }

    #[test]
    fn a_missed_mutation_exit_with_no_readable_survivors_is_not_a_clean_run() {
        let why = unrecorded(&ran(TOTALS, Some(2), false), &none).unwrap_err();
        assert!(why.contains("without naming a survivor"), "{why}");
    }

    #[test]
    fn results_that_cannot_be_read_are_no_measurement() {
        let unreadable = |_| Err("cannot read mutations.json".to_string());
        let why = unrecorded(&ran(TOTALS, Some(2), false), &unreadable).unwrap_err();
        assert_eq!(why, "cannot read mutations.json");
    }

    #[test]
    fn an_ineligible_integration_target_is_reported_without_becoming_a_survivor() {
        let mut out = ran(TOTALS, Some(2), false);
        out.stderr = SKIPPED.to_string();
        let was = Series([("src/a.rs#eq_op_invert".to_string(), 1)].into());
        let measured = read(&out, &one, &was).unwrap();
        assert_eq!(measured.series.get("src/a.rs#eq_op_invert"), Some(1));
        assert_eq!(
            measured.findings,
            vec![Finding::at("", NOT_LINKED).item("integration test commands")]
        );
    }

    #[test]
    fn skips_are_deduplicated_across_streams() {
        let mut out = ran(&format!("{SKIPPED}{TOTALS}"), Some(0), false);
        out.stderr = format!(
            "{SKIPPED}warning: integration test `heredoc` links no mutant crate, so no mutation can reach its tests\n"
        );
        let measured = unrecorded(&out, &one).unwrap();
        let names: Vec<_> = measured
            .findings
            .iter()
            .filter_map(|f| f.item.as_deref())
            .collect();
        assert_eq!(
            names,
            ["integration test commands", "integration test heredoc"]
        );
        assert_eq!(skipped("warning: integration test `commands` failed"), None);
        assert_eq!(
            skipped(
                "warning: integration test `` links no mutant crate, so no mutation can reach its tests"
            ),
            None
        );
    }

    #[test]
    fn a_coloured_failed_target_diagnostic_on_stdout_is_still_fatal() {
        let out = ran(
            &format!(
                "{TOTALS}\u{1b}[31merror\u{1b}[0m: the test harness of `launch` in package `app-adapters` did not build, so none of its mutations were evaluated\n"
            ),
            Some(0),
            false,
        );
        let why = unrecorded(&out, &one).unwrap_err();
        assert!(why.contains("launch"), "{why}");
    }

    #[test]
    fn every_mutation_is_isolated_and_written_where_chock_reads_it_under_the_project_features() {
        let at = "--metadata-out-root-dir=/results";
        let expected = |features: &[&'static str]| {
            let mut argv = vec![
                "mutest",
                "run",
                "--call-graph-depth-limit",
                "3",
                "--isolate",
                "all",
                "--parallel-mutants",
                at,
                #[cfg(target_os = "linux")]
                "--require-progress",
            ];
            argv.extend(features);
            argv
        };
        let features = ["--no-default-features", "--features", "testkit,extra"];
        assert_eq!(
            invocation(&features.map(String::from), at, None),
            expected(&features)
        );
        assert_eq!(
            invocation(&["--all-features".into()], at, None),
            expected(&["--all-features"])
        );
        assert_eq!(invocation(&[], at, None), expected(&[]));
        let filter = "--filter-mutations=file:src/a.rs";
        assert_eq!(
            invocation(&["--all-features".into()], at, Some(filter)),
            expected(&[filter, "--all-features"])
        );
    }

    #[test]
    fn the_results_are_held_to_the_count_the_totals_gave() {
        let asked = std::cell::Cell::new(None);
        let results = |expected| {
            asked.set(Some(expected));
            one(expected)
        };
        unrecorded(&ran(TOTALS, Some(0), false), &results).unwrap();
        assert_eq!(asked.get(), Some(1));
        assert!(
            unrecorded(
                &ran(&format!("{TOTALS}mutations: incomplete\n"), Some(0), false),
                &one
            )
            .is_err()
        );
    }

    #[test]
    fn quiet_survivors_are_accounted_for_before_operator_filtering() {
        let quiet = |_| Ok(vec![survivor("call_delete")]);
        assert_eq!(
            unrecorded(&ran(TOTALS, Some(2), false), &quiet)
                .unwrap()
                .series,
            Series::new()
        );
    }

    #[test]
    fn an_exit_reporting_misses_cannot_claim_zero_undetected_mutations() {
        let out = "mutations: 100%. 10 detected; 0 undetected; 10 total\n";
        assert!(
            unrecorded(&ran(out, Some(2), false), &none)
                .unwrap_err()
                .contains("without naming a survivor")
        );
    }

    #[test]
    fn a_run_containing_only_ineligible_targets_is_not_a_zero_survivor_measurement() {
        let why = unrecorded(&ran(SKIPPED, Some(0), false), &none).unwrap_err();
        assert!(
            why.contains("not applicable to linked-crate mutation"),
            "{why}"
        );
        assert!(why.contains("commands"), "{why}");
        assert!(why.contains("no mutation totals"), "{why}");
    }
}
