//! Proof kills: with `mutest_proofs` on, a mutation no test detects is killed where a Kani
//! harness that verifies on its file as it is fails on a copy of the project that holds it.

mod crate_root;
mod prover;
mod splice;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::results::Joined;
use super::survivors::{Survivor, Totals};
use crate::exec;
use crate::gates::proofs::{cleared, patch};
use crate::gates::tools::proof;
use crate::run::Ctx;
use crate::run::report::Finding;
use prover::Prover;

/// The copy of the project that holds each mutation in turn.
const SCRATCH: &str = "target/test-scratch/chock/mutest-proofs";

/// What Kani showed of one mutation.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// This harness failed on it.
    Killed(String),
    /// Each harness Kani checked verified it.
    Verified,
    /// Kani checked no harness, such as where the mutant does not build; why, as Kani said.
    Unchecked(String),
    /// Kani ran past its limit.
    Hung,
    /// No file holds the mutation, so Kani did not run.
    Unbuilt,
    /// No harness of its file verifies on the file as it is.
    Unproved,
}

/// The survivors no proof kills, with a lead for each one a proof kills and one that sums up.
pub(super) fn killed(
    ctx: &Ctx,
    found: Joined,
    kani: &proof::Kani<'_>,
) -> Result<(Joined, Vec<Finding>), String> {
    if !ctx.mutest_proofs || found.survivors.is_empty() {
        return Ok((found, Vec::new()));
    }
    let root = patch::normalized(&ctx.root);
    let copy = root.join(SCRATCH);
    cleared(&copy)?;
    patch::copied(&root, &copy)
        .map_err(|e| format!("cannot copy the project to {}: {e}", copy.display()))?;
    let tried = Prover::listed(ctx, &root, &copy, kani).and_then(|prover| prover.tried(found));
    cleared(&copy)?;
    tried
}

/// The harnesses of `asked` that Kani verified in `out`.
fn verified(out: &exec::Output, asked: &BTreeSet<String>) -> BTreeSet<String> {
    let summaries = proof::summarised(&out.stdout).unwrap_or_default();
    summaries
        .iter()
        .filter(|held| asked.contains(&held.name))
        .filter(|held| proof::findings(std::slice::from_ref(*held), false).is_empty())
        .map(|held| held.name.clone())
        .collect()
}

/// A kill takes a harness that chock asked for and that Kani names at fault. A run that hung, or
/// that names no such harness, kills nothing.
fn verdict(
    run: Result<exec::Output, exec::ExecError>,
    asked: &BTreeSet<String>,
) -> Result<Outcome, String> {
    let out = match run {
        Ok(out) => out,
        Err(e) if e.hung() => return Ok(Outcome::Hung),
        Err(e) => return Err(e.to_string()),
    };
    let summaries = proof::summarised(&out.stdout).unwrap_or_default();
    let ran: Vec<&proof::Harness> = summaries
        .iter()
        .filter(|held| asked.contains(&held.name))
        .collect();
    let failed = ran
        .iter()
        .find(|held| !proof::findings(std::slice::from_ref(**held), false).is_empty());
    Ok(match (failed, ran.is_empty()) {
        (Some(held), _) => Outcome::Killed(held.name.clone()),
        (None, false) => Outcome::Verified,
        (None, true) => Outcome::Unchecked(out.why_it_stopped()),
    })
}

/// The lead for a survivor a proof killed: the tests missed it, and this harness did not.
fn kill(survivor: &Survivor, harness: &str) -> Finding {
    let (what, operator) = (&survivor.what, &survivor.operator);
    let said = format!("no test detected {what} ({operator}), and this proof did");
    Finding::at(&survivor.file, &said)
        .line(survivor.line)
        .item(harness)
        .candidate()
}

/// A package: its directory and the root file of each lib and bin target, from the workspace
/// root, and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Package {
    dir: PathBuf,
    name: String,
    roots: Vec<PathBuf>,
}

/// Each package of the workspace.
fn packages_of(metadata: &str) -> Result<Vec<Package>, String> {
    let read: serde_json::Value = serde_json::from_str(metadata)
        .map_err(|e| format!("cargo metadata wrote what chock cannot read: {e}"))?;
    let root = Path::new(read["workspace_root"].as_str().unwrap_or_default());
    let packages = read["packages"].as_array().into_iter().flatten();
    Ok(packages
        .filter_map(|package| {
            let manifest = Path::new(package["manifest_path"].as_str()?);
            let dir = manifest.parent()?.strip_prefix(root).ok()?;
            let targets = package["targets"].as_array().into_iter().flatten();
            let roots = targets.filter(|target| {
                let kinds = target["kind"].as_array().into_iter().flatten();
                let built = |kind: &str| kind == "bin" || kind.ends_with("lib");
                kinds.filter_map(|kind| kind.as_str()).any(built)
            });
            let roots = roots.filter_map(|target| {
                let path = Path::new(target["src_path"].as_str()?);
                Some(path.strip_prefix(root).ok()?.to_path_buf())
            });
            Some(Package {
                dir: dir.to_path_buf(),
                name: package["name"].as_str()?.to_string(),
                roots: roots.collect(),
            })
        })
        .collect())
}

/// The package whose directory is the nearest one above `file`.
fn package_of<'a>(packages: &'a [Package], file: &str) -> Option<&'a Package> {
    packages
        .iter()
        .filter(|package| Path::new(file).starts_with(&package.dir))
        .max_by_key(|package| package.dir.components().count())
}

/// How many survivors met each outcome, and the first reason Kani checked none.
#[derive(Debug, Default)]
struct Tally {
    killed: usize,
    verified: usize,
    unchecked: usize,
    hung: usize,
    unbuilt: usize,
    unproved: usize,
    first_unchecked: Option<String>,
}

impl Tally {
    fn add(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Killed(_) => self.killed += 1,
            Outcome::Verified => self.verified += 1,
            Outcome::Unchecked(why) => {
                self.unchecked += 1;
                self.first_unchecked.get_or_insert(why);
            }
            Outcome::Hung => self.hung += 1,
            Outcome::Unbuilt => self.unbuilt += 1,
            Outcome::Unproved => self.unproved += 1,
        }
    }

    /// The summary lead: what the tests and the proofs killed, and why each other survivor stays.
    fn lead(&self, totals: Option<&Totals>) -> Finding {
        let tried = self.killed + self.verified + self.unchecked + self.hung;
        let all = tried + self.unbuilt + self.unproved;
        let proofs = format!(
            "proofs killed {} of the {all} mutation(s) no test detected",
            self.killed
        );
        let mut said = vec![match totals {
            Some(totals) => {
                let missed = totals.undetected.saturating_add(totals.timed_out);
                let detected = totals.total.saturating_sub(missed);
                format!(
                    "tests detected {detected} of the {} mutation(s), and {proofs}",
                    totals.total
                )
            }
            None => proofs,
        }];
        let first = self.first_unchecked.as_deref().unwrap_or_default();
        let parts = [
            (
                self.verified,
                "verified under each harness of their file".to_string(),
            ),
            (
                self.unproved,
                "are in a file no verified harness checks".to_string(),
            ),
            (
                self.unbuilt,
                "could not be rebuilt from mutest's edits".to_string(),
            ),
            (
                self.unchecked,
                format!("stopped Kani before it checked a harness: {first}"),
            ),
            (
                self.hung,
                "ran past Kani's limit, `proof.timeout_s`".to_string(),
            ),
        ];
        said.extend(
            parts
                .iter()
                .filter(|(count, _)| *count > 0)
                .map(|(count, why)| format!("{count} {why}")),
        );
        Finding::at("", &said.join("; ")).item("kani").candidate()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    const RAN: &str = "\
Checking harness a::fails...
SUMMARY:
 ** 1 of 2 failed
Failed Checks: assertion failed: x < 200
VERIFICATION:- FAILED
Checking harness a::holds...
SUMMARY:
 ** 0 of 2 failed
VERIFICATION:- SUCCESSFUL
";

    fn asked(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn ran(stdout: &str, code: i32) -> Result<exec::Output, exec::ExecError> {
        Ok(exec::Output::of(Some(code), stdout, ""))
    }

    fn survivor() -> Survivor {
        Survivor {
            operator: "call_delete".to_string(),
            file: "src/a.rs".to_string(),
            line: 4,
            what: "delete a call".to_string(),
            edits: Vec::new(),
        }
    }

    #[test]
    fn with_the_setting_off_or_no_survivor_kani_never_runs_and_nothing_changes() {
        // A root that does not exist: a run that copied it or ran Kani in it would fail.
        let mut ctx = Ctx::at(Path::new("/nonexistent/chock-proved"));
        let never = |_: &[&str], _: &Path, _: &Path| -> Result<exec::Output, exec::ExecError> {
            Err(exec::ExecError::of("cargo", exec::Stage::Spawn, "ran"))
        };
        let found = Joined {
            survivors: vec![survivor()],
            ..Joined::default()
        };
        assert_eq!(killed(&ctx, found.clone(), &never), Ok((found, Vec::new())));
        ctx.mutest_proofs = true;
        let none = Joined::default();
        assert_eq!(killed(&ctx, none.clone(), &never), Ok((none, Vec::new())));
    }

    #[test]
    fn only_a_harness_chock_asked_for_and_kani_names_at_fault_kills() {
        let both = asked(&["a::fails", "a::holds"]);
        let killed = Outcome::Killed("a::fails".to_string());
        assert_eq!(verdict(ran(RAN, 1), &both), Ok(killed));
        assert_eq!(
            verdict(ran(RAN, 1), &asked(&["a::holds"])),
            Ok(Outcome::Verified)
        );
        let other = verdict(ran(RAN, 1), &asked(&["b::other"]));
        assert!(matches!(other, Ok(Outcome::Unchecked(_))), "{other:?}");
        let error = "warning: unused\nerror[E0308]: mismatched types\n --> src/a.rs:4:20\n";
        let summary = "error: could not compile `a` (lib) due to 1 previous error\n";
        let unbuilt = verdict(Ok(exec::Output::of(Some(101), error, summary)), &both);
        let said = "error[E0308]: mismatched types at src/a.rs:4:20".to_string();
        assert_eq!(unbuilt, Ok(Outcome::Unchecked(said)));
        let unplaced = verdict(ran("error[E0308]: mismatched types\n", 101), &both);
        let said = "error[E0308]: mismatched types".to_string();
        assert_eq!(unplaced, Ok(Outcome::Unchecked(said)));
        let unsaid = verdict(Ok(exec::Output::of(Some(1), "", summary)), &both);
        let said = summary.trim_end().to_string();
        assert_eq!(unsaid, Ok(Outcome::Unchecked(said)));
    }

    #[test]
    fn a_run_that_hung_kills_nothing_and_one_that_did_not_start_is_an_error() {
        let hung = Err(exec::ExecError::of("cargo", exec::Stage::Hung, "gone"));
        assert_eq!(verdict(hung, &asked(&["a::fails"])), Ok(Outcome::Hung));
        let gone = exec::ExecError::of("cargo", exec::Stage::Spawn, "gone");
        let said = verdict(Err(gone.clone()), &asked(&["a::fails"]));
        assert_eq!(said, Err(gone.to_string()));
    }

    #[test]
    fn a_file_keeps_only_the_harnesses_that_verify_on_it_as_it_is() {
        let out = exec::Output::of(Some(1), RAN, "");
        let all = asked(&["a::fails", "a::holds", "b::other"]);
        assert_eq!(verified(&out, &all), asked(&["a::holds"]));
        let vacuous = "Checking harness a::holds...\nSUMMARY:\n ** 0 of 2 failed (2 unreachable)\n\
                       VERIFICATION:- SUCCESSFUL\n";
        let out = exec::Output::of(Some(0), vacuous, "");
        assert_eq!(verified(&out, &asked(&["a::holds"])), BTreeSet::new());
    }

    #[test]
    fn a_file_belongs_to_the_package_nearest_above_it() {
        let metadata = r#"{"workspace_root":"/w","packages":[
            {"name":"top","manifest_path":"/w/Cargo.toml","targets":[
                {"kind":["lib"],"src_path":"/w/src/lib.rs"},
                {"kind":["bin"],"src_path":"/w/src/main.rs"},
                {"kind":["test"],"src_path":"/w/tests/cli.rs"},
                {"kind":["custom-build"],"src_path":"/w/build.rs"}]},
            {"name":"inner","manifest_path":"/w/crates/inner/Cargo.toml"},
            {"name":"away","manifest_path":"/elsewhere/Cargo.toml"}]}"#;
        let packages = packages_of(metadata).unwrap();
        let wanted = [
            ("", "top", vec!["src/lib.rs", "src/main.rs"]),
            ("crates/inner", "inner", vec![]),
        ];
        let wanted: Vec<_> = wanted
            .map(|(dir, name, roots)| Package {
                dir: PathBuf::from(dir),
                name: name.to_string(),
                roots: roots.into_iter().map(PathBuf::from).collect(),
            })
            .into();
        assert_eq!(packages, wanted);
        let of = |file: &str| package_of(&packages, file).map(|package| package.name.as_str());
        assert_eq!(of("crates/inner/src/lib.rs"), Some("inner"));
        assert_eq!(of("crates/inner2/src/lib.rs"), Some("top"));
        assert_eq!(of("src/main.rs"), Some("top"));
        let inner = &packages[1..];
        assert_eq!(package_of(inner, "src/main.rs"), None);
        assert!(packages_of("not json").is_err());
    }

    #[test]
    fn a_kill_names_the_mutation_where_it_is_and_the_harness_that_killed_it() {
        let said = kill(&survivor(), "a::fails").render();
        let wanted = "src/a.rs:4: a::fails: no test detected delete a call (call_delete), and this \
                      proof did";
        assert_eq!(said, wanted);
    }

    #[test]
    fn the_lead_counts_each_outcome_and_names_the_first_reason_kani_checked_none() {
        let mut tally = Tally::default();
        let outcomes = [
            Outcome::Killed("a::fails".to_string()),
            Outcome::Verified,
            Outcome::Verified,
            Outcome::Unchecked("first".to_string()),
            Outcome::Unchecked("second".to_string()),
            Outcome::Hung,
            Outcome::Unbuilt,
            Outcome::Unproved,
        ];
        for outcome in outcomes {
            tally.add(outcome);
        }
        let totals = Totals {
            undetected: 7,
            timed_out: 1,
            total: 20,
        };
        let wanted = "kani: tests detected 12 of the 20 mutation(s), and proofs killed 1 of the 8 \
                      mutation(s) no test detected; 2 verified under each harness of their file; \
                      1 are in a file no verified harness checks; 1 could not be rebuilt from \
                      mutest's edits; 2 stopped Kani before it checked a harness: first; 1 ran \
                      past Kani's limit, `proof.timeout_s`";
        assert_eq!(tally.lead(Some(&totals)).render(), wanted);
        let none = Tally::default().lead(None).render();
        assert_eq!(
            none,
            "kani: proofs killed 0 of the 0 mutation(s) no test detected"
        );
    }
}
