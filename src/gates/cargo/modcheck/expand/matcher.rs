//! Whether a call's tokens have the shape of one `macro_rules!` matcher, and the identifier
//! each fragment takes. It reads `ident`, `literal`, `lifetime` and `tt` fragments only.

use proc_macro2::{Delimiter, Spacing, TokenTree};
use syn::ext::IdentExt;

use super::{is_punct, trees};

/// Why an arm gives no modules for a call.
pub(super) enum Miss {
    /// The call does not have the arm's shape, so the next arm is tried.
    Unmatched,
    /// The arm has a fragment this matcher does not read, such as `expr`, so the search stops:
    /// rustc may give the call to this arm and not to a later one.
    Unreadable,
}

/// Each identifier a fragment took, under its variable.
pub(super) type Bindings = Vec<(String, String)>;

/// How many tokens at the start of `input` match all of `matcher`, without backtracking.
pub(super) fn matched(
    matcher: &[TokenTree],
    input: &[TokenTree],
    bound: &mut Bindings,
) -> Result<usize, Miss> {
    let (mut at, mut next) = (0, 0);
    while let Some(rest) = matcher.get(next..).filter(|rest| !rest.is_empty()) {
        let (used, written) = step(rest, input.get(at..).unwrap_or_default(), bound)?;
        at += used;
        next += written;
    }
    Ok(at)
}

/// Matches the first element of `matcher`, and gives how many tokens of `input` and of `matcher`
/// it takes.
fn step(
    matcher: &[TokenTree],
    input: &[TokenTree],
    bound: &mut Bindings,
) -> Result<(usize, usize), Miss> {
    match matcher {
        [
            dollar,
            TokenTree::Ident(variable),
            colon,
            TokenTree::Ident(kind),
            ..,
        ] if is_punct(dollar, '$') && is_punct(colon, ':') => {
            let used = fragment(&kind.to_string(), input)?;
            if let Some(TokenTree::Ident(name)) = input.first() {
                bound.push((variable.to_string(), name.unraw().to_string()));
            }
            Ok((used, 4))
        }
        [dollar, TokenTree::Group(group), after @ ..]
            if is_punct(dollar, '$') && group.delimiter() == Delimiter::Parenthesis =>
        {
            let (separator, op, written) = operator(after)?;
            let used = repeated(&trees(&group.stream()), separator, op, input, bound)?;
            Ok((used, 2 + written))
        }
        [TokenTree::Group(group), ..] => match input.first() {
            Some(TokenTree::Group(given)) if given.delimiter() == group.delimiter() => {
                let inner = trees(&given.stream());
                let used = matched(&trees(&group.stream()), &inner, bound)?;
                (used == inner.len())
                    .then_some((1, 1))
                    .ok_or(Miss::Unmatched)
            }
            _ => Err(Miss::Unmatched),
        },
        [token, ..] if input.first().is_some_and(|given| same(token, given)) => Ok((1, 1)),
        _ => Err(Miss::Unmatched),
    }
}

/// How many tokens one `kind` fragment takes at the start of `input`.
fn fragment(kind: &str, input: &[TokenTree]) -> Result<usize, Miss> {
    let used = match (kind, input) {
        ("ident", [TokenTree::Ident(name), ..]) if *name != "_" => 1,
        ("literal", [TokenTree::Literal(_), ..]) | ("tt", [_, ..]) => 1,
        ("literal", [TokenTree::Ident(word), ..]) if *word == "true" || *word == "false" => 1,
        ("literal", [minus, TokenTree::Literal(_), ..]) if is_punct(minus, '-') => 2,
        ("lifetime", [TokenTree::Punct(quote), TokenTree::Ident(_), ..])
            if quote.as_char() == '\'' && quote.spacing() == Spacing::Joint =>
        {
            2
        }
        ("ident" | "literal" | "lifetime" | "tt", _) => return Err(Miss::Unmatched),
        _ => return Err(Miss::Unreadable),
    };
    Ok(used)
}

/// The separator and the operator after a `$( )`, and how many tokens they take. As in rustc, an
/// operator right after the group has no separator, and `?` never takes one.
fn operator(after: &[TokenTree]) -> Result<(Option<&TokenTree>, char, usize), Miss> {
    match (
        after.first().and_then(kleene),
        after.get(1).and_then(kleene),
    ) {
        (Some(op), _) => Ok((None, op, 1)),
        (None, Some(op @ ('*' | '+'))) => Ok((after.first(), op, 2)),
        _ => Err(Miss::Unreadable),
    }
}

fn kleene(tree: &TokenTree) -> Option<char> {
    match tree {
        TokenTree::Punct(punct) if matches!(punct.as_char(), '*' | '+' | '?') => {
            Some(punct.as_char())
        }
        _ => None,
    }
}

/// How many tokens at the start of `input` match `inner` as often as it can, at most once for `?`.
fn repeated(
    inner: &[TokenTree],
    separator: Option<&TokenTree>,
    op: char,
    input: &[TokenTree],
    bound: &mut Bindings,
) -> Result<usize, Miss> {
    let (mut at, mut times) = (0, 0);
    while op != '?' || times == 0 {
        let start = match separator {
            Some(separator) if times > 0 => match input.get(at) {
                Some(given) if same(separator, given) => at + 1,
                _ => break,
            },
            _ => at,
        };
        let kept = bound.len();
        match matched(inner, input.get(start..).unwrap_or_default(), bound) {
            Ok(used) if used > 0 => {
                at = start + used;
                times += 1;
            }
            Ok(_) | Err(Miss::Unmatched) => {
                bound.truncate(kept);
                break;
            }
            Err(Miss::Unreadable) => return Err(Miss::Unreadable),
        }
    }
    if op == '+' && times == 0 {
        return Err(Miss::Unmatched);
    }
    Ok(at)
}

fn same(written: &TokenTree, given: &TokenTree) -> bool {
    match (written, given) {
        (TokenTree::Ident(a), TokenTree::Ident(b)) => a == b,
        (TokenTree::Punct(a), TokenTree::Punct(b)) => a.as_char() == b.as_char(),
        (TokenTree::Literal(a), TokenTree::Literal(b)) => a.to_string() == b.to_string(),
        _ => false,
    }
}
