//! Outpost's syntax-tree rules, and taint flows from untrusted input to a sink.

use serde::Deserialize;

use crate::exec;
use crate::gates::metrics::prodlines;
use crate::run::baseline::{Keys, Series};
use crate::run::report::{Detail, Place};
use crate::run::{Ctx, Details, Gate, Group, Kind, Measurement};

/// Counts defects that shipped before, and untrusted input that reaches a sink.
pub const GATE: Gate = Gate {
    name: "scan",
    about: "a defect that has shipped before, or untrusted input reaching a sink; needs `outpost`",
    group: Group::OptIn,
    builds: false,
    reads: Some(crate::run::verdicts::OUTPOST),
    kind: Kind::AnnotatedRatchet {
        measure,
        keys: Keys::Items,
        unit: "finding(s)",
    },
};

fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let code = prodlines::TestCode::of(ctx)?;
    let out = super::spawn(&ctx.root, &["scan", "rules", "--json"]);
    out.and_then(|out| read_scan(&out, &|file, line| code.holds(file, line)))
}

/// Whether only tests build a line of a file: the file and its 1-based line.
type Tested<'a> = &'a dyn Fn(&str, usize) -> bool;

#[derive(Deserialize)]
struct Scan {
    #[serde(default)]
    findings: Vec<Found>,
    /// Flows from a source to a sink, which Outpost reports apart from `findings`.
    #[serde(default)]
    taint: Vec<Found>,
    #[serde(default)]
    languages: std::collections::BTreeMap<String, Language>,
}

#[derive(Deserialize)]
struct Language {
    #[serde(default)]
    files: u64,
}

#[derive(Deserialize)]
struct Found {
    rule: String,
    file: String,
    #[serde(default)]
    line: Option<u32>,
}

/// Rule and taint findings outside the code only tests build, which Outpost does not set aside: a
/// test file, or a test module. A scan that read no files is an error, not a clean tree.
fn read_scan(out: &exec::Output, tested: Tested<'_>) -> Result<Measurement, String> {
    let scan: Scan = super::read(out, "scan")?;
    if scan.languages.values().all(|read| read.files == 0) {
        return Err(super::nothing_read("rule"));
    }
    let at = |found: &Found| {
        found
            .line
            .map_or(0, |line| usize::try_from(line).unwrap_or(usize::MAX))
    };
    let shipped =
        |found: &&Found| !prodlines::is_test_path(&found.file) && !tested(&found.file, at(found));
    Ok(keyed(
        scan.findings.iter().chain(&scan.taint).filter(shipped),
    ))
}

/// Keyed by file and rule, not line, so an edit above a finding does not read as a new one. The
/// first line stands for the key; each further one past it becomes a place `chock explain` shows.
fn keyed<'a>(findings: impl Iterator<Item = &'a Found>) -> Measurement {
    let mut series = Series::new();
    let mut details = Details::new();
    for found in findings {
        let key = format!("{}#{}", found.file, found.rule);
        series.set(&key, series.get(&key).unwrap_or(0) + 1);
        site(&mut details, key, found);
    }
    Measurement {
        series,
        findings: Vec::new(),
        details,
        unmeasured: Vec::new(),
        faults: Vec::new(),
    }
}

/// A finding with no line leaves no detail. The first line under a key becomes its `line`; each
/// further one becomes a place, so `chock explain` names the line of every hit.
fn site(details: &mut Details, key: String, found: &Found) {
    let Some(line) = found.line else { return };
    let detail: &mut Detail = details.entry(key).or_default();
    if detail.line.is_none() {
        detail.line = Some(line);
    } else {
        detail.places.push(Place::at("hit", &found.file, line));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn untested(_: &str, _: usize) -> bool {
        false
    }

    fn scanned(files: u64, findings: &str) -> exec::Output {
        flowed(files, findings, "")
    }

    fn flowed(files: u64, findings: &str, taint: &str) -> exec::Output {
        exec::Output::of(
            Some(0),
            &format!(
                r#"{{"findings":[{findings}],"taint":[{taint}],"languages":{{"rs":{{"files":{files}}}}}}}"#
            ),
            "",
        )
    }

    fn found_at(file: &str, rule: &str, line: u32) -> String {
        format!(r#"{{"file":"{file}","rule":"{rule}","line":{line}}}"#)
    }

    /// Two findings of `rule` in `src/a.rs`, at lines `first` and `then`, from a scan of 51 files.
    fn two(rule: &str, first: u32, then: u32) -> exec::Output {
        let both = [first, then].map(|line| found_at("src/a.rs", rule, line));
        scanned(51, &both.join(","))
    }

    fn found(file: &str, rule: &str) -> String {
        found_at(file, rule, 1)
    }

    #[test]
    fn a_finding_in_test_code_of_a_production_file_is_left_out() {
        let tested = |file: &str, line: usize| file == "src/a.rs" && (8..=10).contains(&line);
        let series = read_scan(&two("unwrap", 3, 9), &tested).unwrap().series;
        assert_eq!(series.get("src/a.rs#unwrap"), Some(1));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn test_code_is_a_test_module_a_gated_file_or_a_file_gated_from_within() {
        let held = crate::testdir::Held::tree(
            "scan-test-code",
            &[
                (
                    "src/lib.rs",
                    "pub mod a;\n#[cfg(all(test, feature = \"x\"))]\nmod law;\nmod own;\n",
                ),
                (
                    "src/a.rs",
                    "pub fn a() {}\n\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n",
                ),
                ("src/law.rs", "mod deep;\n"),
                ("src/law/deep.rs", "fn d() {}\n"),
                ("src/own.rs", "#![cfg(test)]\nfn o() {}\n"),
            ],
        );
        let code = prodlines::TestCode::of(&held).unwrap();
        let tested = |file: &str, line: usize| code.holds(file, line);
        assert!(!tested("src/a.rs", 1) && tested("src/a.rs", 3) && tested("src/a.rs", 6));
        assert!(!tested("src/a.rs", 7) && !tested("src/lib.rs", 1));
        assert!(tested("src/law.rs", 1) && tested("src/law/deep.rs", 1) && tested("src/own.rs", 2));
    }

    #[test]
    fn a_finding_is_keyed_by_the_file_and_the_rule_that_found_it() {
        let out = scanned(51, &found("src/a.rs", "interpolated-command"));
        let series = read_scan(&out, &untested).unwrap().series;
        assert_eq!(series.get("src/a.rs#interpolated-command"), Some(1));
    }

    #[test]
    fn two_findings_of_one_rule_in_one_file_count_twice_under_one_key() {
        let series = read_scan(&two("discarded-result", 1, 1), &untested)
            .unwrap()
            .series;
        assert_eq!(series.get("src/a.rs#discarded-result"), Some(2));
    }

    #[test]
    fn a_finding_keeps_the_line_outpost_gave_it() {
        let out = scanned(51, &found_at("src/a.rs", "interpolated-command", 7));
        let read = read_scan(&out, &untested).unwrap();
        let detail = read.details.get("src/a.rs#interpolated-command").unwrap();
        assert_eq!(detail.line, Some(7));
    }

    #[test]
    fn a_second_hit_under_one_key_is_a_place_beside_the_first_line() {
        let read = read_scan(&two("discarded-result", 3, 9), &untested).unwrap();
        let detail = read.details.get("src/a.rs#discarded-result").unwrap();
        assert_eq!(detail.line, Some(3));
        assert_eq!(detail.places, [Place::at("hit", "src/a.rs", 9)]);
    }

    #[test]
    fn every_hit_under_one_key_past_the_first_is_a_place() {
        let hits: Vec<String> = (0..23)
            .map(|n| {
                found_at(
                    "src/a.rs",
                    "discarded-result",
                    3 + u32::try_from(n).unwrap(),
                )
            })
            .collect();
        let read = read_scan(&scanned(51, &hits.join(",")), &untested).unwrap();
        let detail = read.details.get("src/a.rs#discarded-result").unwrap();
        let places: Vec<Place> = (1..23)
            .map(|n| Place::at("hit", "src/a.rs", 3 + n))
            .collect();
        assert_eq!(detail.line, Some(3));
        assert_eq!(detail.places, places);
    }

    #[test]
    fn findings_in_different_files_are_held_apart() {
        let both = format!(
            "{},{}",
            found("src/a.rs", "half-an-inverse"),
            found("src/b.rs", "half-an-inverse")
        );
        let series = read_scan(&scanned(51, &both), &untested).unwrap().series;
        assert_eq!(series.get("src/a.rs#half-an-inverse"), Some(1));
        assert_eq!(series.get("src/b.rs#half-an-inverse"), Some(1));
    }

    #[test]
    fn a_flow_from_a_source_to_a_sink_is_held_beside_the_rule_findings() {
        let out = flowed(
            55,
            &found("src/a.rs", "discarded-result"),
            &found("src/cli.rs", "rust-path-traversal"),
        );
        let series = read_scan(&out, &untested).unwrap().series;
        assert_eq!(series.get("src/a.rs#discarded-result"), Some(1));
        assert_eq!(series.get("src/cli.rs#rust-path-traversal"), Some(1));
    }

    #[test]
    fn a_tree_whose_only_problem_is_a_flow_does_not_read_as_clean() {
        let out = flowed(55, "", &found("src/cli.rs", "rust-command-injection"));
        let series = read_scan(&out, &untested).unwrap().series;
        assert_eq!(series.get("src/cli.rs#rust-command-injection"), Some(1));
    }

    #[test]
    fn a_finding_in_a_file_only_tests_build_is_not_counted() {
        let tested = [
            found("crates/a/tests/contract/model.rs", "unwrap-in-production"),
            found("site/tests/play.rs", "unwrap-in-production"),
            found("src/parse_tests.rs", "unwrap-in-production"),
            found("crates/a/src/parse/tests.rs", "unwrap-in-production"),
        ];
        let shipped = found("crates/a/src/lib.rs", "unwrap-in-production");
        let out = flowed(9, &tested.join(","), &shipped);
        let mut only = Series::new();
        only.set("crates/a/src/lib.rs#unwrap-in-production", 1);
        assert_eq!(read_scan(&out, &untested).unwrap().series, only);
    }

    #[test]
    fn a_tree_with_nothing_wrong_in_it_holds_nothing() {
        assert_eq!(
            read_scan(&scanned(51, ""), &untested).unwrap().series,
            Series::new()
        );
    }

    #[test]
    fn a_scan_that_read_no_files_could_not_run() {
        let err = read_scan(&scanned(0, ""), &untested).unwrap_err();
        assert_eq!(err, "outpost read no files, so no rule ran");
    }

    #[test]
    fn a_tree_outpost_refused_is_not_a_tree_with_nothing_wrong() {
        let refused = exec::Output::of(Some(1), "", "not a repository");
        assert!(read_scan(&refused, &untested).is_err());
    }

    #[test]
    fn a_scan_in_a_shape_chock_does_not_know_stops_the_scan_gate() {
        let odd = exec::Output::of(Some(0), "not json", "");
        assert!(
            read_scan(&odd, &untested)
                .unwrap_err()
                .starts_with("outpost printed a scan chock cannot read")
        );
    }
}
