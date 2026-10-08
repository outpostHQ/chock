//! How deep blocks nest inside each function, ratcheted apart from the complexity score.

use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

/// Depth a function may reach before it is keyed; shallower nesting is too common to be signal.
pub const SHALLOW: u32 = 4;

pub const GATE: Gate = Gate {
    name: "nesting",
    about: "how deeply blocks nest inside a function, past a depth of four",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "level(s) past four",
    },
};

fn measure(ctx: &Ctx) -> Result<Series, String> {
    let mut series = Series::new();
    for (shown, found) in crate::gates::metrics::prodlines::for_each_source(ctx, &depths)? {
        for (name, depth) in found {
            if depth > SHALLOW {
                series.set(&format!("{shown}#{name}"), u64::from(depth - SHALLOW));
            }
        }
    }
    Ok(series)
}

/// Each named function and the depth of its deepest block; the body itself is depth one.
pub fn depths(src: &str) -> Result<Vec<(String, u32)>, String> {
    let file = crate::gates::metrics::prodlines::parse_rust(src)?;
    Ok(crate::gates::metrics::complexity::bodies(&file, &|_| false)
        .into_iter()
        .map(|found| (found.name, deepest(found.body, 1)))
        .collect())
}

/// The deepest level under `block`, itself at depth `at`. Not a visitor, so it stops at closures.
fn deepest(block: &syn::Block, at: u32) -> u32 {
    block
        .stmts
        .iter()
        .map(|stmt| in_stmt(stmt, at))
        .max()
        .unwrap_or(at)
}

fn in_stmt(stmt: &syn::Stmt, at: u32) -> u32 {
    match stmt {
        syn::Stmt::Expr(expr, _) => in_expr(expr, at),
        syn::Stmt::Local(local) => local
            .init
            .as_ref()
            .map_or(at, |init| in_expr(&init.expr, at)),
        syn::Stmt::Macro(_) | syn::Stmt::Item(_) => at,
    }
}

fn in_expr(expr: &syn::Expr, at: u32) -> u32 {
    let deeper = at.saturating_add(1);
    match expr {
        syn::Expr::Block(inner) => deepest(&inner.block, deeper),
        syn::Expr::Unsafe(inner) => deepest(&inner.block, deeper),
        syn::Expr::Loop(inner) => deepest(&inner.body, deeper),
        syn::Expr::While(inner) => deepest(&inner.body, deeper).max(in_expr(&inner.cond, at)),
        syn::Expr::ForLoop(inner) => deepest(&inner.body, deeper).max(in_expr(&inner.expr, at)),
        syn::Expr::If(inner) => in_if(inner, at),
        syn::Expr::Match(inner) => inner
            .arms
            .iter()
            .map(|arm| in_expr(&arm.body, deeper))
            .max()
            .unwrap_or(deeper)
            .max(in_expr(&inner.expr, at)),
        // Not charged to the enclosing function, and `bodies` does not list a closure on its own.
        syn::Expr::Closure(_) | syn::Expr::Async(_) => at,
        syn::Expr::Try(inner) => in_expr(&inner.expr, at),
        syn::Expr::Return(inner) => inner.expr.as_ref().map_or(at, |e| in_expr(e, at)),
        syn::Expr::Let(inner) => in_expr(&inner.expr, at),
        syn::Expr::MethodCall(inner) => in_expr(&inner.receiver, at),
        _ => at,
    }
}

/// The `else` arm is read at the `if`'s own depth, so an `else if` chain stays one level deep.
fn in_if(node: &syn::ExprIf, at: u32) -> u32 {
    let body = deepest(&node.then_branch, at.saturating_add(1));
    let otherwise = node
        .else_branch
        .as_ref()
        .map_or(at, |(_, expr)| in_expr(expr, at));
    body.max(otherwise).max(in_expr(&node.cond, at))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn depth_of(src: &str) -> u32 {
        depths(src).unwrap().first().map(|(_, d)| *d).unwrap()
    }

    #[test]
    fn a_function_body_with_nothing_nested_in_it_is_one_level_deep() {
        assert_eq!(depth_of("fn f() { g(); }\n"), 1);
    }

    #[test]
    fn each_block_a_reader_steps_into_is_another_level() {
        assert_eq!(depth_of("fn f() { if a { g(); } }\n"), 2);
        assert_eq!(depth_of("fn f() { if a { if b { g(); } } }\n"), 3);
        assert_eq!(depth_of("fn f() { for x in y { while z { g(); } } }\n"), 3);
    }

    #[test]
    fn an_else_if_chain_is_as_deep_as_one_if() {
        assert_eq!(
            depth_of("fn f() { if a { g(); } else if b { g(); } else if c { g(); } }\n"),
            2
        );
    }

    #[test]
    fn a_match_arm_is_one_level_below_the_match() {
        assert_eq!(depth_of("fn f() { match a { A => g(), B => h() } }\n"), 2);
        assert_eq!(
            depth_of("fn f() { match a { A => if b { g() }, B => h() } }\n"),
            3
        );
    }

    #[test]
    fn a_closure_body_does_not_deepen_the_function_holding_it() {
        assert_eq!(
            depth_of("fn f() { let g = |x| { if a { if b { h(); } } }; }\n"),
            1
        );
    }

    #[test]
    fn the_deepest_branch_is_the_one_reported_not_the_last() {
        assert_eq!(
            depth_of("fn f() { if a { if b { if c { g(); } } } if d { h(); } }\n"),
            4
        );
    }

    #[test]
    fn every_function_in_a_file_is_measured() {
        let found = depths("fn a() { if x { g(); } }\nfn b() { g(); }\n").unwrap();
        assert_eq!(found, vec![("a".to_string(), 2), ("b".to_string(), 1)]);
    }

    #[test]
    fn a_file_the_parser_rejects_stops_the_gate() {
        assert!(depths("fn f( {\n").is_err());
    }
}
