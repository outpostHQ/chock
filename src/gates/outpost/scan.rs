//! Outpost's syntax-tree rules, and taint flows from untrusted input to a sink.

use serde::Deserialize;

use crate::exec;
use crate::gates::metrics::prodlines;
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

/// Counts defects that shipped before, and untrusted input that reaches a sink.
pub const GATE: Gate = Gate {
    name: "scan",
    about: "a defect that has shipped before, or untrusted input reaching a sink; needs `outpost`",
    group: Group::OptIn,
    builds: false,
    reads: Some(crate::run::verdicts::OUTPOST),
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "finding(s)",
    },
};

fn measure(ctx: &Ctx) -> Result<Series, String> {
    read_scan(&super::spawn(&ctx.root, &["scan", "rules", "--json"])?)
}

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
}

/// Rule and taint findings outside the files only tests build, which Outpost may miss; a scan
/// that read no files is an error, not a clean tree.
fn read_scan(out: &exec::Output) -> Result<Series, String> {
    let scan: Scan = super::read(out, "scan")?;
    if scan.languages.values().all(|read| read.files == 0) {
        return Err(super::nothing_read("rule"));
    }
    let shipped = |found: &&Found| !prodlines::is_test_path(&found.file);
    Ok(keyed(
        scan.findings.iter().chain(&scan.taint).filter(shipped),
    ))
}

/// Keyed by file and rule, not line, so an edit above a finding does not read as a new one.
fn keyed<'a>(findings: impl Iterator<Item = &'a Found>) -> Series {
    let mut series = Series::new();
    for found in findings {
        let key = format!("{}#{}", found.file, found.rule);
        series.set(&key, series.get(&key).unwrap_or(0) + 1);
    }
    series
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn found(file: &str, rule: &str) -> String {
        format!(r#"{{"file":"{file}","rule":"{rule}","line":1}}"#)
    }

    #[test]
    fn a_finding_is_keyed_by_the_file_and_the_rule_that_found_it() {
        let out = scanned(51, &found("src/a.rs", "interpolated-command"));
        let series = read_scan(&out).unwrap();
        assert_eq!(series.get("src/a.rs#interpolated-command"), Some(1));
    }

    #[test]
    fn two_findings_of_one_rule_in_one_file_count_twice_under_one_key() {
        let both = format!(
            "{},{}",
            found("src/a.rs", "discarded-result"),
            found("src/a.rs", "discarded-result")
        );
        let series = read_scan(&scanned(51, &both)).unwrap();
        assert_eq!(series.get("src/a.rs#discarded-result"), Some(2));
    }

    #[test]
    fn findings_in_different_files_are_held_apart() {
        let both = format!(
            "{},{}",
            found("src/a.rs", "half-an-inverse"),
            found("src/b.rs", "half-an-inverse")
        );
        let series = read_scan(&scanned(51, &both)).unwrap();
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
        let series = read_scan(&out).unwrap();
        assert_eq!(series.get("src/a.rs#discarded-result"), Some(1));
        assert_eq!(series.get("src/cli.rs#rust-path-traversal"), Some(1));
    }

    #[test]
    fn a_tree_whose_only_problem_is_a_flow_does_not_read_as_clean() {
        let out = flowed(55, "", &found("src/cli.rs", "rust-command-injection"));
        let series = read_scan(&out).unwrap();
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
        assert_eq!(read_scan(&out).unwrap(), only);
    }

    #[test]
    fn a_tree_with_nothing_wrong_in_it_holds_nothing() {
        assert_eq!(read_scan(&scanned(51, "")).unwrap(), Series::new());
    }

    #[test]
    fn a_scan_that_read_no_files_could_not_run() {
        let err = read_scan(&scanned(0, "")).unwrap_err();
        assert_eq!(err, "outpost read no files, so no rule ran");
    }

    #[test]
    fn a_tree_outpost_refused_is_not_a_tree_with_nothing_wrong() {
        let refused = exec::Output::of(Some(1), "", "not a repository");
        assert!(read_scan(&refused).is_err());
    }

    #[test]
    fn a_scan_in_a_shape_chock_does_not_know_stops_the_scan_gate() {
        let odd = exec::Output::of(Some(0), "not json", "");
        assert!(
            read_scan(&odd)
                .unwrap_err()
                .starts_with("outpost printed a scan chock cannot read")
        );
    }
}
