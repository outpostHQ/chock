//! The mutants of the `proof` gate: each patch puts a fault in one file, and the harness that its
//! line 2 names must fail on a copy of the project that holds that fault.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::exec;
use crate::gates::proofs::{cleared, patch};
use crate::project;
use crate::run::Ctx;
use crate::run::report::Finding;

/// Where the mutant patches are when `proof.mutants` is left out.
const MUTANTS: &str = "proofs/kani/mutants";
/// The copy of the project that each mutant patches in turn.
const SCRATCH: &str = "target/test-scratch/chock/kani";

const NAMES: &str = "line 1 must be `--- <the file it patches, from the project root>` and line 2 \
                     `+++ <the harness that must fail>`, such as `+++ proofs::put_keeps_len`";

const VERIFIES: &str = "the mutant verifies, so no harness sees the fault it puts in; make the \
                        harness stronger, or the mutant break what the harness checks";

/// What kani said of the mutants: each that it did not refute, and a note where there is none.
#[derive(Debug, Default)]
pub(super) struct Refuted {
    pub(super) faults: Vec<Finding>,
    pub(super) notes: Vec<Finding>,
}

/// One run of kani: its arguments, the directory it runs in, and the target directory it uses.
pub(super) type Kani<'a> =
    dyn Fn(&[&str], &Path, &Path) -> Result<exec::Output, exec::ExecError> + 'a;

/// Runs the harness of each mutant on a copy of the project that holds the mutant's fault. Each
/// run builds in the project's target directory, so it reuses the build of the dependencies.
pub(super) fn refuted(
    ctx: &Ctx,
    listed: &BTreeSet<String>,
    limit: u64,
    kani: &Kani,
) -> Result<Refuted, String> {
    let root = patch::normalized(&ctx.root);
    let dir = root.join(ctx.proof.mutants.as_deref().unwrap_or(MUTANTS));
    let patches = patch::patches(&dir);
    if patches.is_empty() {
        let why = "holds no mutant patch, so nothing shows that kani can fail on these harnesses";
        let notes = vec![Finding::at(&project::relative(&root, &dir), why)];
        return Ok(Refuted {
            notes,
            ..Refuted::default()
        });
    }
    let copy = root.join(SCRATCH);
    cleared(&copy)?;
    patch::copied(&root, &copy)
        .map_err(|e| format!("cannot copy the project to {}: {e}", copy.display()))?;
    let mut faults = Vec::new();
    for path in &patches {
        let shown = project::relative(&root, path);
        let mutant = match patch::mutant(&root, &root, path, NAMES) {
            Ok(mutant) => mutant,
            Err(why) => {
                faults.push(Finding::at(&shown, &why));
                continue;
            }
        };
        let why = match listed.contains(&mutant.name) {
            true => tried(ctx, &mutant, &copy, limit, kani)?,
            false => Some(format!(
                "names the harness {}, which kani does not list",
                mutant.name
            )),
        };
        faults.extend(why.map(|why| Finding::at(&shown, &why).item(&mutant.name)));
    }
    cleared(&copy)?;
    Ok(Refuted {
        faults,
        notes: Vec::new(),
    })
}

/// Why kani does not refute one mutant, if it does not. The copy gets its own file back after.
fn tried(
    ctx: &Ctx,
    mutant: &patch::Mutant,
    copy: &Path,
    limit: u64,
    kani: &Kani,
) -> Result<Option<String>, String> {
    let harness = mutant.name.as_str();
    let root = patch::normalized(&ctx.root);
    let inside = copy.join(mutant.target.strip_prefix(&root).unwrap_or(&mutant.target));
    let written = |result: std::io::Result<()>| {
        result.map_err(|e| format!("cannot write {}: {e}", inside.display()))
    };
    written(project::document::write(&inside, &mutant.patched))?;
    let packages = ctx.proof.packages.as_deref().unwrap_or_default();
    let only = ["--harness", harness, "--exact", "--output-format", "terse"];
    let argv = super::invocation(packages, &ctx.features, &only);
    let run = kani(&argv, copy, &target_dir(&root));
    written(std::fs::copy(&mutant.target, &inside).map(drop))?;
    verdict(run, harness, limit)
}

/// The project's target directory: `CARGO_TARGET_DIR` from the project root, else `target`.
fn target_dir(root: &Path) -> PathBuf {
    let set = std::env::var_os("CARGO_TARGET_DIR").filter(|dir| !dir.is_empty());
    root.join(set.map_or_else(|| PathBuf::from("target"), PathBuf::from))
}

/// Why a run of `harness` on a mutant refutes nothing, if it does not: the harness must fail.
fn verdict(
    run: Result<exec::Output, exec::ExecError>,
    harness: &str,
    limit: u64,
) -> Result<Option<String>, String> {
    let out = match run {
        Ok(out) => out,
        Err(e) if e.hung() => {
            return Ok(Some(format!(
                "kani ran past its {limit} s limit on the mutant; raise `proof.timeout_s` if it \
                 needs longer"
            )));
        }
        Err(e) => return Err(e.to_string()),
    };
    let summaries = super::summarised(&out.stdout).unwrap_or_default();
    let held = summaries.iter().find(|held| held.name == harness);
    Ok(match held {
        // A mutant asks only whether a harness fails, so a missing cover answers nothing.
        Some(held) if !super::findings(std::slice::from_ref(held), false).is_empty() => None,
        Some(_) => Some(VERIFIES.into()),
        None => Some(format!(
            "the mutant stops kani before it checks {harness}, which refutes nothing: {}",
            out.why_it_stopped()
        )),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    const LIB: &str = "pub fn add(a: u8, b: u8) -> u8 {\n    a.wrapping_add(b)\n}\n";

    fn sub(harness: &str) -> String {
        format!(
            "--- src/lib.rs\n+++ {harness}\n@@ -2 +2 @@\n-    a.wrapping_add(b)\n+    a.wrapping_sub(b)\n"
        )
    }

    fn listed(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    /// A kani that fails `proofs::adds` and `proofs::halves`, verifies `proofs::bounded`, stops
    /// before it checks `proofs::stops`, and runs past its limit on the rest.
    fn kani(argv: &[&str]) -> Result<exec::Output, exec::ExecError> {
        let at = argv.iter().position(|it| *it == "--harness").unwrap();
        let harness = argv[at + 1];
        let checked = |verdict: &str, failed: u32| {
            format!(
                "Checking harness {harness}...\nSUMMARY:\n ** {failed} of 2 failed\nVERIFICATION:- {verdict}\n"
            )
        };
        match harness {
            "proofs::adds" | "proofs::halves" => {
                Ok(exec::Output::of(Some(1), &checked("FAILED", 1), ""))
            }
            "proofs::bounded" => Ok(exec::Output::of(Some(0), &checked("SUCCESSFUL", 0), "")),
            "proofs::stops" => Ok(exec::Output::of(
                Some(101),
                "",
                "error[E0308]: mismatched types\n",
            )),
            _ => Err(exec::ExecError::of("cargo", exec::Stage::Hung, "gone")),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot copy a file")]
    fn each_mutant_must_make_its_harness_fail_on_a_copy_that_holds_its_fault() {
        let (adds, bounded, stops) = (
            sub("proofs::adds"),
            sub("proofs::bounded"),
            sub("proofs::stops"),
        );
        let (slow, gone) = (sub("proofs::slow"), sub("proofs::gone"));
        let stale = "--- src/lib.rs\n+++ proofs::adds\n@@ -2 +2 @@\n-    a + b\n+    a - b\n";
        let half = "--- src/half.rs\n+++ proofs::halves\n@@ -1 +1 @@\n-x / 2\n+x / 3\n";
        let tree = [
            ("src/lib.rs", LIB),
            ("src/half.rs", "x / 2\n"),
            ("proofs/kani/mutants/a_caught.patch", adds.as_str()),
            ("proofs/kani/mutants/b_missed.patch", bounded.as_str()),
            ("proofs/kani/mutants/c_stops.patch", stops.as_str()),
            ("proofs/kani/mutants/d_slow.patch", slow.as_str()),
            ("proofs/kani/mutants/e_unlisted.patch", gone.as_str()),
            ("proofs/kani/mutants/f_stale.patch", stale),
            ("proofs/kani/mutants/g_half.patch", half),
        ];
        let ctx = crate::testdir::Held::tree("kani-mutants", &tree);
        let root = patch::normalized(&ctx.root);
        let runs = RefCell::new(Vec::new());
        let seen = |argv: &[&str], dir: &Path, target: &Path| {
            let text = |file: &str| std::fs::read_to_string(dir.join(file)).unwrap();
            let faults = (
                text("src/lib.rs").contains("sub"),
                text("src/half.rs").contains('3'),
            );
            runs.borrow_mut()
                .push((argv.join(" "), faults, target.to_path_buf()));
            kani(argv)
        };
        let names = [
            "proofs::adds",
            "proofs::bounded",
            "proofs::stops",
            "proofs::slow",
        ];
        let names = listed(&[&names[..], &["proofs::halves"]].concat());
        let refuted = refuted(&ctx, &names, 9, &seen).unwrap();
        let found: Vec<String> = refuted.faults.iter().map(Finding::render).collect();
        let at = |stem: &str| format!("proofs/kani/mutants/{stem}.patch");
        assert_eq!(
            found,
            [
                format!("{}: proofs::bounded: {VERIFIES}", at("b_missed")),
                format!(
                    "{}: proofs::stops: the mutant stops kani before it checks proofs::stops, \
                     which refutes nothing: error[E0308]: mismatched types",
                    at("c_stops")
                ),
                format!(
                    "{}: proofs::slow: kani ran past its 9 s limit on the mutant; raise \
                     `proof.timeout_s` if it needs longer",
                    at("d_slow")
                ),
                format!(
                    "{}: proofs::gone: names the harness proofs::gone, which kani does not list",
                    at("e_unlisted")
                ),
                format!(
                    "{}: the hunk at line 2 no longer matches in src/lib.rs; make the mutant again",
                    at("f_stale")
                ),
            ]
        );
        assert!(refuted.notes.is_empty());
        // Each run holds its own fault and no other: the copy got the file back after each run.
        let runs = runs.into_inner();
        let held: Vec<(bool, bool)> = runs.iter().map(|(_, faults, _)| *faults).collect();
        let lib = (true, false);
        assert_eq!(held, [lib, lib, lib, lib, (false, true)]);
        assert_eq!(
            runs[0].0,
            "kani --workspace --harness proofs::adds --exact --output-format terse"
        );
        assert!(
            runs.iter()
                .all(|(_, _, target)| *target == target_dir(&root))
        );
        assert!(!root.join(SCRATCH).exists(), "the copy stayed");
        assert_eq!(
            std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
            LIB
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot copy a file")]
    fn a_run_sees_the_mutant_and_the_copy_gets_its_own_file_back_after() {
        let tree = [
            ("src/lib.rs", LIB),
            ("copy/src/lib.rs", LIB),
            ("m.patch", &sub("proofs::adds")),
        ];
        let ctx = crate::testdir::Held::tree("kani-tried", &tree);
        let root = patch::normalized(&ctx.root);
        let mutant = patch::mutant(&root, &root, &root.join("m.patch"), NAMES).unwrap();
        let copy = root.join("copy");
        let seen = RefCell::new(String::new());
        let run = |argv: &[&str], dir: &Path, _: &Path| {
            seen.replace(std::fs::read_to_string(dir.join("src/lib.rs")).unwrap());
            kani(argv)
        };
        assert_eq!(tried(&ctx, &mutant, &copy, 9, &run), Ok(None));
        assert_eq!(seen.into_inner(), mutant.patched);
        let back = std::fs::read_to_string(copy.join("src/lib.rs")).unwrap();
        assert_eq!(back, LIB);
    }

    #[test]
    fn a_mutant_refutes_nothing_unless_kani_fails_its_harness() {
        let said = |harness: &str| verdict(kani(&["--harness", harness]), harness, 9);
        assert_eq!(said("proofs::adds"), Ok(None));
        assert_eq!(said("proofs::bounded"), Ok(Some(VERIFIES.into())));
        let stops = "the mutant stops kani before it checks proofs::stops, which refutes nothing: \
                     error[E0308]: mismatched types";
        assert_eq!(said("proofs::stops"), Ok(Some(stops.into())));
        let slow = "kani ran past its 9 s limit on the mutant; raise `proof.timeout_s` if it needs \
                    longer";
        assert_eq!(said("proofs::slow"), Ok(Some(slow.into())));
        let unstarted = Err(exec::ExecError::of("cargo", exec::Stage::Spawn, "gone"));
        assert!(verdict(unstarted, "proofs::adds", 9).is_err());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_project_with_no_mutant_gets_a_note_and_no_run() {
        let ctx = crate::testdir::Held::tree("kani-no-mutant", &[("src/lib.rs", LIB)]);
        // A run would be an error, which the unwrap below reports.
        let never = |_: &[&str], _: &Path, _: &Path| {
            Err(exec::ExecError::of("cargo", exec::Stage::Spawn, "ran"))
        };
        let refuted = refuted(&ctx, &listed(&["proofs::adds"]), 9, &never).unwrap();
        assert!(refuted.faults.is_empty());
        let notes: Vec<String> = refuted.notes.iter().map(Finding::render).collect();
        assert_eq!(
            notes,
            [
                "proofs/kani/mutants: holds no mutant patch, so nothing shows that kani can fail on \
              these harnesses"
            ]
        );
    }
}
