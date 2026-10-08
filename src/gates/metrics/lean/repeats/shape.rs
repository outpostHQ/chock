//! The shape of a run of tokens, which copies share, and the values that may differ between them.

use std::hash::{DefaultHasher, Hash};

use proc_macro2::{Delimiter, Ident, Literal, Spacing, TokenStream, TokenTree};

use super::{FORMAT, TEXT};

/// The macros that format their strings: the standard library's, and those of `anyhow`, `eyre`
/// and `log`.
const FORMATS: &[&str] = &[
    "anyhow",
    "assert",
    "assert_eq",
    "assert_ne",
    "bail",
    "debug",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "ensure",
    "eprint",
    "eprintln",
    "error",
    "eyre",
    "format",
    "format_args",
    "info",
    "panic",
    "print",
    "println",
    "todo",
    "trace",
    "unimplemented",
    "unreachable",
    "warn",
    "write",
    "writeln",
];

/// Words a shape keeps as written: keywords, `true` and `false`, and the variants of `Option` and
/// `Result`. Every other name is a value that may differ.
const KEPT: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "yield", "Some", "None", "Ok", "Err",
];

/// Where a token stands: in code, in a macro that formats its strings, or in another macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Inside {
    Code,
    Format,
    Macro,
}

/// Feeds the shape of `tokens` to `hasher` and their values to `leaves`, and counts the tokens. A
/// called method or macro stays as written; a string a macro reads is marked, without its names.
pub(super) fn shape(
    tokens: TokenStream,
    inside: Inside,
    hasher: &mut DefaultHasher,
    leaves: &mut Vec<String>,
) -> usize {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    let mut count = tokens.len();
    for (at, token) in tokens.iter().enumerate() {
        let before = at.checked_sub(1).and_then(|it| tokens.get(it));
        match token {
            TokenTree::Group(group) => {
                format!("{:?}", group.delimiter()).hash(hasher);
                count += shape(
                    group.stream(),
                    within(&tokens[..at], inside),
                    hasher,
                    leaves,
                );
                0_u8.hash(hasher);
            }
            TokenTree::Ident(ident) if kept(ident, before, tokens.get(at + 1)) => {
                ident.to_string().hash(hasher);
            }
            TokenTree::Ident(ident) => {
                '_'.hash(hasher);
                leaves.push(ident.to_string());
            }
            TokenTree::Literal(literal) => leaves.push(value(literal, inside, hasher)),
            TokenTree::Punct(punct) => punct.as_char().hash(hasher),
        }
    }
    count
}

/// What a group after `before` stands in: a macro, which formats its strings or not, or else what
/// `before` stands in.
fn within(before: &[TokenTree], inside: Inside) -> Inside {
    match before {
        [.., TokenTree::Ident(name), TokenTree::Punct(bang)] if bang.as_char() == '!' => {
            if FORMATS.iter().any(|it| name == it) {
                Inside::Format
            } else {
                Inside::Macro
            }
        }
        _ => inside,
    }
}

/// Whether a name stays as written: a word `KEPT` holds, a called macro, or a method called or
/// named with a turbofish.
fn kept(ident: &Ident, before: Option<&TokenTree>, after: Option<&TokenTree>) -> bool {
    let dotted = matches!(before, Some(TokenTree::Punct(dot)) if dot.as_char() == '.');
    KEPT.iter().any(|it| ident == it)
        || match after {
            Some(TokenTree::Punct(next)) if next.as_char() == '!' => {
                next.spacing() == Spacing::Alone
            }
            Some(TokenTree::Punct(next)) => dotted && next.as_char() == ':',
            Some(TokenTree::Group(args)) => dotted && args.delimiter() == Delimiter::Parenthesis,
            _ => false,
        }
}

/// A literal as a value, its kind hashed: a number, or the quote or prefix that opens it. A string
/// a macro reads is marked, and loses the names it formats inline.
fn value(literal: &Literal, inside: Inside, hasher: &mut DefaultHasher) -> String {
    let text = literal.to_string();
    let first = text.chars().next().filter(|it| !it.is_ascii_digit());
    first.unwrap_or('0').hash(hasher);
    match (inside, text.ends_with(['"', '#'])) {
        (Inside::Format, true) => format!("{TEXT}{}", unnamed(&text)),
        (Inside::Macro, true) => format!("{FORMAT}{}", unnamed(&text)),
        _ => text,
    }
}

/// A format string without the names it reads inline, so `"{told}: {next}"` and
/// `"{reason}: {under}"` read as one.
pub(super) fn unnamed(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(it) = chars.next() {
        out.push(it);
        if it == '{' && chars.next_if_eq(&'{').is_some() {
            out.push('{');
        } else if it == '{' {
            while chars
                .next_if(|next| next.is_alphanumeric() || *next == '_')
                .is_some()
            {}
        }
    }
    out
}
