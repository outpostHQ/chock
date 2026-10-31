//! Kani on the copy of the project: which harnesses verify on each file, and their verdict on
//! each mutation of it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use super::{Outcome, Package, Tally, crate_root, kill, package_of, packages_of, splice};
use super::{verdict, verified};
use crate::exec;
use crate::gates::mutation::results::Joined;
use crate::gates::mutation::survivors::Survivor;
use crate::gates::tools::proof;
use crate::project;
use crate::run::Ctx;
use crate::run::report::Finding;

/// Kani on the copy: the harnesses of each file, and the packages that hold the files.
pub(super) struct Prover<'a> {
    ctx: &'a Ctx,
    root: &'a Path,
    copy: &'a Path,
    target: PathBuf,
    kani: &'a proof::Kani<'a>,
    harnesses: BTreeMap<String, BTreeSet<String>>,
    packages: Vec<Package>,
}

/// Where one Kani job ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// After every harness ran.
    AfterAll,
    /// At the first harness that fails: `--fail-fast`.
    AtFirstFailure,
}

impl<'a> Prover<'a> {
    /// Lists the harnesses in the copy, so the listing Kani writes stays out of the tree. Each
    /// run of Kani goes through `kani`.
    pub(super) fn listed(
        ctx: &'a Ctx,
        root: &'a Path,
        copy: &'a Path,
        kani: &'a proof::Kani<'a>,
    ) -> Result<Self, String> {
        let metadata = project::metadata(root)?;
        let unstable = proof::unstable(&metadata)?;
        let packages = ctx.proof.packages.as_deref().unwrap_or_default();
        let argv = proof::invocation(packages, &ctx.features, &proof::list_with(&unstable));
        let target = proof::target_dir(root);
        let listed = kani(&argv, copy, &target).map_err(|e| e.to_string())?;
        if !listed.success() || listed.truncated {
            return Err(format!(
                "cargo kani could not completely list harnesses for mutest's proofs: {}",
                listed.why_it_failed()
            ));
        }
        Ok(Self {
            ctx,
            root,
            copy,
            target,
            kani,
            harnesses: proof::by_file(&proof::read_listing(copy)?)?,
            packages: packages_of(&metadata)?,
        })
    }

    /// Tries each survivor, file by file, and keeps those no proof kills in their order.
    pub(super) fn tried(&self, mut found: Joined) -> Result<(Joined, Vec<Finding>), String> {
        let mut by_file: BTreeMap<String, Vec<Survivor>> = BTreeMap::new();
        for survivor in std::mem::take(&mut found.survivors) {
            by_file
                .entry(survivor.file.clone())
                .or_default()
                .push(survivor);
        }
        let (mut tally, mut leads) = (Tally::default(), Vec::new());
        let totals = found.totals;
        for (file, survivors) in by_file {
            let held = self.holding(&file)?;
            for survivor in survivors {
                let outcome = self.outcome(&file, held.as_ref(), &survivor)?;
                match &outcome {
                    Outcome::Killed(harness) => leads.push(kill(&survivor, harness)),
                    _ => found.survivors.push(survivor),
                }
                tally.add(outcome);
            }
        }
        leads.push(tally.lead(totals.as_ref()));
        Ok((found, leads))
    }

    /// What a proof needs to kill mutations in `file`; `None` where no harness of the file
    /// verifies on it as it is, or where the file is not under the project root.
    fn holding(&self, file: &str) -> Result<Option<Held>, String> {
        let inside = Path::new(file)
            .components()
            .all(|part| matches!(part, Component::Normal(_)));
        let asked = self.harnesses.get(file).filter(|asked| !asked.is_empty());
        let package = package_of(&self.packages, file);
        let (true, Some(asked), Some(package)) = (inside, asked, package) else {
            return Ok(None);
        };
        let verified = match self.run(&package.name, asked, Stop::AfterAll) {
            Ok(out) => verified(&out, asked),
            Err(e) if e.hung() => BTreeSet::new(),
            Err(e) => return Err(e.to_string()),
        };
        if verified.is_empty() {
            return Ok(None);
        }
        let path = self.root.join(file);
        let original = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Ok(Some(Held {
            package: package.name.clone(),
            roots: package.roots.clone(),
            harnesses: verified,
            original,
        }))
    }

    /// Kani's verdict on one survivor, in the copy that holds it. The copy gets its files back.
    fn outcome(
        &self,
        file: &str,
        held: Option<&Held>,
        survivor: &Survivor,
    ) -> Result<Outcome, String> {
        let Some(held) = held else {
            return Ok(Outcome::Unproved);
        };
        let rebuilt = splice::rebuilt(held.original.as_bytes(), file, &survivor.edits)
            .and_then(|bytes| String::from_utf8(bytes).ok());
        let Some(mutant) = rebuilt else {
            return Ok(Outcome::Unbuilt);
        };
        let code: Vec<&str> = survivor
            .edits
            .iter()
            .map(|edit| edit.substitute.replacement.as_str())
            .collect();
        let changes = self.changes(file, mutant, held, &code)?;
        let put = |text: fn(&(PathBuf, String, String)) -> &String| {
            changes.iter().try_for_each(|change| {
                project::document::write(&change.0, text(change))
                    .map_err(|e| format!("cannot write {}: {e}", change.0.display()))
            })
        };
        put(|change| &change.1)?;
        let run = self.run(&held.package, &held.harnesses, Stop::AtFirstFailure);
        put(|change| &change.2)?;
        verdict(run, &held.harnesses)
    }

    /// Each file of the copy a mutant changes, with its text in the mutant and as it is: the
    /// mutant's file, and each crate root of its package that lacks what `code` needs.
    fn changes(
        &self,
        file: &str,
        mut mutant: String,
        held: &Held,
        code: &[&str],
    ) -> Result<Vec<(PathBuf, String, String)>, String> {
        let mut changes = Vec::new();
        for root in held
            .roots
            .iter()
            .filter(|root| root.as_path() != Path::new(file))
        {
            let path = self.copy.join(root);
            let was = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            if let Some(now) = crate_root::equipped(&was, code) {
                changes.push((path, now, was));
            }
        }
        if held
            .roots
            .iter()
            .any(|root| root.as_path() == Path::new(file))
        {
            mutant = crate_root::equipped(&mutant, code).unwrap_or(mutant);
        }
        changes.push((self.copy.join(file), mutant, held.original.clone()));
        Ok(changes)
    }

    /// One Kani job on `harnesses` of `package` in the copy, which ends where `stop` says.
    fn run(
        &self,
        package: &str,
        harnesses: &BTreeSet<String>,
        stop: Stop,
    ) -> Result<exec::Output, exec::ExecError> {
        let jobs = proof::jobs(exec::budget::cap(), self.ctx.proof_jobs).to_string();
        let named = harnesses
            .iter()
            .flat_map(|name| ["--harness", name.as_str()]);
        let mut then: Vec<&str> = named.collect();
        then.extend([
            "--exact",
            "--output-format",
            "terse",
            "--jobs",
            jobs.as_str(),
        ]);
        then.extend((stop == Stop::AtFirstFailure).then_some("--fail-fast"));
        let packages = [package.to_string()];
        let argv = proof::invocation(&packages, &self.ctx.features, &then);
        (self.kani)(&argv, self.copy, &self.target)
    }
}

/// A file a proof can kill mutations in: its package and the package's crate roots, its
/// harnesses that verify on it as it is, and its text.
struct Held {
    package: String,
    roots: Vec<PathBuf>,
    harnesses: BTreeSet<String>,
    original: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gates::mutation::survivors::{Edit, Location, Span, Substitute};
    use std::cell::RefCell;

    const MANIFEST: &str =
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n";
    const LIB: &str = "mod half;\nmod plain;\npub fn add(a: u8, b: u8) -> u8 {\n    a + b\n}\n";
    const HALF: &str = "pub fn half(x: u8) -> u8 {\n    x / 2\n}\n";
    const LISTING: &str = r#"{"standard-harnesses":{
        "src/lib.rs":["proofs::adds","proofs::broken"],"src/half.rs":["proofs::halves"]},
        "contract-harnesses":{}}"#;

    /// One run of the fake: the harnesses it was asked for, `--fail-fast`, and whether the crate
    /// root held `extern crate alloc` then.
    type Run = (Vec<String>, bool, bool);

    /// What one run of Kani gives back.
    type Ran = Result<exec::Output, exec::ExecError>;

    fn files() -> [(&'static str, &'static str); 4] {
        [
            ("Cargo.toml", MANIFEST),
            ("src/lib.rs", LIB),
            ("src/half.rs", HALF),
            ("src/plain.rs", "pub fn plain() {}\n"),
        ]
    }

    /// Kani as a test sees it: `list` writes the listing, and a run judges each harness by the
    /// files of the copy. `proofs::broken` fails on any text.
    fn fake(runs: &RefCell<Vec<Run>>) -> impl Fn(&[&str], &Path, &Path) -> Ran + '_ {
        move |argv, dir, _target| {
            if argv.contains(&"list") {
                std::fs::write(dir.join("kani-list.json"), LISTING).unwrap();
                return Ok(exec::Output::of(Some(0), "", ""));
            }
            let text = |file: &str| std::fs::read_to_string(dir.join(file)).unwrap();
            let (lib, half) = (text("src/lib.rs"), text("src/half.rs"));
            let names: Vec<String> = argv
                .windows(2)
                .filter(|pair| pair[0] == "--harness")
                .map(|pair| pair[1].to_string())
                .collect();
            let alloc = lib.contains("extern crate alloc");
            runs.borrow_mut()
                .push((names.clone(), argv.contains(&"--fail-fast"), alloc));
            let fails = |name: &str| match name {
                "proofs::adds" => lib.contains("a - b"),
                "proofs::halves" => half.contains("::alloc::"),
                _ => true,
            };
            let said: String = names
                .iter()
                .map(|name| {
                    if fails(name) {
                        format!(
                            "Checking harness {name}...\nSUMMARY:\n ** 1 of 2 failed\n\
                             Failed Checks: assertion failed\nVERIFICATION:- FAILED\n"
                        )
                    } else {
                        format!(
                            "Checking harness {name}...\nSUMMARY:\n ** 0 of 2 failed\n\
                             VERIFICATION:- SUCCESSFUL\n"
                        )
                    }
                })
                .collect();
            let code = i32::from(said.contains("FAILED"));
            Ok(exec::Output::of(Some(code), &said, ""))
        }
    }

    /// A survivor in `file` whose one edit puts `code` at `begin..end`, or with no edit.
    fn survivor(
        file: &str,
        line: u32,
        at: Option<((u32, u32), (u32, u32))>,
        code: &str,
    ) -> Survivor {
        let edits = at.map(|(begin, end)| Edit {
            location: Location {
                kind: "replace".to_string(),
                span: Span {
                    path: file.to_string(),
                    begin,
                    end,
                },
            },
            substitute: Substitute {
                kind: "expr".to_string(),
                replacement: code.to_string(),
            },
        });
        Survivor {
            operator: "math_op_swap".to_string(),
            file: file.to_string(),
            line,
            what: format!("put `{code}`"),
            edits: edits.into_iter().collect(),
        }
    }

    fn survivors() -> Vec<Survivor> {
        let sum = Some(((4, 5), (4, 10)));
        vec![
            survivor("src/lib.rs", 4, sum, "a - b"),
            survivor("src/lib.rs", 4, sum, "b + a"),
            survivor("src/lib.rs", 4, None, "nothing"),
            survivor(
                "src/half.rs",
                2,
                Some(((2, 5), (2, 10))),
                "::alloc::vec::Vec::<u8>::new().len() as u8",
            ),
            survivor("src/plain.rs", 1, Some(((1, 1), (1, 4))), "fn"),
            survivor("../outside.rs", 1, Some(((1, 1), (1, 2))), "x"),
        ]
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_proof_kills_what_it_fails_on_and_the_copy_gets_back_each_file_a_mutant_changed() {
        let ctx = crate::testdir::Held::tree("prover-kills", &files());
        let copy = crate::testdir::tree("prover-kills-copy", &files());
        let runs = RefCell::new(Vec::new());
        let kani = fake(&runs);
        let prover = Prover::listed(&ctx, &ctx.root, &copy, &kani).unwrap();
        let found = Joined {
            survivors: survivors(),
            ..Joined::default()
        };
        let (left, leads) = prover.tried(found).unwrap();
        let all = survivors();
        let kept = [&all[5], &all[1], &all[2], &all[4]].map(Clone::clone);
        assert_eq!(left.survivors, kept);
        let leads: Vec<String> = leads.iter().map(Finding::render).collect();
        let tally = "kani: proofs killed 2 of the 6 mutation(s) no test detected; 1 verified \
                     under each harness of their file; 2 are in a file no verified harness \
                     checks; 1 could not be rebuilt from mutest's edits";
        let wanted = [
            "src/half.rs:2: proofs::halves: no test detected put \
             `::alloc::vec::Vec::<u8>::new().len() as u8` (math_op_swap), and this proof did",
            "src/lib.rs:4: proofs::adds: no test detected put `a - b` (math_op_swap), and this \
             proof did",
            tally,
        ];
        assert_eq!(leads, wanted);
        let asked = |names: &[&str]| names.iter().map(|name| (*name).to_string()).collect();
        let wanted: Vec<Run> = vec![
            (asked(&["proofs::halves"]), false, false),
            (asked(&["proofs::halves"]), true, true),
            (asked(&["proofs::adds", "proofs::broken"]), false, false),
            (asked(&["proofs::adds"]), true, false),
            (asked(&["proofs::adds"]), true, false),
        ];
        assert_eq!(runs.take(), wanted);
        for (file, text) in files() {
            assert_eq!(
                std::fs::read_to_string(copy.join(file)).unwrap(),
                text,
                "{file}"
            );
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn with_the_setting_on_the_project_is_copied_tried_and_the_copy_removed() {
        let mut ctx = crate::testdir::Held::tree("prover-copied", &files());
        ctx.mutest_proofs = true;
        let dirs = RefCell::new(Vec::new());
        let runs = RefCell::new(Vec::new());
        let inner = fake(&runs);
        let kani = |argv: &[&str], dir: &Path, target: &Path| {
            dirs.borrow_mut().push(dir.to_path_buf());
            inner(argv, dir, target)
        };
        // The mutant's own file is the crate root, so the mutant itself takes what its code needs.
        let code = "a - b + ::alloc::vec::Vec::<u8>::new().len() as u8";
        let found = Joined {
            survivors: vec![survivor("src/lib.rs", 4, Some(((4, 5), (4, 10))), code)],
            ..Joined::default()
        };
        let (left, leads) = super::super::killed(&ctx, found, &kani).unwrap();
        assert_eq!(left.survivors, Vec::<Survivor>::new());
        let leads: Vec<String> = leads.iter().map(Finding::render).collect();
        let wanted = [
            format!(
                "src/lib.rs:4: proofs::adds: no test detected put `{code}` (math_op_swap), and \
                 this proof did"
            ),
            "kani: proofs killed 1 of the 1 mutation(s) no test detected".to_string(),
        ];
        assert_eq!(leads, wanted);
        let asked = |names: &[&str]| names.iter().map(|name| (*name).to_string()).collect();
        let wanted: Vec<Run> = vec![
            (asked(&["proofs::adds", "proofs::broken"]), false, false),
            (asked(&["proofs::adds"]), true, true),
        ];
        assert_eq!(runs.take(), wanted);
        let copy = crate::gates::proofs::patch::normalized(&ctx.root).join(super::super::SCRATCH);
        assert_eq!(
            dirs.into_inner(),
            [copy.clone(), copy.clone(), copy.clone()]
        );
        assert!(!copy.exists());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_listing_that_failed_or_was_cut_short_is_an_error_and_one_that_did_not_start_too() {
        let ctx = crate::testdir::Held::tree("prover-unlisted", &files());
        let said = "cargo kani could not completely list harnesses for mutest's proofs";
        let failed =
            |_: &[&str], _: &Path, _: &Path| -> Ran { Ok(exec::Output::of(Some(1), "", "no")) };
        let error = Prover::listed(&ctx, &ctx.root, &ctx.root, &failed)
            .err()
            .unwrap();
        assert!(error.starts_with(said), "{error}");
        let cut = |_: &[&str], _: &Path, _: &Path| -> Ran {
            let mut out = exec::Output::of(Some(0), "", "");
            out.truncated = true;
            Ok(out)
        };
        let error = Prover::listed(&ctx, &ctx.root, &ctx.root, &cut)
            .err()
            .unwrap();
        assert!(error.starts_with(said), "{error}");
        let gone = exec::ExecError::of("cargo", exec::Stage::Spawn, "gone");
        let unstarted = |_: &[&str], _: &Path, _: &Path| -> Ran { Err(gone.clone()) };
        let error = Prover::listed(&ctx, &ctx.root, &ctx.root, &unstarted).err();
        assert_eq!(error, Some(gone.to_string()));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_run_that_hung_on_the_file_as_it_is_leaves_it_unproved_and_one_that_did_not_start_is_an_error()
     {
        let ctx = crate::testdir::Held::tree("prover-hung", &files());
        let copy = crate::testdir::tree("prover-hung-copy", &files());
        let stage = std::cell::Cell::new(exec::Stage::Hung);
        let kani = |argv: &[&str], dir: &Path, _: &Path| -> Ran {
            if argv.contains(&"list") {
                std::fs::write(dir.join("kani-list.json"), LISTING).unwrap();
                return Ok(exec::Output::of(Some(0), "", ""));
            }
            Err(exec::ExecError::of("cargo", stage.get(), "gone"))
        };
        let prover = Prover::listed(&ctx, &ctx.root, &copy, &kani).unwrap();
        let found = || Joined {
            survivors: survivors()[..1].to_vec(),
            ..Joined::default()
        };
        let (left, leads) = prover.tried(found()).unwrap();
        assert_eq!(left.survivors, found().survivors);
        let tally = "kani: proofs killed 0 of the 1 mutation(s) no test detected; 1 are in a \
                     file no verified harness checks";
        assert_eq!(
            leads.iter().map(Finding::render).collect::<Vec<_>>(),
            [tally]
        );
        stage.set(exec::Stage::Spawn);
        let gone = exec::ExecError::of("cargo", exec::Stage::Spawn, "gone");
        assert_eq!(prover.tried(found()).err(), Some(gone.to_string()));
    }
}
