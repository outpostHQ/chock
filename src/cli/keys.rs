//! `chock baseline GATE... --keys KEY...` and `chock baseline [GATE...] --moved OLD=NEW...`: the
//! words each takes, and the keys each writes.

use std::process::ExitCode;

use super::{cannot_run, emit, unit_of};
use crate::project::config::Config;
use crate::run::{self, baseline::Baseline};

/// What `chock baseline` was asked: the gates, `--lower`, and the keys after `--keys`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Recording<'a> {
    pub(super) names: Vec<&'a str>,
    pub(super) lower: bool,
    pub(super) keys: Option<Vec<&'a str>>,
}

/// Every word after `--keys` is a key, since a key may look like anything else on the line.
pub(super) fn recording<'a>(asked: &[&'a str]) -> Result<Recording<'a>, String> {
    let mut parts = asked.splitn(2, |word| *word == "--keys");
    let before = parts.next().unwrap_or_default();
    let keys = parts.next().map(<[&str]>::to_vec);
    let lower = before.contains(&"--lower");
    let names: Vec<&str> = before.iter().copied().filter(|a| *a != "--lower").collect();
    let crap = crate::gates::coverage::crap::GATE.name;
    let refused = match &keys {
        Some(keys) if keys.is_empty() => "`--keys` names no key: put each key to write after it",
        Some(_) if lower => "`--keys` never raises a key, so it takes no `--lower`",
        Some(_) if names.is_empty() => {
            "`--keys` needs the gates whose records it writes: `chock baseline GATE... --keys KEY...`"
        }
        Some(_) if names.contains(&crap) => {
            "`crap` keeps its own record file, which `--keys` does not write"
        }
        _ => return Ok(Recording { names, lower, keys }),
    };
    Err(refused.to_string())
}

/// The request and the gates it records: the gates it names, else every gate that keeps a record.
pub(super) fn chosen<'a>(
    asked: &[&'a str],
    config: Option<&Config>,
) -> Result<(Recording<'a>, Vec<&'static run::Gate>), String> {
    let asked = recording(asked)?;
    let gates = match asked.names.is_empty() {
        true => super::recordable(config),
        false => super::select(&asked.names, config)?,
    };
    Ok((asked, gates))
}

/// Without `--keys`, each gate's whole record, and the gates that could not measure. With it, see
/// [`keys_written`].
pub(super) fn written(
    asked: &Recording,
    chosen: &[&run::Gate],
    ctx: &run::Ctx,
    into: &mut Baseline,
) -> Result<Vec<&'static str>, ExitCode> {
    let Some(keys) = &asked.keys else {
        let failed = chosen
            .iter()
            .filter(|gate| !super::record_one(gate, ctx, into, asked.lower));
        return Ok(failed.map(|gate| gate.name).collect());
    };
    keys_written(chosen, ctx, into, keys)
}

/// `--keys` writes each key into every named gate that measures or records it, or writes nothing:
/// a key no gate knows, a gate that cannot measure, or a key that would go up stops it.
fn keys_written(
    chosen: &[&run::Gate],
    ctx: &run::Ctx,
    into: &mut Baseline,
    keys: &[&str],
) -> Result<Vec<&'static str>, ExitCode> {
    let mut known = std::collections::BTreeSet::new();
    let mut refused = Vec::new();
    for gate in chosen {
        let series = match run::measured(gate, ctx) {
            Ok(read) => read.series,
            Err(reason) => return Err(cannot_run(&format!("{}: {reason}", gate.name))),
        };
        let held = into.gate(gate.name);
        let mine: Vec<&str> = keys
            .iter()
            .copied()
            .filter(|key| series.get(key).or(held.get(key)).is_some())
            .collect();
        known.extend(mine.iter().copied());
        match into.write_keys(gate.name, unit_of(gate), &series, &mine) {
            Ok(()) => emit(&format!(
                "  {:<9} {:<12} {}\n",
                "wrote",
                gate.name,
                run::report::plural(mine.len(), "key")
            )),
            Err(why) => refused.extend(why.into_iter().map(|it| format!("{}: {it}", gate.name))),
        }
    }
    let unknown: Vec<&str> = keys
        .iter()
        .copied()
        .filter(|key| !known.contains(key))
        .collect();
    if !unknown.is_empty() {
        let names = unknown.join(", ");
        return Err(cannot_run(&format!(
            "no gate named here measures or records {names}"
        )));
    }
    if refused.is_empty() {
        return Ok(Vec::new());
    }
    eprintln!("chock: `--keys` never raises a key, so it wrote nothing; these went up:");
    refused.iter().for_each(|it| eprintln!("  {it}"));
    Err(ExitCode::from(1))
}

/// What `chock baseline [GATE...] --moved OLD=NEW...` was asked: the gates, and each moved file.
pub(super) type Moves<'a> = (Vec<&'a str>, Vec<(&'a str, &'a str)>);

/// The request of `--moved`, or `None` where the words hold none. Every word after it is a pair.
pub(super) fn moves<'a>(asked: &[&'a str]) -> Option<Result<Moves<'a>, String>> {
    let at = asked.iter().position(|word| *word == "--moved")?;
    let (names, rest) = asked.split_at(at);
    let refused = |why: &str| Some(Err(why.to_string()));
    if names.iter().any(|name| name.starts_with("--")) {
        return refused(
            "`--moved` takes no other flag: `chock baseline [GATE...] --moved OLD=NEW...`",
        );
    }
    if names.contains(&crate::gates::coverage::crap::GATE.name) {
        return refused("`crap` keeps its own record file, which `--moved` does not write");
    }
    let pair = |word: &&'a str| {
        let (old, new) = word.split_once('=')?;
        (!old.is_empty() && !new.is_empty() && old != new).then_some((old, new))
    };
    let pairs: Option<Vec<(&str, &str)>> = rest.iter().skip(1).map(pair).collect();
    match pairs {
        Some(pairs) if !pairs.is_empty() => Some(Ok((names.to_vec(), pairs))),
        _ => refused("`--moved` takes each moved file as OLD=NEW, two different paths"),
    }
}

/// Renames the keys of each moved file in `into`, or writes nothing: the old path must be gone
/// from the tree, the new one a file in it, and some record named here must hold the old one.
pub(super) fn moved(
    (names, pairs): &Moves,
    ctx: &run::Ctx,
    config: Option<&Config>,
    into: &mut Baseline,
) -> Result<(), String> {
    if !names.is_empty() {
        super::select(names, config)?;
    }
    let mut record = into.clone();
    for (old, new) in pairs {
        if ctx.root.join(old).exists() {
            return Err(format!("{old} is still in the tree, so it did not move"));
        }
        if !ctx.root.join(new).is_file() {
            return Err(format!("{new} is not a file in the tree"));
        }
        let keys = match record.moved_file(names, old, new)? {
            0 => return Err(format!("no record named here holds a key of {old}")),
            keys => run::report::plural(keys, "key"),
        };
        emit(&format!("  {:<9} {old} to {new}: {keys}\n", "moved"));
    }
    *into = record;
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::run::baseline::Series;

    #[test]
    fn every_word_after_keys_is_a_key_and_a_request_that_cannot_hold_is_refused() {
        let asked = recording(&["admits", "--keys", "A.lean#sorry", "--lower", "lean"]).unwrap();
        let keys = Some(vec!["A.lean#sorry", "--lower", "lean"]);
        assert_eq!(
            asked,
            Recording {
                names: vec!["admits"],
                lower: false,
                keys
            }
        );
        let asked = recording(&["--lower", "slop"]).unwrap();
        assert_eq!(
            asked,
            Recording {
                names: vec!["slop"],
                lower: true,
                keys: None
            }
        );
        for (words, says) in [
            (&["admits", "--keys"][..], "names no key"),
            (&["--lower", "admits", "--keys", "k"], "no `--lower`"),
            (&["--keys", "k"], "needs the gates"),
            (&["crap", "--keys", "k"], "own record file"),
        ] {
            assert!(recording(words).unwrap_err().contains(says), "{words:?}");
        }
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn each_key_a_gate_knows_is_written_and_an_unknown_key_or_a_raise_writes_nothing() {
        let gate = &crate::gates::metrics::admits::GATE;
        let ctx = crate::testdir::Held::tree("cli-keys", &[("B.lean", "def b := sorry\n")]);
        let mut held = Baseline::empty("0.1.0");
        let mut was = Series::new();
        was.set("A.lean#sorry", 1);
        held.record(gate.name, unit_of(gate), was);
        let wrote = keys_written(&[gate], &ctx, &mut held, &["B.lean#sorry", "A.lean#sorry"]);
        assert_eq!(wrote, Ok(Vec::new()));
        let now = [("B.lean#sorry".to_string(), 1)];
        assert_eq!(held.gate(gate.name).0.into_iter().collect::<Vec<_>>(), now);
        let unknown = keys_written(&[gate], &ctx, &mut held, &["C.lean#sorry"]);
        assert_eq!(unknown, Err(ExitCode::from(2)));
        let mut lower = Series::new();
        lower.set("B.lean#sorry", 0);
        held.set(gate.name, lower.clone());
        let raised = keys_written(&[gate], &ctx, &mut held, &["B.lean#sorry"]);
        assert_eq!(raised, Err(ExitCode::from(1)));
        assert_eq!(held.gate(gate.name), lower);
    }

    #[test]
    fn the_request_names_its_gates_or_takes_every_gate_that_keeps_a_record() {
        let (asked, gates) = chosen(&["admits", "--keys", "A.lean#sorry"], None).unwrap();
        assert_eq!(asked.keys, Some(vec!["A.lean#sorry"]));
        let names: Vec<&str> = gates.iter().map(|gate| gate.name).collect();
        assert_eq!(names, ["admits"]);
        let (_, every) = chosen(&[], None).unwrap();
        let named = |gates: Vec<&run::Gate>| gates.iter().map(|gate| gate.name).collect::<Vec<_>>();
        assert_eq!(named(every), named(super::super::recordable(None)));
        assert!(chosen(&["no-such-gate"], None).is_err());
        assert!(chosen(&["admits", "--keys"], None).is_err());
    }

    #[test]
    fn every_word_after_moved_is_a_moved_file_and_a_request_that_cannot_hold_is_refused() {
        let asked = moves(&["slop", "--moved", "a.rs=b.rs", "c.rs=d/c.rs"]);
        let pairs = vec![("a.rs", "b.rs"), ("c.rs", "d/c.rs")];
        assert_eq!(asked, Some(Ok((vec!["slop"], pairs))));
        assert_eq!(moves(&["slop", "--keys", "a.rs"]), None);
        let refused = [
            &["--lower", "--moved", "a.rs=b.rs"][..],
            &["crap", "--moved", "a.rs=b.rs"],
            &["--moved"],
            &["--moved", "a.rs"],
            &["--moved", "a.rs=a.rs"],
            &["--moved", "=b.rs"],
            &["--moved", "a.rs=b.rs", "--keys"],
        ];
        for words in refused {
            assert!(matches!(moves(words), Some(Err(_))), "{words:?}");
        }
    }

    /// A moved file takes its keys only once the old path is gone and the new one is there.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_moved_file_takes_its_keys_once_the_tree_shows_the_move() {
        let tree = crate::testdir::Held::tree("cli-moved", &[("b.rs", ""), ("c.rs", "")]);
        let mut held = Baseline::empty("0.1.0");
        held.set("slop", Series([("a.rs".to_string(), 3)].into()));
        let was = held.clone();
        let asked = |pairs: Vec<(&'static str, &'static str)>| (vec!["slop"], pairs);
        let refusals = [
            (
                asked(vec![("b.rs", "c.rs")]),
                "b.rs is still in the tree, so it did not move",
            ),
            (
                asked(vec![("a.rs", "d.rs")]),
                "d.rs is not a file in the tree",
            ),
            (
                asked(vec![("x.rs", "b.rs")]),
                "no record named here holds a key of x.rs",
            ),
            (
                asked(vec![("a.rs", "b.rs"), ("x.rs", "c.rs")]),
                "no record named here holds a key of x.rs",
            ),
        ];
        for (moves, why) in refusals {
            assert_eq!(moved(&moves, &tree, None, &mut held), Err(why.to_string()));
            assert_eq!(held, was);
        }
        assert!(
            moved(
                &(vec!["no-such-gate"], vec![("a.rs", "b.rs")]),
                &tree,
                None,
                &mut held
            )
            .is_err()
        );
        assert_eq!(
            moved(&asked(vec![("a.rs", "b.rs")]), &tree, None, &mut held),
            Ok(())
        );
        assert_eq!(held.gate("slop"), Series([("b.rs".to_string(), 3)].into()));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_whole_record_is_written_and_a_gate_that_cannot_measure_writes_no_key() {
        let gate = &crate::gates::metrics::admits::GATE;
        let proofs = crate::testdir::Held::tree("cli-whole", &[("B.lean", "def b := sorry\n")]);
        let mut held = Baseline::empty("0.1.0");
        let whole = Recording {
            names: vec![gate.name],
            lower: false,
            keys: None,
        };
        assert_eq!(written(&whole, &[gate], &proofs, &mut held), Ok(Vec::new()));
        assert_eq!(held.gate(gate.name).get("B.lean#sorry"), Some(1));
        let none = crate::testdir::Held::tree("cli-no-proof", &[("README.md", "no proof\n")]);
        assert_eq!(
            written(&whole, &[gate], &none, &mut held),
            Ok(vec![gate.name])
        );
        let keyed = Recording {
            keys: Some(vec!["B.lean#sorry"]),
            ..whole
        };
        assert_eq!(
            written(&keyed, &[gate], &none, &mut held),
            Err(ExitCode::from(2))
        );
        assert_eq!(held.gate(gate.name).get("B.lean#sorry"), Some(1));
    }
}
