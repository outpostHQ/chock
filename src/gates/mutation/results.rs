//! Reads survivors from the JSON mutest writes per target: parallel runs interleave its text
//! output, so a verdict there cannot be matched to its mutation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::survivors::Survivor;
use crate::run::report::{Finding, plural};

/// Where one run writes its results: a directory made for that run, so no earlier run's are read.
pub(super) struct Results {
    directory: PathBuf,
    /// Whether each test's detections are read, from a run that gave every test a verdict.
    tests: bool,
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
            tests: false,
            keep: false,
        })
    }

    pub(super) fn flag(&self) -> String {
        format!("--metadata-out-root-dir={}", self.directory.display())
    }

    /// These results, reading each test's detections where `tests`, and kept where `keep`.
    pub(super) fn reading(self, tests: bool, keep: bool) -> Self {
        Self {
            tests,
            keep,
            ..self
        }
    }

    /// Every evaluated target's verdicts, joined per mutation. The undetected ones must number the
    /// `expected` from the text totals, or a target that wrote no results would read as clean.
    pub(super) fn survivors(&self, expected: u64) -> Result<Joined, String> {
        let read = |path: &Path| {
            std::fs::read_to_string(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))
        };
        let mut judged = Vec::new();
        let (mut tested, mut ids) = (Vec::new(), Ids::new());
        // A target no test reaches is analysed but never evaluated, so it has no evaluation.json.
        for evaluation in crate::project::walk(&self.directory, &|_| true, &|name, _| {
            name == "evaluation.json"
        })? {
            let mutations = read(&evaluation.with_file_name("mutations.json"))?;
            let evaluated = read(&evaluation)?;
            judged.extend(verdicts(&mutations, &evaluated)?);
            if self.tests {
                let target = self.label(&evaluation).unwrap_or_default();
                tested.extend(tested_in(&target, &mutations, &evaluated, &mut ids)?);
            }
        }
        let undetected = judged
            .iter()
            .filter(|(_, verdict)| *verdict == Verdict::Undetected)
            .count();
        if u64::try_from(undetected) != Ok(expected) {
            return Err(format!(
                "mutest reported {expected} undetected mutations but its results name {undetected}; the result is incomplete"
            ));
        }
        let mut found = joined(judged);
        found.tests = idle(tested);
        Ok(found)
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

    pub(super) fn retained(&self) -> String {
        format!("; mutest results retained at {}", self.directory.display())
    }
}

/// Each mutation every target missed, and each one only a time limit stopped.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Joined {
    pub survivors: Vec<Survivor>,
    pub timed_out: Vec<Survivor>,
    /// Each test that detects no mutation, or only ones another test detects too, as a lead.
    pub tests: Vec<Finding>,
}

#[derive(Deserialize)]
struct Mutations {
    mutations: Vec<Mutation>,
}

#[derive(Deserialize)]
struct Mutation {
    mutation_id: u32,
    mutation_op: String,
    display_name: String,
    origin_span: Span,
}

#[derive(Deserialize)]
struct Span {
    path: String,
    begin: (u32, u32),
    end: (u32, u32),
}

/// The same change in every target that reaches it, since mutest numbers mutations per target.
type Identity = (String, (u32, u32), (u32, u32), String, String);

impl Mutation {
    fn identity(&self) -> Identity {
        let span = &self.origin_span;
        let file = span.path.replace('\\', "/");
        let (op, what) = (self.mutation_op.clone(), self.display_name.clone());
        (file, span.begin, span.end, op, what)
    }

    fn survivor(self) -> Survivor {
        Survivor {
            operator: self.mutation_op,
            file: self.origin_span.path.replace('\\', "/"),
            line: self.origin_span.begin.0,
            what: self.display_name,
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
        .all(|(id, mutation)| mutation.mutation_id == id);
    if verdicts.len() != mutations.len() || !in_order {
        return Err("mutest's verdicts do not line up with its mutations".to_string());
    }
    mutations
        .into_iter()
        .zip(verdicts)
        .map(|(mutation, mark)| Ok((mutation, Verdict::of(*mark)?)))
        .collect()
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
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
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
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_run_that_reads_each_test_names_the_idle_ones_and_keeps_its_results_if_asked() {
        let root = crate::testdir::make("mutest-results-tests");
        let results = Results::create(&root).unwrap().reading(true, true);
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
            tests: false,
            keep: false,
        };
        assert_eq!(plain.survivors(1).unwrap().tests, Vec::new());
        let idle = lead("t::idle", "detects no mutation in fixture/lib");
        assert_eq!(results.survivors(1).unwrap().tests, [idle]);
        results.accept().unwrap();
        assert!(
            directory.exists(),
            "results kept for the project's flags were removed"
        );
        std::fs::remove_dir_all(&directory).unwrap();
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
        let mut operators: Vec<String> = results
            .survivors(3)
            .unwrap()
            .survivors
            .into_iter()
            .map(|survivor| survivor.operator)
            .collect();
        operators.sort();
        assert_eq!(operators, ["bool_expr_negate", "eq_op_invert"]);
        for expected in [2, 4] {
            let why = results.survivors(expected).unwrap_err();
            assert!(why.contains("its results name 3"), "{why}");
        }
        std::fs::remove_file(directory.join("fixture/lib/mutations.json")).unwrap();
        assert!(results.survivors(3).unwrap_err().contains("cannot read"));
        assert!(
            results
                .retained()
                .ends_with(&directory.display().to_string())
        );
        results.accept().unwrap();
        assert!(!directory.exists(), "accepted results outlived their run");
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
