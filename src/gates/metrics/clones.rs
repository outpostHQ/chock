//! `.clone()` abuse: one chained on another, or made on every pass through a loop. A clone whose
//! source goes unused after it, clippy's own `redundant_clone` already catches more precisely.

use syn::visit::Visit;
use syn::{Expr, ExprForLoop, ExprLoop, ExprMethodCall, ExprWhile};

use crate::gates::rules::{is_test, line_of};
use crate::run::baseline::Keys;
use crate::run::{Ctx, Gate, Group, Kind, Measurement};

use super::prodlines::{IntoHits, for_each_source, tallied, visited};

/// The `clone-abuse` gate: a `.clone()` chained on another, or inside a loop.
pub const GATE: Gate = Gate {
    name: "clone-abuse",
    about: "a `.clone()` chained on another clone, or made on every pass through a loop",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::AnnotatedRatchet {
        measure,
        keys: Keys::Items,
        unit: "clone(s)",
    },
};

fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let read = for_each_source(ctx, &|src| hits(src))?;
    let (series, details) = tallied(read, "clone");
    Ok(Measurement::detailed(series, details))
}

/// Every abusive `.clone()` in a file's production code, by pattern and line.
fn hits(src: &str) -> Result<Vec<(&'static str, u32)>, String> {
    visited(
        src,
        Clones {
            depth: 0,
            hits: Vec::new(),
        },
    )
}

struct Clones {
    /// How many loops this point in the walk sits inside; a closure or a nested function defined
    /// in a loop does not leave it, since calling the clone still costs one pass.
    depth: u32,
    hits: Vec<(&'static str, u32)>,
}

impl IntoHits for Clones {
    fn into_hits(self) -> Vec<(&'static str, u32)> {
        self.hits
    }
}

impl Clones {
    fn within_loop(&mut self, body: impl FnOnce(&mut Self)) {
        self.depth += 1;
        body(self);
        self.depth -= 1;
    }
}

impl<'ast> Visit<'ast> for Clones {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !is_test(&node.attrs) {
            syn::visit::visit_item_fn(self, node);
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !is_test(&node.attrs) {
            syn::visit::visit_impl_item_fn(self, node);
        }
    }

    fn visit_expr_for_loop(&mut self, node: &'ast ExprForLoop) {
        self.within_loop(|me| syn::visit::visit_expr_for_loop(me, node));
    }

    fn visit_expr_while(&mut self, node: &'ast ExprWhile) {
        self.within_loop(|me| syn::visit::visit_expr_while(me, node));
    }

    fn visit_expr_loop(&mut self, node: &'ast ExprLoop) {
        self.within_loop(|me| syn::visit::visit_expr_loop(me, node));
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if node.method == "clone" {
            let chained = matches!(
                &*node.receiver,
                Expr::MethodCall(inner) if inner.method == "clone"
            );
            let pattern = if chained {
                Some("clone-chain")
            } else if self.depth > 0 {
                Some("clone-in-loop")
            } else {
                None
            };
            if let Some(pattern) = pattern {
                self.hits.push((pattern, line_of(node.method.span())));
            }
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(src: &str, pattern: &str) -> Vec<u32> {
        hits(src)
            .unwrap()
            .into_iter()
            .filter(|(found, _)| *found == pattern)
            .map(|(_, line)| line)
            .collect()
    }

    #[test]
    fn a_clone_made_on_another_clone_is_a_chain() {
        let src = "fn f(a: &A) -> A {\n    a.clone().clone()\n}\n";
        assert_eq!(lines(src, "clone-chain"), [2]);
        assert_eq!(lines(src, "clone-in-loop"), Vec::<u32>::new());
    }

    #[test]
    fn a_clone_made_once_is_neither_pattern() {
        let src = "fn f(a: &A) -> A {\n    a.clone()\n}\n";
        assert_eq!(lines(src, "clone-chain"), Vec::<u32>::new());
        assert_eq!(lines(src, "clone-in-loop"), Vec::<u32>::new());
    }

    #[test]
    fn a_clone_inside_a_for_loop_is_made_every_pass() {
        let src =
            "fn f(a: &A, xs: &[u8]) {\n    for _ in xs {\n        let _ = a.clone();\n    }\n}\n";
        assert_eq!(lines(src, "clone-in-loop"), [3]);
    }

    #[test]
    fn a_clone_inside_a_while_or_a_bare_loop_is_also_in_loop() {
        let while_src = "fn f(a: &A, mut n: u8) {\n    while n > 0 {\n        let _ = a.clone();\n        n -= 1;\n    }\n}\n";
        assert_eq!(lines(while_src, "clone-in-loop"), [3]);
        let loop_src =
            "fn f(a: &A) {\n    loop {\n        let _ = a.clone();\n        break;\n    }\n}\n";
        assert_eq!(lines(loop_src, "clone-in-loop"), [3]);
    }

    #[test]
    fn a_clone_after_a_loop_is_not_in_one() {
        let src = "fn f(a: &A, xs: &[u8]) {\n    for _ in xs {}\n    let _ = a.clone();\n}\n";
        assert_eq!(lines(src, "clone-in-loop"), Vec::<u32>::new());
    }

    #[test]
    fn a_chain_inside_a_loop_still_reads_as_a_chain() {
        let src = "fn f(a: &A, xs: &[u8]) {\n    for _ in xs {\n        let _ = a.clone().clone();\n    }\n}\n";
        assert_eq!(lines(src, "clone-chain"), [3]);
        assert_eq!(lines(src, "clone-in-loop"), [3]);
    }

    #[test]
    fn a_closure_made_inside_a_loop_does_not_leave_it() {
        let src = "fn f(a: &A, xs: &[u8]) {\n    for _ in xs {\n        let g = || a.clone();\n        g();\n    }\n}\n";
        assert_eq!(lines(src, "clone-in-loop"), [3]);
    }

    #[test]
    fn a_clone_in_a_test_function_is_not_counted() {
        let src = "#[test]\nfn t(a: &A) {\n    for _ in 0..1 {\n        let _ = a.clone().clone();\n    }\n}\n";
        assert_eq!(hits(src).unwrap(), Vec::<(&str, u32)>::new());
    }

    #[test]
    fn a_clone_in_a_test_impl_method_is_not_counted() {
        let src = "struct S;\nimpl S {\n    #[test]\n    fn t(&self, a: &A) {\n        let _ = a.clone().clone();\n    }\n}\n";
        assert_eq!(hits(src).unwrap(), Vec::<(&str, u32)>::new());
    }

    #[test]
    fn a_clone_in_a_non_test_impl_method_is_counted() {
        let src = "struct S;\nimpl S {\n    fn f(&self, a: &A) -> A {\n        a.clone().clone()\n    }\n}\n";
        assert_eq!(lines(src, "clone-chain"), [4]);
    }

    #[test]
    fn a_method_call_that_is_not_clone_is_ignored() {
        let src = "fn f(a: &A) -> A {\n    a.other()\n}\n";
        assert_eq!(hits(src).unwrap(), Vec::<(&str, u32)>::new());
    }

    #[test]
    fn a_tree_with_no_abusive_clone_holds_nothing() {
        let src = "fn f(a: &A) -> A {\n    a.clone()\n}\nfn g() -> u8 {\n    1 + 1\n}\n";
        assert_eq!(hits(src).unwrap(), Vec::<(&str, u32)>::new());
    }
}
