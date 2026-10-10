//! A Rust source file as a flat run of tokens, each with its place, and the two comparisons a
//! refactor has to survive: nothing but comments changed, and nothing was lost in a move.

use std::collections::BTreeMap;
use std::ops::Range;
use std::str::FromStr;

use proc_macro2::{Delimiter, Spacing, Span, TokenStream, TokenTree};

/// One token without its place. A `Punct` keeps whether the next one touches it, so `->` and `- >`
/// differ.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Tok {
    Ident(String),
    Punct(char, bool),
    Literal(String),
    Open(char),
    Close(char),
}

/// A string is one `Literal` and a comment is no token, so a brace or a keyword inside either is
/// never read as code.
#[derive(Clone, Debug, Default)]
pub struct Lexed {
    pub toks: Vec<Tok>,
    /// The 1-based line each token starts on.
    pub line: Vec<usize>,
    /// The byte range of each token in the file.
    pub at: Vec<Range<usize>>,
    /// For an `Open` or a `Close`, the index of its partner; for any other token, its own index.
    pub pair: Vec<usize>,
}

/// A run of outer attributes and the item under it, as token indexes: the first `#`, the first
/// token past the attributes, and the token that ends the item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Item {
    pub first: usize,
    pub head: usize,
    pub end: usize,
}

impl Lexed {
    pub fn new(source: &str) -> Result<Self, String> {
        let stream = TokenStream::from_str(source).map_err(|e| format!("does not lex: {e}"))?;
        let mut out = Self::default();
        out.flatten(stream);
        Ok(out)
    }

    fn push(&mut self, tok: Tok, span: Span) -> usize {
        self.toks.push(tok);
        self.line.push(span.start().line);
        self.at.push(span.byte_range());
        self.pair.push(self.pair.len());
        self.pair.len() - 1
    }

    fn flatten(&mut self, stream: TokenStream) {
        for tree in stream {
            match tree {
                TokenTree::Ident(i) => self.push(Tok::Ident(i.to_string()), i.span()),
                TokenTree::Punct(p) => {
                    let joint = p.spacing() == Spacing::Joint;
                    self.push(Tok::Punct(p.as_char(), joint), p.span())
                }
                TokenTree::Literal(l) => self.push(Tok::Literal(l.to_string()), l.span()),
                TokenTree::Group(g) => {
                    let (open, close) = delimiters(g.delimiter());
                    let o = self.push(Tok::Open(open), g.span_open());
                    self.flatten(g.stream());
                    let c = self.push(Tok::Close(close), g.span_close());
                    self.pair[o] = c;
                    self.pair[c] = o;
                    c
                }
            };
        }
    }

    /// The identifier at `i`, or `""` for any other token and past the end.
    #[must_use]
    pub fn ident(&self, i: usize) -> &str {
        match self.toks.get(i) {
            Some(Tok::Ident(s)) => s,
            _ => "",
        }
    }

    #[must_use]
    pub fn is_punct(&self, i: usize, c: char) -> bool {
        matches!(self.toks.get(i), Some(Tok::Punct(p, _)) if *p == c)
    }

    #[must_use]
    pub fn opens(&self, i: usize, c: char) -> bool {
        self.toks.get(i) == Some(&Tok::Open(c))
    }

    /// The `[` of the attribute whose `#` is token `i`, and whether it is an inner `#![…]`.
    #[must_use]
    pub fn attr(&self, i: usize) -> Option<(usize, bool)> {
        let inner = self.is_punct(i + 1, '!');
        let open = i + 1 + usize::from(inner);
        (self.is_punct(i, '#') && self.opens(open, '[')).then_some((open, inner))
    }

    /// The `[` of each attribute among the tokens of `range`, outer and inner, nested ones too.
    pub fn attrs(&self, range: Range<usize>) -> impl Iterator<Item = usize> + '_ {
        range.filter_map(|i| Some(self.attr(i)?.0))
    }

    /// The tokens strictly inside the group that opens at `open`.
    #[must_use]
    pub fn inside(&self, open: usize) -> &[Tok] {
        self.toks.get(open + 1..self.pair[open]).unwrap_or(&[])
    }

    /// Whether the attribute at `i` is a `///` or `//!` comment: the lexer gives every token of
    /// one the place of the whole comment.
    fn is_doc_comment(&self, i: usize) -> bool {
        let same_place = |(open, _): (usize, bool)| self.at[open].start == self.at[i].start;
        self.attr(i).is_some_and(same_place)
    }

    /// Every run of outer attributes with the item under it, nested items too. A run starts at a
    /// written attribute: a reader looks for `#[test]`, not for the comment over it.
    #[must_use]
    pub fn items(&self) -> Vec<Item> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.toks.len() {
            let Some((_, false)) = self.attr(i).filter(|_| !self.is_doc_comment(i)) else {
                i += 1;
                continue;
            };
            let mut head = i;
            while let Some((open, false)) = self.attr(head) {
                head = self.pair[open] + 1;
            }
            if head < self.toks.len() {
                let end = self.item_end(head);
                out.push(Item {
                    first: i,
                    head,
                    end,
                });
            }
            i = head;
        }
        out
    }

    /// Every `fn` that has a block, nested ones too: the token of its name and the tokens of the
    /// block, braces included.
    #[must_use]
    pub fn functions(&self) -> Vec<(usize, Range<usize>)> {
        let block = |keyword: usize| {
            let end = self.item_end(keyword);
            let closes = self.toks.get(end) == Some(&Tok::Close('}'));
            let named = !self.ident(keyword + 1).is_empty();
            (closes && named).then(|| (keyword + 1, self.pair[end]..end + 1))
        };
        let keywords = (0..self.toks.len()).filter(|&i| self.ident(i) == "fn");
        keywords.filter_map(block).collect()
    }

    /// Where the item at `head` ends: the brace closing its first block, its `;`, or the token
    /// before the delimiter that closes whatever holds it.
    fn item_end(&self, head: usize) -> usize {
        let mut j = head;
        loop {
            match self.toks.get(j) {
                Some(Tok::Punct(';', _)) => return j,
                Some(Tok::Open('{')) => return self.pair[j],
                Some(Tok::Open(_)) => j = self.pair[j] + 1,
                Some(Tok::Close(_)) | None => return j.saturating_sub(1),
                Some(_) => j += 1,
            }
        }
    }
}

fn delimiters(delimiter: Delimiter) -> (char, char) {
    match delimiter {
        Delimiter::Parenthesis => ('(', ')'),
        Delimiter::Bracket => ('[', ']'),
        Delimiter::Brace => ('{', '}'),
        Delimiter::None => (' ', ' '),
    }
}

/// The tokens of a source file, places dropped.
pub fn lex(source: &str) -> Result<Vec<Tok>, String> {
    Lexed::new(source).map(|lexed| lexed.toks)
}

/// The tokens without their `#[doc = "…"]` and `#![doc = "…"]` runs, and how many runs there were.
fn without_docs(toks: &[Tok]) -> (Vec<Tok>, usize) {
    let (mut out, mut runs, mut i) = (Vec::with_capacity(toks.len()), 0, 0);
    while i < toks.len() {
        let open = i + 1 + usize::from(matches!(toks.get(i + 1), Some(Tok::Punct('!', _))));
        let is_doc = matches!(toks[i], Tok::Punct('#', _))
            && toks.get(open) == Some(&Tok::Open('['))
            && toks.get(open + 1) == Some(&Tok::Ident("doc".into()))
            && matches!(toks.get(open + 2), Some(Tok::Punct('=', _)));
        let close = toks[open.min(toks.len())..]
            .iter()
            .position(|t| *t == Tok::Close(']'));
        match close {
            Some(close) if is_doc => {
                runs += 1;
                i = open + close + 1;
            }
            _ => {
                out.push(toks[i].clone());
                i += 1;
            }
        }
    }
    (out, runs)
}

/// What one file's change amounts to.
#[derive(Debug, Eq, PartialEq)]
pub enum Verdict {
    /// Not one token moved: only `//` comments and blank lines changed.
    Clean,
    /// Only `///` and `//!` text changed.
    DocsOnly { removed: usize, added: usize },
    /// Code changed. `at` is the index of the first token that differs, doc comments left out.
    CodeChanged {
        at: usize,
        before: Option<Tok>,
        after: Option<Tok>,
    },
}

#[must_use]
pub fn classify(before: &[Tok], after: &[Tok]) -> Verdict {
    if before == after {
        return Verdict::Clean;
    }
    let ((b, b_docs), (a, a_docs)) = (without_docs(before), without_docs(after));
    if b == a {
        return Verdict::DocsOnly {
            removed: b_docs.saturating_sub(a_docs),
            added: a_docs.saturating_sub(b_docs),
        };
    }
    let at = b.iter().zip(&a).take_while(|(x, y)| x == y).count();
    Verdict::CodeChanged {
        at,
        before: b.get(at).cloned(),
        after: a.get(at).cloned(),
    }
}

/// How many times each distinct token appears on one side of a delta.
pub type TokenCounts = Vec<(Tok, usize)>;

/// The tokens `before` holds more of, then those `after` holds more of. Order and the spacing of
/// punctuation are ignored: rustfmt turns `> >` into `>>`, which moves no content.
#[must_use]
pub fn multiset_delta(before: &[Tok], after: &[Tok]) -> (TokenCounts, TokenCounts) {
    let mut counts: BTreeMap<Tok, isize> = BTreeMap::new();
    for (toks, step) in [(before, -1), (after, 1)] {
        for tok in toks {
            let tok = match tok {
                Tok::Punct(c, _) => Tok::Punct(*c, false),
                other => other.clone(),
            };
            *counts.entry(tok).or_default() += step;
        }
    }
    let side = |sign: isize| -> TokenCounts {
        let moved = counts.iter().filter(|(_, n)| **n * sign > 0);
        moved.map(|(t, n)| (t.clone(), n.unsigned_abs())).collect()
    };
    (side(-1), side(1))
}

/// A token in the words of a report.
#[must_use]
pub fn describe(tok: Option<&Tok>) -> String {
    match tok {
        None => "<end of file>".to_string(),
        Some(Tok::Ident(s)) => format!("ident `{s}`"),
        Some(Tok::Literal(s)) => format!("literal {s}"),
        Some(Tok::Punct(c, true)) => format!("punct `{c}` (joint)"),
        Some(Tok::Punct(c, false)) => format!("punct `{c}`"),
        Some(Tok::Open(c)) => format!("open `{c}`"),
        Some(Tok::Close(c)) => format!("close `{c}`"),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn ident(name: &str) -> Tok {
        Tok::Ident(name.to_string())
    }

    fn verdict(before: &str, after: &str) -> Verdict {
        classify(&lex(before).unwrap(), &lex(after).unwrap())
    }

    #[test]
    fn every_token_keeps_its_byte_range_and_its_line() {
        let src = "let s = \"hello\"; // trailing\nlet t = 'x';\n";
        let lexed = Lexed::new(src).unwrap();
        let texts: Vec<&str> = lexed.at.iter().map(|at| &src[at.clone()]).collect();
        let expected = [
            "let",
            "s",
            "=",
            "\"hello\"",
            ";",
            "let",
            "t",
            "=",
            "'x'",
            ";",
        ];
        assert_eq!(texts, expected);
        assert_eq!(lexed.line, [1, 1, 1, 1, 1, 2, 2, 2, 2, 2]);
        assert_eq!(lexed.pair, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn a_group_is_an_open_and_a_close_that_name_each_other() {
        let lexed = Lexed::new("f(a[0], { b })").unwrap();
        let expected = [
            ident("f"),
            Tok::Open('('),
            ident("a"),
            Tok::Open('['),
            Tok::Literal("0".to_string()),
            Tok::Close(']'),
            Tok::Punct(',', false),
            Tok::Open('{'),
            ident("b"),
            Tok::Close('}'),
            Tok::Close(')'),
        ];
        assert_eq!(lexed.toks, expected);
        assert_eq!(lexed.pair, [0, 10, 2, 5, 4, 3, 6, 9, 8, 7, 1]);
        assert_eq!(lexed.inside(3), [Tok::Literal("0".to_string())]);
        assert_eq!(lexed.inside(7), [ident("b")]);
    }

    #[test]
    fn a_comment_is_no_token_and_a_nested_one_ends_at_its_true_end() {
        let lexed = Lexed::new("/* outer /* inner */ still */ fn f() {} // #[test]").unwrap();
        assert_eq!(lexed.ident(0), "fn");
        assert_eq!(lexed.ident(1), "f");
        assert_eq!(lexed.ident(2), "");
        assert_eq!(lexed.ident(99), "");
        let rest = [
            Tok::Open('('),
            Tok::Close(')'),
            Tok::Open('{'),
            Tok::Close('}'),
        ];
        assert_eq!(lexed.toks[2..], rest);
    }

    #[test]
    fn a_token_is_asked_for_by_its_kind_and_its_character() {
        let lexed = Lexed::new("a -> [b]").unwrap();
        assert_eq!(lexed.toks[1], Tok::Punct('-', true));
        assert_eq!(lexed.toks[2], Tok::Punct('>', false));
        assert!(lexed.is_punct(1, '-'));
        assert!(!lexed.is_punct(1, '>'));
        assert!(!lexed.is_punct(0, 'a'));
        assert!(!lexed.is_punct(9, '-'));
        assert!(lexed.opens(3, '['));
        assert!(!lexed.opens(3, '('));
        assert!(!lexed.opens(5, '['));
    }

    #[test]
    fn an_attribute_is_found_at_its_hash_whether_outer_or_inner() {
        let lexed = Lexed::new("#![a] #[b] # c ! [d]").unwrap();
        assert_eq!(lexed.attr(0), Some((2, true)));
        assert_eq!(lexed.attr(5), Some((6, false)));
        assert_eq!(lexed.attr(1), None);
        assert_eq!(lexed.attr(9), None);
        assert_eq!(lexed.attr(11), None);
        let all: Vec<usize> = lexed.attrs(0..lexed.toks.len()).collect();
        assert_eq!(all, [2, 6]);
        let later: Vec<usize> = lexed.attrs(1..lexed.toks.len()).collect();
        assert_eq!(later, [6]);
    }

    #[test]
    fn an_item_runs_from_its_first_attribute_to_its_closing_brace_or_semicolon() {
        let src = "#[a]\n#[b]\nfn f(x: [u8; 2]) -> u8 { 1 }\n#[c]\nstruct S;\nfn bare() {}\n";
        let lexed = Lexed::new(src).unwrap();
        let items = lexed.items();
        let places: Vec<(usize, &str, usize)> = items
            .iter()
            .map(|item| {
                (
                    lexed.line[item.first],
                    lexed.ident(item.head),
                    lexed.line[item.end],
                )
            })
            .collect();
        assert_eq!(places, [(1, "fn", 3), (4, "struct", 5)]);
        assert_eq!(lexed.toks[items[0].end], Tok::Close('}'));
        assert_eq!(lexed.toks[items[1].end], Tok::Punct(';', false));
    }

    #[test]
    fn a_nested_item_is_found_and_one_with_no_body_ends_inside_its_holder() {
        let src = "#[cfg(test)]\nmod m {\n    #[test]\n    fn inner() {}\n}\nf(#[x] y);\n#[z]";
        let lexed = Lexed::new(src).unwrap();
        let heads: Vec<(&str, &Tok)> = lexed
            .items()
            .iter()
            .map(|item| (lexed.ident(item.head), &lexed.toks[item.end]))
            .collect();
        let expected = [
            ("mod", &Tok::Close('}')),
            ("fn", &Tok::Close('}')),
            ("y", &Tok::Ident("y".to_string())),
        ];
        assert_eq!(heads, expected);
    }

    #[test]
    fn every_function_with_a_block_is_found_by_its_name_and_its_block() {
        let src = "fn a<T: Fn() -> u8>(f: T) -> u8 { f() }\ntrait T { fn declared(); fn given() {} }\ntype F = fn(u8) -> u8;\nstruct S { f: fn() }\nmod m { async fn inner() { fn nested() -> [u8; 2] { [1, 2] } } }\n";
        let lexed = Lexed::new(src).unwrap();
        let text =
            |block: Range<usize>| &src[lexed.at[block.start].start..lexed.at[block.end - 1].end];
        let found = lexed.functions().into_iter();
        let shown: Vec<(&str, &str)> = found
            .map(|(name, block)| (lexed.ident(name), text(block)))
            .collect();
        let expected = [
            ("a", "{ f() }"),
            ("given", "{}"),
            ("inner", "{ fn nested() -> [u8; 2] { [1, 2] } }"),
            ("nested", "{ [1, 2] }"),
        ];
        assert_eq!(shown, expected);
    }

    #[test]
    fn a_doc_comment_starts_no_item_but_a_written_doc_attribute_does() {
        let commented = Lexed::new("/// Says so.\nfn f() {}").unwrap();
        assert_eq!(commented.items(), []);
        let written = Lexed::new("#[doc = \"Says so.\"]\nfn f() {}").unwrap();
        let expected = Item {
            first: 0,
            head: 6,
            end: 11,
        };
        assert_eq!(written.items(), [expected]);
        let under = Lexed::new("#[test]\n/// Says so.\nfn f() {}").unwrap();
        let heads: Vec<&str> = under.items().iter().map(|i| under.ident(i.head)).collect();
        assert_eq!(heads, ["fn"]);
    }

    #[test]
    fn an_empty_delimiter_is_written_as_spaces() {
        assert_eq!(delimiters(Delimiter::None), (' ', ' '));
        assert_eq!(delimiters(Delimiter::Parenthesis), ('(', ')'));
        assert_eq!(delimiters(Delimiter::Bracket), ('[', ']'));
        assert_eq!(delimiters(Delimiter::Brace), ('{', '}'));
    }

    #[test]
    fn source_that_does_not_lex_is_an_error_that_says_so() {
        let why = lex("fn a( {").unwrap_err();
        assert!(why.starts_with("does not lex: "), "{why}");
    }

    #[test]
    fn deleting_a_plain_comment_moves_no_token() {
        let before = "fn a() -> u8 {\n // add one\n 1 + 1\n}";
        assert_eq!(verdict(before, "fn a() -> u8 {\n 1 + 1\n}"), Verdict::Clean);
    }

    #[test]
    fn a_doc_comment_gone_or_new_is_docs_only_and_counted_on_its_side() {
        let (bare, one) = ("fn a() -> u8 { 1 }", "/// Adds one.\nfn a() -> u8 { 1 }");
        let two = "//! Module docs.\n/// Adds one.\nfn a() -> u8 { 1 }";
        let docs = |removed, added| Verdict::DocsOnly { removed, added };
        assert_eq!(verdict(one, bare), docs(1, 0));
        assert_eq!(verdict(bare, two), docs(0, 2));
        assert_eq!(verdict(two, one), docs(1, 0));
        assert_eq!(verdict(one, "/// Adds 1.\nfn a() -> u8 { 1 }"), docs(0, 0));
    }

    #[test]
    fn a_changed_literal_is_code_and_the_first_difference_is_named() {
        let found = verdict("/// Doc.\nfn a() -> u8 { 1 }", "fn a() -> u8 { 2 }");
        let expected = Verdict::CodeChanged {
            at: 8,
            before: Some(Tok::Literal("1".to_string())),
            after: Some(Tok::Literal("2".to_string())),
        };
        assert_eq!(found, expected);
    }

    #[test]
    fn a_statement_dropped_behind_a_comment_sweep_is_code() {
        let before = "fn a() {\n // step one\n f();\n // step two\n g();\n}";
        let expected = Verdict::CodeChanged {
            at: 9,
            before: Some(ident("g")),
            after: Some(Tok::Close('}')),
        };
        assert_eq!(verdict(before, "fn a() {\n f();\n}"), expected);
    }

    #[test]
    fn code_cut_from_the_end_differs_at_the_end_of_the_shorter_file() {
        let expected = Verdict::CodeChanged {
            at: 2,
            before: Some(ident("b")),
            after: None,
        };
        assert_eq!(verdict("a; b", "a;"), expected);
    }

    #[test]
    fn an_attribute_that_is_not_a_doc_comment_is_code() {
        let found = verdict("#[derive(Debug)]\nstruct S;", "struct S;");
        let expected = Verdict::CodeChanged {
            at: 0,
            before: Some(Tok::Punct('#', false)),
            after: Some(ident("struct")),
        };
        assert_eq!(found, expected);
        let docless = verdict("#[doc(hidden)]\nstruct S;", "struct S;");
        assert!(
            matches!(docless, Verdict::CodeChanged { at: 0, .. }),
            "{docless:?}"
        );
    }

    #[test]
    fn a_hash_at_the_end_of_the_tokens_is_kept_as_code() {
        let toks = [ident("a"), Tok::Punct('#', false)];
        assert_eq!(without_docs(&toks), (toks.to_vec(), 0));
        let bang = [Tok::Punct('#', false), Tok::Punct('!', false)];
        assert_eq!(without_docs(&bang), (bang.to_vec(), 0));
    }

    #[test]
    fn a_move_across_two_files_loses_nothing_and_adds_only_the_wiring() {
        let before = lex("fn a() {}\nfn b() {}").unwrap();
        let mut after = lex("mod part;\nfn a() {}").unwrap();
        after.extend(lex("fn b() {}").unwrap());
        let added = vec![
            (ident("mod"), 1),
            (ident("part"), 1),
            (Tok::Punct(';', false), 1),
        ];
        assert_eq!(multiset_delta(&before, &after), (Vec::new(), added));
    }

    #[test]
    fn a_lost_token_is_counted_on_the_removed_side() {
        let before = lex("f(); f(); g();").unwrap();
        let after = lex("g(); h();").unwrap();
        let removed = vec![
            (ident("f"), 2),
            (Tok::Punct(';', false), 1),
            (Tok::Open('('), 1),
            (Tok::Close(')'), 1),
        ];
        assert_eq!(
            multiset_delta(&before, &after),
            (removed, vec![(ident("h"), 1)])
        );
    }

    #[test]
    fn a_nested_generic_that_rustfmt_closed_up_is_no_lost_token() {
        let before = lex("fn a() -> Vec<Vec<u8> > { vec![] }").unwrap();
        let after = lex("fn a() -> Vec<Vec<u8>> { vec![] }").unwrap();
        assert_ne!(before, after);
        assert_eq!(multiset_delta(&before, &after), (Vec::new(), Vec::new()));
    }

    #[test]
    fn a_token_is_described_by_its_kind() {
        let described: Vec<String> = [
            None,
            Some(ident("f")),
            Some(Tok::Literal("\"s\"".to_string())),
            Some(Tok::Punct('-', true)),
            Some(Tok::Punct('>', false)),
            Some(Tok::Open('{')),
            Some(Tok::Close('}')),
        ]
        .iter()
        .map(|tok| describe(tok.as_ref()))
        .collect();
        let expected = [
            "<end of file>",
            "ident `f`",
            "literal \"s\"",
            "punct `-` (joint)",
            "punct `>`",
            "open `{`",
            "close `}`",
        ];
        assert_eq!(described, expected);
    }
}
