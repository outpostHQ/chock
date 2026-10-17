//! `sorry`, `admit`, `native_decide` and `axiom` in Lean files, and Verus's escapes in Rust files
//! that hold a `verus!` block, counted per file and word. Each one lets a proof skip a step.

use std::collections::BTreeMap;
use std::path::Path;

use crate::gates::metrics::prodlines::keyed;
use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

pub const GATE: Gate = Gate {
    name: "admits",
    about: "proof steps nothing checked: `sorry` or `axiom` in Lean, `assume(` or \
            `verifier::external` in Verus, counted per file and word",
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
        let extension = Path::new(name).extension();
        extension.is_some_and(|it| it == "rs" || it == "lean") && name != "lakefile.lean"
    };
    let files = project::walked(&ctx.listing, &ctx.root, &entered, &named)?;
    let mut read = Vec::new();
    for path in files {
        let shown = project::relative(&ctx.root, &path);
        let words = words_in(&path, &shown)?;
        read.push((shown, words));
    }
    read.retain(|(_, words)| !words.is_empty());
    Ok(keyed(read))
}

/// The words of one file: the escapes of a Rust file, the steps of a Lean one.
fn words_in(path: &Path, shown: &str) -> Result<BTreeMap<&'static str, u64>, String> {
    let rust = shown.ends_with(".rs");
    match std::fs::read_to_string(path) {
        Ok(text) if rust => Ok(escapes(&text)),
        Ok(text) => Ok(counted(&text)),
        // A Rust file that is not UTF-8 holds no `verus!` block.
        Err(e) if rust && e.kind() == std::io::ErrorKind::InvalidData => Ok(BTreeMap::new()),
        Err(e) => Err(format!("{shown}: {e}")),
    }
}

/// Verus's ways past its verifier, in a file with a `verus!` block: a call to `assume` or `admit`,
/// `verifier::external` in any form, and `assume_specification`. Comments and strings hold none.
fn escapes(text: &str) -> BTreeMap<&'static str, u64> {
    let mut found = BTreeMap::new();
    if !text.contains("verus!") {
        return found;
    }
    let Ok(tokens) = text.parse::<proc_macro2::TokenStream>() else {
        return found;
    };
    escapes_in(tokens, &mut found);
    found
}

fn escapes_in(tokens: proc_macro2::TokenStream, found: &mut BTreeMap<&'static str, u64>) {
    use proc_macro2::{Delimiter, TokenTree};
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    for (at, tree) in trees.iter().enumerate() {
        let next = trees.get(at + 1);
        let word = match tree {
            TokenTree::Group(group) => {
                escapes_in(group.stream(), found);
                None
            }
            TokenTree::Ident(ident) => {
                let called = matches!(next, Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis);
                escape(&ident.to_string(), called, &trees[at + 1..])
            }
            _ => None,
        };
        if let Some(word) = word {
            *found.entry(word).or_default() += 1;
        }
    }
}

/// The escape a name starts, given whether a call follows it and the tokens after it.
fn escape(name: &str, called: bool, after: &[proc_macro2::TokenTree]) -> Option<&'static str> {
    use proc_macro2::TokenTree;
    match name {
        "assume" if called => Some("assume"),
        "admit" if called => Some("admit"),
        _ if name.starts_with("assume_spec") => Some("assume_specification"),
        // `verifier::external_body`, and the older `verifier(external_body)`.
        "verifier" => {
            let path: String = after.iter().take(3).map(ToString::to_string).collect();
            let args = match after.first() {
                Some(TokenTree::Group(args)) => args.stream().to_string(),
                _ => String::new(),
            };
            let external = path.starts_with("::external") || args.starts_with("external");
            external.then_some("verifier::external")
        }
        _ => None,
    }
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
    fn each_verus_escape_in_code_is_counted_and_a_mention_is_not() {
        let text = "use vstd::prelude::*;\nverus! {\n\
                    #[verifier::external_body]\nfn a() {}\n#[verifier(external)]\nfn b() {}\n\
                    #[verifier::opaque]\n#[verifier(opaque)]\nfn e() {}\n\
                    pub assume_specification[ f ](x: u8);\n\
                    proof fn c() { assume(false); admit(); }\n\
                    // assume(true) and verifier::external_body in a comment\n\
                    fn d() { let s = \"admit()\"; let assume = 1; }\n}\n";
        let found: Vec<(&str, u64)> = escapes(text).into_iter().collect();
        let all = [("admit", 1), ("assume", 1), ("assume_specification", 1)];
        assert_eq!(found, [&all[..], &[("verifier::external", 2)]].concat());
        assert_eq!(escapes("fn f() { assume(x) }\n"), BTreeMap::new());
        assert_eq!(escapes("verus! { fn f( }\n"), BTreeMap::new());
        let mut walked = BTreeMap::new();
        escapes_in("{ [admit()] }".parse().unwrap(), &mut walked);
        assert_eq!(walked, BTreeMap::from([("admit", 1)]));
        let after = |text: &str| -> Vec<proc_macro2::TokenTree> {
            let stream: proc_macro2::TokenStream = text.parse().unwrap();
            stream.into_iter().collect()
        };
        let external = Some("verifier::external");
        for text in [
            "::external",
            "::external_body",
            "(external)",
            "(external_body)",
        ] {
            assert_eq!(escape("verifier", false, &after(text)), external, "{text}");
        }
        for text in ["::opaque", "(opaque)", "", ": external", "external"] {
            assert_eq!(escape("verifier", false, &after(text)), None, "{text}");
        }
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
            ],
        );
        // In name order this file comes first, so the walk must go on past it.
        std::fs::write(ctx.root.join("src/a.rs"), [0xff, 0xfe]).unwrap();
        let series = measure(&ctx).unwrap();
        let keys: Vec<(&str, u64)> = series.0.iter().map(|(k, n)| (k.as_str(), *n)).collect();
        assert_eq!(
            keys,
            [
                ("Proofs/A.lean#axiom", 1),
                ("Proofs/A.lean#sorry", 1),
                ("src/law.rs#assume", 1)
            ]
        );
        let none = crate::testdir::Held::tree("gate-admits-none", &[("src/lib.rs", "")]);
        assert_eq!(measure(&none).unwrap(), Series::new());
        let raw = crate::testdir::Held::tree("gate-admits-raw", &[("src/lib.rs", "")]);
        std::fs::write(raw.root.join("Raw.lean"), [0xff, 0xfe]).unwrap();
        assert!(measure(&raw).unwrap_err().starts_with("Raw.lean: "));
    }
}
