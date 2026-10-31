//! Reads survivors from the JSON mutest writes per target: parallel runs interleave its text
//! output, so a verdict there cannot be matched to its mutation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::survivors::{Edit, Span, Survivor, Totals};
use crate::run::report::{Finding, plural};

/// What chock reads of each mutation from a mutest run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reads {
    /// Its verdict and each test that detected it, from a run that gave every test a verdict.
    EachTest,
    /// Killed or not, from a run without `--exhaustive`, which names no detecting test.
    VerdictOnly,
}

/// Where one run writes its results: a directory made for that run, so no earlier run's are read.
pub(super) struct Results {
    directory: PathBuf,
    /// What is read of each mutation.
    reads: Reads,
    /// Whether the directory stays once read, since the project's own flags may write there.
    keep: bool,
}

impl Results {
    pub(super) fn create(root: &Path) -> Result<Self, String> {
        let made = |error: std::io::Error| {
            format!("cannot make a directory for mutest's results: {error}")
        };
        let parent = root.join("target/test-scratch");
        crate::project::document::rooted(&parent).map_err(made)?;
        std::fs::create_dir_all(&parent).map_err(made)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default();
        let directory = parent.join(format!("mutest-results-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&directory).map_err(made)?;
        Ok(Self {
            directory,
            reads: Reads::VerdictOnly,
            keep: false,
        })
    }

    pub(super) fn flag(&self) -> String {
        format!("--metadata-out-root-dir={}", self.directory.display())
    }

    /// These results, read as `reads` says, and kept where the project passes `own` flags to
    /// mutest, since those may write there.
    pub(super) fn reading(self, reads: Reads, own: &[String]) -> Self {
        Self {
            reads,
            keep: !own.is_empty(),
            ..self
        }
    }

    /// Whether each test's detections are read, so mutest must give every test a verdict.
    pub(super) fn reads_each_test(&self) -> bool {
        self.reads == Reads::EachTest
    }

    /// Every evaluated target's verdicts, joined per mutation, and their totals: the text mutest
    /// prints interleaves its parallel targets, so a count there can be torn.
    pub(super) fn survivors(&self) -> Result<Joined, String> {
        let (mut judged, mut totals) = (Vec::new(), None);
        let (mut tested, mut ids) = (Vec::new(), Ids::new());
        for evaluation in self.evaluations()? {
            let mutations = read(&evaluation.with_file_name("mutations.json"))?;
            let evaluated = read(&evaluation)?;
            let target = verdicts(&mutations, &evaluated)?;
            totals = Some(counted(totals.unwrap_or_default(), &target)?);
            judged.extend(target);
            if self.reads == Reads::EachTest {
                let target = self.label(&evaluation).unwrap_or_default();
                tested.extend(tested_in(&target, &mutations, &evaluated, &mut ids)?);
            }
        }
        let mut found = joined(judged);
        found.tests = idle(tested);
        found.totals = totals;
        Ok(found)
    }

    /// Each mutation the run evaluated, once, under the strongest verdict any target gave it, with
    /// the tests that killed it where each test was read.
    pub(super) fn kills(&self) -> Result<Vec<Kill>, String> {
        let mut joined: BTreeMap<Identity, (Mutation, Verdict, BTreeSet<String>)> = BTreeMap::new();
        for evaluation in self.evaluations()? {
            let mutations = read(&evaluation.with_file_name("mutations.json"))?;
            let evaluated = read(&evaluation)?;
            for (mutation, verdict, tests) in killed_in(&mutations, &evaluated, self.reads)? {
                let identity = mutation.identity();
                let held = joined
                    .entry(identity)
                    .or_insert((mutation, verdict, BTreeSet::new()));
                held.1 = held.1.max(verdict);
                held.2.extend(tests);
            }
        }
        Ok(joined.into_values().filter_map(Kill::of).collect())
    }

    /// Each target's `evaluation.json`. A target no test reaches is analysed but never evaluated,
    /// so it has none.
    fn evaluations(&self) -> Result<Vec<PathBuf>, String> {
        let evaluation = |name: &str, _: &Path| name == "evaluation.json";
        crate::project::walk(&self.directory, &|_| true, &evaluation)
    }

    /// Each target mutest analysed, as `package/lib` or `package/tests/cli`: where it wrote mutations.
    pub(super) fn targets(&self) -> Result<std::collections::BTreeSet<String>, String> {
        let found = crate::project::walk(&self.directory, &|_| true, &|name, _| {
            name == "mutations.json"
        })?;
        Ok(found.iter().filter_map(|file| self.label(file)).collect())
    }

    /// The target `file` is under, such as `package/lib`: where mutest wrote it.
    fn label(&self, file: &Path) -> Option<String> {
        let target = file.parent()?.strip_prefix(&self.directory).ok()?;
        Some(target.to_string_lossy().replace('\\', "/"))
    }

    /// Removes the results once read, unless kept for the project's own flags; a run whose results
    /// could not be read keeps them to inspect.
    pub(super) fn accept(self) -> Result<(), String> {
        if self.keep {
            return Ok(());
        }
        std::fs::remove_dir_all(&self.directory)
            .map_err(|error| format!("cannot remove mutest's results: {error}"))
    }

    /// `error`, once mutest's captured stderr is kept in the results, since a finding quotes
    /// only its tail.
    pub(super) fn with_stderr(&self, out: &crate::exec::Output, error: &str) -> String {
        let kept = self.directory.join("stderr.txt");
        match crate::project::document::write(&kept, &out.stderr) {
            Ok(()) => format!("{error}; mutest's stderr is at {}", kept.display()),
            Err(cannot) => format!("{error}; cannot keep mutest's stderr: {cannot}"),
        }
    }

    pub(super) fn retained(&self) -> String {
        format!("; mutest results retained at {}", self.directory.display())
    }
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}

/// One mutation a run evaluated, under its strongest verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Kill {
    pub file: String,
    pub line: u32,
    /// mutest's column, counted from 1.
    pub column: u32,
    pub operator: String,
    pub mutation: String,
    /// `killed`, `survived` or `timed_out`.
    pub verdict: &'static str,
    /// The tests that detected it or made it crash, where each test was read.
    pub tests: Vec<String>,
}

impl Kill {
    /// `None` for a mutation no test ran, which the run never evaluated.
    fn of((mutation, verdict, tests): (Mutation, Verdict, BTreeSet<String>)) -> Option<Self> {
        let verdict = match verdict {
            Verdict::NotRun => return None,
            Verdict::Undetected => "survived",
            Verdict::TimedOut => "timed_out",
            Verdict::Crashed | Verdict::Detected => "killed",
        };
        let column = mutation.origin_span.begin.1;
        let held = mutation.survivor();
        Some(Self {
            file: held.file,
            line: held.line,
            column,
            operator: held.operator,
            mutation: held.what,
            verdict,
            tests: tests.into_iter().collect(),
        })
    }
}

/// Each mutation every target missed, and each one only a time limit stopped.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Joined {
    pub survivors: Vec<Survivor>,
    pub timed_out: Vec<Survivor>,
    /// Each test that detects no mutation, or only ones another test detects too, as a lead.
    pub tests: Vec<Finding>,
    /// Each target's verdicts summed, as mutest's text totals sum them; `None` where no target
    /// was evaluated.
    pub totals: Option<Totals>,
}

/// `totals` with one target's verdicts added. A mutation no test ran is in no total.
fn counted(totals: Totals, target: &[(Mutation, Verdict)]) -> Result<Totals, String> {
    let count = |wanted: fn(Verdict) -> bool| {
        let n = target
            .iter()
            .filter(|(_, verdict)| wanted(*verdict))
            .count();
        u64::try_from(n).map_err(|_| "mutest wrote more verdicts than chock counts".to_string())
    };
    let next = Totals {
        undetected: count(|verdict| verdict == Verdict::Undetected)?,
        timed_out: count(|verdict| verdict == Verdict::TimedOut)?,
        total: count(|verdict| verdict != Verdict::NotRun)?,
    };
    totals
        .add(next)
        .ok_or_else(|| "mutest mutation totals overflowed".to_string())
}

#[derive(Deserialize)]
struct Mutations {
    mutations: Vec<Mutation>,
}

#[derive(Deserialize)]
struct Mutation {
    #[serde(rename = "mutation_id")]
    id: u32,
    #[serde(rename = "mutation_op")]
    operator: String,
    display_name: String,
    origin_span: Span,
    /// What the mutation changes; a mutest that writes none leaves nothing to rebuild.
    #[serde(default)]
    substs: Vec<Edit>,
}

/// The same change in every target that reaches it, since mutest numbers mutations per target.
type Identity = (String, (u32, u32), (u32, u32), String, String);

impl Mutation {
    fn identity(&self) -> Identity {
        let span = &self.origin_span;
        let file = span.path.replace('\\', "/");
        let (op, what) = (self.operator.clone(), self.display_name.clone());
        (file, span.begin, span.end, op, what)
    }

    fn survivor(self) -> Survivor {
        Survivor {
            operator: self.operator,
            file: self.origin_span.path.replace('\\', "/"),
            line: self.origin_span.begin.0,
            what: self.display_name,
            edits: self.substs,
        }
    }
}

/// One target's verdict on a mutation, weakest first: across targets the strongest one holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Verdict {
    NotRun,
    Undetected,
    TimedOut,
    Crashed,
    Detected,
}

impl Verdict {
    fn of(mark: u8) -> Result<Self, String> {
        match mark {
            b'.' => Ok(Self::NotRun),
            b'-' => Ok(Self::Undetected),
            b'T' => Ok(Self::TimedOut),
            b'C' => Ok(Self::Crashed),
            b'D' => Ok(Self::Detected),
            _ => Err(format!(
                "mutest wrote a verdict chock does not know: {:?}",
                char::from(mark)
            )),
        }
    }
}

#[derive(Deserialize)]
struct Evaluation {
    mutation_runs: Vec<Run>,
    /// In the order of each run's `test_detections`; an older mutest writes none.
    #[serde(default)]
    tests: Vec<Test>,
}

#[derive(Deserialize)]
struct Test {
    name: String,
}

#[derive(Deserialize)]
struct Run {
    mutation_detection_matrix: Matrix,
}

#[derive(Deserialize)]
struct Matrix {
    overall_detections: String,
    /// A row per test, a mark per mutation, as in `overall_detections`.
    #[serde(default)]
    test_detections: Vec<String>,
}

/// One target's mutations, each with its verdict. Verdicts are one character per mutation in id
/// order, so a length mismatch or out-of-order ids is refused.
fn verdicts(mutations: &str, evaluation: &str) -> Result<Vec<(Mutation, Verdict)>, String> {
    let (mutations, _, matrix) = parsed(mutations, evaluation)?;
    let verdicts = matrix.overall_detections.as_bytes();
    let in_order = (1..)
        .zip(&mutations)
        .all(|(id, mutation)| mutation.id == id);
    if verdicts.len() != mutations.len() || !in_order {
        return Err("mutest's verdicts do not line up with its mutations".to_string());
    }
    mutations
        .into_iter()
        .zip(verdicts)
        .map(|(mutation, mark)| Ok((mutation, Verdict::of(*mark)?)))
        .collect()
}

/// One target's mutations, each with its verdict and, where `reads` takes each test, the tests that
/// detected it or made it crash. A timeout kills nothing, so it names no test.
fn killed_in(
    mutations: &str,
    evaluation: &str,
    reads: Reads,
) -> Result<Vec<(Mutation, Verdict, Vec<String>)>, String> {
    let each = reads == Reads::EachTest;
    let judged = verdicts(mutations, evaluation)?;
    let (_, tests, matrix) = parsed(mutations, evaluation)?;
    let rows = if each {
        matrix.test_detections
    } else {
        Vec::new()
    };
    let fits = rows.iter().all(|row| row.len() == judged.len());
    if each && (rows.len() != tests.len() || !fits) {
        return Err("mutest's tests do not line up with their detections".to_string());
    }
    let killed = |at: usize| {
        let named = tests.iter().zip(&rows);
        let mark = |row: &String| row.as_bytes().get(at).copied();
        let kills = |(_, row): &(&Test, &String)| matches!(mark(row), Some(b'D' | b'C'));
        named
            .filter(kills)
            .map(|(test, _)| test.name.clone())
            .collect()
    };
    let numbered = judged.into_iter().enumerate();
    Ok(numbered
        .map(|(at, (mutation, verdict))| (mutation, verdict, killed(at)))
        .collect())
}

/// One target's mutations, its tests, and its one evaluation of them.
fn parsed(mutations: &str, evaluation: &str) -> Result<(Vec<Mutation>, Vec<Test>, Matrix), String> {
    let unreadable =
        |error: serde_json::Error| format!("mutest wrote results chock cannot read: {error}");
    let mutations: Mutations = serde_json::from_str(mutations).map_err(unreadable)?;
    let evaluation: Evaluation = serde_json::from_str(evaluation).map_err(unreadable)?;
    let Ok([run]) = <[Run; 1]>::try_from(evaluation.mutation_runs) else {
        return Err("mutest recorded other than one evaluation of a target".to_string());
    };
    Ok((
        mutations.mutations,
        evaluation.tests,
        run.mutation_detection_matrix,
    ))
}

/// A number for each mutation, the same in every target that reaches it.
type Ids = BTreeMap<Identity, usize>;

/// One test, and the number of each mutation it detected.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Tested {
    target: String,
    name: String,
    detected: Vec<usize>,
}

/// One target's tests, each with the mutations it detected, from a run that gave every test a
/// verdict. A timeout or a crash counts as a detection, as mutest counts it.
fn tested_in(
    target: &str,
    mutations: &str,
    evaluation: &str,
    ids: &mut Ids,
) -> Result<Vec<Tested>, String> {
    let (mutations, tests, matrix) = parsed(mutations, evaluation)?;
    let rows = matrix.test_detections;
    let fits = rows.iter().all(|row| row.len() == mutations.len());
    if rows.len() != tests.len() || !fits {
        return Err("mutest's tests do not line up with their detections".to_string());
    }
    let numbered: Vec<usize> = mutations
        .iter()
        .map(|mutation| {
            let next = ids.len();
            *ids.entry(mutation.identity()).or_insert(next)
        })
        .collect();
    let tested = tests.into_iter().zip(rows).map(|(test, row)| Tested {
        target: target.to_string(),
        name: test.name,
        detected: numbered
            .iter()
            .zip(row.bytes())
            .filter(|(_, mark)| matches!(mark, b'D' | b'T' | b'C'))
            .map(|(id, _)| *id)
            .collect(),
    });
    Ok(tested.collect())
}

/// Each test that detects no mutation, or only mutations another test detects too, as a lead: a
/// test to cut, or to point at something no other test checks.
fn idle(mut tested: Vec<Tested>) -> Vec<Finding> {
    tested.sort();
    let mut detections: BTreeMap<usize, usize> = BTreeMap::new();
    for id in tested.iter().flat_map(|test| &test.detected) {
        *detections.entry(*id).or_default() += 1;
    }
    let shared = |id: &usize| detections.get(id).is_some_and(|count| *count > 1);
    let mut found = Vec::new();
    for test in &tested {
        let (target, count) = (&test.target, test.detected.len());
        let said = match count {
            0 => format!("detects no mutation in {target}"),
            _ if test.detected.iter().all(&shared) => format!(
                "detects {} in {target}, and another test detects each of them too",
                plural(count, "mutation")
            ),
            _ => continue,
        };
        found.push(Finding::at("", &said).item(&test.name).candidate());
    }
    found
}

/// Each mutation once, under the strongest verdict any target gave it: a test in any target that
/// detects it settles it, so only a mutation every target missed survives.
fn joined(judged: Vec<(Mutation, Verdict)>) -> Joined {
    let mut strongest: BTreeMap<Identity, (Mutation, Verdict)> = BTreeMap::new();
    for (mutation, verdict) in judged {
        let held = strongest
            .entry(mutation.identity())
            .or_insert((mutation, verdict));
        held.1 = held.1.max(verdict);
    }
    let mut joined = Joined::default();
    for (mutation, verdict) in strongest.into_values() {
        match verdict {
            Verdict::Undetected => joined.survivors.push(mutation.survivor()),
            Verdict::TimedOut => joined.timed_out.push(mutation.survivor()),
            Verdict::NotRun | Verdict::Crashed | Verdict::Detected => {}
        }
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutations(ops: &[&str]) -> String {
        let listed: Vec<String> = (1..)
            .zip(ops)
            .map(|(id, op)| {
                format!(
                    r#"{{"mutation_id":{id},"mutation_op":"{op}","display_name":"change {id}","origin_span":{{"path":"src/a.rs","begin":[{id},5],"end":[{id},9]}}}}"#
                )
            })
            .collect();
        format!(
            r#"{{"format_version":1,"mutations":[{}]}}"#,
            listed.join(",")
        )
    }

    fn evaluation(verdicts: &str) -> String {
        format!(
            r#"{{"mutation_runs":[{{"mutation_detection_matrix":{{"overall_detections":"{verdicts}","test_detections":[]}}}}]}}"#
        )
    }

    /// An evaluation that gave each test in `rows` a verdict on every mutation.
    fn tested(overall: &str, rows: &[(&str, &str)]) -> String {
        let names: Vec<String> = rows
            .iter()
            .map(|(name, _)| format!(r#"{{"name":"{name}"}}"#))
            .collect();
        let marks: Vec<String> = rows
            .iter()
            .map(|(_, marks)| format!(r#""{marks}""#))
            .collect();
        format!(
            r#"{{"tests":[{}],"mutation_runs":[{{"mutation_detection_matrix":{{"overall_detections":"{overall}","test_detections":[{}]}}}}]}}"#,
            names.join(","),
            marks.join(",")
        )
    }

    fn lead(name: &str, said: &str) -> Finding {
        Finding::at("", said).item(name).candidate()
    }

    /// A test is a lead when it detects nothing, or when another test detects each thing it does.
    #[test]
    fn a_test_that_detects_nothing_or_only_what_another_detects_is_a_lead() {
        let ops = mutations(&["call_delete", "eq_op_invert", "bool_expr_negate"]);
        let rows = [
            ("t::own", "DTC"),
            ("t::none", "-.-"),
            ("t::shared", "D--"),
            ("t::timed", ".T."),
        ];
        let found = tested_in("pkg/lib", &ops, &tested("DTC", &rows), &mut Ids::new()).unwrap();
        let also = "in pkg/lib, and another test detects each of them too";
        assert_eq!(
            idle(found),
            [
                lead("t::none", "detects no mutation in pkg/lib"),
                lead("t::shared", &format!("detects 1 mutation {also}")),
                lead("t::timed", &format!("detects 1 mutation {also}")),
            ]
        );
    }

    #[test]
    fn tests_that_do_not_line_up_with_their_detections_are_refused() {
        let ops = mutations(&["call_delete", "eq_op_invert"]);
        let mut ids = Ids::new();
        let refused = "mutest's tests do not line up with their detections".to_string();
        let short = tested("DD", &[("t::a", "D")]);
        assert_eq!(tested_in("p", &ops, &short, &mut ids), Err(refused.clone()));
        let unnamed =
            tested("DD", &[]).replace(r#""test_detections":[]"#, r#""test_detections":["DD"]"#);
        assert_eq!(tested_in("p", &ops, &unnamed, &mut ids), Err(refused));
        let fitting = tested("DD", &[("t::a", "-D")]);
        let one = Tested {
            target: "p".to_string(),
            name: "t::a".to_string(),
            detected: vec![1],
        };
        assert_eq!(tested_in("p", &ops, &fitting, &mut ids), Ok(vec![one]));
    }

    #[test]
    fn only_a_run_that_reads_each_test_refuses_tests_that_do_not_line_up() {
        let ops = mutations(&["call_delete", "eq_op_invert"]);
        let named = |evaluation: &str, reads| {
            let killed = killed_in(&ops, evaluation, reads)?;
            Ok::<_, String>(
                killed
                    .into_iter()
                    .map(|(_, _, tests)| tests)
                    .collect::<Vec<_>>(),
            )
        };
        let refused = Err("mutest's tests do not line up with their detections".to_string());
        let short = tested("DD", &[("t::a", "D")]);
        assert_eq!(named(&short, Reads::EachTest), refused);
        let unnamed =
            tested("DD", &[]).replace(r#""test_detections":[]"#, r#""test_detections":["DD"]"#);
        assert_eq!(named(&unnamed, Reads::EachTest), refused);
        let none: Vec<String> = Vec::new();
        assert_eq!(
            named(&short, Reads::VerdictOnly),
            Ok(vec![none.clone(), none.clone()])
        );
        let fitting = tested("DD", &[("t::a", "-D")]);
        let by_a = vec!["t::a".to_string()];
        assert_eq!(named(&fitting, Reads::EachTest), Ok(vec![none, by_a]));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_run_that_reads_each_test_names_the_idle_ones_and_keeps_its_results_if_asked() {
        let root = crate::testdir::make("mutest-results-tests");
        let own = ["--timings".to_string()];
        let results = Results::create(&root)
            .unwrap()
            .reading(Reads::EachTest, &own);
        let flag = results.flag();
        let directory = PathBuf::from(flag.strip_prefix("--metadata-out-root-dir=").unwrap());
        let at = directory.join("fixture").join("lib");
        std::fs::create_dir_all(&at).unwrap();
        let ops = mutations(&["call_delete", "eq_op_invert"]);
        std::fs::write(at.join("mutations.json"), ops).unwrap();
        let rows = [("t::idle", "--"), ("t::kills", "D-")];
        std::fs::write(at.join("evaluation.json"), tested("D-", &rows)).unwrap();
        let plain = Results {
            directory: directory.clone(),
            reads: Reads::VerdictOnly,
            keep: false,
        };
        assert_eq!(plain.survivors().unwrap().tests, Vec::new());
        let idle = lead("t::idle", "detects no mutation in fixture/lib");
        assert_eq!(results.survivors().unwrap().tests, [idle]);
        results.accept().unwrap();
        assert!(
            directory.exists(),
            "results kept for the project's flags were removed"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_failed_run_keeps_mutest_s_stderr_beside_its_results() {
        let root = crate::testdir::make("mutest-results-stderr");
        let results = Results::create(&root).unwrap();
        let out = crate::exec::Output {
            code: Some(4),
            stdout: String::new(),
            stderr: "error[E0252]: the name `f` is defined multiple times\n".into(),
            truncated: false,
        };
        let said = results.with_stderr(&out, "mutest did not finish");
        let kept = results.directory.join("stderr.txt");
        assert_eq!(
            said,
            format!(
                "mutest did not finish; mutest's stderr is at {}",
                kept.display()
            )
        );
        assert_eq!(std::fs::read_to_string(kept).unwrap(), out.stderr);
        std::fs::remove_dir_all(&results.directory).unwrap();
        let lost = results.with_stderr(&out, "mutest did not finish");
        let prefix = "mutest did not finish; cannot keep mutest's stderr: ";
        assert!(lost.starts_with(prefix), "{lost}");
    }

    #[test]
    fn a_results_directory_is_never_made_outside_a_project_root() {
        let refused = Results::create(Path::new("")).err().unwrap();
        assert!(refused.contains("not under a project root"), "{refused}");
    }

    /// One target's verdicts, joined as a run joins every target's.
    fn judged(mutations: &str, evaluation: &str) -> Result<Joined, String> {
        verdicts(mutations, evaluation).map(joined)
    }

    fn named(operator: &str, line: u32) -> Survivor {
        Survivor {
            operator: operator.into(),
            file: "src/a.rs".into(),
            line,
            what: format!("change {line}"),
            edits: Vec::new(),
        }
    }

    #[test]
    fn only_an_undetected_verdict_names_a_survivor_and_only_a_time_limit_a_timeout() {
        let found = judged(
            &mutations(&[
                "eq_op_invert",
                "call_delete",
                "bool_expr_negate",
                "match_arm_delete",
                "x",
            ]),
            &evaluation("D-TC."),
        )
        .unwrap();
        let expected = Joined {
            survivors: vec![named("call_delete", 2)],
            timed_out: vec![named("bool_expr_negate", 3)],
            tests: Vec::new(),
            totals: None,
        };
        assert_eq!(found, expected);
        let twice = judged(
            &mutations(&["call_delete", "call_delete"]),
            &evaluation("--"),
        );
        assert_eq!(
            twice.unwrap().survivors,
            [named("call_delete", 1), named("call_delete", 2)]
        );
    }

    /// As `report.rs:481` on 0.2.0: the `--lib` tests detected it, and a test in `tests/cli` that
    /// only starts chock reached it and missed.
    #[test]
    fn a_mutation_one_target_settles_survives_in_no_other() {
        let one = mutations(&["call_delete"]);
        let joined_of = |marks: &[&str]| {
            let mut judged = Vec::new();
            for mark in marks {
                judged.extend(verdicts(&one, &evaluation(mark)).unwrap());
            }
            joined(judged)
        };
        let survived = Joined {
            survivors: vec![named("call_delete", 1)],
            ..Joined::default()
        };
        let timed = Joined {
            timed_out: vec![named("call_delete", 1)],
            ..Joined::default()
        };
        for (marks, expected) in [
            (&["D", "-"][..], Joined::default()),
            (&["-", "C"], Joined::default()),
            (&["T", "D"], Joined::default()),
            (&[".", "."], Joined::default()),
            (&["-", "T"], timed),
            (&["-", "."], survived.clone()),
            (&["-", "-"], survived),
        ] {
            assert_eq!(joined_of(marks), expected, "{marks:?}");
        }
    }

    /// mutest names a file with the system's separator; keys must not differ between systems.
    #[test]
    fn a_survivor_is_named_with_forward_slashes_on_every_system() {
        let windows = mutations(&["bool_expr_negate"]).replace("src/a.rs", r"src\\gates\\a.rs");
        let found = judged(&windows, &evaluation("-")).unwrap();
        assert_eq!(found.survivors[0].key(), "src/gates/a.rs#bool_expr_negate");
    }

    #[test]
    fn verdicts_that_cannot_be_paired_with_their_mutations_are_refused() {
        let two = mutations(&["a", "b"]);
        for verdicts in ["-", "D--"] {
            let why = judged(&two, &evaluation(verdicts)).unwrap_err();
            assert!(why.contains("do not line up"), "{why}");
        }
        let reordered = two.replace(r#""mutation_id":1"#, r#""mutation_id":3"#);
        assert!(judged(&reordered, &evaluation("--")).is_err());
        assert_eq!(
            judged(&two, &evaluation("D?")).unwrap_err(),
            "mutest wrote a verdict chock does not know: '?'"
        );
        let none = r#"{"mutation_runs":[]}"#;
        assert!(
            judged(&two, none)
                .unwrap_err()
                .contains("other than one evaluation")
        );
        assert!(
            judged("not json", &evaluation("--"))
                .unwrap_err()
                .contains("cannot read")
        );
        assert!(judged(&two, "{}").unwrap_err().contains("cannot read"));
    }

    #[test]
    fn totals_count_each_verdict_but_that_of_a_mutation_no_test_ran() {
        let ops = ["a", "b", "c", "d", "e"];
        let judged = verdicts(&mutations(&ops), &evaluation(".-TCD")).unwrap();
        let once = counted(Totals::default(), &judged).unwrap();
        let expected = Totals {
            undetected: 1,
            timed_out: 1,
            total: 4,
        };
        assert_eq!(once, expected);
        assert_eq!(counted(once, &judged).unwrap().total, 8);
        let full = Totals {
            undetected: u64::MAX,
            ..once
        };
        assert!(counted(full, &judged).unwrap_err().contains("overflowed"));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn every_evaluated_target_is_read_and_the_directory_goes_with_the_run() {
        let root = crate::testdir::make("mutest-results");
        let results = Results::create(&root).unwrap();
        let flag = results.flag();
        let directory = PathBuf::from(flag.strip_prefix("--metadata-out-root-dir=").unwrap());
        let target = |kind: &str, ops: &[&str], verdicts: Option<&str>| {
            let at = directory.join("fixture").join(kind);
            std::fs::create_dir_all(&at).unwrap();
            std::fs::write(at.join("mutations.json"), mutations(ops)).unwrap();
            if let Some(verdicts) = verdicts {
                std::fs::write(at.join("evaluation.json"), evaluation(verdicts)).unwrap();
            }
        };
        target("lib", &["eq_op_invert", "call_delete"], Some("-D"));
        target("tests/cli", &["bool_expr_negate"], Some("-"));
        target("bin", &["unary_op_delete"], None);
        // The totals count each target's verdict, and the joined results name each mutation once.
        target("tests/more", &["eq_op_invert"], Some("-"));
        let found = results.survivors().unwrap();
        let mut operators: Vec<String> = found
            .survivors
            .into_iter()
            .map(|survivor| survivor.operator)
            .collect();
        operators.sort();
        assert_eq!(operators, ["bool_expr_negate", "eq_op_invert"]);
        let totals = Totals {
            undetected: 3,
            timed_out: 0,
            total: 4,
        };
        assert_eq!(found.totals, Some(totals));
        std::fs::remove_file(directory.join("fixture/lib/mutations.json")).unwrap();
        assert!(results.survivors().unwrap_err().contains("cannot read"));
        assert!(
            results
                .retained()
                .ends_with(&directory.display().to_string())
        );
        results.accept().unwrap();
        assert!(!directory.exists(), "accepted results outlived their run");
    }

    /// Each evaluated mutation is one kill under its strongest verdict, survivors too, and names
    /// its killers only where each test was read. Mutant: `Undetected => return None` drops survivors.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn every_evaluated_mutation_is_one_kill_that_names_its_killers_only_where_each_test_was_read() {
        let root = crate::testdir::make("mutest-results-kills");
        let each = Results::create(&root)
            .unwrap()
            .reading(Reads::EachTest, &[]);
        let operators = [
            "call_delete",
            "eq_op_invert",
            "bool_expr_negate",
            "unary_op_delete",
            "x",
        ];
        let target = |kind: &str, evaluated: String| {
            let at = each.directory.join("fixture").join(kind);
            std::fs::create_dir_all(&at).unwrap();
            std::fs::write(at.join("mutations.json"), mutations(&operators)).unwrap();
            std::fs::write(at.join("evaluation.json"), evaluated).unwrap();
        };
        target(
            "lib",
            tested("D-T-.", &[("t::a", "D-T-."), ("t::b", "C....")]),
        );
        target("tests/x", tested("-D-..", &[("x::c", "-D-..")]));
        let kill = |id: u32, verdict: &'static str, tests: &[&str]| Kill {
            file: "src/a.rs".to_string(),
            line: id,
            column: 5,
            operator: operators[usize::try_from(id).unwrap() - 1].to_string(),
            mutation: format!("change {id}"),
            verdict,
            tests: tests.iter().map(ToString::to_string).collect(),
        };
        let named = [
            kill(1, "killed", &["t::a", "t::b"]),
            kill(2, "killed", &["x::c"]),
            kill(3, "timed_out", &[]),
            kill(4, "survived", &[]),
        ];
        assert_eq!(each.kills().unwrap(), named);
        let plain = Results {
            reads: Reads::VerdictOnly,
            ..each
        };
        let unnamed = named.map(|it| Kill {
            tests: Vec::new(),
            ..it
        });
        assert_eq!(plain.kills().unwrap(), unnamed);
        plain.accept().unwrap();
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_root_nothing_can_be_written_under_is_refused() {
        let root = crate::testdir::make("mutest-results-file");
        let file = root.join("not-a-directory");
        std::fs::write(&file, "").unwrap();
        let why = Results::create(&file).err().unwrap();
        assert!(why.contains("cannot make a directory"), "{why}");
    }
}
