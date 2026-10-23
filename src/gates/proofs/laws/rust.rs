//! The laws of a Rust file: each Kani harness, and each Verus function with a contract.

use std::collections::BTreeMap;
use std::ops::Range;

use super::Law;
use crate::gates::metrics::testlint::scan;
use crate::tokens::{Lexed, Tok};

/// The macros whose arguments a Kani harness checks.
const ASSERTS: [&str; 6] = [
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
];

/// Each Verus function with a `requires` or an `ensures`: its signature through its clauses, and
/// each `spec fn` of the file that those reach, whole.
pub(super) fn contracts(lexed: &Lexed) -> Vec<Law> {
    let functions = lexed.functions();
    let specs: BTreeMap<&str, Range<usize>> = functions
        .iter()
        .filter(|(name, _)| is_spec(lexed, name - 1))
        .map(|(name, block)| (lexed.ident(*name), name - 1..block.end))
        .collect();
    let clause =
        |tok: &Tok| matches!(tok, Tok::Ident(word) if word == "requires" || word == "ensures");
    // The tokens of each `spec fn` that `head` reaches, through each other, in file order.
    let reached = |head: Range<usize>| {
        let mut found = BTreeMap::new();
        let mut names: Vec<&str> = head.map(|i| lexed.ident(i)).collect();
        while let Some(name) = names.pop() {
            if let Some(spec) = specs.get(name)
                && found.insert(spec.start, spec.clone()).is_none()
            {
                names.extend(spec.clone().map(|i| lexed.ident(i)));
            }
        }
        found.into_values()
    };
    let law = |(name, block): &(usize, Range<usize>)| {
        let head = name - 1..block.start;
        if !lexed.toks[head.clone()].iter().any(clause) {
            return None;
        }
        let parts = [head.clone()].into_iter().chain(reached(head));
        let texts: Vec<String> = parts.map(|range| joined(&lexed.toks[range])).collect();
        Some(Law {
            name: format!("fn {}", lexed.ident(*name)),
            line: lexed.line[*name],
            text: texts.join("\n"),
        })
    };
    functions.iter().filter_map(law).collect()
}

/// Whether the `fn` at `keyword` is a `spec fn`, `spec(checked)` included.
fn is_spec(lexed: &Lexed, keyword: usize) -> bool {
    let Some(before) = keyword.checked_sub(1) else {
        return false;
    };
    let checked = matches!(lexed.toks[before], Tok::Close(')')).then(|| lexed.pair[before]);
    let word = checked.map_or(Some(before), |open| open.checked_sub(1));
    word.is_some_and(|at| lexed.ident(at) == "spec")
}

/// Each Kani harness: its `kani::` attributes, and each assertion, `kani::assert` and
/// `kani::assume` in its body.
pub(super) fn harnesses(lexed: &Lexed) -> Vec<Law> {
    let kani = |open: &usize| lexed.ident(open + 1) == "kani";
    // The tokens of the assertion, `kani::assert` or `kani::assume` that token `i` starts.
    let check = |i: usize| {
        let open = if ASSERTS.contains(&lexed.ident(i)) && lexed.is_punct(i + 1, '!') {
            i + 2
        } else if lexed.ident(i) == "kani"
            && lexed.is_punct(i + 1, ':')
            && lexed.is_punct(i + 2, ':')
            && matches!(lexed.ident(i + 3), "assert" | "assume")
        {
            i + 4
        } else {
            return None;
        };
        let opened = matches!(lexed.toks.get(open), Some(Tok::Open(_)));
        opened.then(|| i..lexed.pair[open] + 1)
    };
    let law = |item: crate::tokens::Item| {
        let attrs: Vec<usize> = lexed.attrs(item.first..item.head).filter(kani).collect();
        let proves = |open: &usize| {
            let name = scan::attr_name(lexed.inside(*open));
            name == "proof" || name == "proof_for_contract"
        };
        if !attrs.iter().any(proves) {
            return None;
        }
        let keyword = (item.head..item.end).find(|i| lexed.ident(*i) == "fn")?;
        let groups = attrs.iter().map(|open| *open..lexed.pair[*open] + 1);
        let parts = groups.chain((keyword..item.end + 1).filter_map(check));
        let texts: Vec<String> = parts.map(|range| joined(&lexed.toks[range])).collect();
        Some(Law {
            name: lexed.ident(keyword + 1).to_string(),
            line: lexed.line[keyword + 1],
            text: texts.join("\n"),
        })
    };
    lexed.items().into_iter().filter_map(law).collect()
}

/// Rust tokens as one line: a space after each token but a `Punct` joined to the next one.
fn joined(toks: &[Tok]) -> String {
    let mut out = String::new();
    for tok in toks {
        match tok {
            Tok::Ident(text) | Tok::Literal(text) => out.push_str(text),
            Tok::Punct(c, _) | Tok::Open(c) | Tok::Close(c) => out.push(*c),
        }
        if !matches!(tok, Tok::Punct(_, true)) {
            out.push(' ');
        }
    }
    out.truncate(out.trim_end().len());
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::gates::proofs::laws::tests::law;

    #[test]
    fn a_verus_contract_is_its_signature_and_the_spec_fns_it_reaches() {
        let text = "verus! {\nspec fn double(x: int) -> int { twice(x) }\n\
                    open spec fn twice(x: int) -> int { x + x }\nspec fn unused() -> int { 0 }\n\
                    proof fn lemma(x: int)\n    ensures double(x) == x * 2,\n{ assert(x * 2 == x + x); }\n\
                    fn plain(x: u8) -> u8 { x }\n\
                    fn checked(x: u8) -> (r: u8) requires x < 9 ensures r == x { x }\n}\n";
        let read = |text: &str| contracts(&Lexed::new(text).unwrap());
        let found = read(text);
        let lemma = "fn lemma ( x : int ) ensures double ( x ) == x * 2 ,\n\
                     fn double ( x : int ) -> int { twice ( x ) }\n\
                     fn twice ( x : int ) -> int { x + x }";
        let checked = "fn checked ( x : u8 ) -> ( r : u8 ) requires x < 9 ensures r == x";
        assert_eq!(
            found,
            [law("fn lemma", 5, lemma), law("fn checked", 9, checked)]
        );
        let body = text.replace("{ assert(x * 2 == x + x); }", "{ }");
        assert_eq!(read(&body), found);
        let spec = text.replace("{ x + x }", "{ x }");
        assert_ne!(read(&spec)[0].text, found[0].text);
        let lexed = Lexed::new("spec(checked) fn a() {} pub fn b() {} fn c() {}").unwrap();
        let specs: Vec<bool> = lexed
            .functions()
            .iter()
            .map(|(name, _)| is_spec(&lexed, name - 1))
            .collect();
        assert_eq!(specs, [true, false, false]);
        assert!(!is_spec(&lexed, 0));
    }

    #[test]
    fn a_kani_harness_is_its_kani_attributes_and_its_checks() {
        let text = "#[cfg(kani)]\nmod proofs {\n    #[kani::proof]\n    #[kani::unwind(3)]\n\
                    #[inline]\n    fn bounded() {\n        let n: u8 = kani::any();\n\
                    kani::assume(n < 9);\n        let m = n + 1;\n        assert!(m > n, \"grows\");\n\
                    kani::cover!(m == 9);\n    }\n    #[test]\n    fn not_a_harness() { assert!(true); }\n\
                    #[kani::proof_for_contract(f)]\n    fn contract() { assert_eq!(f(1), 1); }\n}\n";
        let read = |text: &str| harnesses(&Lexed::new(text).unwrap());
        let found = read(text);
        let bounded = "[ kani :: proof ]\n[ kani :: unwind ( 3 ) ]\nkani :: assume ( n < 9 )\n\
                       assert ! ( m > n , \"grows\" )";
        let contract = "[ kani :: proof_for_contract ( f ) ]\nassert_eq ! ( f ( 1 ) , 1 )";
        assert_eq!(
            found,
            [law("bounded", 6, bounded), law("contract", 16, contract)]
        );
        let changed = text.replace("let m = n + 1;", "let m = n + 2;");
        assert_eq!(read(&changed), found);
        let weaker = text.replace("m > n", "m >= n");
        assert_ne!(read(&weaker), found);
        assert_eq!(read("fn f() { kani::any(); }"), []);
        assert_eq!(
            super::super::laws_in("a.rs", "#[kani::proof] fn broken( {"),
            []
        );
    }
}
