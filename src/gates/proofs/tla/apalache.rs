//! The Apalache half of the `tla` gate: each inductive invariant holds in `Init`, one step keeps
//! it, and it implies each invariant that the cfg lists.

use super::{Cfg, INDUCTIVE, SCRATCH, limit_of, relative};
use crate::exec;
use crate::gates::proofs::Run;
use crate::run::Ctx;
use crate::run::report::Finding;

/// One Apalache check: the cfg it reads, and what its failure means.
pub(super) struct Step {
    file: String,
    model: String,
    fails: String,
    limit: u64,
}

impl Step {
    /// Exit 0 is Apalache's `NoError`; 12 is a counterexample; anything else is no verdict.
    pub(super) fn judged(
        &self,
        out: Result<exec::Output, exec::ExecError>,
    ) -> Result<Option<Finding>, String> {
        let why = match out {
            Ok(out) if out.code == Some(0) => return Ok(None),
            Ok(out) if out.code == Some(12) => self.fails.clone(),
            Ok(out) => {
                let text = format!("{}\n{}", out.stdout, out.stderr);
                format!(
                    "Apalache could not check it: {}",
                    apalache_error(&text, out.code)
                )
            }
            Err(e) if e.hung() => format!("Apalache ran past its {} s limit", self.limit),
            Err(e) => return Err(e.to_string()),
        };
        Ok(Some(Finding::at(&self.file, &why).item(&self.model)))
    }
}

/// The Apalache checks of a run: each step, the run that gives its verdict, and the notes.
#[derive(Default)]
pub(super) struct Proofs {
    pub(super) steps: Vec<Step>,
    pub(super) runs: Vec<Run>,
    pub(super) notes: Vec<Finding>,
}

pub(super) const UNPINNED: &str = "Apalache is not pinned in tool-versions.env, so no invariant \
                                   was proved inductive; run `chock init --local` to pin it";

/// The encodings Apalache's `--smt-encoding` takes.
const ENCODINGS: [&str; 3] = ["oopsla19", "arrays", "funArrays"];

/// `Err` names the first model that `tla.smt_encoding` gives an encoding Apalache does not take.
fn encodings_known(settings: &crate::project::config::Tla) -> Result<(), String> {
    let known = |encoding: &String| ENCODINGS.contains(&encoding.as_str());
    match settings.smt_encoding.iter().find(|(_, it)| !known(it)) {
        Some((model, it)) => Err(format!(
            "`tla.smt_encoding` gives {model} `{it}`, which Apalache does not take; it takes {}",
            ENCODINGS.join(", ")
        )),
        None => Ok(()),
    }
}

/// The Apalache checks of each model that has an inductive invariant: `Init` establishes it, one
/// step keeps it, and it implies every invariant the cfg lists. `length` adds a bounded run.
pub(super) fn proofs(ctx: &Ctx, cfgs: &[Cfg]) -> Result<Proofs, String> {
    let settings = &ctx.tla;
    encodings_known(settings)?;
    let lists = |cfg: &&Cfg, name: &str| cfg.listed.invariants.iter().any(|held| held == name);
    let models: Vec<(&Cfg, String)> = match &settings.inductive {
        Some(map) => map
            .iter()
            .filter(|(model, _)| ctx.scope.asks_model(model))
            .map(|(model, name)| {
                let own = cfgs.iter().find(|cfg| cfg.is_own() && cfg.model == *model);
                own.map(|cfg| (cfg, name.clone())).ok_or_else(|| {
                    format!("`tla.inductive` names {model}, and no {model}.cfg is under the model directories")
                })
            })
            .collect::<Result<_, _>>()?,
        None => cfgs
            .iter()
            .filter(|cfg| cfg.is_own() && lists(cfg, INDUCTIVE))
            .map(|cfg| (cfg, INDUCTIVE.to_string()))
            .collect(),
    };
    if models.is_empty() {
        return Ok(Proofs::default());
    }
    let jar = match crate::setup::downloads::located(
        &ctx.root,
        ctx.tools.home.as_deref(),
        "APALACHE",
        "apalache.jar",
    ) {
        Ok(jar) => jar,
        Err(why)
            if why.ends_with(crate::setup::downloads::NOT_PINNED)
                && settings.inductive.is_none() =>
        {
            let notes = vec![Finding::at(crate::project::PIN_FILE, UNPINNED)];
            return Ok(Proofs {
                notes,
                ..Proofs::default()
            });
        }
        Err(why) => return Err(why),
    };
    let scratch = ctx.root.join(SCRATCH).join("apalache");
    let mut steps = Vec::new();
    let mut jobs = Vec::new();
    for (n, (cfg, name)) in models.into_iter().enumerate() {
        let limit = limit_of(settings, &cfg.stem, &cfg.model);
        let file = relative(&ctx.root, &cfg.dir.join(cfg.file()));
        let checks = [
            (
                vec![format!("--inv={name}"), "--length=0".into()],
                format!("Init does not establish {name}"),
            ),
            (
                vec![
                    format!("--init={name}"),
                    format!("--inv={name}"),
                    "--length=1".into(),
                ],
                format!(
                    "{name} is not inductive: one step from a state that holds it reaches one that does not"
                ),
            ),
            (
                vec![format!("--init={name}"), "--length=0".into()],
                format!("{name} does not imply every invariant {} lists", cfg.file()),
            ),
        ];
        let bounded = settings.length.filter(|steps| *steps > 0).map(|steps| {
            let fails = format!(
                "an invariant {} lists fails within {steps} steps of Init",
                cfg.file()
            );
            (vec![format!("--length={steps}")], fails)
        });
        for (k, (args, fails)) in checks.into_iter().chain(bounded).enumerate() {
            let out = scratch
                .join(format!("{n}-{k}"))
                .to_string_lossy()
                .into_owned();
            let mut argv: Vec<String> = ["-Xmx4g", "-jar"].map(String::from).to_vec();
            argv.push(jar.to_string_lossy().into_owned());
            argv.extend([
                "check".into(),
                format!("--config={}", cfg.file()),
                format!("--out-dir={out}"),
            ]);
            let encoding = settings.smt_encoding.get(&cfg.model);
            argv.extend(encoding.map(|it| format!("--smt-encoding={it}")));
            argv.extend(args);
            argv.push(format!("{}.tla", cfg.model));
            jobs.push(Run {
                dir: cfg.dir.clone(),
                argv,
                limit,
            });
            steps.push(Step {
                file: file.clone(),
                model: cfg.model.clone(),
                fails,
                limit,
            });
        }
    }
    Ok(Proofs {
        steps,
        runs: jobs,
        notes: Vec::new(),
    })
}

/// Apalache marks an error line with `E@` and its time; the line, without the time.
fn apalache_error(text: &str, code: Option<i32>) -> String {
    let exit = code.map_or("a signal".to_string(), |code| format!("exit {code}"));
    if text.contains("UnsupportedClassVersionError") {
        return format!("{exit}: Apalache needs Java 21 or newer; set JAVA_HOME to one");
    }
    let line = text
        .lines()
        .find(|line| line.contains(" E@") || line.starts_with("Error"))
        .or_else(|| text.lines().rev().find(|line| !line.trim().is_empty()))
        .unwrap_or("no output");
    let line = line.split(" E@").next().unwrap_or(line).trim();
    format!("{exit}: {}", line.chars().take(240).collect::<String>())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn apalache_names_its_error_line_without_the_time() {
        let text = "PASS #1: TypeCheckerSnowcat   I@12:16:08.483\n\
                    Typing input error: Expected a type annotation for VARIABLE x   E@12:16:08.6\n";
        let typing = "exit 255: Typing input error: Expected a type annotation for VARIABLE x";
        assert_eq!(apalache_error(text, Some(255)), typing);
        let old = "Error: LinkageError occurred\njava.lang.UnsupportedClassVersionError: at/forsyte/apalache/tla/Tool\n";
        let java = "exit 1: Apalache needs Java 21 or newer; set JAVA_HOME to one";
        assert_eq!(apalache_error(old, Some(1)), java);
    }

    #[test]
    fn a_step_past_its_limit_fails_and_one_that_did_not_start_is_an_error() {
        let error = |stage| exec::ExecError::of("java", stage, "gone");
        let step = Step {
            file: "M.cfg".into(),
            model: "M".into(),
            fails: "fails".into(),
            limit: 7,
        };
        let hung = step.judged(Err(error(exec::Stage::Hung))).unwrap().unwrap();
        assert_eq!(hung.message, "Apalache ran past its 7 s limit");
        let lost = error(exec::Stage::Spawn).to_string();
        assert_eq!(
            step.judged(Err(error(exec::Stage::Spawn))).unwrap_err(),
            lost
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn each_check_of_a_model_uses_the_smt_encoding_set_for_it() {
        let tree = [
            ("proofs/tla/Spec.tla", "IndInv == TRUE\n"),
            ("proofs/tla/Spec.cfg", "INVARIANT IndInv\n"),
            ("proofs/tla/Other.tla", "IndInv == TRUE\n"),
            ("proofs/tla/Other.cfg", "INVARIANT IndInv\n"),
        ];
        let mut ctx = crate::testdir::Held::tree("apalache-encoding", &tree);
        let pin = crate::testdir::fetched(&ctx.root, "APALACHE", "apalache.jar");
        std::fs::write(ctx.root.join(crate::project::PIN_FILE), pin).unwrap();
        ctx.tools.home = Some(ctx.root.join("tools"));
        let set = r#"{"smt_encoding": {"Spec": "funArrays"}}"#;
        ctx.tla = serde_json::from_str(set).unwrap();
        let (cfgs, _) = super::super::discovered(&ctx.root, &["proofs/tla".to_string()]);
        let runs = proofs(&ctx, &cfgs).unwrap().runs;
        let said: BTreeSet<(&str, Option<&str>)> = runs
            .iter()
            .map(|run| {
                let flag = run.argv.iter().find(|it| it.starts_with("--smt-encoding="));
                (run.argv.last().unwrap().as_str(), flag.map(String::as_str))
            })
            .collect();
        let spec = ("Spec.tla", Some("--smt-encoding=funArrays"));
        assert_eq!(said, BTreeSet::from([("Other.tla", None), spec]));
        let unknown = r#"{"smt_encoding": {"Spec": "bitvectors"}}"#;
        ctx.tla = serde_json::from_str(unknown).unwrap();
        let refused = "`tla.smt_encoding` gives Spec `bitvectors`, which Apalache does not take; \
                       it takes oopsla19, arrays, funArrays";
        assert_eq!(encodings_known(&ctx.tla).unwrap_err(), refused);
        assert_eq!(proofs(&ctx, &cfgs).err(), Some(refused.to_string()));
    }
}
