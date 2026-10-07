//! Complexity × uncoverage per function, via cargo-crap. Pass or fail, not a ratchet: cargo-crap
//! scores each function, and chock holds each score to the record and counts what is over 30.

use std::collections::BTreeMap;
use std::path::Path;

use crate::exec;
use crate::run::debt::{Debt, Held};
use crate::run::report::Finding;
use crate::run::verdicts::Reads;
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
    // Its record is under `.chock/`, which the walk prunes; the run's key reads that file by name.
    reads: Some(Reads::tree_and(&["cargo", "cargo-crap"]).and_coverage()),
    kind: Kind::Binary(check),
};

/// cargo-crap's default threshold: its own table marks every function above this.
const OVER: f64 = 30.0;

/// What the run's two numbers count, so a pass still shows the debt its record holds.
const UNIT: &str = "function(s) over CRAP 30";

/// Both inputs are in place before cargo-crap runs, since it reads a missing lcov as an empty one.
fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let baseline = baseline();
    let held = held_by(&ctx.root, &baseline, ctx.ci)?;
    crate::gates::coverage::ensure(ctx)?;
    let record = or_first(held, &ctx.root, &baseline, &|| scores(ctx))?;
    let out = crap(&ctx.root, &["--baseline", &baseline, "--fail-regression"])?;
    judged(&out.stdout, out.success(), &ctx.root, &record)
}

/// cargo-crap over the run's coverage report, as JSON; `more` asks for a comparison or an order.
/// A build script runs only while cargo builds, so no coverage run reaches it and it stays out.
fn crap(root: &Path, more: &[&str]) -> Result<exec::Output, String> {
    let report = [
        "crap",
        "--lcov",
        COVERAGE,
        "--workspace",
        "--exclude",
        "build.rs",
        "--format",
        "json",
    ];
    exec::run("cargo", &[&report, more].concat(), root).map_err(|e| e.to_string())
}

/// The record this run is held to, or `None` where a local run has none and writes the first one.
/// CI writes no record, so there a missing one stops the gate before anything is built.
fn held_by(root: &Path, name: &str, ci: bool) -> Result<Option<Vec<Scored>>, String> {
    if root.join(name).is_file() {
        return recorded(root).map(Some);
    }
    match ci {
        true => Err(format!(
            "no {name} is committed — `chock run crap` outside CI writes the first one"
        )),
        false => Ok(None),
    }
}

/// The record held, or the first one: the scores `today` gives, written under `root`.
fn or_first(
    held: Option<Vec<Scored>>,
    root: &Path,
    name: &str,
    today: &dyn Fn() -> Result<String, String>,
) -> Result<Vec<Scored>, String> {
    match held {
        Some(record) => Ok(record),
        None => written(root, name, &today()?),
    }
}

/// Every function's score today as cargo-crap reports it, each path relative: what a record holds.
pub fn scores(ctx: &Ctx) -> Result<String, String> {
    as_record(&crap(&ctx.root, &["--sort", "file"])?, &ctx.root)
}

fn as_record(report: &exec::Output, root: &Path) -> Result<String, String> {
    produced(report, "cargo crap")?;
    Ok(crate::run::baseline::relativize(&report.stdout, root))
}

/// Whether a captured run produced something to record. Split from the spawning because a test that
/// ran the real one would start the suite from inside the suite.
fn produced(out: &exec::Output, what: &str) -> Result<(), String> {
    if !out.success() {
        // A file written from a crashed run looks like a baseline and holds no measurement, which is
        // worse than having none.
        return Err(format!("{what} failed; the baseline would measure nothing"));
    }
    if out.truncated {
        return Err(format!("{what} printed more than chock keeps"));
    }
    Ok(())
}

/// Writes `scores` as the first record under `root` and reads it back. It says so on stderr, where
/// the run's other record lines go.
fn written(root: &Path, name: &str, scores: &str) -> Result<Vec<Scored>, String> {
    crate::project::document::write(&root.join(name), scores)
        .map_err(|e| format!("{name}: {e}"))?;
    eprintln!("chock: wrote the first record for crap; commit {name} with this change");
    recorded(root)
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
    cyclomatic: Option<f64>,
    /// A percent. `None` where the coverage report does not name the function's file.
    coverage: Option<f64>,
}

/// What a CRAP score is made of: the function's complexity, and the percent of it the tests run.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Inputs {
    complexity: f64,
    coverage: Option<f64>,
}

impl Inputs {
    /// What a finding says after the score: both inputs, against the record's where it holds them,
    /// and what to do about the one that got worse.
    fn told(self, was: Option<Self>) -> String {
        let (now, covered) = (self.complexity, percent(self.coverage));
        match was {
            Some(was) => format!(
                ": complexity {} → {now}, coverage {} → {covered}{}",
                was.complexity,
                percent(was.coverage),
                self.remedy_since(was)
            ),
            None => format!(": complexity {now}, coverage {covered}. {}", self.remedy()),
        }
    }

    /// For a function with no record to compare: full coverage leaves a score of its complexity.
    fn remedy(self) -> String {
        match self.complexity > OVER {
            true => format!(
                "Split the function: at complexity {} full coverage still leaves it over CRAP {OVER}",
                self.complexity
            ),
            false => "Test more of it, or split the function".to_string(),
        }
    }

    /// For the input that got worse; nothing where neither reads worse than the record's.
    fn remedy_since(self, was: Self) -> &'static str {
        let more_branches = self.complexity > was.complexity;
        let less_tested =
            matches!((was.coverage, self.coverage), (Some(then), Some(now)) if now < then);
        let all_tested = self.coverage.is_some_and(|now| now >= 100.0);
        match (more_branches, less_tested, all_tested) {
            (true, true, _) => {
                ". It has more branches and the tests run less of it: test the new branches, or \
                 split the function"
            }
            (true, false, true) => {
                ". It has more branches and the tests run all of it: split the function"
            }
            (true, false, false) => {
                ". It has more branches: test the new ones, or split the function"
            }
            (false, true, _) => ". The tests run less of it: test the lines that no test runs now",
            (false, false, _) => "",
        }
    }
}

fn percent(coverage: Option<f64>) -> String {
    coverage.map_or_else(|| "unknown".to_string(), |covered| format!("{covered:.1}%"))
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

    /// `None` for a report that names no complexity.
    fn inputs(&self) -> Option<Inputs> {
        let (complexity, coverage) = (self.cyclomatic?, self.coverage);
        Some(Inputs {
            complexity,
            coverage,
        })
    }

    /// `was` is what the record holds for the function this one was judged against.
    fn finding(&self, root: &Path, was: Option<Inputs>) -> Finding {
        let against = match self.baseline_crap {
            Some(was) => format!("was {was:.1}"),
            None => "not in the baseline".to_string(),
        };
        let why = self.inputs().map(|now| now.told(was)).unwrap_or_default();
        Finding::at(
            &crate::project::relative(root, Path::new(&self.file)),
            &format!("CRAP {:.1}, {against}{why}", self.crap),
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

/// The inputs the record holds for the function `entry` was judged against: the one of its file
/// and name with that score. Two that differ tell nothing, and so does a function that moved.
fn before(entry: &Scored, record: &[Scored], root: &Path) -> Option<Inputs> {
    let was = entry.baseline_crap?;
    let key = named(root, &entry.file, &entry.function);
    let mut same = record
        .iter()
        .filter(|held| (held.crap - was).abs() <= EPSILON)
        .filter(|held| named(root, &held.file, &held.function) == key)
        .filter_map(Scored::inputs);
    let first = same.next()?;
    same.all(|other| other == first).then_some(first)
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
        .map(|entry| entry.finding(root, before(entry, record, root)))
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

    fn ran(code: Option<i32>, stdout: &str, truncated: bool) -> exec::Output {
        exec::Output {
            code,
            stdout: stdout.to_string(),
            stderr: String::new(),
            truncated,
        }
    }

    const NO_RECORD_IN_CI: &str = "is committed — `chock run crap` outside CI writes the first one";

    #[test]
    /// CI writes no record, so a missing one stops the gate before it makes coverage.
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn in_ci_a_tree_with_no_record_says_where_the_first_one_comes_from() {
        let dir = crate::testdir::make("crap-no-record-in-ci");
        let ctx = Ctx {
            ci: true,
            ..ctx_in(dir.to_path_buf())
        };
        assert_eq!(
            check(&ctx),
            Err(format!("no {} {NO_RECORD_IN_CI}", baseline()))
        );
        // A coverage report beside it changes nothing: the record is what CI lacks.
        std::fs::write(dir.join(COVERAGE), "TN:\n").unwrap();
        assert_eq!(
            check(&ctx),
            Err(format!("no {} {NO_RECORD_IN_CI}", baseline()))
        );
    }

    const ONE_FUNCTION: &str =
        r#"{"entries": [{"file": "src/a.rs", "function": "f", "line": 1, "crap": 4.0}]}"#;

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_local_run_with_no_record_writes_the_first_one_and_is_held_to_it() {
        let dir = crate::testdir::make("crap-first-record");
        let name = baseline();
        assert!(matches!(held_by(&dir, &name, false), Ok(None)));
        std::fs::create_dir_all(dir.join(".chock")).unwrap();
        let scored = |held: &[Scored]| -> Vec<(String, f64)> {
            let one = |entry: &Scored| (entry.function.clone(), entry.crap);
            held.iter().map(one).collect()
        };
        let record = written(&dir, &name, ONE_FUNCTION).unwrap();
        assert_eq!(scored(&record), [("f".to_string(), 4.0)]);
        assert_eq!(
            std::fs::read_to_string(dir.join(&name)).unwrap(),
            ONE_FUNCTION
        );
        // From here on the file is the record, in CI as on this machine.
        for ci in [false, true] {
            let held = held_by(&dir, &name, ci).unwrap().unwrap();
            assert_eq!(scored(&held), [("f".to_string(), 4.0)]);
        }
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_first_record_with_nowhere_to_go_stops_the_gate_and_names_the_file() {
        // No `.chock/` here, so the write fails.
        let dir = crate::testdir::make("crap-first-record-unwritable");
        let name = baseline();
        let why = written(&dir, &name, ONE_FUNCTION).err().unwrap();
        assert!(why.starts_with(&format!("{name}: ")), "{why}");
        assert!(!dir.join(&name).exists());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_held_record_is_kept_and_only_a_missing_one_asks_for_the_scores_of_today() {
        let dir = crate::testdir::make("crap-or-first");
        std::fs::create_dir_all(dir.join(".chock")).unwrap();
        let name = baseline();
        let today = || -> Result<String, String> { Ok(ONE_FUNCTION.to_string()) };
        let held = or_first(Some(Vec::new()), &dir, &name, &today);
        assert!(held.unwrap().is_empty());
        assert!(!dir.join(&name).exists());
        // Scores that cargo-crap could not give leave no record.
        let failed = or_first(None, &dir, &name, &|| Err("cargo crap failed".to_string()));
        assert_eq!(failed.err(), Some("cargo crap failed".to_string()));
        assert!(!dir.join(&name).exists());
        let first = or_first(None, &dir, &name, &today).unwrap();
        let scored: Vec<&str> = first.iter().map(|one| one.function.as_str()).collect();
        assert_eq!(scored, ["f"]);
        assert!(dir.join(&name).is_file());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_record_holds_what_cargo_crap_reported_with_each_path_relative() {
        let dir = crate::testdir::make("crap-as-record");
        let file = dir.join("src").join("a.rs").display().to_string();
        let report = serde_json::json!({
            "entries": [{"file": file, "function": "f", "line": 1, "crap": 4.0}]
        })
        .to_string();
        let record = as_record(&ran(Some(0), &report, false), &dir).unwrap();
        assert_eq!(read(&record).unwrap().entries[0].file, "src/a.rs");
        assert_eq!(
            as_record(&ran(Some(101), &report, false), &dir),
            Err("cargo crap failed; the baseline would measure nothing".to_string())
        );
    }

    #[test]
    fn a_run_that_failed_produced_no_baseline() {
        let out = ran(Some(101), "", false);
        assert_eq!(
            produced(&out, "cargo crap"),
            Err("cargo crap failed; the baseline would measure nothing".to_string())
        );
    }

    #[test]
    fn a_run_chock_had_to_cut_short_produced_no_baseline() {
        let out = ran(Some(0), "{}", true);
        assert_eq!(
            produced(&out, "the coverage run"),
            Err("the coverage run printed more than chock keeps".to_string())
        );
    }

    #[test]
    fn a_clean_whole_run_produced_a_baseline() {
        assert_eq!(produced(&ran(Some(0), "{}", false), "cargo crap"), Ok(()));
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
                "src/a.rs:12: parse: CRAP 90.0, was 42.0: complexity 9, coverage 0.0%. Test more of \
                 it, or split the function",
                "src/d.rs:5: added: CRAP 30.5, not in the baseline: complexity 5, coverage 0.0%. \
                 Test more of it, or split the function"
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

    /// `parse` as a report or a record names it, with the inputs of its score.
    fn scored(crap: f64, cyclomatic: f64, coverage: &str, rest: &str) -> String {
        format!(
            r#"{{"file":"src/a.rs","function":"parse","line":10,"crap":{crap},
               "cyclomatic":{cyclomatic},"coverage":{coverage}{rest}}}"#
        )
    }

    fn entries(listed: &[String]) -> String {
        format!(r#"{{"entries":[{}]}}"#, listed.join(","))
    }

    /// What the gate says of `parse`, each side given as score, complexity and coverage.
    fn explained(now: (f64, f64, &str), was: (f64, f64, &str)) -> String {
        let rest = format!(r#","status":"regressed","baseline_crap":{}"#, was.0);
        let report = entries(&[scored(now.0, now.1, now.2, &rest)]);
        let record = held(&entries(&[scored(was.0, was.1, was.2, "")]));
        let outcome = judged(&report, false, Path::new("/w"), &record).unwrap();
        rendered(&outcome).join("\n")
    }

    #[test]
    fn a_function_that_got_worse_shows_both_inputs_against_the_record_and_what_to_do() {
        let said = |inputs: &str| format!("src/a.rs:10: parse: CRAP 42.0, was 34.0: {inputs}");
        assert_eq!(
            explained((42.0, 14.0, "71.3"), (34.0, 12.0, "78")),
            said(
                "complexity 12 → 14, coverage 78.0% → 71.3%. It has more branches and the tests \
                 run less of it: test the new branches, or split the function"
            )
        );
        assert_eq!(
            explained((42.0, 14.0, "78"), (34.0, 12.0, "78")),
            said(
                "complexity 12 → 14, coverage 78.0% → 78.0%. It has more branches: test the new \
                 ones, or split the function"
            )
        );
        assert_eq!(
            explained((42.0, 12.0, "60"), (34.0, 12.0, "78")),
            said(
                "complexity 12 → 12, coverage 78.0% → 60.0%. The tests run less of it: test the \
                 lines that no test runs now"
            )
        );
        // Coverage that rose is no reason; complexity that fell is none either.
        assert_eq!(
            explained((42.0, 14.0, "99.9"), (34.0, 12.0, "78")),
            said(
                "complexity 12 → 14, coverage 78.0% → 99.9%. It has more branches: test the new \
                 ones, or split the function"
            )
        );
        // No test is left to write where the tests run every line.
        assert_eq!(
            explained((42.0, 42.0, "100"), (34.0, 34.0, "100")),
            said(
                "complexity 34 → 42, coverage 100.0% → 100.0%. It has more branches and the \
                 tests run all of it: split the function"
            )
        );
        assert_eq!(
            explained((42.0, 10.0, "60"), (34.0, 12.0, "78")),
            said(
                "complexity 12 → 10, coverage 78.0% → 60.0%. The tests run less of it: test the \
                 lines that no test runs now"
            )
        );
    }

    #[test]
    fn inputs_that_read_no_worse_or_cannot_be_compared_are_shown_with_no_advice() {
        let said = |inputs: &str| format!("src/a.rs:10: parse: CRAP 42.0, was 34.0: {inputs}");
        assert_eq!(
            explained((42.0, 12.0, "78"), (34.0, 12.0, "78")),
            said("complexity 12 → 12, coverage 78.0% → 78.0%")
        );
        // A file the coverage report stopped naming has no percent to compare.
        assert_eq!(
            explained((42.0, 12.0, "null"), (34.0, 12.0, "78")),
            said("complexity 12 → 12, coverage 78.0% → unknown")
        );
        assert_eq!(
            explained((42.0, 12.0, "60"), (34.0, 12.0, "null")),
            said("complexity 12 → 12, coverage unknown → 60.0%")
        );
    }

    #[test]
    fn a_function_with_no_record_is_told_whether_tests_alone_can_bring_it_under() {
        let new = |crap: f64, cyclomatic: f64, coverage: &str| {
            let rest = r#","status":"new","baseline_crap":null"#;
            let report = entries(&[scored(crap, cyclomatic, coverage, rest)]);
            rendered(&judged(&report, true, Path::new("/w"), &[]).unwrap()).join("\n")
        };
        let said = |rest: &str| format!("src/a.rs:10: parse: CRAP {rest}");
        assert_eq!(
            new(110.0, 14.0, "20"),
            said(
                "110.0, not in the baseline: complexity 14, coverage 20.0%. Test more of it, or \
                 split the function"
            )
        );
        assert_eq!(
            new(930.0, 30.0, "null"),
            said(
                "930.0, not in the baseline: complexity 30, coverage unknown. Test more of it, \
                 or split the function"
            )
        );
        assert_eq!(
            new(31.5, 31.0, "92.5"),
            said(
                "31.5, not in the baseline: complexity 31, coverage 92.5%. Split the function: \
                 at complexity 31 full coverage still leaves it over CRAP 30"
            )
        );
    }

    #[test]
    fn the_record_explains_a_function_only_where_it_holds_one_answer_for_it() {
        let root = Path::new("/w");
        let run = held(&entries(&[
            scored(42.0, 14.0, "70", r#","baseline_crap":34.0"#),
            scored(42.0, 14.0, "70", ""),
        ]));
        let (entry, fresh) = (&run[0], &run[1]);
        let twin = |crap: f64, cyclomatic: f64| scored(crap, cyclomatic, "78", "");
        let record = |listed: &[String]| held(&entries(listed));
        let was = Some(Inputs {
            complexity: 12.0,
            coverage: Some(78.0),
        });
        assert_eq!(before(entry, &record(&[twin(34.0, 12.0)]), root), was);
        assert_eq!(before(entry, &record(&[twin(34.005, 12.0)]), root), was);
        // Two of one name and one score that agree are one answer; two that differ are none.
        let agreeing = record(&[twin(34.0, 12.0), twin(34.0, 12.0)]);
        assert_eq!(before(entry, &agreeing, root), was);
        let differing = record(&[twin(34.0, 12.0), twin(34.0, 11.0)]);
        assert_eq!(before(entry, &differing, root), None);
        for other in [twin(33.0, 12.0), twin(35.0, 12.0), twin(34.02, 12.0)] {
            assert_eq!(
                before(entry, &record(&[other]), root),
                None,
                "another score"
            );
        }
        let elsewhere = twin(34.0, 12.0).replace("src/a.rs", "src/b.rs");
        assert_eq!(before(entry, &record(&[elsewhere]), root), None);
        let renamed = twin(34.0, 12.0).replace("parse", "read");
        assert_eq!(before(entry, &record(&[renamed]), root), None);
        assert_eq!(before(fresh, &record(&[twin(34.0, 12.0)]), root), None);
        // A record from before cargo-crap wrote the inputs holds a score and nothing to show.
        let bare = r#"{"entries":[{"file":"src/a.rs","function":"parse","line":10,"crap":34.0}]}"#;
        assert_eq!(before(entry, &held(bare), root), None);
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
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
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
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_project_with_no_record_or_nothing_over_the_threshold_holds_no_crap_debt() {
        let bare = crate::testdir::make("crap-no-record").to_path_buf();
        assert_eq!(debt(&bare), Ok(None));
        let edge = r#"{"entries":[{"file":"src/b.rs","function":"edge","line":3,"crap":30.0}]}"#;
        assert_eq!(debt(&kept("crap-none-over", edge)), Ok(None));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_record_chock_cannot_read_stops_the_gate_and_names_the_file() {
        let dir = kept("crap-bad-record", "not json");
        let why = check(&ctx_in(dir.clone())).unwrap_err();
        assert!(why.starts_with(&baseline()), "{why}");
        assert!(why.contains("a report chock cannot read"), "{why}");
        assert_eq!(debt(&dir), Err(why));
    }
}
