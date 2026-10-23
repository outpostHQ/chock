//! `tla.engines`: the engines that check the cfgs of a model. The gate gives a model Apalache
//! alone only where Apalache decides each of its cfgs.

use std::collections::BTreeSet;

use super::{Cfg, Engine, Expect, INDUCTIVE};
use crate::project::config::Tla;
use crate::run::Ctx;

/// The engines that check a model's cfgs; both when `tla.engines` leaves the model out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub(super) struct Engines {
    tlc: bool,
    apalache: bool,
}

/// What the choice of engines reads from one cfg.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
struct Shape {
    own: bool,
    expect: Expect,
    /// The cfg lists a `PROPERTY`.
    property: bool,
    /// TLC checks the cfg for a deadlock: it has no `CHECK_DEADLOCK FALSE`.
    deadlock: bool,
}

impl Shape {
    fn of(cfg: &Cfg) -> Self {
        Self {
            own: cfg.is_own(),
            expect: cfg.expect,
            property: !cfg.listed.properties.is_empty(),
            deadlock: !cfg.listed.no_deadlock,
        }
    }
}

/// Why the engines that `tla.engines` names cannot check a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    NoEngine,
    NotInductive,
    Property,
    Deadlock,
    Verdict,
}

/// `Some` says why `engines` cannot check a model whose cfgs have these shapes, and which cfg is
/// the cause. `inductive` says whether Apalache proves an inductive invariant of the model.
fn refused(engines: Engines, inductive: bool, shapes: &[Shape]) -> Option<(Refusal, usize)> {
    if engines.tlc {
        return None;
    }
    if !engines.apalache {
        return Some((Refusal::NoEngine, 0));
    }
    if !inductive {
        return Some((Refusal::NotInductive, 0));
    }
    let first = |cause: &dyn Fn(&Shape) -> bool| shapes.iter().position(cause);
    let deadlock = |shape: &Shape| shape.deadlock || shape.expect == Expect::Deadlock;
    let verdict = |shape: &Shape| shape.own != (shape.expect == Expect::Pass);
    let property = first(&|shape| shape.property).map(|at| (Refusal::Property, at));
    property
        .or_else(|| first(&deadlock).map(|at| (Refusal::Deadlock, at)))
        .or_else(|| first(&verdict).map(|at| (Refusal::Verdict, at)))
}

/// Whether Apalache proves an inductive invariant of the model whose own cfg is `own`.
pub(super) fn inductive(settings: &Tla, own: &Cfg) -> bool {
    match &settings.inductive {
        Some(map) => map.contains_key(&own.model),
        None => own.listed.invariants.iter().any(|name| name == INDUCTIVE),
    }
}

/// The models that `tla.engines` gives one engine alone.
#[derive(Debug, Default)]
pub(super) struct Split {
    pub(super) apalache: BTreeSet<String>,
    pub(super) tlc: BTreeSet<String>,
}

impl Split {
    /// The engine that gives `cfg` its verdict: Apalache where `tla.engines` gives its model
    /// Apalache alone, else TLC.
    pub(super) fn engine(&self, cfg: &Cfg) -> Engine {
        if self.apalache.contains(&cfg.model) {
            Engine::Apalache
        } else {
            Engine::Tlc
        }
    }
}

/// `Err` names a model that the engines `tla.engines` names cannot check, and why.
pub(super) fn split(ctx: &Ctx, cfgs: &[Cfg]) -> Result<Split, String> {
    let settings = &ctx.tla;
    let mut split = Split::default();
    let asked = settings.engines.iter();
    for (model, names) in asked.filter(|(model, _)| ctx.scope.asks_model(model)) {
        let known = |name: &&String| matches!(name.as_str(), "tlc" | "apalache");
        if let Some(other) = names.iter().find(|name| !known(name)) {
            return Err(format!(
                "`tla.engines` gives {model} `{other}`, and the engines are tlc and apalache"
            ));
        }
        let has = |engine: &str| names.iter().any(|name| name == engine);
        let engines = Engines {
            tlc: has("tlc"),
            apalache: has("apalache"),
        };
        let kin: Vec<&Cfg> = cfgs.iter().filter(|cfg| cfg.model == *model).collect();
        if kin.is_empty() {
            return Err(format!(
                "`tla.engines` names {model}, and no cfg under the model directories is of it"
            ));
        }
        let own = kin.iter().find(|cfg| cfg.is_own());
        let proved = own.is_some_and(|own| inductive(settings, own));
        let shapes: Vec<Shape> = kin.iter().map(|cfg| Shape::of(cfg)).collect();
        if let Some((why, at)) = refused(engines, proved, &shapes) {
            let cfg = kin.get(at).copied();
            return Err(refusal(model, why, cfg));
        }
        if engines.tlc != engines.apalache {
            let alone = if engines.tlc {
                &mut split.tlc
            } else {
                &mut split.apalache
            };
            alone.insert(model.clone());
        }
    }
    Ok(split)
}

fn refusal(model: &str, why: Refusal, cfg: Option<&Cfg>) -> String {
    let file = cfg.map(Cfg::file).unwrap_or_default();
    let reason = match why {
        Refusal::NoEngine => {
            return format!("`tla.engines` gives {model} no engine; give it tlc, apalache or both");
        }
        Refusal::NotInductive => format!(
            "Apalache proves no inductive invariant of it: {model}.cfg lists no {INDUCTIVE}, or `tla.inductive` leaves {model} out"
        ),
        Refusal::Property => format!("{file} lists a PROPERTY, which only TLC checks"),
        Refusal::Deadlock => format!(
            "{file} checks for a deadlock, which only TLC checks here; write `CHECK_DEADLOCK FALSE` in it"
        ),
        Refusal::Verdict => {
            let must = if cfg.is_some_and(Cfg::is_own) {
                "fail"
            } else {
                "pass"
            };
            format!(
                "{file} must {must}, and Apalache shows only that {model}.cfg passes and that each other cfg fails"
            )
        }
    };
    format!("`tla.engines` gives {model} only apalache, and {reason}")
}

/// Kani proofs over every engine choice and every cfg shape of a model with three cfgs; only
/// `cargo kani` builds them.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// What Apalache decides: the own cfg passes, by the inductive proof, and a variant fails, by
    /// a bounded run. It checks no `PROPERTY` and no deadlock. TLC decides each cfg.
    fn apalache_decides(inductive: bool, shape: Shape) -> bool {
        let verdict = match (shape.own, shape.expect) {
            (true, Expect::Pass) => inductive,
            (false, Expect::Violation) => true,
            _ => false,
        };
        verdict && !shape.property && !shape.deadlock
    }

    /// The gate gives no cfg to an engine that cannot decide it.
    #[kani::proof]
    #[kani::unwind(4)]
    fn no_cfg_goes_to_an_engine_that_cannot_decide_it() {
        let engines: Engines = kani::any();
        let inductive: bool = kani::any();
        let shapes: [Shape; 3] = kani::any();
        if refused(engines, inductive, &shapes).is_none() {
            let decided = |shape: &Shape| apalache_decides(inductive, *shape);
            assert!(engines.tlc || (engines.apalache && shapes.iter().all(decided)));
        }
    }

    /// The converse: the gate refuses no model with an inductive invariant that Apalache decides
    /// whole.
    #[kani::proof]
    #[kani::unwind(4)]
    fn a_model_apalache_decides_whole_is_not_refused() {
        let shapes: [Shape; 3] = kani::any();
        let alone = Engines {
            tlc: false,
            apalache: true,
        };
        if shapes.iter().all(|shape| apalache_decides(true, *shape)) {
            assert!(refused(alone, true, &shapes).is_none());
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::super::tests::cfg;
    use super::*;
    use crate::testdir::Held;

    fn split_by(ctx: &mut Held, engines: &str, cfgs: &[Cfg]) -> Result<Split, String> {
        ctx.tla = serde_json::from_str(&format!(r#"{{"engines": {engines}}}"#)).unwrap();
        split(ctx, cfgs)
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn apalache_alone_checks_a_model_only_where_it_decides_each_cfg() {
        let mut ctx = Held::tree("tla-engines-alone", &[]);
        let own = "INVARIANT IndInv Safe\nCHECK_DEADLOCK FALSE\n";
        let model = |bad: &str| vec![cfg("M", "M", own), cfg("M_bad", "M", bad)];
        let apalache = r#"{"M": ["apalache"]}"#;
        let safe = "INVARIANT Safe\nCHECK_DEADLOCK FALSE\n";
        let fine = split_by(&mut ctx, apalache, &model(safe)).unwrap();
        assert_eq!(fine.apalache, BTreeSet::from(["M".to_string()]));
        assert!(fine.tlc.is_empty());
        let only = "`tla.engines` gives M only apalache, and";
        let mut refused = |cfgs: &[Cfg]| split_by(&mut ctx, apalache, cfgs).unwrap_err();
        let property = model("INVARIANT Safe\nPROPERTY Live\nCHECK_DEADLOCK FALSE\n");
        let why = format!("{only} M_bad.cfg lists a PROPERTY, which only TLC checks");
        assert_eq!(refused(&property), why);
        let why = format!(
            "{only} M_bad.cfg checks for a deadlock, which only TLC checks here; write \
             `CHECK_DEADLOCK FALSE` in it"
        );
        assert_eq!(refused(&model("INVARIANT Safe\n")), why);
        let passes = model("\\* expect: pass\nINVARIANT Safe\nCHECK_DEADLOCK FALSE\n");
        let why = format!(
            "{only} M_bad.cfg must pass, and Apalache shows only that M.cfg passes and that each \
             other cfg fails"
        );
        assert_eq!(refused(&passes), why);
        let guessed = [cfg("M", "M", safe)];
        let why = format!(
            "{only} Apalache proves no inductive invariant of it: M.cfg lists no IndInv, or \
             `tla.inductive` leaves M out"
        );
        assert_eq!(refused(&guessed), why);
        let fails = [cfg("M", "M", &format!("\\* expect: violation\n{own}"))];
        let why = format!(
            "{only} M.cfg must fail, and Apalache shows only that M.cfg passes and that each \
             other cfg fails"
        );
        assert_eq!(refused(&fails), why);
        let named = r#"{"engines": {"M": ["apalache"]}, "inductive": {"M": "Safe"}}"#;
        ctx.tla = serde_json::from_str(named).unwrap();
        let proved = split(&ctx, &guessed).unwrap();
        assert_eq!(proved.apalache, BTreeSet::from(["M".to_string()]));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn tla_engines_names_known_engines_of_known_models_and_left_out_means_both() {
        let mut ctx = Held::tree("tla-engines-named", &[]);
        let cfgs = [cfg("M", "M", "INVARIANT Safe\nPROPERTY Live\n")];
        let tlc = split_by(&mut ctx, r#"{"M": ["tlc"]}"#, &cfgs).unwrap();
        assert_eq!(tlc.tlc, BTreeSet::from(["M".to_string()]));
        assert!(tlc.apalache.is_empty());
        for both in [r#"{"M": ["tlc", "apalache"]}"#, "{}"] {
            let split = split_by(&mut ctx, both, &cfgs).unwrap();
            assert!(split.tlc.is_empty() && split.apalache.is_empty(), "{both}");
        }
        let none = "`tla.engines` gives M no engine; give it tlc, apalache or both";
        assert_eq!(split_by(&mut ctx, r#"{"M": []}"#, &cfgs).unwrap_err(), none);
        let spin = "`tla.engines` gives M `spin`, and the engines are tlc and apalache";
        assert_eq!(
            split_by(&mut ctx, r#"{"M": ["spin"]}"#, &cfgs).unwrap_err(),
            spin
        );
        let gone = "`tla.engines` names N, and no cfg under the model directories is of it";
        assert_eq!(
            split_by(&mut ctx, r#"{"N": ["tlc"]}"#, &cfgs).unwrap_err(),
            gone
        );
        ctx.scope.models = vec!["M".into()];
        assert!(split_by(&mut ctx, r#"{"N": ["tlc"]}"#, &cfgs).is_ok());
    }
}
