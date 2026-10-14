//! Rules a project declares in its config that read the parsed source: `seams` (where a path or
//! crate may be used), `owners` (which file reads a value) and `calls` (which calls go together).

pub mod calls;
pub mod owners;
pub mod seams;

use std::collections::BTreeMap;

use syn::spanned::Spanned;
use syn::visit::Visit;

/// What each name a file's `use` items bring in stands for: `use std::env;` makes `env` stand for
/// `std::env`. A glob import names nothing, so a path through one stands for itself.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Aliases(BTreeMap<String, Vec<String>>);

impl Aliases {
    /// Every `use` in the file, nested modules too: a name is rarely brought in twice.
    pub(crate) fn of(file: &syn::File) -> Self {
        let named = used(file)
            .into_iter()
            .filter_map(|leaf| Some((leaf.called?, leaf.path)));
        Self(named.collect())
    }

    /// The full path a written path stands for, as segments. A path whose first name no `use`
    /// brings in, such as one under `crate` or a crate's own name, stands for itself.
    pub(crate) fn full(&self, path: &syn::Path) -> Vec<String> {
        let written: Vec<String> = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        if path.leading_colon.is_some() {
            return written;
        }
        match written.split_first() {
            Some((first, rest)) if self.0.contains_key(first) => {
                self.0[first].iter().chain(rest).cloned().collect()
            }
            _ => written,
        }
    }
}

/// One name a `use` ends in: `use a::{b as c, d::*}` ends in `a::b`, brought in as `c`, and in the
/// glob `a::d`. `self` names its prefix; a glob and `as _` bring in no name.
pub(crate) struct Leaf {
    pub(crate) path: Vec<String>,
    pub(crate) called: Option<String>,
    pub(crate) glob: bool,
    pub(crate) span: proc_macro2::Span,
}

impl Leaf {
    fn named(prefix: &[String], ident: &syn::Ident, called: &syn::Ident) -> Self {
        let path = if ident == "self" {
            prefix.to_vec()
        } else {
            longer(prefix, ident)
        };
        let called = if called == "self" {
            prefix.last().cloned()
        } else {
            Some(called.to_string())
        };
        Self {
            path,
            called: called.filter(|name| name != "_"),
            glob: false,
            span: ident.span(),
        }
    }
}

/// Every leaf of a `use` tree under a prefix, in the order written.
pub(crate) fn leaves(prefix: &[String], tree: &syn::UseTree, found: &mut Vec<Leaf>) {
    match tree {
        syn::UseTree::Path(path) => leaves(&longer(prefix, &path.ident), &path.tree, found),
        syn::UseTree::Name(name) => found.push(Leaf::named(prefix, &name.ident, &name.ident)),
        syn::UseTree::Rename(rename) => {
            found.push(Leaf::named(prefix, &rename.ident, &rename.rename));
        }
        syn::UseTree::Glob(glob) => found.push(Leaf {
            path: prefix.to_vec(),
            called: None,
            glob: true,
            span: glob.span(),
        }),
        syn::UseTree::Group(group) => {
            for item in &group.items {
                leaves(prefix, item, found);
            }
        }
    }
}

/// Every leaf of every `use` in a file, nested modules and test code too.
pub(crate) fn used(file: &syn::File) -> Vec<Leaf> {
    let mut found = Brought(Vec::new());
    found.visit_file(file);
    found.0
}

struct Brought(Vec<Leaf>);

impl<'ast> Visit<'ast> for Brought {
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        leaves(&[], &node.tree, &mut self.0);
    }
}

/// A file's production items: a test module, function or impl is left out, and an inline module
/// gives its own production items in its place.
pub(crate) fn production(items: &[syn::Item]) -> Vec<&syn::Item> {
    let mut kept = Vec::new();
    for item in items {
        match item {
            syn::Item::Mod(module) if is_test(&module.attrs) => {}
            syn::Item::Fn(function) if is_test(&function.attrs) => {}
            syn::Item::Impl(block) if is_test(&block.attrs) => {}
            syn::Item::Mod(syn::ItemMod {
                content: Some((_, inner)),
                ..
            }) => kept.extend(production(inner)),
            _ => kept.push(item),
        }
    }
    kept
}

pub(crate) fn longer(prefix: &[String], ident: &syn::Ident) -> Vec<String> {
    let mut path = prefix.to_vec();
    path.push(ident.to_string());
    path
}

/// What a call calls: the full path of a path call, or the name alone of a method call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Callee {
    Path(Vec<String>),
    Method(String),
}

impl Callee {
    /// A call through a path, read through the file's aliases; a call of any other expression,
    /// such as a closure in a field, has no callee a rule can name.
    pub(crate) fn of(call: &syn::ExprCall, aliases: &Aliases) -> Option<Self> {
        match &*call.func {
            syn::Expr::Path(path) if path.qself.is_none() => {
                Some(Self::Path(aliases.full(&path.path)))
            }
            _ => None,
        }
    }
}

/// A call a rule names: `.name` a method call, `a::b` a path call that ends in those segments,
/// and a plain name either one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pattern {
    Method(String),
    Path(Vec<String>),
    Either(String),
}

impl Pattern {
    pub(crate) fn of(call: &str) -> Option<Self> {
        let call = call.trim();
        if let Some(method) = call.strip_prefix('.') {
            let plain =
                !method.is_empty() && method.chars().all(|c| c.is_alphanumeric() || c == '_');
            return plain.then(|| Self::Method(method.to_string()));
        }
        match segments(call).as_slice() {
            [] => None,
            [one] if !call.starts_with("::") => Some(Self::Either(one.clone())),
            path => Some(Self::Path(path.to_vec())),
        }
    }

    pub(crate) fn calls(&self, callee: &Callee) -> bool {
        match (self, callee) {
            (Self::Path(path), Callee::Path(full)) => full.ends_with(path),
            (Self::Either(name), Callee::Path(full)) => full.last() == Some(name),
            (Self::Method(name) | Self::Either(name), Callee::Method(method)) => name == method,
            (Self::Method(_), Callee::Path(_)) | (Self::Path(_), Callee::Method(_)) => false,
        }
    }
}

/// A path such as `std::env::var` as its segments; a leading `::` is dropped.
pub(crate) fn segments(path: &str) -> Vec<String> {
    path.split("::")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

/// The line a span starts on, as a finding gives it.
pub(crate) fn line_of(span: proc_macro2::Span) -> u32 {
    u32::try_from(span.start().line).unwrap_or(u32::MAX)
}

/// Whether an item is test code: gated on `test`, or a `#[test]` function.
pub(crate) fn is_test(attrs: &[syn::Attribute]) -> bool {
    crate::gates::metrics::prodlines::is_test_gated(attrs)
        || attrs.iter().any(|attr| attr.path().is_ident("test"))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn full(src: &str, written: &str) -> String {
        let file = syn::parse_file(src).unwrap();
        let path: syn::Path = syn::parse_str(written).unwrap();
        Aliases::of(&file).full(&path).join("::")
    }

    #[test]
    fn a_path_is_read_through_the_names_its_file_brings_in() {
        let uses = "use std::env;\nuse std::process::{Command as Cmd, self};\n\
                    use iroh::{endpoint::{self}, Endpoint};\nuse tokio::*;\nuse a::Trait as _;\n\
                    mod inner { use std::fs::read; }\n";
        for (written, stands) in [
            ("env::var", "std::env::var"),
            ("Cmd::new", "std::process::Command::new"),
            ("process::exit", "std::process::exit"),
            ("endpoint::Builder", "iroh::endpoint::Builder"),
            ("Endpoint", "iroh::Endpoint"),
            ("read", "std::fs::read"),
            ("spawn", "spawn"),
            ("::env::var", "env::var"),
            ("crate::env", "crate::env"),
            ("iroh::Endpoint", "iroh::Endpoint"),
        ] {
            assert_eq!(full(uses, written), stands, "{written}");
        }
        assert_eq!(segments("::std::env:: var"), ["std", "env", "var"]);
        assert_eq!(segments(""), Vec::<String>::new());
    }

    #[test]
    fn a_pattern_names_a_method_a_path_by_its_last_segments_or_either() {
        let path = |full: &str| Callee::Path(segments(full));
        let method = |name: &str| Callee::Method(name.to_string());
        let calls = |pattern: &str, callee: &Callee| Pattern::of(pattern).unwrap().calls(callee);
        assert!(calls(".delete", &method("delete")));
        assert!(!calls(".delete", &path("store::delete")));
        assert!(calls("delete", &method("delete")));
        assert!(calls("delete", &path("store::delete")));
        assert!(!calls("delete", &path("store::delete::all")));
        assert!(calls("store::delete", &path("crate::store::delete")));
        assert!(!calls("store::delete", &path("delete")));
        assert!(!calls("store::delete", &method("delete")));
        assert!(calls("::delete", &path("a::delete")));
        assert!(!calls("::delete", &method("delete")));
        for refused in ["", " ", "::", ".", ".a::b", ".a b"] {
            assert_eq!(Pattern::of(refused), None, "{refused:?}");
        }
        let src = "use std::env::var as read;\nfn f() { read(\"A\"); (g.h)(); <T>::new(); }";
        let file = syn::parse_file(src).unwrap();
        let aliases = Aliases::of(&file);
        let mut callees = Vec::new();
        for expr in body(&file) {
            if let syn::Expr::Call(call) = expr {
                callees.push(Callee::of(call, &aliases));
            }
        }
        assert_eq!(callees, [Some(path("std::env::var")), None, None]);
    }

    fn body(file: &syn::File) -> Vec<&syn::Expr> {
        let Some(syn::Item::Fn(function)) = file.items.last() else {
            return Vec::new();
        };
        function
            .block
            .stmts
            .iter()
            .filter_map(|stmt| match stmt {
                syn::Stmt::Expr(expr, _) => Some(expr),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn test_code_is_an_item_gated_on_test_or_a_test_function() {
        let attrs = |src: &str| syn::parse_str::<syn::ItemFn>(src).unwrap().attrs;
        assert!(is_test(&attrs("#[test]\nfn t() {}")));
        assert!(is_test(&attrs("#[cfg(test)]\nfn t() {}")));
        assert!(!is_test(&attrs("#[inline]\nfn t() {}")));
        let item = syn::parse_str::<syn::ItemFn>("\n\nfn f() {}").unwrap();
        assert_eq!(line_of(item.sig.ident.span()), 3);
    }

    #[test]
    fn production_items_leave_out_test_code_and_open_inline_modules() {
        let file = syn::parse_file(
            "fn kept() {}\n#[test]\nfn case() {}\n#[cfg(test)]\nimpl A {}\n\
             #[cfg(test)]\nmod tests { fn t() {} }\n\
             mod inner { fn deep() {} #[test] fn deep_case() {} }\nmod outside;\nimpl B {}\n",
        )
        .unwrap();
        let names: Vec<String> = production(&file.items)
            .iter()
            .map(|item| match item {
                syn::Item::Fn(function) => function.sig.ident.to_string(),
                syn::Item::Mod(module) => format!("mod {}", module.ident),
                _ => "other".to_string(),
            })
            .collect();
        assert_eq!(names, ["kept", "deep", "mod outside", "other"]);
    }

    #[test]
    fn a_use_ends_in_each_path_it_names() {
        let item: syn::ItemUse =
            syn::parse_str("use a::{b as c,\nd::*, e::{self}, f as _};").unwrap();
        let mut found = Vec::new();
        leaves(&[], &item.tree, &mut found);
        let read: Vec<(String, Option<&str>, bool, u32)> = found
            .iter()
            .map(|leaf| {
                let called = leaf.called.as_deref();
                (leaf.path.join("::"), called, leaf.glob, line_of(leaf.span))
            })
            .collect();
        let named = |path: &str, called, glob, line| (path.to_string(), called, glob, line);
        assert_eq!(
            read,
            [
                named("a::b", Some("c"), false, 1),
                named("a::d", None, true, 2),
                named("a::e", Some("e"), false, 2),
                named("a::f", None, false, 2),
            ]
        );
    }
}
