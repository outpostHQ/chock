//! The `laws` gate: what each proof claims, pinned by a hash of its text, so a weaker claim shows
//! as a pin that moved. Only a person moves a pin, with `chock baseline laws`.

mod rust;
mod tla;

use std::path::Path;

use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::report::Detail;
use crate::run::{Ctx, Gate, Group, Kind, Measurement};
use crate::tokens::Lexed;

/// Fails a proof statement whose text differs from the text a person pinned.
pub const GATE: Gate = Gate {
    name: "laws",
    about: "a TLA+ theorem, a Verus `requires` or `ensures`, or the checks of a Kani harness whose \
            text is not the one a person pinned with `chock baseline laws`, per law",
    group: Group::OptIn,
    builds: false,
    reads: None,
    kind: Kind::AnnotatedRatchet {
        measure,
        // One row per law, keyed by the hash of its text, so a changed law is a row that stops.
        keys: Keys::Census,
        unit: "law(s)",
    },
};

/// One claim: its name, the line it starts on, and the text its hash pins.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Law {
    name: String,
    line: usize,
    text: String,
}

/// Keyed `file#name@hash`, so a law whose text changes stops its row and starts a new one.
fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let seen = |dir: &str| !project::SKIPPED.contains(&dir);
    let named = |name: &str, _: &Path| {
        Path::new(name)
            .extension()
            .is_some_and(|ext| ext == "tla" || ext == "rs")
    };
    let files = project::walked(&ctx.listing, &ctx.root, &seen, &named)?;
    let mut measured = Measurement::of(Series::new(), Vec::new());
    for path in files {
        let shown = project::relative(&ctx.root, &path);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            // A Rust file that is not UTF-8 holds no proof.
            Err(e)
                if Path::new(&shown).extension().is_some_and(|ext| ext == "rs")
                    && e.kind() == std::io::ErrorKind::InvalidData =>
            {
                continue;
            }
            Err(e) => return Err(format!("{shown}: {e}")),
        };
        for law in laws_in(&shown, &text) {
            let key = format!("{shown}#{}@{:016x}", law.name, fnv(&law.text));
            let count = measured.series.get(&key).unwrap_or(0);
            measured.series.set(&key, count + 1);
            let line = Some(u32::try_from(law.line).unwrap_or(u32::MAX));
            let detail = || Detail {
                line,
                places: Vec::new(),
                fix: None,
            };
            measured.details.entry(key).or_insert_with(detail);
        }
    }
    Ok(measured)
}

/// The laws of one file: the theorems of a TLA+ module, and the harnesses and Verus contracts of a
/// Rust file. A Rust file that does not lex holds none, since it does not build.
fn laws_in(shown: &str, text: &str) -> Vec<Law> {
    if Path::new(shown).extension().is_some_and(|ext| ext == "tla") {
        return tla::theorems(text);
    }
    let verus = text.contains("verus!");
    if !verus && !text.contains("kani") {
        return Vec::new();
    }
    let Ok(lexed) = Lexed::new(text) else {
        return Vec::new();
    };
    let mut laws = rust::harnesses(&lexed);
    if verus {
        laws.extend(rust::contracts(&lexed));
    }
    laws
}

/// FNV-1a, 64 bits: no Rust release changes it, unlike `DefaultHasher`, so a pin stays put.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn law(name: &str, line: usize, text: &str) -> Law {
        Law {
            name: name.to_string(),
            line,
            text: text.to_string(),
        }
    }

    #[test]
    fn the_hash_is_fnv_1a_of_64_bits() {
        assert_eq!(fnv(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv("foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn each_law_is_keyed_by_file_name_and_hash_and_counted_where_it_repeats() {
        let ctx = crate::testdir::Held::tree(
            "gate-laws",
            &[
                (
                    "specs/M.tla",
                    "THEOREM TRUE\nTHEOREM TRUE\nLEMMA L == FALSE\n",
                ),
                ("src/lib.rs", "#[kani::proof]\nfn h() { assert!(true); }\n"),
                ("src/plain.rs", "fn f() {}\n"),
                (
                    "src/v.rs",
                    "verus! {\nproof fn p(x: int) ensures x == x, {}\n}\n",
                ),
            ],
        );
        std::fs::write(ctx.root.join("src/bytes.rs"), [0xff, 0xfe]).unwrap();
        let read = measure(&ctx).unwrap();
        let theorem = format!("specs/M.tla#THEOREM@{:016x}", fnv("THEOREM TRUE"));
        let lemma = format!("specs/M.tla#LEMMA L@{:016x}", fnv("LEMMA L == FALSE"));
        let harness = format!(
            "src/lib.rs#h@{:016x}",
            fnv("[ kani :: proof ]\nassert ! ( true )")
        );
        let contract = format!(
            "src/v.rs#fn p@{:016x}",
            fnv("fn p ( x : int ) ensures x == x ,")
        );
        let keys = crate::testdir::rows(&read.series);
        assert_eq!(
            keys,
            [
                (lemma.as_str(), 1),
                (theorem.as_str(), 2),
                (harness.as_str(), 1),
                (contract.as_str(), 1)
            ]
        );
        assert_eq!(read.details[&theorem].line, Some(1));
        assert_eq!(read.details[&harness].line, Some(2));
        let raw = crate::testdir::Held::tree("gate-laws-raw", &[("src/lib.rs", "")]);
        std::fs::write(raw.root.join("M.tla"), [0xff, 0xfe]).unwrap();
        assert!(measure(&raw).unwrap_err().starts_with("M.tla: "));
    }
}
