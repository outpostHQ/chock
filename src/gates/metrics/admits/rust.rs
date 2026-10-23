//! The escapes of a Rust file: Verus's ways past its verifier in a file with a `verus!` block, and
//! Kani's `kani::assume` in any file.

use crate::tokens::Lexed;

/// Each escape outside comments and strings, with its line: a call to `assume` or `admit`, any
/// `verifier::external` or `assume_specification` in Verus, and a call to `kani::assume`.
pub(super) fn escapes(text: &str) -> Vec<(&'static str, usize)> {
    let verus = text.contains("verus!");
    if !verus && !text.contains("kani") {
        return Vec::new();
    }
    let Ok(lexed) = Lexed::new(text) else {
        return Vec::new();
    };
    // The escape that the name at token `i` starts. `kani::assume` is Kani's, in a Verus file too.
    let escape = |i: usize| {
        let name = lexed.ident(i);
        let called = lexed.opens(i + 1, '(');
        let kani = i
            .checked_sub(3)
            .is_some_and(|at| lexed.ident(at) == "kani" && pathed(&lexed, at + 1));
        match name {
            "assume" if called && kani => Some("kani::assume"),
            _ if !verus => None,
            "assume" if called => Some("assume"),
            "admit" if called => Some("admit"),
            _ if name.starts_with("assume_spec") => Some("assume_specification"),
            // `verifier::external_body`, and the older `verifier(external_body)`.
            "verifier" => {
                let external = |at: usize| lexed.ident(at).starts_with("external");
                let path = pathed(&lexed, i + 1) && external(i + 3);
                let args = called && external(i + 2);
                (path || args).then_some("verifier::external")
            }
            _ => None,
        }
    };
    let places = lexed.toks.iter().enumerate();
    places
        .filter_map(|(i, _)| Some((escape(i)?, lexed.line[i])))
        .collect()
}

/// Whether tokens `at` and `at + 1` are the `::` of a path.
fn pathed(lexed: &Lexed, at: usize) -> bool {
    lexed.is_punct(at, ':') && lexed.is_punct(at + 1, ':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_verus_escape_in_code_is_counted_on_its_line_and_a_mention_is_not() {
        let text = "use vstd::prelude::*;\nverus! {\n\
                    #[verifier::external_body]\nfn a() {}\n#[verifier(external)]\nfn b() {}\n\
                    #[verifier::opaque]\n#[verifier(opaque)]\nfn e() {}\n\
                    pub assume_specification[ f ](x: u8);\n\
                    proof fn c() { assume(false); { [admit()] } }\n\
                    // assume(true) and verifier::external_body in a comment\n\
                    fn d() { let s = \"admit()\"; let assume = 1; }\n}\n";
        let found = [
            ("verifier::external", 3),
            ("verifier::external", 5),
            ("assume_specification", 10),
            ("assume", 11),
            ("admit", 11),
        ];
        assert_eq!(escapes(text), found);
        assert_eq!(escapes("fn f() { assume(x) }\n"), []);
        assert_eq!(escapes("verus! { fn f( }\n"), []);
        let external = |args: &str| escapes(&format!("verus! {{ #[verifier{args}] fn a() {{}} }}"));
        for args in [
            "::external",
            "::external_body",
            "(external)",
            "(external_body)",
        ] {
            assert_eq!(external(args), [("verifier::external", 1)], "{args}");
        }
        for args in ["::opaque", "(opaque)", "", ": external", " external"] {
            assert_eq!(external(args), [], "{args}");
        }
    }

    #[test]
    fn each_kani_assume_is_counted_in_any_file_and_never_as_a_verus_assume() {
        let text = "fn h() {\n    kani::assume(x < 3);\n    let y: u8 = kani::any();\n}\n\
                    // kani::assume(false)\nfn s() { let t = \"kani::assume(1)\"; assume(t); }\n";
        assert_eq!(escapes(text), [("kani::assume", 2)]);
        let verus = "verus! { proof fn p() { kani::assume(a); assume(b); } }";
        assert_eq!(escapes(verus), [("kani::assume", 1), ("assume", 1)]);
        assert_eq!(escapes("fn k() { kani::assume; other::assume(c); }"), []);
    }
}
