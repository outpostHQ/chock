//! `sorry`, `admit`, `native_decide` and `axiom` in Lean files, counted per file and word. Each one
//! lets a theorem stand on a step that Lean's kernel never checked.

use std::collections::BTreeMap;
use std::path::Path;

use crate::gates::metrics::prodlines::keyed;
use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

pub const GATE: Gate = Gate {
    name: "admits",
    about: "`sorry`, `admit`, `native_decide` and `axiom` in Lean proof files, counted per file \
            and word",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "unchecked proof step(s)",
    },
};

/// The words that let a proof stand without the kernel checking a step.
const WORDS: [&str; 4] = ["sorry", "admit", "native_decide", "axiom"];

/// The bytes of `'\u{10FFFF}'`, the longest character Lean writes.
const LONGEST_CHARACTER: usize = 12;

/// Where Lake keeps its builds and the packages it fetched, which are not the project's proofs.
const FETCHED: [&str; 2] = [".lake", "lake-packages"];

/// Keyed by file and word, never by line, so an edit above a `sorry` does not read as a new one.
/// A tree with no `.lean` file holds no step to count, so the gate stays on in a Rust project.
fn measure(ctx: &Ctx) -> Result<Series, String> {
    let entered = |dir: &str| !project::SKIPPED.contains(&dir) && !FETCHED.contains(&dir);
    let named = |name: &str, _: &Path| {
        Path::new(name).extension().is_some_and(|it| it == "lean") && name != "lakefile.lean"
    };
    let files = project::walked(&ctx.listing, &ctx.root, &entered, &named)?;
    let mut read = Vec::new();
    for path in files {
        let shown = project::relative(&ctx.root, &path);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{shown}: {e}"))?;
        read.push((shown, counted(&text)));
    }
    Ok(keyed(read))
}

/// Each of `WORDS` in Lean code: not in a comment, a string, a character, a `«»` name or a
/// quoted name such as `` `sorry ``, and only as a whole name, so `sorry_free` is no `sorry`.
fn counted(text: &str) -> BTreeMap<&'static str, u64> {
    let mut found = BTreeMap::new();
    // Block comments nest, and inside one only `/-` and `-/` mean anything.
    let mut depth = 0_usize;
    let mut rest = text;
    while let Some(next) = rest.chars().next() {
        let len = match next {
            '/' if rest.starts_with("/-") => {
                depth += 1;
                2
            }
            '-' if depth > 0 && rest.starts_with("-/") => {
                depth -= 1;
                2
            }
            _ if depth > 0 => next.len_utf8(),
            '-' if rest.starts_with("--") => before(rest, |c| c == '\n'),
            '"' => closed(rest).unwrap_or(rest.len()),
            '\'' => closed(rest)
                .filter(|len| *len <= LONGEST_CHARACTER)
                .unwrap_or(1),
            '«' => before(rest, |c| c == '»') + '»'.len_utf8(),
            '`' => before(rest, |c| c != '`' && !is_word(c)),
            _ if is_word(next) => before(rest, |c| !is_word(c)),
            _ => next.len_utf8(),
        };
        // Only a bare name can equal a word; each other token keeps its quote or its comment mark.
        let (token, after) = rest.split_at_checked(len).unwrap_or((rest, ""));
        if let Some(word) = WORDS.iter().find(|it| **it == token) {
            *found.entry(*word).or_default() += 1;
        }
        rest = after;
    }
    found
}

/// A name's characters: Lean's own, and `.` between the parts of a dotted name.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '\'' | '!' | '?' | '.')
}

/// The bytes of `text` before the first character that `ends` it, or all of them.
fn before(text: &str, ends: impl Fn(char) -> bool) -> usize {
    text.find(ends).unwrap_or(text.len())
}

/// The bytes from the quote `text` starts with through the one that closes it, where `\` escapes
/// the next character; `None` when none closes it.
fn closed(text: &str) -> Option<usize> {
    let quote = text.chars().next()?;
    let mut chars = text.char_indices().skip(1);
    while let Some((at, c)) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if c == quote {
            return Some(at + c.len_utf8());
        }
    }
    None
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<(&'static str, u64)> {
        counted(text).into_iter().collect()
    }

    #[test]
    fn each_word_in_code_is_counted_by_name() {
        let text = "axiom choice : False\ntheorem a : P := by\n  · sorry\n  · admit\n\
                    theorem b : 4 - 2 = 2 := by native_decide\n\
                    def c := («ab» sorry, \"(\", sorry, '\"', sorry, `sorry, sorry)\n";
        assert_eq!(
            words(text),
            [
                ("admit", 1),
                ("axiom", 1),
                ("native_decide", 1),
                ("sorry", 5)
            ]
        );
    }

    #[test]
    fn a_word_inside_a_comment_a_string_or_a_quoted_name_is_not_counted() {
        let text = "-- sorry\n/- an axiom /- nested sorry -/ still a comment: admit -/\n\
                    /-- a doc comment: sorry -/\n/-! module doc: axiom -/\n\
                    def s := \"sorry \\\" admit\"\ndef q := '\"'\ndef e := '\\''\n\
                    def «sorry» := 1\ndef n := `sorry\ndef m := ``admit\n";
        assert_eq!(words(text), []);
    }

    #[test]
    fn only_a_whole_name_counts() {
        let text = "theorem sorry_free : True := trivial\n#print axioms sorry_free\n\
                    def admits := Foo.sorry\ndef h' := sorry'\nexample : P := by exact?\n";
        assert_eq!(words(text), []);
    }

    #[test]
    fn a_text_cut_short_ends_the_count_and_a_lone_quote_reads_on() {
        for text in ["/- open", "\"open", "«open", "--"] {
            assert_eq!(words(&format!("{text} sorry")), [], "{text}");
        }
        for text in ["'", "'\\", "'a", "`", "'\\u{1F600}", "-/"] {
            assert_eq!(words(&format!("{text} sorry")), [("sorry", 1)], "{text}");
        }
        let text = "/- a ⟨comment⟩ → ∀ x -/ sorry '\\u{10FFFF}' admit 'a'";
        assert_eq!(words(text), [("admit", 1), ("sorry", 1)]);
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
            ],
        );
        let series = measure(&ctx).unwrap();
        let keys: Vec<(&str, u64)> = series.0.iter().map(|(k, n)| (k.as_str(), *n)).collect();
        assert_eq!(
            keys,
            [("Proofs/A.lean#axiom", 1), ("Proofs/A.lean#sorry", 1)]
        );
        let none = crate::testdir::Held::tree("gate-admits-none", &[("src/lib.rs", "")]);
        assert_eq!(measure(&none).unwrap(), Series::new());
    }
}
