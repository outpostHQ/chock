//! Reads survivors from the JSON mutest writes per target: parallel runs interleave its text
//! output, so a verdict there cannot be matched to its mutation.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::survivors::Survivor;

/// Where one run writes its results: a directory made for that run, so no earlier run's are read.
pub(super) struct Results {
    directory: PathBuf,
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
        Ok(Self { directory })
    }

    pub(super) fn flag(&self) -> String {
        format!("--metadata-out-root-dir={}", self.directory.display())
    }

    /// Every evaluated target's survivors, which must number the `expected` from the text totals,
    /// or a target that wrote no results would read as clean.
    pub(super) fn survivors(&self, expected: u64) -> Result<Vec<Survivor>, String> {
        let read = |path: &Path| {
            std::fs::read_to_string(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))
        };
        let mut survivors = Vec::new();
        // A target no test reaches is analysed but never evaluated, so it has no evaluation.json.
        for evaluation in crate::project::walk(&self.directory, &|_| true, &|name, _| {
            name == "evaluation.json"
        })? {
            let mutations = read(&evaluation.with_file_name("mutations.json"))?;
            survivors.extend(undetected(&mutations, &read(&evaluation)?)?);
        }
        if u64::try_from(survivors.len()) != Ok(expected) {
            return Err(format!(
                "mutest reported {expected} undetected mutations but its results name {}; the result is incomplete",
                survivors.len()
            ));
        }
        Ok(survivors)
    }

    /// Each target mutest analysed, as `package/lib` or `package/tests/cli`: where it wrote mutations.
    pub(super) fn targets(&self) -> Result<std::collections::BTreeSet<String>, String> {
        let found = crate::project::walk(&self.directory, &|_| true, &|name, _| {
            name == "mutations.json"
        })?;
        Ok(found
            .iter()
            .filter_map(|file| file.parent()?.strip_prefix(&self.directory).ok())
            .map(|dir| dir.to_string_lossy().replace('\\', "/"))
            .collect())
    }

    /// Removes the results once read; a run whose results could not be read keeps them to inspect.
    pub(super) fn accept(self) -> Result<(), String> {
        std::fs::remove_dir_all(&self.directory)
            .map_err(|error| format!("cannot remove mutest's results: {error}"))
    }

    pub(super) fn retained(&self) -> String {
        format!("; mutest results retained at {}", self.directory.display())
    }
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
}

#[derive(Deserialize)]
struct Evaluation {
    mutation_runs: Vec<Run>,
}

#[derive(Deserialize)]
struct Run {
    mutation_detection_matrix: Matrix,
}

#[derive(Deserialize)]
struct Matrix {
    overall_detections: String,
}

/// One target's undetected mutations. Verdicts are one character per mutation in id order, so a
/// length mismatch or out-of-order ids is refused.
fn undetected(mutations: &str, evaluation: &str) -> Result<Vec<Survivor>, String> {
    let unreadable =
        |error: serde_json::Error| format!("mutest wrote results chock cannot read: {error}");
    let mutations: Mutations = serde_json::from_str(mutations).map_err(unreadable)?;
    let evaluation: Evaluation = serde_json::from_str(evaluation).map_err(unreadable)?;
    let [run] = evaluation.mutation_runs.as_slice() else {
        return Err("mutest recorded other than one evaluation of a target".to_string());
    };
    let verdicts = run.mutation_detection_matrix.overall_detections.as_bytes();
    let in_order = (1..)
        .zip(&mutations.mutations)
        .all(|(id, mutation)| mutation.mutation_id == id);
    if verdicts.len() != mutations.mutations.len() || !in_order {
        return Err("mutest's verdicts do not line up with its mutations".to_string());
    }
    Ok(mutations
        .mutations
        .into_iter()
        .zip(verdicts)
        .filter(|(_, verdict)| **verdict == b'-')
        .map(|(mutation, _)| Survivor {
            operator: mutation.mutation_op,
            file: mutation.origin_span.path.replace('\\', "/"),
            line: mutation.origin_span.begin.0,
            what: mutation.display_name,
        })
        .collect())
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

    #[test]
    fn a_results_directory_is_never_made_outside_a_project_root() {
        let refused = Results::create(Path::new("")).err().unwrap();
        assert!(refused.contains("not under a project root"), "{refused}");
    }

    #[test]
    fn only_an_undetected_verdict_names_a_survivor() {
        let found = undetected(
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
        assert_eq!(
            found,
            [Survivor {
                operator: "call_delete".into(),
                file: "src/a.rs".into(),
                line: 2,
                what: "change 2".into(),
            }]
        );
    }

    /// mutest names a file with the system's separator; keys must not differ between systems.
    #[test]
    fn a_survivor_is_named_with_forward_slashes_on_every_system() {
        let windows = mutations(&["bool_expr_negate"]).replace("src/a.rs", r"src\\gates\\a.rs");
        let found = undetected(&windows, &evaluation("-")).unwrap();
        assert_eq!(found[0].key(), "src/gates/a.rs#bool_expr_negate");
    }

    #[test]
    fn verdicts_that_cannot_be_paired_with_their_mutations_are_refused() {
        let two = mutations(&["a", "b"]);
        for verdicts in ["-", "D--"] {
            let why = undetected(&two, &evaluation(verdicts)).unwrap_err();
            assert!(why.contains("do not line up"), "{why}");
        }
        let reordered = two.replace(r#""mutation_id":1"#, r#""mutation_id":3"#);
        assert!(undetected(&reordered, &evaluation("--")).is_err());
        let none = r#"{"mutation_runs":[]}"#;
        assert!(
            undetected(&two, none)
                .unwrap_err()
                .contains("other than one evaluation")
        );
        assert!(
            undetected("not json", &evaluation("--"))
                .unwrap_err()
                .contains("cannot read")
        );
        assert!(undetected(&two, "{}").unwrap_err().contains("cannot read"));
    }

    #[test]
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
        let mut operators: Vec<String> = results
            .survivors(2)
            .unwrap()
            .into_iter()
            .map(|survivor| survivor.operator)
            .collect();
        operators.sort();
        assert_eq!(operators, ["bool_expr_negate", "eq_op_invert"]);
        for expected in [1, 3] {
            let why = results.survivors(expected).unwrap_err();
            assert!(why.contains("its results name 2"), "{why}");
        }
        std::fs::remove_file(directory.join("fixture/lib/mutations.json")).unwrap();
        assert!(results.survivors(2).unwrap_err().contains("cannot read"));
        assert!(
            results
                .retained()
                .ends_with(&directory.display().to_string())
        );
        results.accept().unwrap();
        assert!(!directory.exists(), "accepted results outlived their run");
    }

    #[test]
    fn a_root_nothing_can_be_written_under_is_refused() {
        let root = crate::testdir::make("mutest-results-file");
        let file = root.join("not-a-directory");
        std::fs::write(&file, "").unwrap();
        let why = Results::create(&file).err().unwrap();
        assert!(why.contains("cannot make a directory"), "{why}");
    }
}
