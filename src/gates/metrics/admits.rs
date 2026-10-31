//! Proof steps nothing checked, per file and word: Lean's `sorry` and `axiom`, Verus's escapes,
//! `kani::assume`, and a TLA+ `ASSUME` that names no constant, unless `.chock/escapes.json` keeps it.

mod covers;
mod lean;
mod reasons;
mod rust;

use std::collections::BTreeSet;
use std::path::Path;

use crate::gates::proofs::tla::text::{self, Unit};
use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::report::Place;
use crate::run::{Ctx, Gate, Group, Kind, Measurement};
use covers::Covers;
use reasons::Kept;

/// The `admits` gate: proof steps that nothing checked, counted per file and word.
pub const GATE: Gate = Gate {
    name: "admits",
    about: "proof steps nothing checked: `sorry` or `axiom` in Lean, `assume(` or \
            `verifier::external` in Verus, `kani::assume`, and a TLA+ `ASSUME` that names no \
            constant, counted per file and word; `.chock/escapes.json` keeps one with its reason, \
            and `// checked: <crate> <test>` right above a Rust escape keeps it with a test",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::AnnotatedRatchet {
        measure,
        keys: Keys::Items,
        unit: "unchecked proof step(s)",
    },
};

/// The extensions of the files that hold proof steps.
const SCANNED: [&str; 3] = ["rs", "lean", "tla"];

/// Where Lake keeps its builds and the packages it fetched, which are not the project's proofs.
const FETCHED: [&str; 2] = [".lake", "lake-packages"];

/// The words that state a TLA+ fact a proof takes as given.
const ASSUMED: [&str; 3] = ["ASSUME", "ASSUMPTION", "AXIOM"];

/// Keyed by file and word, never by line, so an edit above a `sorry` does not read as a new one.
/// A tree with no proof holds no step to count, so the gate stays on in a Rust project.
fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let entered = |dir: &str| !project::SKIPPED.contains(&dir) && !FETCHED.contains(&dir);
    let named = |name: &str, _: &Path| {
        let extension = Path::new(name).extension().and_then(|it| it.to_str());
        extension.is_some_and(|it| SCANNED.contains(&it)) && name != "lakefile.lean"
    };
    let files = project::walked(&ctx.listing, &ctx.root, &entered, &named)?;
    let mut kept = Kept::read(&ctx.root)?;
    let mut covers = Covers::new(&ctx.root, &files);
    let mut measured = Measurement::of(Series::new(), Vec::new());
    for path in files {
        let shown = project::relative(&ctx.root, &path);
        unkept(&mut measured, (&mut kept, &mut covers), &path, &shown)?;
    }
    measured.faults = kept.unused();
    Ok(measured)
}

/// Counts each step of one file that no entry and no cover keeps, with its place, under
/// `{file}#{word}`. The place of a Rust escape says why no cover keeps it.
fn unkept(
    measured: &mut Measurement,
    (kept, covers): (&mut Kept, &mut Covers<'_>),
    path: &Path,
    shown: &str,
) -> Result<(), String> {
    let Some(text) = read(path, shown)? else {
        return Ok(());
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut earlier = (0, 0);
    for (word, line) in steps(shown, &text) {
        let written = lines.get(line.wrapping_sub(1)).copied().unwrap_or_default();
        if kept.take(shown, written) {
            continue;
        }
        // The escapes before this one on its line, which the covers above take first.
        let nth = if earlier.0 == line { earlier.1 + 1 } else { 0 };
        earlier = (line, nth);
        let mut place = Place::at("escape", shown, u32::try_from(line).unwrap_or(u32::MAX));
        if Path::new(shown).extension().is_some_and(|ext| ext == "rs") {
            match covers.uncovered(&lines, line, nth)? {
                Some(why) => place = place.item(&why),
                None => continue,
            }
        }
        let key = format!("{shown}#{word}");
        let places = &mut measured.details.entry(key.clone()).or_default().places;
        places.push(place);
        let count = u64::try_from(places.len()).unwrap_or(u64::MAX);
        measured.series.set(&key, count);
    }
    Ok(())
}

/// The text of one file, or `None` for a Rust file that is not UTF-8, which holds no proof.
fn read(path: &Path, shown: &str) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e)
            if Path::new(shown).extension().is_some_and(|ext| ext == "rs")
                && e.kind() == std::io::ErrorKind::InvalidData =>
        {
            Ok(None)
        }
        Err(e) => Err(format!("{shown}: {e}")),
    }
}

/// The steps of one file with their lines: the escapes of a Rust file, the assumptions of a TLA+
/// module, the skipped steps of a Lean one.
fn steps(shown: &str, text: &str) -> Vec<(&'static str, usize)> {
    match Path::new(shown).extension().and_then(|it| it.to_str()) {
        Some("rs") => rust::escapes(text),
        Some("tla") => assumed(text),
        _ => lean::counted(text),
    }
}

/// Each `ASSUME`, `ASSUMPTION` or `AXIOM` at column 0 that names no constant of its module: one that
/// does bounds what a model may set, which TLC checks, and one that does not is a bare fact.
fn assumed(text: &str) -> Vec<(&'static str, usize)> {
    let units = text::units(&text::uncommented(text));
    let declares = |unit: &&Unit| matches!(unit.first(), "CONSTANT" | "CONSTANTS");
    let constant_units = units.iter().filter(declares);
    let constants: BTreeSet<&str> = constant_units
        .flat_map(|unit| text::words(&unit.text))
        .collect();
    let fact = |unit: &Unit| {
        let word = ASSUMED.into_iter().find(|word| unit.first() == *word)?;
        // `ASSUME … PROVE` at column 0 is the statement of a theorem, not an assumption.
        let bounds = |name: &str| constants.contains(name) || name == "PROVE";
        let bounded = text::words(&unit.text).any(bounds);
        (!bounded).then_some((word, unit.line))
    };
    units.iter().filter_map(fact).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::report::Finding;

    const BARE: &str = "no `// checked: <crate> <test>` right above it";

    #[test]
    fn a_tla_assumption_counts_where_it_names_no_constant_of_its_module() {
        let text = "---- MODULE M ----\nCONSTANTS N,\n  Max \\* the most\nVARIABLE x\n\
                    ASSUME N \\in Nat\nASSUME\n  Max > 0\nASSUMPTION Fact == \\A n \\in Nat : n >= 0\n\
                    AXIOM Choice == TRUE\n  (* N *)\nTHEOREM T ==\nASSUME NEW y PROVE y = y\n\
                    \\* ASSUME TRUE\n  ASSUME TRUE\n====\n";
        assert_eq!(assumed(text), [("ASSUMPTION", 8), ("AXIOM", 9)]);
        assert_eq!(assumed("CONSTANT C\nASSUME C = 1\n"), []);
        assert_eq!(assumed("ASSUME CONSTANT\n"), [("ASSUME", 1)]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn each_proof_file_is_keyed_by_file_and_word_and_lake_s_own_are_not() {
        let ctx = crate::testdir::Held::tree(
            "gate-admits",
            &[
                ("Proofs/A.lean", "theorem a : P := sorry\naxiom b : Q\n"),
                ("Proofs/B.lean", "theorem c : True := trivial\n"),
                ("lakefile.lean", "-- sorry\nsorry\n"),
                (".lake/packages/x/X.lean", "axiom x : False\n"),
                ("lake-packages/y/Y.lean", "axiom y : False\n"),
                ("src/law.rs", "verus! { proof fn l() { assume(false); } }\n"),
                ("src/plain.rs", "fn f() { assume(x) }\n"),
                ("src/harness.rs", "fn h() {\n    kani::assume(n < 9);\n}\n"),
                (
                    "specs/M.tla",
                    "---- MODULE M ----\nAXIOM Fact == TRUE\n====\n",
                ),
            ],
        );
        // In name order this file comes first, so the walk must go on past it.
        std::fs::write(ctx.root.join("src/a.rs"), [0xff, 0xfe]).unwrap();
        let read = measure(&ctx).unwrap();
        assert_eq!(
            crate::testdir::rows(&read.series),
            [
                ("Proofs/A.lean#axiom", 1),
                ("Proofs/A.lean#sorry", 1),
                ("specs/M.tla#AXIOM", 1),
                ("src/harness.rs#kani::assume", 1),
                ("src/law.rs#assume", 1)
            ]
        );
        let harness = &read.details["src/harness.rs#kani::assume"];
        let bare = Place::at("escape", "src/harness.rs", 2).item(BARE);
        assert_eq!(harness.places, [bare]);
        assert_eq!(read.faults, []);
        let none = crate::testdir::Held::tree("gate-admits-none", &[("src/lib.rs", "")]);
        assert_eq!(
            measure(&none).unwrap(),
            Measurement::of(Series::new(), Vec::new())
        );
        let raw = crate::testdir::Held::tree("gate-admits-raw", &[("src/lib.rs", "")]);
        std::fs::write(raw.root.join("Raw.lean"), [0xff, 0xfe]).unwrap();
        assert!(measure(&raw).unwrap_err().starts_with("Raw.lean: "));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_kept_escape_is_not_counted_and_an_entry_with_none_left_trips() {
        let reasons = r#"[
            {"path": "src/h.rs", "text": "kani::assume(n < 9);", "reason": "bounds the loop"},
            {"path": "src/h.rs", "text": "kani::assume(m < 9);", "reason": "a line since gone"}
        ]"#;
        let ctx = crate::testdir::Held::tree(
            "gate-admits-kept",
            &[
                (reasons::FILE, reasons),
                (
                    "src/h.rs",
                    "fn h() {\n  kani::assume(n < 9);\n  kani::assume(n < 9);\n}\n",
                ),
            ],
        );
        let read = measure(&ctx).unwrap();
        assert_eq!(
            crate::testdir::rows(&read.series),
            [("src/h.rs#kani::assume", 1)]
        );
        let places = &read.details["src/h.rs#kani::assume"].places;
        assert_eq!(places, &[Place::at("escape", "src/h.rs", 3).item(BARE)]);
        let left = "1 entry(s) for `src/h.rs` keep `kani::assume(m < 9);`, and no escape there is left \
                    for them; take each one out, or copy the escape's line again";
        assert_eq!(read.faults, [Finding::at(reasons::FILE, left)]);
    }

    /// A Rust escape counts unless a cover right above it names a test its crate defines, and its
    /// place says why. Mutant: `defined.contains(test)` made `true` takes a test no crate defines.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_cover_keeps_an_escape_only_where_its_crate_defines_the_test_it_names() {
        let law = concat!(
            "// checked: app checks_double\nfn a() { kani::assume(x); }\n",
            "// checked: app no_such_test\nfn b() { kani::assume(x); }\n",
            "// checked: ghost-crate any_test\nfn c() { kani::assume(x); }\n",
            "// checked: app checks_double\n\nfn d() { kani::assume(x); }\n",
            "// checked: app\nfn e() { kani::assume(x); }\n",
            "// checked: app checks_double\n// checked: app no_such_test\n",
            "fn f() { kani::assume(x); kani::assume(y); }\n",
        );
        let package = |name: &str| {
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n")
        };
        let ctx = crate::testdir::Held::tree(
            "gate-admits-covers",
            &[
                (
                    "Cargo.toml",
                    "[workspace]\nmembers = [\"law\", \"app\"]\nresolver = \"3\"\n",
                ),
                ("law/Cargo.toml", &package("law")),
                ("law/src/lib.rs", law),
                ("app/Cargo.toml", &package("app")),
                (
                    "app/src/lib.rs",
                    "#[cfg(test)]\nmod tests {\n    #[test]\n    fn checks_double() {}\n}\n",
                ),
            ],
        );
        let read = measure(&ctx).unwrap();
        let why = |line, said: &str| Place::at("escape", "law/src/lib.rs", line).item(said);
        let missing = "`// checked: app no_such_test`: crate `app` defines no test `no_such_test`";
        let expected = [
            why(4, missing),
            why(
                6,
                "`// checked: ghost-crate any_test`: the workspace has no crate `ghost-crate`",
            ),
            why(9, BARE),
            why(
                11,
                "`// checked: app` names other than one crate and one test",
            ),
            why(14, missing),
        ];
        assert_eq!(read.details["law/src/lib.rs#kani::assume"].places, expected);
        assert_eq!(
            crate::testdir::rows(&read.series),
            [("law/src/lib.rs#kani::assume", 5)]
        );
    }
}
