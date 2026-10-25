//! The `verus` gate: Verus verifies each proof file with no error, and fails on each mutant, a
//! patch that puts a fault in one function.

use std::path::{Path, PathBuf};

use super::patch::{self, Mutant, copied, normalized, patches};

use crate::exec;
use crate::project;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

pub const GATE: Gate = Gate {
    name: "verus",
    about: "Verus verifies each proof file, and finds the fault each mutant patch puts in one",
    group: Group::OptIn,
    builds: true,
    // The tree holds the proofs, the mutants and the pin; only a `verus` on PATH is outside it.
    reads: Some(crate::run::verdicts::Reads::tree_and(&["verus"]).versioned("verus-v1")),
    kind: Kind::Binary(check),
};

const FILES: &str = "proofs/verus/*.rs";
const MUTANTS: &str = "proofs/verus/mutants";
const ARGS: [&str; 1] = ["--crate-type=lib"];
const TIMEOUT_S: u64 = 600;
const SCRATCH: &str = "target/test-scratch/chock/verus";

/// The line Verus ends a run with, as in `verification results:: 12 verified, 0 errors`.
const RESULTS: &str = "verification results::";
const CLEAN: &str = ", 0 errors";

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

/// What Verus said of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Said {
    Verified,
    /// The results line, and the first error above it.
    Failed(String),
    /// No results line, a clean one with nothing verified, or a clean one and a failed exit.
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
        // Verus writes the results line to stdout, ahead of the errors on stderr, and it
        // holds `errors`; the first error is the first line under it that names one.
        let others: Vec<&str> = text
            .lines()
            .filter(|line| !line.contains(RESULTS))
            .collect();
        let error = super::tla::first_error(&others.join("\n"), out.code);
        Ok(
            match text
                .lines()
                .map(str::trim)
                .find(|line| line.contains(RESULTS))
            {
                Some(line) if line.contains(CLEAN) => Self::clean(line, out, &error),
                Some(line) => Self::Failed(format!("{line}; {error}")),
                None => Self::Stopped(error),
            },
        )
    }

    /// A clean results line verifies only when Verus checked a function and exited 0.
    fn clean(line: &str, out: &exec::Output, error: &str) -> Self {
        match (counted(line), out.success()) {
            (Some(1..), true) => Self::Verified,
            (_, true) => Self::Stopped(format!("{line}, so the run proves no function")),
            (_, false) => Self::Stopped(format!("{line}, yet {error}")),
        }
    }
}

/// The count before ` verified` on the results line.
fn counted(line: &str) -> Option<u64> {
    let (_, rest) = line.split_once(RESULTS)?;
    let (count, _) = rest.split_once(" verified")?;
    count.trim().parse().ok()
}

/// The toolchain Verus asked for, from `required rust toolchain 1.98.1-x86_64-… not found`.
fn toolchain_missing(text: &str) -> Option<String> {
    let (_, rest) = text.split_once("required rust toolchain ")?;
    rest.split_whitespace().next().map(str::to_string)
}

/// Whether Verus said what the run must; `Some` says why not. A mutant must fail to verify.
fn judged(mutant: bool, said: &Said, limit: u64) -> Option<String> {
    match (mutant, said) {
        (false, Said::Verified) | (true, Said::Failed(_)) => None,
        (_, Said::TimedOut) => Some(format!(
            "Verus ran past its {limit} s limit; raise `verus.timeout_s` if it needs longer"
        )),
        (false, Said::Failed(why)) => Some(format!("does not verify: {why}")),
        (false, Said::Stopped(why)) => Some(format!("Verus gave no verdict: {why}")),
        (true, Said::Verified) => Some(
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

fn check(ctx: &Ctx) -> Result<Outcome, String> {
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
    let mut findings = Vec::new();
    for patch in &patches {
        let shown = project::relative(&root, patch);
        match planned(&root, &entries, patch) {
            Ok(plan) => jobs.push(plan.staged(&root, &scratch, &shown, &args)?),
            Err(why) => findings.push(Finding::at(&shown, &why)),
        }
    }
    let limit = settings.timeout_s.unwrap_or(TIMEOUT_S);
    // Each job, as many at once as the run's budget; their Verus threads share the same budget.
    let (workers, threads) = shares(exec::budget::cap(), jobs.len());
    let threads = ["--num-threads".to_string(), threads.to_string()];
    let run = |job: &Job| super::Run {
        dir: root.clone(),
        argv: job.argv.iter().chain(&threads).cloned().collect(),
        limit,
    };
    let runs: Vec<super::Run> = jobs.iter().map(run).collect();
    let outs = super::run_all(&ctx.tools, &verus, &runs, workers);
    super::cleared(&scratch)?;
    for (job, out) in jobs.iter().zip(outs) {
        findings.extend(job.finding(out, limit)?);
    }
    let notes = match patches.is_empty() {
        true => vec![Finding::at(
            &project::relative(&root, &mutants),
            "holds no mutant patch, so nothing shows that Verus can fail on these proofs",
        )],
        false => Vec::new(),
    };
    Ok(match findings.is_empty() {
        true => Outcome::noted(notes),
        false => Outcome::failed(findings),
    })
}

impl Job {
    /// What this job's run shows is wrong, if anything; `Err` where Verus did not start.
    fn finding(
        &self,
        out: Result<exec::Output, exec::ExecError>,
        limit: u64,
    ) -> Result<Option<Finding>, String> {
        let said = match out {
            Ok(out) => Said::of(&out)?,
            Err(e) if e.hung() => Said::TimedOut,
            Err(e) => return Err(e.to_string()),
        };
        let why = judged(self.function.is_some(), &said, limit);
        let found = why.map(|why| Finding::at(&self.shown, &why));
        Ok(found.map(|found| match &self.function {
            Some(function) => found.item(function),
            None => found,
        }))
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

    fn output(code: i32, stderr: &str) -> exec::Output {
        exec::Output {
            code: Some(code),
            stdout: String::new(),
            stderr: stderr.into(),
            truncated: false,
        }
    }

    #[test]
    fn a_module_path_verifies_in_its_module_and_a_bare_name_in_the_root() {
        let module = ["--verify-only-module", "cas", "--verify-function", "put"];
        assert_eq!(selected("cas::put"), module);
        let root = ["--verify-root", "--verify-function", "merge"];
        assert_eq!(selected("merge"), root);
    }

    #[test]
    fn verus_s_last_line_is_its_verdict_and_a_missing_toolchain_is_no_verdict() {
        let said = |text: &str| Said::of(&output(1, text));
        let clean = "\u{1b}[1mverification results:: 4 verified, 0 errors\u{1b}[0m\n";
        assert_eq!(Said::of(&output(0, clean)), Ok(Said::Verified));
        let failed = said(
            "error: postcondition not satisfied\n --> a.rs:3:5\nverification results:: 3 verified, 10 errors\n",
        );
        let why = match failed {
            Ok(Said::Failed(why)) => why,
            other => format!("{other:?}"),
        };
        assert!(
            why.starts_with(
                "verification results:: 3 verified, 10 errors; exit 1: error: postcondition"
            ),
            "{why}"
        );
        assert!(matches!(
            said("error: expected `;`\n --> a.rs:1:2\n"),
            Ok(Said::Stopped(_))
        ));
        let missing =
            said("verus: required rust toolchain 1.98.1-x86_64-unknown-linux-gnu not found\n");
        assert!(
            missing
                .unwrap_err()
                .ends_with("run `rustup install 1.98.1-x86_64-unknown-linux-gnu`")
        );
    }

    #[test]
    fn a_clean_results_line_verifies_only_after_a_function_was_checked_and_verus_exited_0() {
        let none = Said::of(&output(0, "verification results:: 0 verified, 0 errors\n"));
        assert!(matches!(none, Ok(Said::Stopped(why)) if why.ends_with("proves no function")));
        let failed = "verification results:: 2 verified, 0 errors\nerror: aborting\n";
        let failed = Said::of(&output(101, failed));
        assert!(matches!(failed, Ok(Said::Stopped(why)) if why.contains(", yet exit 101")));
        assert_eq!(
            counted("verification results:: 12 verified, 0 errors"),
            Some(12)
        );
        assert_eq!(counted("verification results:: verified, 0 errors"), None);
    }

    #[test]
    fn a_proof_file_must_verify_and_a_mutant_must_fail_to() {
        let failed = Said::Failed("1 errors".into());
        let stopped = Said::Stopped("exit 1: error: expected `;`".into());
        assert_eq!(judged(false, &Said::Verified, 9), None);
        assert_eq!(judged(true, &failed, 9), None);
        assert!(
            judged(true, &Said::Verified, 9)
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
  proofs/verus/law.rs|*/same/*) echo 'verification results:: 1 verified, 0 errors' ;;
  proofs/verus/bad.rs|*/put/*)
    echo 'verification results:: 0 verified, 1 errors'
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
        let outcome = check(&ctx).unwrap();
        assert!(!outcome.passed);
        assert!(!ctx.root.join(SCRATCH).exists(), "the scratch stayed");
        let found: Vec<(&str, Option<&str>, &str)> = outcome
            .findings
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
        let verifies = judged(true, &Said::Verified, 0).unwrap();
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
        let put = "target/test-scratch/chock/verus/put/law.rs --crate-type=lib --verify-only-module law --verify-function put --num-threads";
        assert!(log.lines().any(|line| line.starts_with(put)), "{log}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn verus_is_the_pinned_one_else_the_one_on_path_and_a_lone_proof_is_noted() {
        let mut ctx = crate::testdir::Held::tree("gate-verus-path", &[("proofs/verus/law.rs", "")]);
        ctx.tools = crate::testdir::faked(&ctx.root, FAKE);
        let root = ctx.root.clone();
        assert_eq!(verus(&ctx, &root).unwrap(), "verus");
        let noted = check(&ctx).unwrap();
        assert!(noted.passed);
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
        assert_eq!(check(&ctx).unwrap_err(), homeless);
        // A pin that cannot be read is no reason to take the `verus` on `PATH`.
        ctx.tools.launcher = crate::testdir::faked(&root, FAKE).launcher;
        assert_eq!(verus(&ctx, &root).unwrap_err(), homeless);
        ctx.verus.files = Some(vec!["nowhere/*.rs".into()]);
        let none = "no file matches nowhere/*.rs, so Verus would verify nothing";
        assert_eq!(check(&ctx).unwrap_err(), none);
    }

    #[test]
    fn a_run_past_its_limit_is_timed_out_and_one_that_did_not_start_is_an_error() {
        let job = Job {
            shown: "m.patch".into(),
            function: Some("put".into()),
            argv: Vec::new(),
        };
        let error = |stage| exec::ExecError::of("verus", stage, "gone");
        let hung = job
            .finding(Err(error(exec::Stage::Hung)), 9)
            .unwrap()
            .unwrap();
        let past = "Verus ran past its 9 s limit; raise `verus.timeout_s` if it needs longer";
        assert_eq!(
            (hung.item.as_deref(), hung.message.as_str()),
            (Some("put"), past)
        );
        let lost = job.finding(Err(error(exec::Stage::Spawn)), 9).unwrap_err();
        assert_eq!(lost, error(exec::Stage::Spawn).to_string());
        assert_eq!(
            [shares(8, 2), shares(3, 6), shares(0, 0)],
            [(2, 4), (3, 1), (1, 1)]
        );
    }
}
