//! The walk over one file's items that sorts its functions, arms and traits into forwarders and
//! candidates.

use std::collections::HashMap;

use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Block, Expr, ExprMatch, FnArg, Ident, ImplItem, ImplItemFn, ItemFn, ItemImpl,
    ItemMod, ItemTrait, Pat, Signature, Stmt, Visibility,
};

use super::repeats::Facts;
use super::{Forwarder, Read, Uses};
use crate::gates::metrics::prodlines;
use crate::run::report::{Finding, Place};

/// The most lines past its first that a one-caller function may span and still read as well
/// inline; a longer one has earned its name.
const SHORT: u32 = 4;

/// A function with a body, wherever it is written: its attributes, visibility, signature and body.
struct Fun<'a>(&'a [Attribute], &'a Visibility, &'a Signature, &'a Block);

/// Walks the items outside `#[cfg(test)]` and sorts what it meets into forwarders and candidates.
pub(super) struct Lean<'a> {
    pub(super) shown: &'a str,
    pub(super) uses: &'a Uses,
    pub(super) below: &'a dyn Fn(&str) -> bool,
    pub(super) read: Read,
}

impl<'ast> Visit<'ast> for Lean<'_> {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        if !prodlines::is_test_gated(&node.attrs) {
            visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        if !prodlines::is_test_gated(&node.attrs) {
            self.function(&Fun(&node.attrs, &node.vis, &node.sig, &node.block));
            visit::visit_item_fn(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        if prodlines::is_test_gated(&node.attrs) {
            return;
        }
        for item in &node.items {
            match item {
                ImplItem::Fn(method) => self.method(method, node.trait_.is_none()),
                _ => visit::visit_impl_item(self, item),
            }
        }
    }

    fn visit_item_trait(&mut self, node: &'ast ItemTrait) {
        if !prodlines::is_test_gated(&node.attrs) {
            self.one_impl(node);
            visit::visit_item_trait(self, node);
        }
    }

    fn visit_expr_match(&mut self, node: &'ast ExprMatch) {
        self.same_arms(node);
        visit::visit_expr_match(self, node);
    }
}

impl Lean<'_> {
    /// A method outside test code: its body, and the method itself where it is no trait's.
    fn method(&mut self, method: &ImplItemFn, own: bool) {
        if prodlines::is_test_gated(&method.attrs) {
            return;
        }
        if own {
            self.function(&Fun(&method.attrs, &method.vis, &method.sig, &method.block));
        }
        visit::visit_impl_item_fn(self, method);
    }

    fn function(&mut self, fun @ &Fun(_, _, sig, block): &Fun<'_>) {
        let name = sig.ident.to_string();
        let line = line_of(sig.ident.span().start().line);
        let last = line_of(block.brace_token.span.close().end().line);
        let Some(callee) = forwarded(sig, block) else {
            if self.one_caller(fun, last.saturating_sub(line)) {
                let message = format!("`{name}` has one statement and one caller");
                let fix = format!("move the statement of `{name}` into its caller, then remove it");
                self.candidate(Finding::at(self.shown, &message).line(line), fix);
            }
            return;
        };
        match self.unsettled(fun) {
            None => self.read.forwarders.push(Forwarder {
                name,
                callee,
                line,
                last,
            }),
            Some(why) => {
                let message =
                    format!("`{name}` only passes its parameters to `{callee}`, but {why}");
                let fix = format!("call `{callee}` where `{name}` is called if the types agree");
                self.candidate(Finding::at(self.shown, &message).line(line), fix);
            }
        }
    }

    /// Why the source cannot settle a forwarder, or `None` where it can.
    fn unsettled(&self, &Fun(attrs, vis, sig, _): &Fun<'_>) -> Option<&'static str> {
        let reasons = [
            (is_public(vis), "it is visible outside its module"),
            (!attrs.iter().all(is_doc), "it carries an attribute"),
            (!sig.generics.params.is_empty(), "it is generic"),
            (
                !self.only_called(&sig.ident.to_string()),
                "its name is used other than in a call",
            ),
        ];
        reasons
            .into_iter()
            .find_map(|(holds, why)| holds.then_some(why))
    }

    /// A short private function of one statement, called once and named nowhere else.
    fn one_caller(&self, &Fun(attrs, vis, sig, block): &Fun<'_>, spans: u32) -> bool {
        let name = sig.ident.to_string();
        !is_public(vis)
            && attrs.iter().all(is_doc)
            && block.stmts.len() == 1
            && spans <= SHORT
            && count(&self.uses.calls, &name) == 1
            && count(&self.uses.defs, &name) == 1
            && self.only_called(&name)
    }

    /// Every use of `name` in the file is a call, and no file below its module names it.
    fn only_called(&self, name: &str) -> bool {
        let named = count(&self.uses.defs, name) + count(&self.uses.calls, name);
        !(self.below)(name) && count(&self.uses.words, name) == named
    }

    /// Each arm whose body the arm just above repeats, where neither binds a name nor has a guard:
    /// a `|` joins such arms whatever the types, and no arm between them can change what matches.
    fn same_arms(&mut self, node: &ExprMatch) {
        let mut above: Option<(u32, String)> = None;
        for arm in &node.arms {
            // A capitalised name is a variant or a constant, and binds nothing.
            let mut facts = Facts::default();
            facts.visit_pat(&arm.pat);
            let binds = facts
                .bound
                .iter()
                .any(|name| !name.starts_with(char::is_uppercase));
            let open = !binds && !matches!(arm.pat, Pat::Guard(_) | Pat::Wild(_));
            let body = arm
                .body
                .span()
                .source_text()
                .and_then(|text| text.parse::<proc_macro2::TokenStream>().ok());
            let this = body
                .filter(|_| open)
                .map(|body| (line_of(arm.pat.span().start().line), body.to_string()));
            if let (Some((earlier, before)), Some((line, now))) = (&above, &this)
                && before == now
            {
                let message = format!("this arm's body is the same as the arm's at line {earlier}");
                let mut finding = Finding::at(self.shown, &message).line(*line);
                finding.places = vec![Place::at("same body", self.shown, *earlier)];
                let fix = "join the two patterns with `|` into one arm".to_string();
                self.candidate(finding, fix);
            }
            above = this;
        }
    }

    /// A private trait the file implements for one type only, which an inherent `impl` could hold
    /// where the file defines that type.
    fn one_impl(&mut self, node: &ItemTrait) {
        let name = node.ident.to_string();
        if is_public(&node.vis) || (self.below)(&name) {
            return;
        }
        let impls = self.uses.impls.get(&name).map(Vec::as_slice);
        if let Some([only]) = impls
            && self.uses.types.contains(only)
        {
            let message = format!("only `{only}` implements `{name}`");
            let line = line_of(node.ident.span().start().line);
            let fix = format!("move the methods of `{name}` into an inherent `impl {only}`");
            self.candidate(Finding::at(self.shown, &message).line(line), fix);
        }
    }

    fn candidate(&mut self, finding: Finding, fix: String) {
        let mut finding = finding.candidate();
        finding.fix = Some(fix);
        self.read.candidates.push(finding);
    }
}

/// The call a body makes when all it does is pass the parameters on, in order and unchanged; a
/// parameter that is a pattern rather than a name is never passed on as it came.
fn forwarded(sig: &Signature, block: &Block) -> Option<String> {
    let [Stmt::Expr(tail, None)] = block.stmts.as_slice() else {
        return None;
    };
    let (callee, args) = called(tail, &sig.ident)?;
    let passed = sig.asyncness.is_none()
        && !args.is_empty()
        && args.len() == sig.inputs.len()
        && args.iter().zip(&sig.inputs).all(|(arg, input)| match input {
            FnArg::Receiver(_) => is_named(arg, "self"),
            FnArg::Typed(typed) => matches!(&*typed.pat, Pat::Ident(it)
                if it.by_ref.is_none() && it.subpat.is_none() && is_named(arg, &it.ident.to_string())),
        });
    passed.then_some(callee)
}

/// The callee and arguments of a call to another function, or of a method called on `self` with
/// the receiver first; `None` for any other expression, a turbofish, or a call to `own`.
fn called<'a>(tail: &'a Expr, own: &Ident) -> Option<(String, Vec<&'a Expr>)> {
    match tail {
        Expr::Call(call) => {
            let Expr::Path(path) = &*call.func else {
                return None;
            };
            let segments = &path.path.segments;
            let plain = path.qself.is_none() && segments.iter().all(|it| it.arguments.is_none());
            let first = &segments.first()?.ident;
            let recursive =
                segments.last()?.ident == *own && (segments.len() == 1 || *first == "Self");
            let callee: Vec<String> = segments.iter().map(|it| it.ident.to_string()).collect();
            (plain && !recursive).then(|| (callee.join("::"), call.args.iter().collect()))
        }
        Expr::MethodCall(call)
            if call.turbofish.is_none()
                && call.method != *own
                && is_named(&call.receiver, "self") =>
        {
            let args = std::iter::once(&*call.receiver).chain(&call.args);
            Some((format!("self.{}", call.method), args.collect()))
        }
        _ => None,
    }
}

fn is_named(expr: &Expr, name: &str) -> bool {
    matches!(expr, Expr::Path(path) if path.qself.is_none() && path.path.is_ident(name))
}

fn is_public(vis: &Visibility) -> bool {
    !matches!(vis, Visibility::Inherited)
}

fn is_doc(attr: &Attribute) -> bool {
    attr.path().is_ident("doc")
}

fn count(map: &HashMap<String, usize>, word: &str) -> usize {
    map.get(word).copied().unwrap_or(0)
}

fn line_of(line: usize) -> u32 {
    u32::try_from(line).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    /// The callee `called` finds in `src` from inside a function named `f`, and its argument count.
    fn calls(src: &str) -> Option<(String, usize)> {
        let tail: Expr = syn::parse_str(src).unwrap();
        let own: Ident = syn::parse_str("f").unwrap();
        called(&tail, &own).map(|(callee, args)| (callee, args.len()))
    }

    fn forwards(src: &str) -> Option<String> {
        let item: ImplItemFn = syn::parse_str(src).unwrap();
        forwarded(&item.sig, &item.block)
    }

    #[test]
    fn a_plain_call_or_a_method_on_self_names_its_callee_and_counts_the_receiver() {
        assert_eq!(calls("g::h(a, b)"), Some(("g::h".to_string(), 2)));
        assert_eq!(calls("self.g(a)"), Some(("self.g".to_string(), 2)));
        assert_eq!(calls("other::f(a)"), Some(("other::f".to_string(), 1)));
    }

    #[test]
    fn a_call_to_itself_a_turbofish_or_another_receiver_is_no_callee() {
        for src in [
            "f(a)",
            "Self::f(a)",
            "self.f(a)",
            "g::<u8>(a)",
            "self.g::<u8>(a)",
            "<T as R>::g(a)",
            "other.g(a)",
            "a + b",
        ] {
            assert_eq!(calls(src), None, "{src}");
        }
    }

    #[test]
    fn a_body_that_passes_each_parameter_on_in_order_forwards_them() {
        let both = "fn f(a: u8, b: u8) -> u8 { g::h(a, b) }";
        assert_eq!(forwards(both), Some("g::h".to_string()));
        let method = "fn f(&self, a: u8) -> u8 { self.g(a) }";
        assert_eq!(forwards(method), Some("self.g".to_string()));
    }

    #[test]
    fn a_body_that_reorders_drops_or_changes_a_parameter_forwards_nothing() {
        for src in [
            "fn f(a: u8, b: u8) -> u8 { g(b, a) }",
            "fn f(a: u8, b: u8) -> u8 { g(a) }",
            "fn f(a: u8) -> u8 { g(a + 1) }",
            "fn f(ref a: u8) -> u8 { g(a) }",
            "async fn f(a: u8) -> u8 { g(a) }",
            "fn f() -> u8 { g() }",
            "fn f(a: u8) { g(a); }",
            "fn f(a: u8) -> u8 { let b = a; g(b) }",
        ] {
            assert_eq!(forwards(src), None, "{src}");
        }
    }
}
