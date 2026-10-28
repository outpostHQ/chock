//! The walk over one file's items that sorts its functions, arms and traits into forwarders and
//! candidates.

use std::collections::HashMap;

use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Arm, Attribute, Block, Expr, ExprMatch, FnArg, Ident, ImplItem, ImplItemFn, ItemFn, ItemImpl,
    ItemMod, ItemTrait, Pat, Signature, Stmt, Visibility,
};

use super::repeats::Facts;
use super::{Forwarder, Read, Uses};
use crate::gates::metrics::prodlines;
use crate::run::report::{Finding, Place};

/// The most lines past its first that a one-caller function may span and still read as well
/// inline; a longer one has earned its name.
const SHORT: u32 = 4;

/// The widest line rustfmt writes unless a project sets `max_width`.
const WIDTH: usize = 100;

/// A function with a body, wherever it is written: its attributes, visibility, signature and body.
struct Fun<'a>(&'a [Attribute], &'a Visibility, &'a Signature, &'a Block);

/// An arm as the arm below sees it: its first line, its body, and the lines a join would take.
struct Shape {
    line: u32,
    body: String,
    /// The pattern's width where it takes one line, and its lines with one alternative to a line.
    width: Option<usize>,
    stacked: u32,
    /// The arm's lines, its lines from the pattern's last line on, and its first line's width.
    lines: u32,
    tail: u32,
    first: usize,
}

impl Shape {
    /// `arm` with its body as tokens, in a file whose lines have the `widths` given.
    fn new(arm: &Arm, body: String, widths: &[usize]) -> Self {
        let pat = arm.pat.span();
        let (start, end) = (pat.start().line, arm.body.span().end().line);
        let width = pat.source_text().filter(|text| !text.contains('\n'));
        let alternatives = match &arm.pat {
            Pat::Or(or) => or.cases.len(),
            _ => 1,
        };
        let first = start.checked_sub(1).and_then(|at| widths.get(at));
        Self {
            line: line_of(start),
            body,
            width: width.map(|text| text.chars().count()),
            stacked: line_of(alternatives.max(pat.end().line.saturating_sub(start) + 1)),
            lines: line_of(end.saturating_sub(start) + 1),
            tail: line_of(end.saturating_sub(pat.end().line) + 1),
            first: first.copied().unwrap_or(WIDTH),
        }
    }

    /// The most lines rustfmt gives one arm that holds the patterns of `earlier` and this arm.
    fn joined(&self, earlier: &Self) -> u32 {
        match (earlier.width, self.width) {
            (Some(width), Some(_)) if self.first + width + " | ".len() <= WIDTH => self.tail,
            _ => earlier.stacked + self.stacked + self.tail,
        }
    }
}

/// Walks the items outside `#[cfg(test)]` and sorts what it meets into forwarders and candidates.
pub(super) struct Lean<'a> {
    pub(super) shown: &'a str,
    pub(super) widths: Vec<usize>,
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

    fn function(&mut self, fun @ &Fun(_, vis, sig, block): &Fun<'_>) {
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
        // Code outside the module may call it, and this file shows none of those calls.
        if is_public(vis) {
            return;
        }
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
    fn unsettled(&self, &Fun(attrs, _, sig, _): &Fun<'_>) -> Option<&'static str> {
        let reasons = [
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

    /// Each arm whose body the arm just above repeats, where neither binds a name nor has a guard
    /// and one arm with both patterns takes fewer lines: a `|` joins them whatever the types.
    fn same_arms(&mut self, node: &ExprMatch) {
        let mut above: Option<Shape> = None;
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
                .map(|body| Shape::new(arm, body.to_string(), &self.widths));
            if let (Some(earlier), Some(now)) = (&above, &this)
                && earlier.body == now.body
                && now.joined(earlier) < earlier.lines + now.lines
            {
                let message = format!(
                    "this arm's body is the same as the arm's at line {}",
                    earlier.line
                );
                let mut finding = Finding::at(self.shown, &message).line(now.line);
                finding.places = vec![Place::at("same body", self.shown, earlier.line)];
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
pub(crate) fn forwarded(sig: &Signature, block: &Block) -> Option<String> {
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

    /// Each arm of the match `src` as the arm below sees it.
    fn shapes(src: &str) -> Vec<String> {
        let node: ExprMatch = syn::parse_str(src).unwrap();
        let widths: Vec<usize> = src.lines().map(|line| line.chars().count()).collect();
        let shown = |it: Shape| {
            let Shape {
                line,
                width,
                stacked,
                lines,
                tail,
                first,
                ..
            } = it;
            format!("{line} {width:?} {stacked} {lines} {tail} {first}")
        };
        let arms = node.arms.iter();
        arms.map(|arm| shown(Shape::new(arm, String::new(), &widths)))
            .collect()
    }

    #[test]
    fn an_arm_knows_its_lines_its_alternatives_and_the_width_of_its_first_line() {
        let src = "match x {\n    A | B => 1,\n    C\n    | D => {\n        2\n    }\n    S {\n        \
                   a: 1,\n    } => 3,\n}";
        assert_eq!(
            shapes(src),
            ["2 Some(5) 2 1 1 15", "3 None 2 4 3 5", "7 None 3 3 1 7"]
        );
    }

    #[test]
    fn one_arm_puts_both_patterns_on_one_line_only_where_the_line_fits() {
        let shape = |width, first| Shape {
            line: 1,
            body: String::new(),
            width,
            stacked: 2,
            lines: 3,
            tail: 4,
            first,
        };
        let earlier = Shape {
            stacked: 3,
            ..shape(Some(10), 0)
        };
        // 87 columns, the 10 of the pattern above, and ` | ` make the 100 rustfmt allows.
        let joins = [
            shape(Some(1), 20).joined(&earlier),
            shape(Some(1), 87).joined(&earlier),
            shape(Some(1), 88).joined(&earlier),
            shape(None, 0).joined(&earlier),
            shape(Some(1), 0).joined(&shape(None, 0)),
        ];
        assert_eq!(joins, [4, 4, 9, 9, 8]);
    }
}
