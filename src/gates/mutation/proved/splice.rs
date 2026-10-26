//! A mutant's file rebuilt from mutest's edits. mutest counts lines and characters from 1, and an
//! edit that names no place, overlaps another or cuts code a macro made rebuilds no file.

use crate::gates::mutation::survivors::Edit;

/// The byte order mark rustc drops before it counts a column.
const BOM: &[u8] = b"\xEF\xBB\xBF";

/// One edit in bytes: the original's `start..end` gives way to `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Cut {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) text: Vec<u8>,
}

/// The file at `path` as the edits make it, or `None` where an edit is in another file, names
/// no place in this one, is of a kind chock does not rebuild, or changes code a macro made.
pub(super) fn rebuilt(original: &[u8], path: &str, edits: &[Edit]) -> Option<Vec<u8>> {
    if edits.is_empty() {
        return None;
    }
    let mut cuts = edits
        .iter()
        .map(|edit| {
            let here = edit.location.span.path.replace('\\', "/") == path;
            here.then(|| cut(original, edit)).flatten()
        })
        .collect::<Option<Vec<Cut>>>()?;
    spliced(original, &mut cuts)
}

/// The cut one edit makes. A replaced expression keeps the old one as a dead `else`, as mutest's
/// own build does, so the old one gives it its type; an inserted statement keeps a space.
fn cut(original: &[u8], edit: &Edit) -> Option<Cut> {
    let span = &edit.location.span;
    let (start, end) = (offset(original, span.begin)?, offset(original, span.end)?);
    if made_by_a_macro(original.get(start..end)?) {
        return None;
    }
    let (code, old) = (
        edit.substitute.replacement.as_bytes(),
        original.get(start..end)?,
    );
    let location = edit.location.kind.as_str();
    let (start, end, text) = match (location, edit.substitute.kind.as_str()) {
        ("replace", "expr") => {
            let text = [b"(if true { ", code, b" } else { ", old, b" })"].concat();
            (start, end, text)
        }
        ("replace", "stmt" | "local") => (start, end, code.to_vec()),
        ("insert_before", "stmt" | "local") => (start, start, [code, b" "].concat()),
        ("insert_after", "stmt" | "local") => (end, end, [b" ", code].concat()),
        _ => return None,
    };
    Some(Cut { start, end, text })
}

/// Whether `code` holds a macro call or an attribute. For code a macro expanded, mutest names
/// the macro's call, so a cut there would change the call and not the code the mutant changed.
fn made_by_a_macro(code: &[u8]) -> bool {
    let called = |at: usize| {
        let named = at
            .checked_sub(1)
            .and_then(|before| code.get(before))
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
        let next = code
            .iter()
            .skip(at.saturating_add(1))
            .find(|byte| !byte.is_ascii_whitespace());
        named && matches!(next, Some(b'(' | b'[' | b'{'))
    };
    let attribute = code.windows(2).any(|pair| matches!(pair, b"#[" | b"#!"));
    let bang = |(at, byte): (usize, &u8)| *byte == b'!' && called(at);
    attribute || code.iter().enumerate().any(bang)
}

/// Whether `byte` starts a character in UTF-8, rather than continuing one.
const fn starts_char(byte: u8) -> bool {
    byte & 0xC0 != 0x80
}

/// The byte where `line` and `column` start in `text`, or its length where they name the place
/// just past its last character. `None` where they name no place in it.
pub(super) fn offset(text: &[u8], (line, column): (u32, u32)) -> Option<usize> {
    let skipped = if text.starts_with(BOM) { BOM.len() } else { 0 };
    let (mut at_line, mut at_column) = (1_u32, 1_u32);
    for (index, &byte) in text.iter().enumerate().skip(skipped) {
        if !starts_char(byte) {
            continue;
        }
        if (at_line, at_column) == (line, column) {
            return Some(index);
        }
        (at_line, at_column) = match byte {
            b'\n' => (at_line.checked_add(1)?, 1),
            _ => (at_line, at_column.checked_add(1)?),
        };
    }
    ((at_line, at_column) == (line, column)).then_some(text.len())
}

/// `original` with each cut made, or `None` where a cut ends before it starts, falls outside
/// the original, or overlaps another. Cuts at one place keep the order they are given in.
pub(super) fn spliced(original: &[u8], cuts: &mut [Cut]) -> Option<Vec<u8>> {
    cuts.sort_by_key(|cut| (cut.start, cut.end));
    let mut rebuilt = Vec::with_capacity(original.len());
    let mut cursor = 0;
    for cut in cuts.iter() {
        if cut.end < cut.start {
            return None;
        }
        // A cut that starts before the cursor overlaps the one before it, and `get` refuses it.
        rebuilt.extend_from_slice(original.get(cursor..cut.start)?);
        rebuilt.extend_from_slice(&cut.text);
        cursor = cut.end;
    }
    rebuilt.extend_from_slice(original.get(cursor..)?);
    Some(rebuilt)
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Longest text the proofs cover; a proof says nothing past it.
    const LEN: usize = 4;

    /// Any cut, with up to 2 bytes of text.
    fn any_cut() -> Cut {
        let (bytes, len) = crate::bounded::bytes::<2>();
        Cut {
            start: kani::any(),
            end: kani::any(),
            text: bytes[..len].to_vec(),
        }
    }

    /// How mutest names the place of byte `at`: lines, and characters since the last line
    /// break, each from 1 and after any byte order mark.
    fn place(text: &[u8], at: usize) -> (u32, u32) {
        let before = &text[skipped(text)..at];
        let breaks = before.iter().filter(|byte| **byte == b'\n').count();
        let tail = before
            .rsplit(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        let chars = tail.iter().filter(|byte| starts_char(**byte)).count();
        (1 + breaks as u32, 1 + chars as u32)
    }

    fn skipped(text: &[u8]) -> usize {
        if text.starts_with(BOM) { BOM.len() } else { 0 }
    }

    /// `offset` and `place` agree both ways: a byte `offset` finds starts a character, or is the
    /// end, and has that place; each such byte's place leads back to it.
    #[kani::proof]
    #[kani::unwind(6)]
    fn an_offset_is_the_byte_at_its_place() {
        let (bytes, len) = crate::bounded::bytes::<LEN>();
        let text = &bytes[..len];
        let named = |at: usize| at == text.len() || starts_char(text[at]);
        let wanted: (u32, u32) = kani::any();
        if let Some(at) = offset(text, wanted) {
            assert!(skipped(text) <= at && at <= text.len() && named(at));
            assert_eq!(place(text, at), wanted);
        }
        let at: usize = kani::any();
        if skipped(text) <= at && at <= text.len() && named(at) {
            assert_eq!(offset(text, place(text, at)), Some(at));
        }
    }

    /// One cut puts its text in place of its span, and a span that is not in the text is refused.
    #[kani::proof]
    #[kani::unwind(7)]
    fn one_cut_puts_its_text_in_place_of_its_span() {
        let (bytes, len) = crate::bounded::bytes::<LEN>();
        let original = &bytes[..len];
        let cut = any_cut();
        let fits = cut.start <= cut.end && cut.end <= original.len();
        let expected =
            fits.then(|| [&original[..cut.start], &cut.text[..], &original[cut.end..]].concat());
        assert_eq!(spliced(original, &mut [cut]), expected);
    }

    /// Two cuts give one file in either order, and two that overlap are refused.
    #[kani::proof]
    #[kani::unwind(9)]
    fn two_cuts_rebuild_one_file_in_either_order() {
        let (bytes, len) = crate::bounded::bytes::<LEN>();
        let original = &bytes[..len];
        let (first, second) = (any_cut(), any_cut());
        kani::assume((first.start, first.end) < (second.start, second.end));
        let fits = first.start <= first.end
            && first.end <= second.start
            && second.start <= second.end
            && second.end <= original.len();
        let expected = fits.then(|| {
            let between = &original[first.end..second.start];
            let (head, tail) = (&original[..first.start], &original[second.end..]);
            [head, &first.text[..], between, &second.text[..], tail].concat()
        });
        let given = spliced(original, &mut [first.clone(), second.clone()]);
        assert_eq!(given, expected);
        assert_eq!(spliced(original, &mut [second, first]), expected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gates::mutation::survivors::{Location, Span, Substitute};

    /// An edit of `src/a.rs` at `begin..end`.
    fn edit(
        location: &str,
        kind: &str,
        (begin, end): ((u32, u32), (u32, u32)),
        code: &str,
    ) -> Edit {
        let path = "src/a.rs".to_string();
        Edit {
            location: Location {
                kind: location.to_string(),
                span: Span { path, begin, end },
            },
            substitute: Substitute {
                kind: kind.to_string(),
                replacement: code.to_string(),
            },
        }
    }

    fn text(edits: &[Edit]) -> Option<String> {
        let original = "fn h(x: u8) -> u8 {\n    x / 2\n}\n";
        rebuilt(original.as_bytes(), "src/a.rs", edits)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The span of `x / 2`.
    const BODY: ((u32, u32), (u32, u32)) = ((2, 5), (2, 10));

    #[test]
    fn a_column_counts_characters_after_a_byte_order_mark() {
        let text = "é=1\nx".as_bytes();
        let places = [(1, 1), (1, 2), (1, 3), (1, 4), (2, 1), (2, 2)];
        let offsets: Vec<Option<usize>> = places.iter().map(|at| offset(text, *at)).collect();
        assert_eq!(
            offsets,
            [Some(0), Some(2), Some(3), Some(4), Some(5), Some(6)]
        );
        for nowhere in [(2, 3), (3, 1), (0, 0), (1, 0)] {
            assert_eq!(offset(text, nowhere), None, "{nowhere:?}");
        }
        assert_eq!(offset(b"\xEF\xBB\xBFab", (1, 1)), Some(3));
        assert_eq!(offset(b"", (1, 1)), Some(0));
    }

    #[test]
    fn a_replaced_expression_keeps_the_old_one_for_its_type_and_a_statement_does_not() {
        let expression = text(&[edit("replace", "expr", BODY, "Default::default()")]);
        let wanted =
            "fn h(x: u8) -> u8 {\n    (if true { Default::default() } else { x / 2 })\n}\n";
        assert_eq!(expression.as_deref(), Some(wanted));
        let statement = text(&[edit("replace", "stmt", BODY, "return 0;")]);
        assert_eq!(
            statement.as_deref(),
            Some("fn h(x: u8) -> u8 {\n    return 0;\n}\n")
        );
    }

    #[test]
    fn an_inserted_statement_keeps_a_space_and_inserts_at_one_place_keep_their_order() {
        let before = text(&[edit("insert_before", "stmt", BODY, "return 0;")]);
        assert_eq!(
            before.as_deref(),
            Some("fn h(x: u8) -> u8 {\n    return 0; x / 2\n}\n")
        );
        let after = text(&[edit("insert_after", "local", BODY, "let y = 1;")]);
        assert_eq!(
            after.as_deref(),
            Some("fn h(x: u8) -> u8 {\n    x / 2 let y = 1;\n}\n")
        );
        let both = [
            edit("insert_before", "local", BODY, "let x: u8 = 0;"),
            edit("insert_before", "stmt", BODY, "return x;"),
        ];
        let wanted = "fn h(x: u8) -> u8 {\n    let x: u8 = 0; return x; x / 2\n}\n";
        assert_eq!(text(&both).as_deref(), Some(wanted));
    }

    #[test]
    fn an_edit_elsewhere_overlapping_or_of_another_kind_rebuilds_no_file() {
        assert_eq!(text(&[]), None);
        let mut away = edit("replace", "expr", BODY, "x % 2");
        away.location.span.path = "src/b.rs".to_string();
        assert_eq!(text(&[away.clone()]), None);
        away.location.span.path = "src\\a.rs".to_string();
        assert!(text(&[away]).is_some());
        assert_eq!(text(&[edit("replace", "item", BODY, "fn g() {}")]), None);
        assert_eq!(text(&[edit("delete", "expr", BODY, "")]), None);
        assert_eq!(
            text(&[edit("replace", "expr", ((2, 10), (2, 5)), "0")]),
            None
        );
        assert_eq!(
            text(&[edit("replace", "expr", ((2, 5), (9, 1)), "0")]),
            None
        );
        let overlapping = [
            edit("replace", "expr", BODY, "0"),
            edit("replace", "expr", ((2, 9), (2, 10)), "3"),
        ];
        assert_eq!(text(&overlapping), None);
    }

    #[test]
    fn code_a_macro_made_rebuilds_no_file() {
        for made in [
            "println!(\"a\")",
            "vec! [1]",
            "m!{}",
            "#[cfg(test)] x",
            "#![allow(x)]",
        ] {
            assert!(made_by_a_macro(made.as_bytes()), "{made}");
        }
        for plain in ["a != b", "x!=y", "!done", "!(x > 3)", "x / 2"] {
            assert!(!made_by_a_macro(plain.as_bytes()), "{plain}");
        }
        let original = b"fn t() {\n    assert!(ok);\n}\n";
        let call = edit("replace", "expr", ((2, 5), (2, 16)), "()");
        assert_eq!(rebuilt(original, "src/a.rs", &[call]), None);
    }

    #[test]
    fn a_cut_that_ends_before_it_starts_rebuilds_no_file() {
        let reversed = Cut {
            start: 2,
            end: 1,
            text: b"x".to_vec(),
        };
        assert_eq!(spliced(b"abc", &mut [reversed]), None);
    }
}
