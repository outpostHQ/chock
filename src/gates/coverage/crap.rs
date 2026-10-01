//! Complexity × uncoverage per function, via cargo-crap. Pass or fail, not a ratchet: cargo-crap
//! scores each function, and chock holds each score to the record and counts what is over 30.

use std::collections::BTreeMap;
use std::path::Path;

use crate::exec;
use crate::run::debt::{Debt, Held};
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

/// Under `.chock/` with chock's other committed state, so it never overwrites a project's own file.
/// Each system keeps its own, as each keeps its own records in `.chock/baseline.json`.
#[must_use]
pub fn baseline() -> String {
    kept_on(std::env::consts::OS)
}

/// Linux keeps the plain name, as every system did before each kept its own.
fn kept_on(system: &str) -> String {
    match system {
        "linux" => ".chock/crap-baseline.json".to_string(),
        _ => format!(".chock/crap-baseline@{system}.json"),
    }
}

/// The lcov file the `coverage` gate writes, named once so both gates read the same file.
pub use super::FILE as COVERAGE;

pub const GATE: Gate = Gate {
    name: "crap",
    about: "complexity x uncoverage per function, against this system's .chock/crap-baseline",
    group: Group::Quality,
    builds: true,
    reads: None,
    kind: Kind::Binary(check),
};

/// cargo-crap's default threshold: its own table marks every function above this.
const OVER: f64 = 30.0;

/// What the run's two numbers count, so a pass still shows the debt its record holds.
const UNIT: &str = "function(s) over CRAP 30";

/// Both inputs are in place before cargo-crap runs, since it reads a missing lcov as an empty one.
fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let baseline = baseline();
    missing(&ctx.root, &baseline, "run `chock baseline`")?;
    let record = recorded(&ctx.root)?;
    crate::gates::coverage::ensure(ctx)?;
    let out = exec::run(
        "cargo",
        &[
            "crap",
            "--lcov",
            COVERAGE,
            "--workspace",
            "--baseline",
            &baseline,
            "--fail-regression",
            "--format",
            "json",
        ],
        &ctx.root,
    )
    .map_err(|e| e.to_string())?;
    judged(&out.stdout, out.success(), &ctx.root, &record)
}

/// cargo-crap's JSON report, and its record file, which has the same shape: only the fields read.
#[derive(serde::Deserialize)]
struct Delta {
    entries: Vec<Scored>,
    /// What the record holds that cargo-crap matched to no function. Absent in the record file.
    #[serde(default)]
    removed: Vec<Gone>,
}

/// A recorded function cargo-crap matched to nothing in this run: only the fields this gate reads.
#[derive(serde::Deserialize)]
struct Gone {
    file: String,
    function: String,
    baseline_crap: f64,
}

/// cargo-crap's own tolerance: a score this close to the record is the same score.
const EPSILON: f64 = 0.01;

/// What cargo-crap says about one function: only the fields this gate reads.
#[derive(serde::Deserialize)]
struct Scored {
    file: String,
    function: String,
    line: u32,
    crap: f64,
    /// Absent in the record file, which holds scores and no comparison.
    #[serde(default)]
    status: String,
    baseline_crap: Option<f64>,
}

impl Scored {
    fn over(&self) -> bool {
        self.crap > OVER
    }

    /// A score above the record's, or a function the record lacks that is over the threshold.
    fn worse(&self) -> bool {
        match self.status.as_str() {
            "regressed" => true,
            "new" => self.over(),
            _ => false,
        }
    }

    /// Judged as the recorded function that scored `was`.
    fn against(&mut self, was: f64) {
        self.baseline_crap = Some(was);
        let status = match self.crap > was + EPSILON {
            true => "regressed",
            false => "unchanged",
        };
        self.status = status.to_string();
    }

    fn finding(&self, root: &Path) -> Finding {
        let against = match self.baseline_crap {
            Some(was) => format!("was {was:.1}"),
            None => "not in the baseline".to_string(),
        };
        Finding::at(
            &crate::project::relative(root, Path::new(&self.file)),
            &format!("CRAP {:.1}, {against}", self.crap),
        )
        .line(self.line)
        .item(&self.function)
    }
}

fn read(json: &str) -> Result<Delta, String> {
    serde_json::from_str(json)
        .map_err(|e| format!("cargo-crap produced a report chock cannot read: {e}"))
}

/// What tells one function from the next in the record and in the run: its file and its name.
fn named(root: &Path, file: &str, function: &str) -> (String, String) {
    let file = crate::project::relative(root, Path::new(file));
    (file, function.to_string())
}

/// The recorded scores cargo-crap matched to nothing, by file and name, each list ascending.
fn unmatched(removed: &[Gone], root: &Path) -> BTreeMap<(String, String), Vec<f64>> {
    let mut scores: BTreeMap<(String, String), Vec<f64>> = BTreeMap::new();
    for gone in removed {
        let key = named(root, &gone.file, &gone.function);
        scores.entry(key).or_default().push(gone.baseline_crap);
    }
    for list in scores.values_mut() {
        list.sort_by(f64::total_cmp);
    }
    scores
}

/// One function among those of its file and name: its line, its place in the report, its score.
struct Site {
    line: u32,
    at: usize,
    crap: f64,
}

/// Each file and name with its functions in file order.
fn by_name(entries: &[Scored], root: &Path) -> BTreeMap<(String, String), Vec<Site>> {
    let mut found: BTreeMap<(String, String), Vec<Site>> = BTreeMap::new();
    for (at, entry) in entries.iter().enumerate() {
        let (line, crap) = (entry.line, entry.crap);
        let key = named(root, &entry.file, &entry.function);
        found.entry(key).or_default().push(Site { line, at, crap });
    }
    for sites in found.values_mut() {
        sites.sort_by_key(|site| (site.line, site.at));
    }
    found
}

/// The recorded score for each function of `measured`, by its place there. A file and name pair
/// in file order, and only where the record holds as many: a line moves, and the order stays.
fn held_to(measured: &[Scored], record: &[Scored], root: &Path) -> BTreeMap<usize, f64> {
    let held = by_name(record, root);
    let mut floors = BTreeMap::new();
    for (key, sites) in by_name(measured, root) {
        let even = held.get(&key).filter(|was| was.len() == sites.len());
        for (site, was) in sites.iter().zip(even.into_iter().flatten()) {
            floors.insert(site.at, was.crap);
        }
    }
    floors
}

/// `held_to` for `chock baseline crap`, which has both reports as text. A record that cannot be
/// read holds nothing.
pub fn floors(measured: &str, record: &str, root: &Path) -> Result<BTreeMap<usize, f64>, String> {
    let record = read(record).map(|held| held.entries).unwrap_or_default();
    Ok(held_to(&read(measured)?.entries, &record, root))
}

/// Holds each function to the record by file, name and file order. Where the record has another
/// number of that name, cargo-crap's own match stands, then a new one takes a removed one.
fn paired(mut delta: Delta, record: &[Scored], root: &Path) -> Delta {
    let floors = held_to(&delta.entries, record, root);
    for (at, entry) in delta.entries.iter_mut().enumerate() {
        if let Some(was) = floors.get(&at) {
            entry.against(*was);
        }
    }
    let mut scores = unmatched(&delta.removed, root);
    // Worst against worst, since nothing tells two functions of one name apart.
    delta.entries.sort_by(|a, b| b.crap.total_cmp(&a.crap));
    let new = delta
        .entries
        .iter_mut()
        .filter(|entry| entry.status == "new");
    for entry in new {
        let key = named(root, &entry.file, &entry.function);
        if let Some(was) = scores.get_mut(&key).and_then(Vec::pop) {
            entry.against(was);
        }
    }
    delta
}

/// Every function in the record under `root`.
fn recorded(root: &Path) -> Result<Vec<Scored>, String> {
    let name = baseline();
    let text = std::fs::read_to_string(root.join(&name)).map_err(|e| format!("{name}: {e}"))?;
    let record = read(&text).map_err(|e| format!("{name}: {e}"))?;
    Ok(record.entries)
}

/// The functions over the threshold in the record under `root`, worst first.
pub fn on_record(root: &Path) -> Result<Vec<(String, f64)>, String> {
    let mut over: Vec<(String, f64)> = recorded(root)?
        .iter()
        .filter(|entry| entry.over())
        .map(|entry| {
            let file = crate::project::relative(root, Path::new(&entry.file));
            (format!("{file}: {}", entry.function), entry.crap)
        })
        .collect();
    over.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(over)
}

/// That record as debt for `chock explain`: each function with its score, rounded. A project with
/// no record file holds none.
pub fn debt(root: &Path) -> Result<Option<Debt>, String> {
    if !root.join(baseline()).is_file() {
        return Ok(None);
    }
    let held = on_record(root)?;
    Ok((!held.is_empty()).then(|| Debt {
        gate: GATE.name.to_string(),
        unit: UNIT.to_string(),
        total: held.len() as u64,
        items: held
            .into_iter()
            .map(|(key, score)| Held {
                key,
                count: score.round() as u64,
            })
            .collect(),
        fix: crate::gates::fixes::fix(GATE.name),
    }))
}

fn missing(root: &Path, name: &str, remedy: &str) -> Result<(), String> {
    if root.join(name).is_file() {
        return Ok(());
    }
    Err(format!("no {name} — {remedy}"))
}

/// What a pass says where the record lacks functions: nothing holds one until it is over 30.
fn unheld(delta: &Delta) -> Vec<Finding> {
    let new = delta.entries.iter().filter(|entry| entry.status == "new");
    let lacking = new.count();
    let note = format!(
        "{lacking} function(s) are not in the record, so a rise in one passes until it is over \
         CRAP 30: `chock baseline crap` records them"
    );
    let noted = (lacking > 0).then(|| Finding::at("", &note).item("record"));
    noted.into_iter().collect()
}

/// The verdict from cargo-crap's report, with how many functions are over the threshold now and
/// on record. cargo-crap exits zero over a new function at any score, so the report decides.
fn judged(json: &str, succeeded: bool, root: &Path, record: &[Scored]) -> Result<Outcome, String> {
    let report = read(json)?;
    // cargo-crap exits non-zero over a regression it names, which the record's order may clear.
    let explained = succeeded
        || report
            .entries
            .iter()
            .any(|entry| entry.status == "regressed");
    let held = record.iter().filter(|entry| entry.over()).count() as u64;
    let delta = paired(report, record, root);
    let worse: Vec<Finding> = delta
        .entries
        .iter()
        .filter(|entry| entry.worse())
        .map(|entry| entry.finding(root))
        .collect();
    let over = delta.entries.iter().filter(|entry| entry.over()).count() as u64;
    let outcome = match (worse.is_empty(), explained) {
        (true, false) => {
            return Err(
                "cargo-crap failed but named no regression, so nothing was measured".to_string(),
            );
        }
        (true, true) => Outcome::noted(unheld(&delta)),
        (false, _) => Outcome::failed(worse),
    };
    Ok(outcome.counting(over, held, UNIT))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::run::baseline::Baseline;

    fn ctx_in(dir: std::path::PathBuf) -> Ctx {
        Ctx::for_root(dir, Baseline::empty("0.1.0"))
    }

    #[test]
    /// Only a missing baseline stops the gate early; the gate makes coverage itself.
    fn a_tree_with_no_baseline_says_which_step_was_skipped() {
        let dir = crate::testdir::make("crap-no-baseline-file");
        let ctx = ctx_in(dir.to_path_buf());
        assert_eq!(
            check(&ctx),
            Err(format!("no {} — run `chock baseline`", baseline()))
        );
    }

    #[test]
    fn a_tree_with_coverage_but_no_baseline_asks_for_the_baseline() {
        let dir = crate::testdir::make("crap-no-baseline");
        std::fs::write(dir.join(COVERAGE), "TN:\n").unwrap();
        assert_eq!(
            check(&ctx_in(dir.to_path_buf())),
            Err(format!("no {} — run `chock baseline`", baseline()))
        );
    }

    #[test]
    fn each_system_keeps_its_own_baseline_and_linux_keeps_the_plain_name() {
        assert_eq!(kept_on("linux"), ".chock/crap-baseline.json");
        assert_eq!(kept_on("macos"), ".chock/crap-baseline@macos.json");
        assert_eq!(baseline(), kept_on(std::env::consts::OS));
        assert_eq!(GATE.name, "crap");
    }

    const REPORT: &str = r#"{"entries":[
      {"file":"/w/src/a.rs","function":"parse","line":12,"cyclomatic":9.0,"coverage":0.0,
       "crap":90.0,"status":"regressed","baseline_crap":42.0},
      {"file":"/w/src/b.rs","function":"fresh","line":3,"cyclomatic":5.0,"coverage":0.0,
       "crap":30.0,"status":"new","baseline_crap":null},
      {"file":"/w/src/c.rs","function":"steady","line":7,"cyclomatic":2.0,"coverage":100.0,
       "crap":2.0,"status":"unchanged","baseline_crap":2.0},
      {"file":"/w/src/d.rs","function":"added","line":5,"cyclomatic":5.0,"coverage":0.0,
       "crap":30.5,"status":"new","baseline_crap":null},
      {"file":"/w/src/e.rs","function":"old","line":9,"cyclomatic":7.0,"coverage":0.0,
       "crap":56.0,"status":"unchanged","baseline_crap":56.0}]}"#;

    fn rendered(outcome: &Outcome) -> Vec<String> {
        outcome.findings.iter().map(Finding::render).collect()
    }

    /// A record as `judged` takes it.
    fn held(record: &str) -> Vec<Scored> {
        read(record).unwrap().entries
    }

    const HELD: &str = r#"{"entries":[
      {"file":"src/a.rs","function":"parse","line":10,"crap":42.0},
      {"file":"src/e.rs","function":"old","line":9,"crap":56.0}]}"#;

    #[test]
    fn a_function_that_got_worse_and_a_new_one_over_the_threshold_both_trip() {
        let outcome = judged(REPORT, false, Path::new("/w"), &held(HELD)).unwrap();
        assert_eq!(
            rendered(&outcome),
            [
                "src/a.rs:12: parse: CRAP 90.0, was 42.0",
                "src/d.rs:5: added: CRAP 30.5, not in the baseline"
            ]
        );
        assert!(!outcome.passed);
        assert_eq!(outcome.counted, Some((3, 2, "function(s) over CRAP 30")));
    }

    #[test]
    fn a_new_function_over_the_threshold_trips_though_cargo_crap_exited_zero() {
        let only_new = r#"{"entries":[{"file":"/w/src/d.rs","function":"added","line":5,
          "crap":110.0,"status":"new","baseline_crap":null}]}"#;
        let outcome = judged(only_new, true, Path::new("/w"), &[]).unwrap();
        assert!(!outcome.passed);
        assert_eq!(
            rendered(&outcome),
            ["src/d.rs:5: added: CRAP 110.0, not in the baseline"]
        );
    }

    #[test]
    fn a_pass_still_counts_the_functions_over_the_threshold() {
        let steady = r#"{"entries":[
          {"file":"/w/src/e.rs","function":"old","line":9,"crap":56.0,"status":"unchanged",
           "baseline_crap":56.0},
          {"file":"/w/src/b.rs","function":"fresh","line":3,"crap":30.0,"status":"new",
           "baseline_crap":null}]}"#;
        let outcome = judged(steady, true, Path::new("/w"), &held(HELD)).unwrap();
        assert!(outcome.passed);
        assert_eq!(outcome.counted, Some((1, 2, UNIT)));
        assert_eq!(
            rendered(&outcome),
            [
                "record: 1 function(s) are not in the record, so a rise in one passes until it is \
                 over CRAP 30: `chock baseline crap` records them"
            ]
        );
    }

    const QUIET: &str = r#"{"entries":[{"file":"/w/src/e.rs","function":"old","line":9,
      "crap":56.0,"status":"unchanged","baseline_crap":56.0}]}"#;

    #[test]
    fn a_pass_with_every_function_on_record_says_nothing_more() {
        assert_eq!(
            judged(QUIET, true, Path::new("/w"), &held(HELD)),
            Ok(Outcome::passed().counting(1, 2, UNIT))
        );
    }

    #[test]
    fn a_failure_the_report_does_not_explain_is_a_gate_that_could_not_run() {
        assert_eq!(
            judged(QUIET, false, Path::new("/w"), &[]),
            Err("cargo-crap failed but named no regression, so nothing was measured".to_string())
        );
        assert!(
            judged("not json", true, Path::new("/w"), &[])
                .unwrap_err()
                .starts_with("cargo-crap produced a report chock cannot read")
        );
    }

    /// Two functions of one name in one file, as cargo-crap reports them where their lines moved.
    fn twins(first: f64, second: f64, recorded: &str) -> String {
        let entry = |line: u32, crap: f64| {
            format!(
                r#"{{"file":"/w/src/w.rs","function":"Watch::poll","line":{line},"crap":{crap},
                   "status":"new","baseline_crap":null}}"#
            )
        };
        let (first, second) = (entry(115, first), entry(203, second));
        format!(r#"{{"entries":[{first},{second}],"removed":[{recorded}]}}"#)
    }

    const BOTH: &str = r#"{"file":"src/w.rs","function":"Watch::poll","baseline_crap":380.0},
      {"file":"src/w.rs","function":"Watch::poll","baseline_crap":2.0}"#;

    /// The record of those two, the later one listed first: the first in the file scored 380.
    const PAIR: &str = r#"{"entries":[
      {"file":"src/w.rs","function":"Watch::poll","line":190,"crap":2.0},
      {"file":"src/w.rs","function":"Watch::poll","line":100,"crap":380.0}]}"#;

    #[test]
    fn two_functions_of_one_name_in_one_file_are_held_to_the_record_in_file_order() {
        let (root, pair) = (Path::new("/w"), held(PAIR));
        assert_eq!(
            judged(&twins(380.0, 2.0 + EPSILON, BOTH), true, root, &pair),
            Ok(Outcome::passed().counting(1, 1, UNIT))
        );
        let risen = judged(&twins(380.0, 6.0, BOTH), true, root, &pair).unwrap();
        assert_eq!(
            rendered(&risen),
            ["src/w.rs:203: Watch::poll: CRAP 6.0, was 2.0"]
        );
        // The two scores changed places: the worst is no worse, and the second function rose.
        let swapped = judged(&twins(2.0, 380.0, BOTH), true, root, &pair).unwrap();
        assert_eq!(
            rendered(&swapped),
            ["src/w.rs:203: Watch::poll: CRAP 380.0, was 2.0"]
        );
    }

    /// cargo-crap matches by line first, so a function that moved onto the other's old line reads
    /// there as a rise.
    #[test]
    fn a_rise_cargo_crap_names_and_the_file_order_clears_is_a_pass() {
        let moved = r#"{"entries":[
          {"file":"/w/src/w.rs","function":"Watch::poll","line":190,"crap":380.0,
           "status":"regressed","baseline_crap":2.0},
          {"file":"/w/src/w.rs","function":"Watch::poll","line":280,"crap":2.0,
           "status":"improved","baseline_crap":380.0}]}"#;
        assert_eq!(
            judged(moved, false, Path::new("/w"), &held(PAIR)),
            Ok(Outcome::passed().counting(1, 1, UNIT))
        );
    }

    #[test]
    fn where_the_record_has_another_number_of_a_name_the_worst_takes_the_worst_on_record() {
        let one = r#"{"file":"src/w.rs","function":"Watch::poll","baseline_crap":50.0},
          {"file":"src/other.rs","function":"Watch::poll","baseline_crap":90.0}"#;
        let record = held(
            r#"{"entries":[
          {"file":"src/w.rs","function":"Watch::poll","line":100,"crap":50.0},
          {"file":"src/other.rs","function":"Watch::poll","line":5,"crap":90.0}]}"#,
        );
        let outcome = judged(&twins(40.0, 50.0, one), true, Path::new("/w"), &record).unwrap();
        assert_eq!(
            rendered(&outcome),
            ["src/w.rs:115: Watch::poll: CRAP 40.0, not in the baseline"]
        );
    }

    #[test]
    fn the_recorder_gets_each_function_with_the_score_its_record_holds() {
        let root = Path::new("/w");
        let measured = r#"{"entries":[
          {"file":"/w/src/w.rs","function":"Watch::poll","line":203,"crap":1.0},
          {"file":"/w/src/a.rs","function":"fresh","line":3,"crap":4.0},
          {"file":"/w/src/w.rs","function":"Watch::poll","line":115,"crap":9.0}]}"#;
        assert_eq!(
            floors(measured, PAIR, root),
            Ok(BTreeMap::from([(0, 2.0), (2, 380.0)]))
        );
        assert_eq!(floors(measured, "", root), Ok(BTreeMap::new()));
        assert_eq!(floors(measured, HELD, root), Ok(BTreeMap::new()));
        assert!(floors("not json", PAIR, root).is_err());
    }

    const RECORD: &str = r#"{"entries":[
      {"file":"src/a.rs","function":"parse","line":12,"crap":42.4},
      {"file":"src/e.rs","function":"old","line":9,"crap":56.0},
      {"file":"src/b.rs","function":"edge","line":3,"crap":30.0}]}"#;

    fn kept(name: &str, record: &str) -> std::path::PathBuf {
        let dir = crate::testdir::make(name).to_path_buf();
        std::fs::create_dir_all(dir.join(".chock")).unwrap();
        std::fs::write(dir.join(baseline()), record).unwrap();
        dir
    }

    #[test]
    fn the_record_lists_what_it_holds_over_the_threshold_worst_first() {
        let dir = kept("crap-record", RECORD);
        assert_eq!(
            on_record(&dir),
            Ok(vec![
                ("src/e.rs: old".to_string(), 56.0),
                ("src/a.rs: parse".to_string(), 42.4)
            ])
        );
        let debt = debt(&dir).unwrap().unwrap();
        assert_eq!(
            (debt.gate.as_str(), debt.total, debt.unit.as_str()),
            ("crap", 2, "function(s) over CRAP 30")
        );
        let held: Vec<(&str, u64)> = debt
            .items
            .iter()
            .map(|held| (held.key.as_str(), held.count))
            .collect();
        assert_eq!(held, [("src/e.rs: old", 56), ("src/a.rs: parse", 42)]);
    }

    #[test]
    fn a_project_with_no_record_or_nothing_over_the_threshold_holds_no_crap_debt() {
        let bare = crate::testdir::make("crap-no-record").to_path_buf();
        assert_eq!(debt(&bare), Ok(None));
        let edge = r#"{"entries":[{"file":"src/b.rs","function":"edge","line":3,"crap":30.0}]}"#;
        assert_eq!(debt(&kept("crap-none-over", edge)), Ok(None));
    }

    #[test]
    fn a_record_chock_cannot_read_stops_the_gate_and_names_the_file() {
        let dir = kept("crap-bad-record", "not json");
        let why = check(&ctx_in(dir.clone())).unwrap_err();
        assert!(why.starts_with(&baseline()), "{why}");
        assert!(why.contains("a report chock cannot read"), "{why}");
        assert_eq!(debt(&dir), Err(why));
    }
}
