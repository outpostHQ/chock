//! The gate's run: TLC on each cfg, Apalache where `tla.engines` or an inductive invariant asks
//! for it, and TLAPS on each theorem; then the findings from what each one said.

use std::collections::BTreeMap;

use super::{
    Cfg, DIR, Engine, INDUCTIVE, SCRATCH, Said, Tlc, WORKERS, alone, apalache, asked, engines,
    java, judged, limit_of, no_trace_spec, read_tlc, relative, tlaps, tlc, uncovered, unnamed,
    with_tmp,
};
use crate::exec;
use crate::gates::proofs::{Run, cleared, made, run_all};
use crate::project::config::Tla;
use crate::run::report::Finding;
use crate::run::{Ctx, Outcome};

/// Reads what an engine said of a cfg from its run, within the run's limit in seconds.
type Read = fn(Result<exec::Output, exec::ExecError>, u64) -> Result<Said, String>;

/// Runs every engine on the cfgs and theorems `--model` asks for, and judges what each said.
pub(super) fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let settings = &ctx.tla;
    let dirs = settings
        .dirs
        .clone()
        .unwrap_or_else(|| vec![DIR.to_string()]);
    let (cfgs, mut findings) = asked(ctx, &dirs)?;
    let java = java(ctx)?;
    let jar = crate::setup::downloads::located(
        &ctx.root,
        ctx.tools.home.as_deref(),
        "TLA_TOOLS",
        "tla2tools*.jar",
    )?;
    let version = crate::setup::downloads::version(&ctx.root, "TLA_TOOLS");
    let traces = version.as_deref().and_then(no_trace_spec);
    let tool = Tlc { jar: &jar, traces };
    let split = engines::split(ctx, &cfgs)?;
    let apalache::Proofs {
        steps,
        runs: proofs,
        notes,
        refutes,
    } = apalache::proofs(ctx, &cfgs, &split)?;
    let scratch = ctx.root.join(SCRATCH);
    let workers = settings.workers.unwrap_or(WORKERS).max(1);
    let theorems = tlaps::theorems(ctx, &dirs, workers)?;
    if let Some(gone) = unknown(&ctx.scope.models, &cfgs, &theorems.modules) {
        let dirs = dirs.join(", ");
        return Err(format!(
            "`--model` names {gone}, and no cfg and no theorem under {dirs} is of that model"
        ));
    }
    let checked = cfgs.iter().enumerate();
    let checked = checked.filter(|(_, cfg)| split.engine(cfg) == Engine::Tlc);
    let checked = checked.map(|(n, cfg)| {
        let meta = scratch.join(format!("{n}-{}", cfg.stem));
        let limit = limit_of(settings, &cfg.stem, &cfg.model);
        let run = tlc(&tool, cfg, &meta, workers, limit);
        (n, (run, read_tlc as Read))
    });
    let refuted = refutes.into_iter();
    let refuted = refuted.map(|(n, run)| (n, (run, apalache::said as Read)));
    let jobs: BTreeMap<usize, (Run, Read)> = checked.chain(refuted).collect();
    let (readers, mut jobs): (Vec<_>, Vec<_>) = jobs
        .into_iter()
        .map(|(n, (run, reader))| ((n, reader, run.limit), run))
        .unzip();
    jobs.extend(proofs);
    with_tmp(&mut jobs, &scratch)?;
    let lanes = lanes(exec::budget::cap(), workers);
    let outs = run_all(&ctx.tools, &java, &jobs, lanes);
    let caches = scratch.join(tlaps::CACHES);
    made(&caches).map_err(|e| format!("cannot make {}: {e}", caches.display()))?;
    let proved = run_all(&ctx.tools, &theorems.program, &theorems.runs, lanes);
    cleared(&scratch)?;
    let mut outs = outs.into_iter();
    let read = readers.into_iter().zip(outs.by_ref());
    let read = read.map(|((n, reader, limit), out)| reader(out, limit).map(|said| (n, said)));
    let mut read: BTreeMap<usize, Said> = read.collect::<Result<_, String>>()?;
    // The own cfg of a model that Apalache alone checks runs nothing: the proof steps judge it.
    let said: Vec<Said> = cfgs
        .iter()
        .enumerate()
        .map(|(n, _)| read.remove(&n).unwrap_or(Said::Passed))
        .collect();
    let wrong = cfgs.iter().zip(&said).flat_map(|(cfg, said)| {
        let file = relative(&ctx.root, &cfg.dir.join(cfg.file()));
        let why = judged(split.engine(cfg), cfg, said);
        why.map(|why| Finding::at(&file, &why).item(&cfg.model))
    });
    findings.extend(wrong);
    let inductive = |model: &str| inductive_of(settings, model);
    let asks = unnamed(&cfgs, &said, &split, &inductive);
    let (also, undecided) = alone(ctx, &java, asks, lanes)?;
    findings.extend(undecided);
    findings.extend(uncovered(&ctx.root, &cfgs, &said, &also, &inductive));
    let stepped = steps.iter().zip(outs).map(|(step, out)| step.judged(out));
    let stepped: Vec<Option<Finding>> = stepped.collect::<Result<_, String>>()?;
    findings.extend(stepped.into_iter().flatten());
    findings.extend(tlaps::judged_each(&theorems.modules, proved)?);
    Ok(match findings.is_empty() {
        true => Outcome::noted(notes.into_iter().chain(theorems.notes).collect()),
        false => Outcome::failed(findings),
    })
}

/// The first model `--model` names that no cfg checks and no module of that name proves.
fn unknown<'a>(named: &'a [String], cfgs: &[Cfg], modules: &[tlaps::Module]) -> Option<&'a String> {
    let checked = |model: &&String| cfgs.iter().any(|cfg| &cfg.model == *model);
    let proved = |model: &&String| modules.iter().any(|it| &it.model == *model);
    named.iter().find(|model| !checked(model) && !proved(model))
}

/// The runs that go at once: the run's budget `cap`, shared by the TLC workers each run starts.
pub(super) fn lanes(cap: usize, workers: u32) -> usize {
    cap / usize::try_from(workers).unwrap_or(1)
}

/// The inductive invariant of `model`: the one `tla.inductive` names, else `IndInv`.
fn inductive_of(settings: &Tla, model: &str) -> String {
    let named = settings.inductive.as_ref().and_then(|map| map.get(model));
    named.map_or(INDUCTIVE, String::as_str).to_string()
}
