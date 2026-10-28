//! The `verus` gate: Verus verifies each proof file with no error, and fails on each mutant, a
//! patch that puts a fault in one function.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::IgnoredAny;

use super::patch::{self, Mutant, copied, normalized, patches};

use crate::exec;
use crate::project;
use crate::run::Measurement;
use crate::run::baseline::{Keys, Series};
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind};

pub const GATE: Gate = Gate {
    name: "verus",
    about: "Verus verifies each proof file, and finds the fault each mutant patch puts in one",
    group: Group::OptIn,
    builds: true,
    // The tree holds the proofs, the mutants and the pin; only a `verus` on PATH is outside it.
    reads: Some(crate::run::verdicts::Reads::tree_and(&["verus"]).versioned("verus-v2")),
    kind: Kind::AnnotatedRatchet {
        measure,
        // One row per function a proof file verifies, zero while it does. A failed run is a fault,
        // which trips whatever the record holds and is never recorded.
        keys: Keys::Census,
        unit: "unverified function(s)",
    },
};

const FILES: &str = "proofs/verus/*.rs";
const MUTANTS: &str = "proofs/verus/mutants";
const ARGS: [&str; 1] = ["--crate-type=lib"];
const TIMEOUT_S: u64 = 600;
const SCRATCH: &str = "target/test-scratch/chock/verus";

/// How a finding begins its count, as Verus ends a run: `verification results:: 12 verified, 0
/// errors`.
const RESULTS: &str = "verification results::";
/// Each run reports as JSON, so the record can name each function it verified.
const REPORT: &str = "--output-json";

const NAMES: &str = "line 1 must be `--- <the file it patches>` and line 2 `+++ <the function \
                     that must fail>`, such as `+++ cas::put` or `+++ merge`";

/// One run of Verus, and what it must say.
struct Job {
    /// What a finding names: the proof file, or the mutant's patch.
    shown: String,
    /// The function a mutant breaks.
    function: Option<String>,
    argv: Vec<String>,
}

/// What Verus writes to stdout under `--output-json`.
#[derive(Deserialize)]
struct Report {
    #[serde(rename = "verification-results")]
    results: Results,
    /// Each function Verus checked, by its path from the crate.
    #[serde(default, rename = "func-details")]
    functions: BTreeMap<String, IgnoredAny>,
}

#[derive(Deserialize)]
struct Results {
    verified: u64,
    errors: u64,
    /// Verus writes `success` only for a run of the whole crate, and this for each run.
    #[serde(rename = "encountered-error")]
    erred: bool,
}

/// What Verus said of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Said {
    /// Each function the report names, by its path: the crate's, and the `vstd` and `core`
    /// specifications it uses.
    Verified(Vec<String>),
    /// The results line, and the first error above it.
    Failed(String),
    /// No report, a clean one with nothing verified, or a clean one and a failed run.
    Stopped(String),
    TimedOut,
}

impl Said {
    /// `Err` when Verus could not start, which no proof file is to blame for.
    fn of(out: &exec::Output) -> Result<Self, String> {
        let text = exec::strip_colour(&format!("{}\n{}", out.stdout, out.stderr));
        if let Some(toolchain) = toolchain_missing(&text) {
            return Err(format!(
                "Verus runs on the Rust toolchain {toolchain}, which is not installed; run \
                 `rustup install {toolchain}`"
            ));
        }
        // Verus writes its report to stdout and its errors to stderr.
        let error = super::tla::first_error(&exec::strip_colour(&out.stderr), out.code);
        Ok(match serde_json::from_str::<Report>(&out.stdout) {
            Ok(report) => Self::read(report, out.success(), &error),
            Err(_) => Self::Stopped(error),
        })
    }

    /// A report verifies only when Verus checked a function, found no error and exited 0.
    fn read(report: Report, exited: bool, error: &str) -> Self {
        let Results {
            verified,
            errors,
            erred,
        } = report.results;
        let line = format!("{RESULTS} {verified} verified, {errors} errors");
        match (errors, verified, exited && !erred) {
            (1.., _, _) => Self::Failed(format!("{line}; {error}")),
            (0, 1.., true) => Self::Verified(report.functions.into_keys().collect()),
            (0, 0, true) => Self::Stopped(format!("{line}, so the run proves no function")),
            (0, 1.., false) => Self::Stopped(format!("{line}, yet {error}")),
            (0, 0, false) => Self::Stopped(error.to_string()),
        }
    }
}

/// The toolchain Verus asked for, from `required rust toolchain 1.98.1-x86_64-… not found`.
fn toolchain_missing(text: &str) -> Option<String> {
    let (_, rest) = text.split_once("required rust toolchain ")?;
    rest.split_whitespace().next().map(str::to_string)
}

/// Whether Verus said what the run must; `Some` says why not. A mutant must fail to verify.
fn judged(mutant: bool, said: &Said, limit: u64) -> Option<String> {
    match (mutant, said) {
        (false, Said::Verified(_)) | (true, Said::Failed(_)) => None,
        (_, Said::TimedOut) => Some(format!(
            "Verus ran past its {limit} s limit; raise `verus.timeout_s` if it needs longer"
        )),
        (false, Said::Failed(why)) => Some(format!("does not verify: {why}")),
        (false, Said::Stopped(why)) => Some(format!("Verus gave no verdict: {why}")),
        (true, Said::Verified(_)) => Some(
            "the mutant verifies, so no proof sees the fault it puts in; make the proof \
             stronger, or the mutant break what the proof states"
                .into(),
        ),
        (true, Said::Stopped(why)) => Some(format!(
            "the mutant gets no verdict from Verus, which refutes nothing: {why}"
        )),
    }
}

/// The Verus that the `VERUS` download pin holds, else the one on `PATH`.
fn verus(ctx: &Ctx, root: &Path) -> Result<String, String> {
    let program = if cfg!(windows) { "verus.exe" } else { "verus" };
    let home = ctx.tools.home.as_deref();
    match crate::setup::downloads::located(root, home, "VERUS", program) {
        Ok(path) => Ok(path.to_string_lossy().into_owned()),
        Err(unpinned) if unpinned.ends_with(crate::setup::downloads::NOT_PINNED) => {
            let on_path = ctx.tools.run("verus", &["--version"], root).is_ok();
            on_path.then(|| "verus".to_string()).ok_or(unpinned)
        }
        Err(e) => Err(e),
    }
}

fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let settings = &ctx.verus;
    let root = normalized(&ctx.root);
    let files = settings
        .files
        .clone()
        .unwrap_or_else(|| vec![FILES.to_string()]);
    let entries = entries(ctx, &files)?;
    let mutants = root.join(settings.mutants.as_deref().unwrap_or(MUTANTS));
    let patches = patches(&mutants);
    if entries.is_empty() && patches.is_empty() {
        let files = files.join(", ");
        return Err(format!(
            "no file matches {files}, so Verus would verify nothing"
        ));
    }
    let verus = verus(ctx, &root)?;
    let args = settings
        .args
        .clone()
        .unwrap_or_else(|| ARGS.map(str::to_string).to_vec());
    let scratch = root.join(SCRATCH);
    super::cleared(&scratch)?;
    let mut jobs: Vec<Job> = entries
        .iter()
        .map(|entry| job_of(&root, entry, &args))
        .collect();
    let mut faults = Vec::new();
    for patch in &patches {
        let shown = project::relative(&root, patch);
        match planned(&root, &entries, patch) {
            Ok(plan) => jobs.push(plan.staged(&root, &scratch, &shown, &args)?),
            Err(why) => faults.push(Finding::at(&shown, &why)),
        }
    }
    let limit = settings.timeout_s.unwrap_or(TIMEOUT_S);
    // Each job, as many at once as the run's budget; their Verus threads share the same budget.
    let (workers, threads) = shares(exec::budget::cap(), jobs.len());
    let fixed = [REPORT, "--num-threads"].map(str::to_string);
    let fixed = [fixed.as_slice(), &[threads.to_string()]].concat();
    let run = |job: &Job| super::Run {
        dir: root.clone(),
        argv: job.argv.iter().chain(&fixed).cloned().collect(),
        limit,
    };
    let runs: Vec<super::Run> = jobs.iter().map(run).collect();
    let outs = super::run_all(&ctx.tools, &verus, &runs, workers);
    super::cleared(&scratch)?;
    let mut rows = Series::default();
    for (job, out) in jobs.iter().zip(outs) {
        let (found, verified) = job.read(out, limit)?;
        faults.extend(found);
        let row = |function: String| (format!("{}#{function}", job.shown), 0);
        rows.0.extend(verified.into_iter().map(row));
    }
    let notes = match patches.is_empty() {
        true => vec![Finding::at(
            &project::relative(&root, &mutants),
            "holds no mutant patch, so nothing shows that Verus can fail on these proofs",
        )],
        false => Vec::new(),
    };
    Ok(Measurement {
        faults,
        ..Measurement::of(rows, notes)
    })
}

impl Job {
    /// What this job's run shows is wrong, if anything, and the functions a proof file's run
    /// verified; `Err` where Verus did not start.
    fn read(
        &self,
        out: Result<exec::Output, exec::ExecError>,
        limit: u64,
    ) -> Result<(Option<Finding>, Vec<String>), String> {
        let said = match out {
            Ok(out) => Said::of(&out)?,
            Err(e) if e.hung() => Said::TimedOut,
            Err(e) => return Err(e.to_string()),
        };
        let why = judged(self.function.is_some(), &said, limit);
        let found = why.map(|why| Finding::at(&self.shown, &why));
        let found = found.map(|found| match &self.function {
            Some(function) => found.item(function),
            None => found,
        });
        // A mutant's run verifies one function of a patched copy, which the record never holds.
        let verified = match (said, &self.function) {
            (Said::Verified(functions), None) => {
                // Rust names the crate after its root file.
                let stem = Path::new(&self.shown).file_stem().unwrap_or_default();
                let crate_root = format!("{}::", stem.to_string_lossy().replace('-', "_"));
                let own = |name: &String| name.strip_prefix(&crate_root).map(str::to_string);
                functions.iter().filter_map(own).collect()
            }
            _ => Vec::new(),
        };
        Ok((found, verified))
    }
}

/// The runs that go at once out of the run's budget `cap`, and the Verus threads each gets.
fn shares(cap: usize, jobs: usize) -> (usize, usize) {
    let cap = cap.max(1);
    let workers = cap.min(jobs).max(1);
    (workers, (cap / workers).max(1))
}

/// The files `verus.files` names, each a crate root Verus verifies whole.
fn entries(ctx: &Ctx, files: &[String]) -> Result<Vec<PathBuf>, String> {
    let named = |_: &str, path: &Path| {
        let shown = project::relative(&ctx.root, path);
        let globbed = |pattern: &String| crate::gates::text::phrases::globbed(pattern, &shown);
        shown.ends_with(".rs") && files.iter().any(globbed)
    };
    let entered = |dir: &str| !project::SKIPPED.contains(&dir);
    let found = project::walked(&ctx.listing, &ctx.root, &entered, &named)?;
    Ok(found.iter().map(|path| normalized(path)).collect())
}

fn job_of(root: &Path, entry: &Path, args: &[String]) -> Job {
    let shown = project::relative(root, entry);
    Job {
        argv: std::iter::once(shown.clone())
            .chain(args.iter().cloned())
            .collect(),
        shown,
        function: None,
    }
}

/// A mutant, and the proof file that holds what it patches.
#[derive(Debug)]
struct Plan {
    mutant: Mutant,
    /// The proof file Verus checks: the patched file itself, or the crate root that holds it.
    entry: PathBuf,
}

/// The mutant at `patch`; `Err` says why it cannot run, and is the patch's fault.
fn planned(root: &Path, entries: &[PathBuf], patch: &Path) -> Result<Plan, String> {
    // Line 1 is a path from the directory that holds the mutants directory.
    let base = patch.parent().and_then(Path::parent).unwrap_or(root);
    let mutant = patch::mutant(root, base, patch, NAMES)?;
    Ok(Plan {
        entry: holder(entries, &mutant.target).to_path_buf(),
        mutant,
    })
}

impl Plan {
    /// The job that verifies a copy of the entry's directory, with the patched file in it.
    fn staged(
        &self,
        root: &Path,
        scratch: &Path,
        shown: &str,
        args: &[String],
    ) -> Result<Job, String> {
        let home = self.entry.parent().unwrap_or(root);
        let stem = Path::new(shown).file_stem().unwrap_or_default();
        let copy = scratch.join(stem);
        copied(home, &copy)
            .map_err(|e| format!("cannot copy {} to {}: {e}", home.display(), copy.display()))?;
        let inside = |path: &Path| copy.join(path.strip_prefix(home).unwrap_or(path));
        let target = inside(&self.mutant.target);
        crate::project::document::write(&target, &self.mutant.patched)
            .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
        let verified = project::relative(root, &inside(&self.entry));
        Ok(Job {
            shown: shown.to_string(),
            function: Some(self.mutant.name.clone()),
            argv: std::iter::once(verified)
                .chain(args.iter().cloned())
                .chain(selected(&self.mutant.name))
                .collect(),
        })
    }
}

/// Verus's flags that verify one function: `mod::fn` in module `mod`, a bare name in the root.
fn selected(function: &str) -> Vec<String> {
    let flags = match function.split_once("::") {
        Some((module, name)) => vec!["--verify-only-module", module, "--verify-function", name],
        None => vec!["--verify-root", "--verify-function", function],
    };
    flags.into_iter().map(str::to_string).collect()
}

/// The proof file that holds `target`: itself where it is one, else the crate root whose
/// directory is deepest above it, else `target` alone.
fn holder<'a>(entries: &'a [PathBuf], target: &'a Path) -> &'a Path {
    if entries.iter().any(|entry| entry == target) {
        return target;
    }
    let above = |entry: &&PathBuf| entry.parent().is_some_and(|home| target.starts_with(home));
    let deepest = entries
        .iter()
        .filter(above)
        .max_by_key(|entry| entry.components().count());
    deepest.map_or(target, PathBuf::as_path)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    const DIFF: &str =
        "--- ../../crates/k/src/cas.rs\n+++ cas::put\n@@ -2,3 +2,3 @@\n a\n-b\n+B\n c\n";

    fn output(code: i32, stdout: &str, stderr: &str) -> exec::Output {
        exec::Output {
            code: Some(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
            truncated: false,
        }
    }

    /// Verus's `--output-json` report of a run of one function, which holds no `success`.
    fn report(verified: u64, errors: u64, erred: bool, functions: &[&str]) -> String {
        let functions: serde_json::Map<String, serde_json::Value> = functions
            .iter()
            .map(|name| ((*name).to_string(), serde_json::json!({})))
            .collect();
        let results = serde_json::json!({
            "encountered-error": erred, "encountered-vir-error": false, "verified": verified,
            "errors": errors, "is-verifying-entire-crate": false
        });
        serde_json::json!({"verification-results": results, "func-details": functions}).to_string()
    }

    #[test]
    fn a_module_path_verifies_in_its_module_and_a_bare_name_in_the_root() {
        let module = ["--verify-only-module", "cas", "--verify-function", "put"];
        assert_eq!(selected("cas::put"), module);
        let root = ["--verify-root", "--verify-function", "merge"];
        assert_eq!(selected("merge"), root);
    }

    #[test]
    fn verus_s_report_is_its_verdict_and_a_missing_toolchain_is_no_verdict() {
        let clean = report(4, 0, false, &["law::put"]);
        assert_eq!(
            Said::of(&output(0, &clean, "")),
            Ok(Said::Verified(vec!["law::put".into()]))
        );
        let whole = concat!(
            r#"{"verification-results": {"encountered-error": false, "success": true, "#,
            r#""verified": 4, "errors": 0}, "func-details": {"law::put": {}}}"#
        );
        assert_eq!(
            Said::of(&output(0, whole, "")),
            Ok(Said::Verified(vec!["law::put".into()]))
        );
        let failed = report(3, 10, true, &["law::put"]);
        let error = "\u{1b}[1merror\u{1b}[0m: postcondition not satisfied\n --> a.rs:3:5\n";
        let why = match Said::of(&output(1, &failed, error)) {
            Ok(Said::Failed(why)) => why,
            other => format!("{other:?}"),
        };
        let shown = "verification results:: 3 verified, 10 errors; exit 1: error: postcondition";
        assert!(why.starts_with(shown), "{why}");
        // Verus reports a file that does not compile as one that checked nothing and erred nowhere.
        let broken = report(0, 0, true, &[]);
        let broken = Said::of(&output(
            1,
            &broken,
            "error[E0425]: cannot find type `u64x`\n",
        ));
        assert!(
            matches!(broken, Ok(Said::Stopped(why)) if why.starts_with("exit 1: error[E0425]"))
        );
        assert!(matches!(
            Said::of(&output(1, "", "error: expected `;`\n --> a.rs:1:2\n")),
            Ok(Said::Stopped(_))
        ));
        let toolchain = "required rust toolchain 1.98.1-x86_64-unknown-linux-gnu not found\n";
        assert!(
            Said::of(&output(1, "", toolchain))
                .unwrap_err()
                .ends_with("run `rustup install 1.98.1-x86_64-unknown-linux-gnu`")
        );
    }

    #[test]
    fn a_clean_report_verifies_only_after_a_function_was_checked_and_verus_exited_0() {
        let none = Said::of(&output(0, &report(0, 0, false, &[]), ""));
        assert!(matches!(none, Ok(Said::Stopped(why)) if why.ends_with("proves no function")));
        let clean = report(2, 0, false, &["law::put"]);
        let failed = Said::of(&output(101, &clean, "error: aborting\n"));
        assert!(matches!(failed, Ok(Said::Stopped(why)) if why.contains(", yet exit 101")));
        let unsure = Said::of(&output(0, &report(2, 0, true, &["law::put"]), ""));
        assert!(matches!(unsure, Ok(Said::Stopped(why)) if why.contains("0 errors, yet ")));
    }

    #[test]
    fn a_proof_file_s_rows_are_its_crate_s_functions_and_a_mutant_has_none() {
        let named = [
            "vstd::seq::axiom_seq_len",
            "core::convert::num::impl&%67::from",
            "law::put",
            "my_law::put",
            "my_law::inner::min",
        ];
        let clean = || Ok(output(0, &report(3, 0, false, &named), ""));
        let file = Job {
            shown: "proofs/verus/my-law.rs".into(),
            function: None,
            argv: Vec::new(),
        };
        let rows = (None, vec!["inner::min".to_string(), "put".to_string()]);
        assert_eq!(file.read(clean(), 9), Ok(rows));
        let mutant = Job {
            function: Some("put".into()),
            ..file
        };
        let (found, verified) = mutant.read(clean(), 9).unwrap();
        assert!(found.is_some() && verified.is_empty());
    }

    #[test]
    fn a_proof_file_must_verify_and_a_mutant_must_fail_to() {
        let failed = Said::Failed("1 errors".into());
        let stopped = Said::Stopped("exit 1: error: expected `;`".into());
        assert_eq!(judged(false, &Said::Verified(Vec::new()), 9), None);
        assert_eq!(judged(true, &failed, 9), None);
        assert!(
            judged(true, &Said::Verified(Vec::new()), 9)
                .unwrap()
                .starts_with("the mutant verifies")
        );
        assert!(
            judged(true, &stopped, 9)
                .unwrap()
                .contains("refutes nothing")
        );
        assert!(
            judged(false, &failed, 9)
                .unwrap()
                .starts_with("does not verify")
        );
        assert!(
            judged(false, &Said::TimedOut, 9)
                .unwrap()
                .contains("its 9 s limit")
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot copy a file")]
    fn a_mutant_of_a_crate_file_verifies_a_patched_copy_of_its_crate() {
        let ctx = crate::testdir::Held::tree(
            "gate-verus",
            &[
                ("crates/k/src/lib.rs", "mod cas;\n"),
                ("crates/k/src/cas.rs", "0\na\nb\nc\n"),
                ("proofs/verus/law3_merge.rs", "fn merge() {}\n"),
                ("proofs/verus/mutants/k_cas.patch", DIFF),
                (
                    "proofs/verus/mutants/law3.patch",
                    "--- law3_merge.rs\n+++ merge\n@@ -1 +1 @@\n-fn merge() {}\n+fn merge() { }\n",
                ),
                (
                    "proofs/verus/mutants/stale.patch",
                    "--- law3_merge.rs\n+++ merge\n@@ -1 +1 @@\n-fn gone() {}\n+fn x() {}\n",
                ),
            ],
        );
        let root = normalized(&ctx.root);
        let files = [
            "crates/k/src/lib.rs".to_string(),
            "proofs/verus/law?_*.rs".to_string(),
        ];
        let entries = entries(&ctx, &files).unwrap();
        let shown: Vec<String> = entries
            .iter()
            .map(|it| project::relative(&root, it))
            .collect();
        assert_eq!(shown, ["crates/k/src/lib.rs", "proofs/verus/law3_merge.rs"]);
        let mutants = patches(&root.join(MUTANTS));
        let plan = planned(&root, &entries, &mutants[0]).unwrap();
        assert_eq!(project::relative(&root, &plan.entry), "crates/k/src/lib.rs");
        let scratch = root.join(SCRATCH);
        let job = plan
            .staged(&root, &scratch, "proofs/verus/mutants/k_cas.patch", &[])
            .unwrap();
        let copy = scratch.join("k_cas");
        assert_eq!(
            std::fs::read_to_string(copy.join("cas.rs")).unwrap(),
            "0\na\nB\nc\n"
        );
        assert_eq!(
            std::fs::read_to_string(copy.join("lib.rs")).unwrap(),
            "mod cas;\n"
        );
        let verified = format!("{SCRATCH}/k_cas/lib.rs");
        assert_eq!(
            job.argv,
            [
                verified.as_str(),
                "--verify-only-module",
                "cas",
                "--verify-function",
                "put"
            ]
        );
        let law = planned(&root, &entries, &mutants[1]).unwrap();
        assert_eq!(
            project::relative(&root, &law.entry),
            "proofs/verus/law3_merge.rs"
        );
        let stale = planned(&root, &entries, &mutants[2]).unwrap_err();
        assert_eq!(
            stale,
            "the hunk at line 1 no longer matches in proofs/verus/law3_merge.rs; make the mutant again"
        );
    }

    /// A Verus that verifies `law.rs` and the `same` mutant, fails on `bad.rs` and the `put`
    /// mutant, and stops on the rest; it writes each run's arguments to `runs.log`.
    const FAKE: &str = r#"shift
echo "$*" >> runs.log
case "$1" in
  --version) exit 0 ;;
  proofs/verus/law.rs|*/same/*)
    ok='{"verification-results": {"verified": 1, "errors": 0, "encountered-error": false}, '
    echo "$ok"'"func-details": {"law::put": {}, "vstd::seq::axiom_seq_len": {}}}' ;;
  proofs/verus/bad.rs|*/put/*)
    echo '{"verification-results": {"verified": 0, "errors": 1, "encountered-error": true}}'
    echo 'error: postcondition not satisfied' >&2; exit 1 ;;
  *) echo 'error: expected one of' >&2; exit 1 ;;
esac
"#;

    /// A mutant of `law.rs` named `--- {file}`, which breaks `{function}`.
    fn mutant(file: &str, function: &str) -> String {
        format!("--- {file}\n+++ {function}\n@@ -1 +1 @@\n-fn put() {{}}\n+fn put() {{ }}\n")
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn each_proof_file_must_verify_and_each_mutant_must_fail() {
        let (put, same) = (mutant("./law.rs", "law::put"), mutant("law.rs", "put"));
        let (away, gone) = (
            mutant("../../../outside.rs", "put"),
            mutant("nothere.rs", "put"),
        );
        let mut ctx = crate::testdir::Held::tree(
            "gate-verus-run",
            &[
                ("proofs/verus/law.rs", "fn put() {}\n"),
                ("proofs/verus/bad.rs", ""),
                ("proofs/verus/stop.rs", ""),
                ("proofs/verus/target/kept.rs", ""),
                ("proofs/verus/mutants/put.patch", put.as_str()),
                ("proofs/verus/mutants/same.patch", same.as_str()),
                ("proofs/verus/mutants/halt.patch", same.as_str()),
                ("proofs/verus/mutants/away.patch", away.as_str()),
                ("proofs/verus/mutants/gone.patch", gone.as_str()),
                ("proofs/verus/mutants/noname.patch", "+++ put\n"),
            ],
        );
        std::fs::write(
            ctx.root.join("proofs/verus/mutants/raw.patch"),
            [0xff, 0xfe],
        )
        .unwrap();
        ctx.tools = crate::testdir::faked(&ctx.root, FAKE);
        let program = if cfg!(windows) { "verus.exe" } else { "verus" };
        let pin = crate::testdir::fetched(&ctx.root, "VERUS", program);
        std::fs::write(ctx.root.join(crate::project::PIN_FILE), pin).unwrap();
        let read = measure(&ctx).unwrap();
        // The `same` mutant verifies too, and its copy of `law::put` is no row.
        let rows = BTreeMap::from([("proofs/verus/law.rs#put".to_string(), 0)]);
        assert_eq!(read.series, Series(rows));
        assert!(!ctx.root.join(SCRATCH).exists(), "the scratch stayed");
        let found: Vec<(&str, Option<&str>, &str)> = read
            .faults
            .iter()
            .map(|it| (it.file.as_str(), it.item.as_deref(), it.message.as_str()))
            .collect();
        let (unread, raw) = (found[1].2, found[3].2);
        let shown = "patches proofs/verus/nothere.rs, which chock cannot read: ";
        assert!(unread.starts_with(shown), "{unread}");
        assert!(raw.starts_with("chock cannot read it: "), "{raw}");
        let at = |name: &str| format!("proofs/verus/mutants/{name}.patch");
        let patches = ["away", "gone", "noname", "raw", "halt", "same"].map(at);
        let stopped = "exit 1: error: expected one of";
        let halted =
            format!("the mutant gets no verdict from Verus, which refutes nothing: {stopped}");
        let failed = "verification results:: 0 verified, 1 errors; exit 1: error: postcondition not satisfied";
        let (failed, stopped) = (
            format!("does not verify: {failed}"),
            format!("Verus gave no verdict: {stopped}"),
        );
        let verifies = judged(true, &Said::Verified(Vec::new()), 0).unwrap();
        let expected: [(&str, Option<&str>, &str); 8] = [
            (
                &patches[0],
                None,
                "patches ../../../outside.rs, which is outside the project",
            ),
            (&patches[1], None, unread),
            (&patches[2], None, NAMES),
            (&patches[3], None, raw),
            ("proofs/verus/bad.rs", None, &failed),
            ("proofs/verus/stop.rs", None, &stopped),
            (&patches[4], Some("put"), &halted),
            (&patches[5], Some("put"), &verifies),
        ];
        assert_eq!(found, expected);
        let log = std::fs::read_to_string(ctx.root.join("runs.log")).unwrap();
        let put = concat!(
            "target/test-scratch/chock/verus/put/law.rs --crate-type=lib --verify-only-module law ",
            "--verify-function put --output-json --num-threads"
        );
        assert!(log.lines().any(|line| line.starts_with(put)), "{log}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn verus_is_the_pinned_one_else_the_one_on_path_and_a_lone_proof_is_noted() {
        let mut ctx = crate::testdir::Held::tree("gate-verus-path", &[("proofs/verus/law.rs", "")]);
        ctx.tools = crate::testdir::faked(&ctx.root, FAKE);
        let root = ctx.root.clone();
        assert_eq!(verus(&ctx, &root).unwrap(), "verus");
        let noted = measure(&ctx).unwrap();
        assert!(noted.faults.is_empty());
        let notes: Vec<(&str, &str)> = noted
            .findings
            .iter()
            .map(|it| (it.file.as_str(), it.message.as_str()))
            .collect();
        let lone = "holds no mutant patch, so nothing shows that Verus can fail on these proofs";
        assert_eq!(notes, [(MUTANTS, lone)]);
        ctx.tools.launcher = vec!["chock-no-such-program".into()];
        let unpinned = format!("`verus`{}", crate::setup::downloads::NOT_PINNED);
        assert_eq!(verus(&ctx, &root).unwrap_err(), unpinned);
        let pin = crate::testdir::fetched(&root, "VERUS", "verus");
        std::fs::write(root.join(crate::project::PIN_FILE), pin).unwrap();
        ctx.tools.home = None;
        let homeless = "no home directory holds chock's tools; set CHOCK_TOOLS";
        assert_eq!(measure(&ctx).unwrap_err(), homeless);
        // A pin that cannot be read is no reason to take the `verus` on `PATH`.
        ctx.tools.launcher = crate::testdir::faked(&root, FAKE).launcher;
        assert_eq!(verus(&ctx, &root).unwrap_err(), homeless);
        ctx.verus.files = Some(vec!["nowhere/*.rs".into()]);
        let none = "no file matches nowhere/*.rs, so Verus would verify nothing";
        assert_eq!(measure(&ctx).unwrap_err(), none);
    }

    #[test]
    fn a_run_past_its_limit_is_timed_out_and_one_that_did_not_start_is_an_error() {
        let job = Job {
            shown: "m.patch".into(),
            function: Some("put".into()),
            argv: Vec::new(),
        };
        let error = |stage| exec::ExecError::of("verus", stage, "gone");
        let (hung, verified) = job.read(Err(error(exec::Stage::Hung)), 9).unwrap();
        let hung = hung.unwrap();
        assert!(verified.is_empty());
        let past = "Verus ran past its 9 s limit; raise `verus.timeout_s` if it needs longer";
        assert_eq!(
            (hung.item.as_deref(), hung.message.as_str()),
            (Some("put"), past)
        );
        let lost = job.read(Err(error(exec::Stage::Spawn)), 9).unwrap_err();
        assert_eq!(lost, error(exec::Stage::Spawn).to_string());
        assert_eq!(
            [shares(8, 2), shares(3, 6), shares(0, 0)],
            [(2, 4), (3, 1), (1, 1)]
        );
    }
}
