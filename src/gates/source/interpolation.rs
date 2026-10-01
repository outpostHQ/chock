//! Whether a value is written in the source or built at run time, as the shell rule asks.

use std::collections::BTreeMap;

use syn::punctuated::Punctuated;
use syn::{Expr, ExprLit, Lit, Token};

/// A value built at run time rather than written down: a concatenation with a non-literal operand,
/// or a `format!` at least one of whose fields is not a literal.
pub(super) fn dynamic(expr: &Expr) -> bool {
    match bare(expr) {
        Expr::Binary(binary) if matches!(binary.op, syn::BinOp::Add(_)) => {
            [&*binary.left, &*binary.right]
                .into_iter()
                .any(interpolated)
        }
        Expr::Macro(call) => format_dynamic(&call.mac),
        _ => false,
    }
}

pub(super) fn interpolated(expr: &Expr) -> bool {
    match bare(expr) {
        Expr::Binary(binary) if matches!(binary.op, syn::BinOp::Add(_)) => {
            [&*binary.left, &*binary.right]
                .into_iter()
                .any(interpolated)
        }
        Expr::Lit(_) => false,
        _ => true,
    }
}

/// A `format!` whose text names a field no literal argument fills. An inline capture such as
/// `{user}` is dynamic unless a literal is passed as `user`.
pub(super) fn format_dynamic(mac: &syn::Macro) -> bool {
    if !mac.path.is_ident("format") {
        return false;
    }
    let Ok(args) = mac.parse_body_with(Punctuated::<Expr, Token![,]>::parse_terminated) else {
        return false;
    };
    let mut args = args.into_iter();
    let Some(layout) = args.next().as_ref().and_then(text) else {
        return false;
    };
    let fields = fields(&layout);
    if fields.is_empty() {
        return false;
    }
    let (positional, named) = sorted(args);
    let mut next = 0;
    fields.iter().any(|field| {
        let field = field.split(':').next().unwrap_or_default();
        if field.is_empty() {
            next += 1;
            return !positional.get(next - 1).copied().unwrap_or(true);
        }
        match field.parse::<usize>() {
            Ok(index) => !positional.get(index).copied().unwrap_or(true),
            Err(_) => !named.get(field).copied().unwrap_or(false),
        }
    })
}

pub(super) fn sorted(args: impl Iterator<Item = Expr>) -> (Vec<bool>, BTreeMap<String, bool>) {
    let mut positional = Vec::new();
    let mut named = BTreeMap::new();
    for arg in args {
        match assigned(&arg) {
            Some((name, value)) => {
                named.insert(name, matches!(bare(value), Expr::Lit(_)));
            }
            None => positional.push(matches!(bare(&arg), Expr::Lit(_))),
        }
    }
    (positional, named)
}

pub(super) fn assigned(expr: &Expr) -> Option<(String, &Expr)> {
    let Expr::Assign(assign) = expr else {
        return None;
    };
    let Expr::Path(path) = &*assign.left else {
        return None;
    };
    Some((path.path.get_ident()?.to_string(), &assign.right))
}

/// The `{…}` fields of a format text, `{{` stepped over the way `format!` steps over it.
pub(super) fn fields(layout: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = layout.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' {
            continue;
        }
        if chars.peek() == Some(&'{') {
            chars.next();
            continue;
        }
        let mut field = String::new();
        for c in chars.by_ref() {
            if c == '}' {
                out.push(field);
                break;
            }
            field.push(c);
        }
    }
    out
}

pub(super) fn bare(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(paren) => bare(&paren.expr),
        Expr::Reference(reference) => bare(&reference.expr),
        other => other,
    }
}

pub(super) fn only(args: &Punctuated<Expr, Token![,]>) -> Option<&Expr> {
    args.first().filter(|_| args.len() == 1)
}

pub(super) fn text(expr: &Expr) -> Option<String> {
    match bare(expr) {
        Expr::Lit(ExprLit {
            lit: Lit::Str(value),
            ..
        }) => Some(value.value()),
        _ => None,
    }
}

pub(super) fn is_true(expr: &Expr) -> bool {
    matches!(expr, Expr::Lit(ExprLit { lit: Lit::Bool(value), .. }) if value.value)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap or panic in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;

    fn expr(src: &str) -> Expr {
        syn::parse_str(src).unwrap()
    }

    /// A named field nothing passes is captured, so dynamic; a positional one would not compile.
    #[test]
    fn a_format_field_is_dynamic_exactly_when_no_literal_fills_it() {
        for built in [
            r#"format!("ls {user}")"#,
            r#"format!("ls {}", name)"#,
            r#"format!("ls {0}", name)"#,
            r#"format!("ls {n}", n = name)"#,
        ] {
            assert!(dynamic(&expr(built)), "{built}");
        }
        for written in [
            r#"format!("ls {}", "here")"#,
            r#"format!("ls {n}", n = "here")"#,
            r#"format!("ls {{user}}")"#,
            r#""ls -l""#,
        ] {
            assert!(!dynamic(&expr(written)), "{written}");
        }
    }

    /// `.to_string()` is a call, not a literal, so `"ls ".to_string() + "-l"` counts as dynamic.
    #[test]
    fn a_concatenation_is_dynamic_when_any_operand_was_not_written_down() {
        assert!(dynamic(&expr(r#""ls " .to_string() + name"#)));
        assert!(dynamic(&expr(r#""ls ".to_string() + "-l""#)));
        assert!(dynamic(&expr(r#"("a" + "b") + name"#)));
        assert!(!dynamic(&expr(r#""a" + "b""#)));
        assert!(!dynamic(&expr(r#"("a" + "b") + "c""#)));
    }

    fn call_args(src: &str) -> Punctuated<Expr, Token![,]> {
        match expr(src) {
            Expr::Call(call) => call.args,
            _ => panic!("{src} is not a call"),
        }
    }

    #[test]
    fn a_call_with_one_argument_yields_it_and_a_call_with_two_yields_nothing() {
        assert_eq!(
            only(&call_args(r#"f("sh")"#)).and_then(text),
            Some("sh".to_string())
        );
        assert_eq!(only(&call_args(r#"f("sh", "-c")"#)).and_then(text), None);
        assert_eq!(only(&call_args("f()")).and_then(text), None);
    }

    #[test]
    fn a_macro_that_is_not_a_readable_format_is_not_dynamic() {
        let mac = |src: &str| match expr(src) {
            Expr::Macro(held) => held.mac,
            _ => panic!("{src} is not a macro"),
        };
        assert!(!format_dynamic(&mac(r#"println!("ls {user}")"#)));
        assert!(!format_dynamic(&mac("format!(+)")));
        assert!(!format_dynamic(&mac("format!(name, 1)")));
        assert!(format_dynamic(&mac(r#"format!("ls {user}")"#)));
    }

    #[test]
    fn a_named_argument_is_read_only_when_its_left_side_is_a_name() {
        assert_eq!(
            assigned(&expr(r#"n = "here""#)).map(|(name, _)| name),
            Some("n".to_string())
        );
        assert!(assigned(&expr(r#"held[0] = "here""#)).is_none());
        assert!(assigned(&expr(r#""here""#)).is_none());
    }

    #[test]
    fn parentheses_and_a_reference_are_read_through_to_the_value_inside() {
        assert!(dynamic(&expr(r#"&("ls ".to_string() + name)"#)));
        assert!(dynamic(&expr(r#"(("ls ".to_string() + name))"#)));
        assert!(!dynamic(&expr(r#"&("a" + "b")"#)));
    }

    #[test]
    fn a_literal_reads_back_as_itself_and_an_expression_reads_as_nothing() {
        assert_eq!(text(&expr(r#""sh""#)), Some("sh".to_string()));
        assert_eq!(text(&expr("name")), None);
        assert!(is_true(&expr("true")));
        assert!(!is_true(&expr("false")));
        assert!(!is_true(&expr("flag")));
    }
}
