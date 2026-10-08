//! Gates over Outpost's parsed index and call graph: hazards, lenses, unreferenced and unread code.
//! Each needs `outpost`, so each is opt-in.

use serde::Deserialize;

use crate::exec;
use crate::run::baseline::{Keys, Series};
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Measurement, Outcome};

pub const HAZARDS: Gate = Gate {
    name: "hazards",
    about: "syntax that is about to be wrong — a value spliced into a command, a comparison whose \
            halves match, a loop re-reading what it just read; needs `outpost`",
    group: Group::OptIn,
    builds: false,
    reads: Some(crate::run::verdicts::OUTPOST),
    kind: Kind::Ratchet {
        measure: hazards,
        // Keyed by file, so a hazard in a file that had none shows as new debt there.
        keys: Keys::Items,
        unit: "hazard(s)",
    },
};

pub const UNREFERENCED: Gate = Gate {
    name: "unreferenced",
    about: "shipped Rust that nothing reaches, or that only its own tests reach; needs `outpost`",
    group: Group::OptIn,
    builds: false,
    reads: Some(crate::run::verdicts::OUTPOST),
    kind: Kind::Ratchet {
        measure: unreferenced,
        keys: Keys::Items,
        unit: "line(s) no shipped path reaches",
    },
};

pub const UNREAD: Gate = Gate {
    name: "unread",
    about: "regions outpost could not read, so no lens judged them; an advisory that never fails, \
            because a grammar lagging the language is nobody's debt to pay; needs `outpost`",
    group: Group::OptIn,
    builds: false,
    reads: Some(crate::run::verdicts::OUTPOST),
    kind: Kind::Binary(unread),
};

/// Regions Outpost's grammar could not model, reported but never ratcheted: the gap is the tool's.
fn unread(ctx: &Ctx) -> Result<Outcome, String> {
    Ok(gaps(super::once(ctx)?))
}

/// Every unread region as an advisory, with a passing verdict.
fn gaps(check: &super::Check) -> Outcome {
    Outcome::noted(check.read_around.iter().map(|at| gap(at)).collect())
}

/// One region, given as `path:line` or a bare path.
fn gap(at: &str) -> Finding {
    let message = "outpost read around this line, so no lens judged it";
    match at
        .rsplit_once(':')
        .and_then(|(file, line)| line.parse::<u32>().ok().map(|line| (file, line)))
    {
        Some((file, line)) => Finding::at(file, message).line(line),
        None => Finding::at(at, message),
    }
}

pub const LENSES: Gate = Gate {
    name: "lenses",
    about: "every hazard lens outpost still reports, so one retired does not read as the hazards \
            being fixed; needs `outpost`",
    group: Group::OptIn,
    builds: false,
    reads: Some(crate::run::verdicts::OUTPOST),
    kind: Kind::AnnotatedRatchet {
        measure: lenses,
        // One row per lens, zeros included, so a missing row means the lens was retired.
        keys: Keys::Census,
        unit: "hazard(s)",
    },
};

fn hazards(ctx: &Ctx) -> Result<Series, String> {
    read_hazards(super::once(ctx)?)
}

/// Its own `outpost measure dead` call: the shared check's `dead_items` is a different analysis
/// and carries no `uncertain` field to filter on.
fn unreferenced(ctx: &Ctx) -> Result<Series, String> {
    let out = super::spawn(&ctx.root, &["measure", "dead", "--json"])?;
    read_unreferenced(&out, ctx)
}

/// One row per lens, with the sites of any lens counted past its record.
fn lenses(ctx: &Ctx) -> Result<Measurement, String> {
    let check = super::once(ctx)?;
    let series = read_lenses(check)?;
    let findings = check.sites_over(&series, &ctx.record(LENSES.name));
    Ok(Measurement::of(series, findings))
}

/// The prefix of every hazard lens; the roster comes from Outpost's response, not a list here.
const LENS: &str = "hazard_";

fn read_hazards(check: &super::Check) -> Result<Series, String> {
    let mut series = Series::new();
    for lens in check.measures(&|name| name.starts_with(LENS)) {
        for found in lens
            .ran()?
            .findings
            .iter()
            .filter(|found| !found.advisory())
        {
            let key = format!("{}#{}", found.file, lens.measure);
            series.set(&key, series.get(&key).unwrap_or(0) + 1);
        }
    }
    Ok(series)
}

/// One row per lens, zeros included, so a retired lens differs from a quiet one. No `note` counts.
fn read_lenses(check: &super::Check) -> Result<Series, String> {
    let roster = check.measures(&|name| name.starts_with(LENS));
    if roster.is_empty() {
        return Err(super::nothing_read("lens"));
    }
    let mut series = Series::new();
    for lens in roster {
        let held = &lens.ran()?.findings;
        let debt = held.iter().filter(|found| !found.advisory()).count();
        series.set(&lens.measure, u64::try_from(debt).unwrap_or(u64::MAX));
    }
    Ok(series)
}

#[derive(Deserialize)]
struct Dead {
    #[serde(default)]
    files_indexed: u64,
    #[serde(default)]
    found: Vec<Unreached>,
}

#[derive(Deserialize)]
struct Unreached {
    path: String,
    name: String,
    #[serde(default)]
    lines: u64,
    #[serde(default)]
    origin: String,
    /// Who the graph found reaching it: `nothing`, or `tests_only` when every caller is a test.
    #[serde(default)]
    callers: String,
    /// Present when Outpost could not resolve every edge to this entity, e.g. an unknown receiver.
    #[serde(default)]
    uncertain: Option<serde_json::Value>,
}

/// The `origin` Outpost gives shipped code, the only origin this gate reports.
const SHIPPED: &str = "production";

/// The graph found callers and all are tests. The mention-count veto is skipped, since the mentions
/// it would count are those tests.
const TESTS_ONLY: &str = "tests_only";

fn read_unreferenced(out: &exec::Output, ctx: &Ctx) -> Result<Series, String> {
    let read: Dead = super::read(out, "measure")?;
    if read.files_indexed == 0 {
        return Err(super::nothing_read("reference"));
    }
    // The graph alone misses calls, so only what a token scan also finds unreached is reported.
    let named = crate::gates::metrics::dead::names_in_tree(ctx)?;
    let resolved: std::collections::BTreeSet<(&str, &str)> = read
        .found
        .iter()
        .filter(|one| one.callers == TESTS_ONLY)
        .map(|one| (one.path.as_str(), one.name.as_str()))
        .collect();
    keyed(&read.found, &|path, name| {
        // chock's token scan reads only Rust, and an uncorroborated finding is not reported.
        if !path.ends_with(".rs") {
            return Ok(true);
        }
        if !resolved.contains(&(path, name)) && named.get(name).is_some_and(|m| *m > 1) {
            return Ok(true);
        }
        let src = std::fs::read_to_string(ctx.root.join(path))
            .map_err(|e| format!("outpost indexed {path}, which chock cannot read: {e}"))?;
        crate::gates::metrics::dead::is_entry_point(&src, name)
            .map_err(|why| format!("{path}: {why}"))
    })
}

/// Shipped, certain findings that are not entry points, keyed by file and name at their largest
/// size. `entry_point` is injected for tests, and its error stops the gate.
fn keyed(
    found: &[Unreached],
    entry_point: &dyn Fn(&str, &str) -> Result<bool, String>,
) -> Result<Series, String> {
    let mut series = Series::new();
    let shipped_and_certain = |f: &&Unreached| f.origin == SHIPPED && f.uncertain.is_none();
    for one in found.iter().filter(shipped_and_certain) {
        if entry_point(&one.path, &one.name)? {
            continue;
        }
        let key = format!("{}#{}", one.path, one.name);
        let worst = series.get(&key).unwrap_or(0).max(one.lines);
        series.set(&key, worst);
    }
    Ok(series)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::testdir::Held;

    fn never(_path: &str, _name: &str) -> Result<bool, String> {
        Ok(false)
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_function_the_graph_and_the_tree_both_say_nothing_reaches_is_reported() {
        let dir = Held::tree(
            "intel-unreferenced",
            &[
                ("Cargo.toml", "[package]\nname = \"p\"\n"),
                ("src/lib.rs", "fn helper() {}\n"),
            ],
        );
        let json = r#"{"files_indexed":1,"found":[
            {"path":"src/lib.rs","name":"helper","lines":21,"origin":"production"}]}"#;
        let series = read_unreferenced(&said(json), &dir).unwrap();
        assert_eq!(series.get("src/lib.rs#helper"), Some(21));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_function_the_tree_still_names_is_not_reported_however_the_graph_scored_it() {
        let dir = Held::tree(
            "intel-corroborated",
            &[
                ("Cargo.toml", "[package]\nname = \"p\"\n"),
                ("src/lib.rs", "fn helper() {}\nfn caller() { helper(); }\n"),
            ],
        );
        let json = r#"{"files_indexed":1,"found":[
            {"path":"src/lib.rs","name":"helper","lines":21,"origin":"production"}]}"#;
        assert_eq!(read_unreferenced(&said(json), &dir).unwrap(), Series::new());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_function_the_graph_says_only_tests_reach_is_reported_though_the_tree_names_it_twice() {
        let dir = Held::tree(
            "intel-tests-only",
            &[
                ("Cargo.toml", "[package]\nname = \"p\"\n"),
                (
                    "src/lib.rs",
                    "fn helper() {}\n#[test]\nfn t() { helper(); }\n",
                ),
            ],
        );
        let said_by = |callers: &str| {
            format!(
                r#"{{"files_indexed":1,"found":[
                {{"path":"src/lib.rs","name":"helper","lines":21,"origin":"production",
                  "callers":"{callers}"}}]}}"#
            )
        };
        let series = read_unreferenced(&said(&said_by("tests_only")), &dir).unwrap();
        assert_eq!(series.get("src/lib.rs#helper"), Some(21));
        // With `nothing`, the mention may be a caller the graph missed, so the veto still applies.
        assert_eq!(
            read_unreferenced(&said(&said_by("nothing")), &dir).unwrap(),
            Series::new()
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_method_reached_through_a_trait_is_not_reported() {
        let dir = Held::tree(
            "intel-trait-method",
            &[
                ("Cargo.toml", "[package]\nname = \"p\"\n"),
                (
                    "src/lib.rs",
                    "struct S;\ntrait T { fn go(&self); }\nimpl T for S { fn go(&self) {} }\n",
                ),
            ],
        );
        let json = r#"{"files_indexed":1,"found":[
            {"path":"src/lib.rs","name":"go","lines":4,"origin":"production"}]}"#;
        assert_eq!(read_unreferenced(&said(json), &dir).unwrap(), Series::new());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn an_entity_in_a_language_this_scan_cannot_read_is_left_alone() {
        let dir = Held::tree(
            "intel-other-language",
            &[
                ("Cargo.toml", "[package]\nname = \"p\"\n"),
                ("src/lib.rs", "fn helper() {}\n"),
            ],
        );
        let json = r#"{"files_indexed":2,"found":[
            {"path":"py/app.py","name":"read","lines":12,"origin":"production"}]}"#;
        assert_eq!(read_unreferenced(&said(json), &dir).unwrap(), Series::new());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_outpost_named_and_the_tree_does_not_hold_stops_the_gate() {
        let dir = Held::tree(
            "intel-absent-file",
            &[
                ("Cargo.toml", "[package]\nname = \"p\"\n"),
                ("src/lib.rs", "fn helper() {}\n"),
            ],
        );
        let json = r#"{"files_indexed":2,"found":[
            {"path":"src/gone.rs","name":"helper","lines":3,"origin":"production"}]}"#;
        let err = read_unreferenced(&said(json), &dir).unwrap_err();
        assert!(err.starts_with("outpost indexed src/gone.rs"), "{err}");
    }

    fn unreached(path: &str, name: &str, lines: u64, origin: &str) -> Unreached {
        Unreached {
            path: path.to_string(),
            name: name.to_string(),
            lines,
            origin: origin.to_string(),
            callers: String::new(),
            uncertain: None,
        }
    }

    #[test]
    fn an_entity_outpost_is_not_sure_about_is_not_reported_however_the_scan_read_it() {
        let mut doubted = unreached("src/a.rs", "helper", 9, SHIPPED);
        doubted.uncertain = Some(serde_json::json!({"receiver_unknown": true}));
        assert_eq!(keyed(&[doubted], &never).unwrap(), Series::new());
    }

    #[test]
    fn an_entity_outpost_is_sure_about_is_still_reported() {
        let found = [unreached("src/a.rs", "helper", 9, SHIPPED)];
        let series = keyed(&found, &never).unwrap();
        assert_eq!(series.get("src/a.rs#helper"), Some(9));
    }

    fn nowhere() -> &'static std::path::Path {
        std::path::Path::new("/nonexistent")
    }

    /// A context over a missing tree, for cases that refuse before reading one.
    fn nowhere_ctx() -> Ctx {
        Ctx::at(nowhere())
    }

    fn checked_from(json: &str) -> super::super::Check {
        super::super::checked(&said(json)).unwrap()
    }

    #[test]
    fn a_region_outpost_read_around_is_no_part_of_the_hazard_count() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "hazard_long_comment_block", "verdict": "passed",
               "findings": [{"file": "crates/a/src/x.rs", "item": "f"}]}],
              "read_around": ["crates/a/src/y.rs:412", "crates/b/src/z.rs:9"]}"#,
        );
        let mut only = Series::new();
        only.set("crates/a/src/x.rs#hazard_long_comment_block", 1);
        assert_eq!(read_hazards(&check).unwrap(), only);
    }

    #[test]
    fn a_region_outpost_read_around_is_reported_as_an_advisory_that_passes() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [],
              "read_around": ["crates/a/src/y.rs:412", "crates/b/src/z.rs"]}"#,
        );
        let outcome = gaps(&check);
        assert!(outcome.passed);
        assert_eq!(
            Finding::rendered(&outcome.findings),
            [
                // The line is split off so an editor opens where the region begins.
                "crates/a/src/y.rs:412: outpost read around this line, so no lens judged it",
                "crates/b/src/z.rs: outpost read around this line, so no lens judged it"
            ]
        );
    }

    #[test]
    fn a_tree_with_no_unread_region_reports_nothing() {
        let check = checked_from(r#"{"contract": 1, "measures": []}"#);
        let outcome = gaps(&check);
        assert!(outcome.passed);
        assert_eq!(outcome.findings, Vec::new());
    }

    #[test]
    fn a_lens_that_could_not_run_refuses_rather_than_counting_zero() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "hazard_interpolated_command", "verdict": "cannot_run",
               "cannot_run_reason": "the grammar could not open the tree", "findings": []}]}"#,
        );
        assert_eq!(
            read_hazards(&check).unwrap_err(),
            "outpost could not measure `hazard_interpolated_command`: the grammar could not open \
             the tree"
        );
        assert!(read_lenses(&check).is_err());
    }

    #[test]
    fn the_roster_carries_a_row_for_every_lens_including_the_quiet_ones() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "hazard_half_an_inverse", "verdict": "passed", "findings": []},
              {"measure": "hazard_long_comment_block", "verdict": "passed",
               "findings": [{"file": "a.rs"}, {"file": "b.rs"}]},
              {"measure": "duplicate_groups", "verdict": "passed", "findings": [{"file": "c.rs"}]}]}"#,
        );
        let series = read_lenses(&check).unwrap();
        assert_eq!(series.get("hazard_half_an_inverse"), Some(0));
        assert_eq!(series.get("hazard_long_comment_block"), Some(2));
        // A measure that is not a hazard lens belongs to another gate, not this roster.
        assert_eq!(series.get("duplicate_groups"), None);
    }

    #[test]
    fn a_note_is_advice_and_no_part_of_a_lens_count() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "hazard_expensive_call_in_loop", "verdict": "passed", "findings": [
                {"file": "src/a.rs", "severity": "warn"},
                {"file": "src/a.rs", "severity": "note"},
                {"file": "src/b.rs"}]}]}"#,
        );
        let series = read_lenses(&check).unwrap();
        assert_eq!(series.get("hazard_expensive_call_in_loop"), Some(2));
    }

    /// The `note` beside the warning is neither counted nor named.
    #[test]
    fn a_lens_past_its_record_names_where_its_findings_sit() {
        let ctx = Ctx::for_root(
            std::path::PathBuf::from("/nowhere"),
            crate::run::baseline::Baseline::default(),
        );
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "hazard_expensive_call_in_loop", "verdict": "passed", "findings": [
                {"file": "src/a.rs", "line": 4, "message": "sync_all per item", "severity": "warn"},
                {"file": "src/a.rs", "line": 9, "severity": "note"}]}]}"#,
        );
        assert!(ctx.checked.set(Ok(check)).is_ok());
        let measured = lenses(&ctx).unwrap();
        assert_eq!(
            measured.series.get("hazard_expensive_call_in_loop"),
            Some(1)
        );
        let told: Vec<String> = Finding::rendered(&measured.findings);
        assert_eq!(
            told,
            ["src/a.rs:4: hazard_expensive_call_in_loop: sync_all per item"]
        );
    }

    #[test]
    fn a_response_with_no_lens_in_it_is_a_gate_that_could_not_run() {
        let check = checked_from(r#"{"contract": 1, "measures": []}"#);
        assert_eq!(
            read_lenses(&check).unwrap_err(),
            "outpost read no files, so no lens ran"
        );
    }

    fn said(stdout: &str) -> exec::Output {
        exec::Output::of(Some(0), stdout, "")
    }

    fn refused() -> exec::Output {
        exec::Output::of(Some(1), "", "not a repository")
    }

    #[test]
    fn a_hazard_is_counted_against_the_file_that_holds_it() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "hazard_half_an_inverse", "verdict": "passed", "findings": [
                {"file": "src/a.rs", "item": "f"}, {"file": "src/b.rs", "item": "g"}]}]}"#,
        );
        let found = read_hazards(&check).unwrap();
        assert_eq!(found.get("src/a.rs#hazard_half_an_inverse"), Some(1));
        assert_eq!(found.get("src/b.rs#hazard_half_an_inverse"), Some(1));
    }

    #[test]
    fn an_advisory_hazard_is_not_debt_and_one_without_a_severity_still_is() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "hazard_shell_splice", "verdict": "passed", "findings": [
                {"file": "tests/a.rs", "item": "f", "severity": "note"},
                {"file": "src/a.rs", "item": "g", "severity": "deny"},
                {"file": "src/a.rs", "item": "h", "severity": "warn"},
                {"file": "src/b.rs", "item": "k"}]}]}"#,
        );
        let series = read_hazards(&check).unwrap();
        assert_eq!(series.get("tests/a.rs#hazard_shell_splice"), None);
        assert_eq!(series.get("src/a.rs#hazard_shell_splice"), Some(2));
        assert_eq!(series.get("src/b.rs#hazard_shell_splice"), Some(1));
    }

    #[test]
    fn two_hazards_of_one_lens_in_one_file_are_counted_twice() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "hazard_long_comment_block", "verdict": "passed", "findings": [
                {"file": "src/a.rs", "item": "f"}, {"file": "src/a.rs", "item": "g"}]}]}"#,
        );
        assert_eq!(
            read_hazards(&check)
                .unwrap()
                .get("src/a.rs#hazard_long_comment_block"),
            Some(2)
        );
    }

    #[test]
    fn a_lens_that_found_nothing_contributes_no_key_to_the_hazards() {
        let check = checked_from(
            r#"{"contract": 1, "measures": [
              {"measure": "hazard_half_an_inverse", "verdict": "passed", "findings": []}]}"#,
        );
        assert_eq!(read_hazards(&check).unwrap(), Series::new());
    }

    #[test]
    fn shipped_code_only_tests_reach_is_carried_with_its_size() {
        let found = keyed(&[unreached("src/a.rs", "helper", 21, SHIPPED)], &never).unwrap();
        assert_eq!(found.get("src/a.rs#helper"), Some(21));
    }

    #[test]
    fn test_code_nothing_references_is_left_alone() {
        let found = keyed(&[unreached("src/a.rs", "fixture", 9, "test")], &never).unwrap();
        assert_eq!(found, Series::new());
    }

    /// The shipped entity comes after the skipped one.
    #[test]
    fn a_skipped_entity_is_stepped_over_rather_than_ending_the_read() {
        let found = keyed(
            &[
                unreached("src/a.rs", "fixture", 9, "test"),
                unreached("src/b.rs", "shipped", 7, SHIPPED),
            ],
            &never,
        )
        .unwrap();
        assert_eq!(found.get("src/b.rs#shipped"), Some(7));
    }

    /// The entry point comes before the shipped entity.
    #[test]
    fn an_entry_point_is_stepped_over_rather_than_ending_the_read() {
        let found = [
            unreached("src/a.rs", "proof", 15, SHIPPED),
            unreached("src/b.rs", "shipped", 7, SHIPPED),
        ];
        let series = keyed(&found, &|_, name| Ok(name == "proof")).unwrap();
        assert_eq!(series.get("src/b.rs#shipped"), Some(7));
        assert_eq!(series.get("src/a.rs#proof"), None);
    }

    #[test]
    fn two_entities_sharing_a_name_are_held_at_the_larger_of_them() {
        let found = keyed(
            &[
                unreached("src/a.rs", "parse", 4, SHIPPED),
                unreached("src/a.rs", "parse", 12, SHIPPED),
            ],
            &never,
        )
        .unwrap();
        assert_eq!(found.get("src/a.rs#parse"), Some(12));
    }

    #[test]
    fn an_entity_in_a_language_chock_cannot_corroborate_is_not_reported() {
        let found = [unreached(
            "outpost-python/python/outpost/data_frame.py",
            "read",
            12,
            SHIPPED,
        )];
        let series = keyed(&found, &|path, _| Ok(!path.ends_with(".rs"))).unwrap();
        assert_eq!(series, Series::new());
    }

    #[test]
    fn a_probe_that_could_not_answer_stops_the_gate() {
        let found = [unreached("src/a.rs", "helper", 21, SHIPPED)];
        let err = keyed(&found, &|path, _| Err(format!("cannot read {path}"))).unwrap_err();
        assert_eq!(err, "cannot read src/a.rs");
    }

    #[test]
    fn an_entity_the_tree_still_mentions_is_not_reported() {
        let found = [
            unreached("src/a.rs", "mentioned", 9, SHIPPED),
            unreached("src/b.rs", "nowhere", 7, SHIPPED),
        ];
        let series = keyed(&found, &|_, name| Ok(name == "mentioned")).unwrap();
        assert_eq!(series.get("src/b.rs#nowhere"), Some(7));
        assert_eq!(series.get("src/a.rs#mentioned"), None);
    }

    #[test]
    fn an_index_that_read_nothing_is_not_a_tree_everything_is_reached_in() {
        let err = read_unreferenced(&said(r#"{"files_indexed":0,"found":[]}"#), &nowhere_ctx())
            .unwrap_err();
        assert_eq!(err, "outpost read no files, so no reference ran");
    }

    /// Outpost resolves references, not attributes, so it reports a kani harness or a test as
    /// unreached.
    #[test]
    fn an_entry_point_is_not_code_nothing_reaches() {
        let found = [unreached("src/version.rs", "a_proof", 15, SHIPPED)];
        assert_eq!(keyed(&found, &|_, _| Ok(true)).unwrap(), Series::new());
        assert_eq!(
            keyed(&found, &|_, _| Ok(false))
                .unwrap()
                .get("src/version.rs#a_proof"),
            Some(15)
        );
    }

    #[test]
    fn a_tree_outpost_refused_stops_either_gate() {
        assert!(super::super::checked(&refused()).is_err());
        assert!(read_unreferenced(&refused(), &nowhere_ctx()).is_err());
    }

    #[test]
    fn a_contract_this_chock_does_not_read_is_refused() {
        let ahead = said(r#"{"contract": 2, "measures": []}"#);
        assert_eq!(
            super::super::checked(&ahead).unwrap_err(),
            "outpost speaks contract 2 and this chock reads 1; upgrade chock"
        );
    }

    #[test]
    fn output_in_a_shape_chock_does_not_know_stops_either_gate() {
        let unreadable = said("not json");
        for (err, what) in [
            (super::super::checked(&unreadable).unwrap_err(), "check"),
            (
                read_unreferenced(&unreadable, &nowhere_ctx()).unwrap_err(),
                "measure",
            ),
        ] {
            assert!(
                err.starts_with(&format!("outpost printed a {what} chock cannot read")),
                "{err}"
            );
        }
    }
}
