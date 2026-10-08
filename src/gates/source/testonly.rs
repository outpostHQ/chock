//! Whether a `cfg` holds only in a test build, and which files a test-only `mod x;` names.

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use syn::{Attribute, Meta};

/// The deepest `cfg` nesting followed, so the recursion stays bounded; deeper is not a test gate.
const CFG_NESTING: usize = 32;

/// The files a test-only `mod x;` in this source names. `#[path]` redirects it, and a module with
/// a body names no file at all.
pub(super) fn test_modules(src: &str, path: &str, test_only: &dyn Fn(&str) -> bool) -> Vec<String> {
    let Ok(file) = syn::parse_file(src) else {
        return Vec::new();
    };
    let (dir, base) = (directory(path), beside(path));
    file.items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(held) => Some(held),
            _ => None,
        })
        .filter(|held| held.semi.is_some() && test_gate(&held.attrs, test_only))
        .flat_map(|held| named_files(held, dir, &base))
        .collect()
}

/// The directory the file sits in, which a `#[path]` outside any inline module is relative to.
pub(super) fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The directory a `mod x;` in this file looks in: beside a crate root or `mod.rs`, otherwise the
/// directory named after the file.
pub(super) fn beside(path: &str) -> String {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    match name {
        "lib.rs" | "main.rs" | "mod.rs" => dir.to_string(),
        _ => format!("{dir}/{}", name.trim_end_matches(".rs")),
    }
}

/// `x.rs` and `x/mod.rs` under `base` for a `mod x;`, or what its `#[path]` names under `dir`.
pub(super) fn named_files(held: &syn::ItemMod, dir: &str, base: &str) -> Vec<String> {
    if let Some(redirect) = path_attribute(&held.attrs) {
        return vec![format!("{dir}/{redirect}")];
    }
    let name = held.ident.to_string();
    let name = name.strip_prefix("r#").unwrap_or(&name);
    vec![format!("{base}/{name}.rs"), format!("{base}/{name}/mod.rs")]
}

/// The single file a `#[path = "…"]` names, which replaces both candidates above.
pub(super) fn path_attribute(attrs: &[Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| match &attr.meta {
        Meta::NameValue(pair) if pair.path.is_ident("path") => match &pair.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(text),
                ..
            }) => Some(text.value()),
            _ => None,
        },
        _ => None,
    })
}

/// A `#[cfg(test)]` item or a `#[test]` function: code no shipped build compiles.
pub(super) fn test_gate(attrs: &[Attribute], test_only: &dyn Fn(&str) -> bool) -> bool {
    attrs.iter().any(|attr| match &attr.meta {
        Meta::Path(path) => path.is_ident("test"),
        Meta::List(list) => {
            list.path.is_ident("cfg") && gates_on_test(list.tokens.clone(), 0, test_only)
        }
        Meta::NameValue(_) => false,
    })
}

/// Whether this `cfg` holds only in a test build. `all(...)` needs one test-only arm; `any(...)`
/// needs every arm to be test-only.
pub(super) fn gates_on_test(
    tokens: TokenStream,
    depth: usize,
    test_only: &dyn Fn(&str) -> bool,
) -> bool {
    if depth > CFG_NESTING {
        return false;
    }
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    match trees.as_slice() {
        [TokenTree::Ident(name)] => name == "test",
        [
            TokenTree::Ident(name),
            TokenTree::Punct(eq),
            TokenTree::Literal(value),
        ] if name == "feature" && eq.as_char() == '=' => test_only(&unquoted(&value.to_string())),
        [TokenTree::Ident(name), TokenTree::Group(group)]
            if group.delimiter() == Delimiter::Parenthesis && (name == "all" || name == "any") =>
        {
            let arms = arguments(group.stream());
            // An empty `any()` is false and an empty `all()` always holds: neither is a test gate.
            !arms.is_empty()
                && match name == "all" {
                    true => arms
                        .into_iter()
                        .any(|arm| gates_on_test(arm, depth + 1, test_only)),
                    false => arms
                        .into_iter()
                        .all(|arm| gates_on_test(arm, depth + 1, test_only)),
                }
        }
        _ => false,
    }
}

/// A string literal's text without its quotes.
pub(super) fn unquoted(literal: &str) -> String {
    literal.trim_matches('"').to_string()
}

pub(super) fn arguments(tokens: TokenStream) -> Vec<TokenStream> {
    let mut out = vec![TokenStream::new()];
    for tree in tokens {
        match &tree {
            TokenTree::Punct(punct) if punct.as_char() == ',' => out.push(TokenStream::new()),
            _ => {
                if let Some(last) = out.last_mut() {
                    last.extend(std::iter::once(tree));
                }
            }
        }
    }
    out.into_iter().filter(|arm| !arm.is_empty()).collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;

    #[test]
    fn a_feature_nothing_shipped_enables_makes_an_any_test_cfg_a_test_gate() {
        let gate = |cfg: &str, test_only: &dyn Fn(&str) -> bool| {
            let src = format!("#[cfg({cfg})]\nfn f() {{}}\n");
            let file = syn::parse_file(&src).unwrap();
            test_gate(&file.items.first().map(attrs_of).unwrap(), test_only)
        };
        let only_utils = |f: &str| f == "test-utils";
        assert!(gate("any(test, feature = \"test-utils\")", &only_utils));
        // One arm reaches a shipped build, and `any` holds whenever an arm does.
        assert!(!gate("any(test, feature = \"wire\")", &only_utils));
        // `all` needs every arm, so one test-only arm is enough.
        assert!(gate("all(test, feature = \"wire\")", &only_utils));
        assert!(gate("test", &only_utils));
        assert!(!gate("not(test)", &only_utils));
        // An `any()` with no arms is `false` and an `all()` with none holds everywhere.
        assert!(!gate("any()", &|_| true));
        assert!(!gate("all()", &|_| true));
        // Only the `feature` key names a feature, whatever its value.
        assert!(!gate("target_os = \"linux\"", &|_| true));
    }

    fn attrs_of(item: &syn::Item) -> Vec<Attribute> {
        match item {
            syn::Item::Fn(held) => held.attrs.clone(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn a_cfg_nested_past_the_limit_is_not_a_test_gate() {
        let nested = |depth: usize| {
            let cfg = "all(".repeat(depth) + "test" + &")".repeat(depth);
            let src = format!("#[cfg({cfg})]\nfn f() {{}}\n");
            let file = syn::parse_file(&src).unwrap();
            test_gate(&attrs_of(file.items.first().unwrap()), &|_| true)
        };
        assert!(nested(CFG_NESTING));
        assert!(!nested(CFG_NESTING + 1));
    }
}
